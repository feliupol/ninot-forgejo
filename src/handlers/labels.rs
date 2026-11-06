//! Labels of a repository, and the labels its issues carry.
//!
//! Labels belong to a repository and are managed by whoever may write to it, as Forgejo's
//! `reqRepoWriter` requires. Issues carry labels by id, and the label operations name them
//! either way.

use std::sync::Mutex;

use serde::Deserialize;

use super::Handler;
use super::issues::{LabelReference, label_ids};
use super::repos::{Found, find_repo, find_writable_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult, PatchField, paginate};
use crate::operation::ops;
use crate::parse;
use crate::render;
use crate::state::{NewLabel, State, lock, now};
use crate::types;

/// Longest label name Forgejo accepts.
const NAME_LIMIT: usize = 50;

/// Colour a label is given when the request names none.
const DEFAULT_COLOR: &str = "ededed";

/// Label-create input with optional fields so the handler can keep its defaults and validation
/// responses while still decoding typed JSON fields.
#[derive(Deserialize)]
struct CreateLabelInput {
    /// Name to give the label.
    #[serde(default)]
    name: PatchField<String>,
    /// Hex color; omitted colors use [`DEFAULT_COLOR`].
    #[serde(default)]
    color: PatchField<String>,
    /// Optional description.
    description: Option<String>,
    /// Whether the label is exclusive.
    exclusive: Option<bool>,
    /// Whether the label is archived.
    is_archived: Option<bool>,
}

/// A label edit's fields, preserving presence for name and color validation.
#[derive(Deserialize)]
struct EditLabelInput {
    /// Label name.
    #[serde(default)]
    name: PatchField<String>,
    /// Hex color.
    #[serde(default)]
    color: PatchField<String>,
    /// Label description.
    description: Option<String>,
    /// Exclusive label flag.
    exclusive: Option<bool>,
    /// Archived label flag.
    is_archived: Option<bool>,
}

/// Label ids or names to add or replace on an issue.
#[derive(Deserialize)]
struct IssueLabelsInput {
    /// Labels named by id or name; omission or null means an empty list.
    labels: Option<Vec<LabelReference>>,
}

/// The label `request` addresses, which a path parameter that is not an id is a 404.
fn label_id(request: &ApiRequest) -> Result<u64, ApiResponse> {
    request
        .param("id")
        .parse()
        .map_err(|_| ApiResponse::not_found())
}

/// The API URL of `found`, which labels are named after.
fn api_of(request: &ApiRequest, found: &Found) -> String {
    format!("{}/repos/{}/{}", request.api_url, found.owner, found.name)
}

/// Renders the label `id` of `found`, which the caller may see.
fn rendered(state: &State, request: &ApiRequest, found: &Found, id: u64) -> types::Label {
    let api = api_of(request, found);
    let label = state
        .tracker(&found.owner, &found.name)
        .label(id)
        .expect("the label was found");
    render::label(&api, label)
}

/// The name of a label, from a create or an edit request, or `None` when the body leaves it
/// alone.
fn wanted_name(field: PatchField<String>) -> Result<Option<String>, ApiResponse> {
    let name = match field {
        PatchField::Missing => return Ok(None),
        PatchField::Null => "",
        PatchField::Value(name) => return wanted_name_value(name),
    };
    wanted_name_value(name.to_owned())
}

fn wanted_name_value(name: String) -> Result<Option<String>, ApiResponse> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_LIMIT {
        return Err(ApiResponse::error(422, "invalid label name"));
    }
    Ok(Some(name.to_owned()))
}

/// The colour of a label, as Forgejo stores it: lowercase hexadecimal digits, with the `#`
/// clients may write removed. `None` when the body leaves the colour alone.
fn wanted_color(field: PatchField<String>) -> Result<Option<String>, ApiResponse> {
    let written = match field {
        PatchField::Missing => return Ok(None),
        PatchField::Null => "".to_owned(),
        PatchField::Value(written) => written,
    };
    parse::color(&written)
        .map(Some)
        .map_err(|written| ApiResponse::error(422, &format!("invalid colour {written}")))
}

/// The fields of a label an edit sets, which it sets only when the request names them.
fn wanted_flags(body: &EditLabelInput) -> (Option<String>, Option<bool>, Option<bool>) {
    (body.description.clone(), body.exclusive, body.is_archived)
}

impl Handler for ops::IssueListLabels {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let api = api_of(request, &found);
        let items = state
            .tracker(&found.owner, &found.name)
            .labels()
            .into_iter()
            .map(|label| render::label(&api, label))
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::IssueCreateLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let body: CreateLabelInput = request.json_as_with_status(422)?;
        let name =
            wanted_name(body.name)?.ok_or_else(|| ApiResponse::error(422, "name is required"))?;
        let color = match body.color {
            PatchField::Missing => DEFAULT_COLOR.to_owned(),
            value => wanted_color(value)?.unwrap_or_else(|| DEFAULT_COLOR.to_owned()),
        };
        let description = body.description.unwrap_or_default();
        let exclusive = body.exclusive.unwrap_or(false);
        let is_archived = body.is_archived.unwrap_or(false);
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        if state
            .tracker(&found.owner, &found.name)
            .label_id(&name)
            .is_some()
        {
            return Err(ApiResponse::error(
                422,
                &format!("label {name} already exists"),
            ));
        }
        let id = state.next_id();
        state.tracker_mut(&found.owner, &found.name).add_label(
            id,
            NewLabel {
                name,
                color,
                description,
                exclusive,
                is_archived,
            },
        );
        Ok(ApiResponse::json(
            201,
            rendered(&state, request, &found, id),
        ))
    }
}

impl Handler for ops::IssueGetLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let id = label_id(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        if state.tracker(&found.owner, &found.name).label(id).is_none() {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::ok(rendered(&state, request, &found, id)))
    }
}

impl Handler for ops::IssueEditLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let body: EditLabelInput = request.json_as_with_status(422)?;
        let id = label_id(request)?;
        let name = wanted_name(body.name.clone())?;
        let color = wanted_color(body.color.clone())?;
        let (description, exclusive, is_archived) = wanted_flags(&body);
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let tracker = state.tracker_mut(&found.owner, &found.name);
        if let Some(name) = &name
            && tracker.label_id(name).is_some_and(|taken| taken != id)
        {
            return Err(ApiResponse::error(
                422,
                &format!("label {name} already exists"),
            ));
        }
        if tracker.label(id).is_none() {
            return Err(ApiResponse::not_found());
        }
        let label = tracker.label_mut(id).expect("the label exists");
        if let Some(name) = name {
            label.name = name;
        }
        if let Some(color) = color {
            label.color = color;
        }
        if let Some(description) = description {
            label.description = description;
        }
        if let Some(exclusive) = exclusive {
            label.exclusive = exclusive;
        }
        if let Some(is_archived) = is_archived {
            label.is_archived = is_archived;
        }
        label.updated_at = now();
        Ok(ApiResponse::ok(rendered(&state, request, &found, id)))
    }
}

impl Handler for ops::IssueDeleteLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let id = label_id(request)?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        if state
            .tracker_mut(&found.owner, &found.name)
            .remove_label(id)
            .is_none()
        {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::no_content())
    }
}

/// The labels of the issue `index` of `found`, which the caller may see.
fn of_issue(state: &State, request: &ApiRequest, found: &Found, index: u64) -> Vec<types::Label> {
    let api = api_of(request, found);
    let tracker = state.tracker(&found.owner, &found.name);
    tracker
        .issue(index)
        .expect("the issue was found")
        .labels
        .iter()
        .filter_map(|id| tracker.label(*id))
        .map(|label| render::label(&api, label))
        .collect()
}

/// Resolves the issue the request labels, which is one the repository has.
fn issue_of(state: &State, request: &ApiRequest, found: &Found) -> Result<u64, ApiResponse> {
    let index = super::issues::issue_index(request)?;
    state
        .tracker(&found.owner, &found.name)
        .issue(index)
        .is_some()
        .then_some(index)
        .ok_or_else(ApiResponse::not_found)
}

/// The labels `body` names, which must be labels the repository has.
fn wanted_labels(
    state: &State,
    found: &Found,
    labels: &[LabelReference],
) -> Result<Vec<u64>, ApiResponse> {
    let mut wanted = Vec::new();
    for id in label_ids(state.tracker(&found.owner, &found.name), labels)? {
        if !wanted.contains(&id) {
            wanted.push(id);
        }
    }
    Ok(wanted)
}

impl Handler for ops::IssueGetLabels {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let index = issue_of(&state, request, &found)?;
        Ok(ApiResponse::ok(of_issue(&state, request, &found, index)))
    }
}

impl Handler for ops::IssueAddLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let body: IssueLabelsInput = request.json_as_with_status(422)?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let index = issue_of(&state, request, &found)?;
        let wanted = wanted_labels(&state, &found, body.labels.as_deref().unwrap_or_default())?;
        let issue = state
            .tracker_mut(&found.owner, &found.name)
            .issue_mut(index)
            .expect("the issue was found");
        for id in wanted {
            if !issue.labels.contains(&id) {
                issue.labels.push(id);
            }
        }
        Ok(ApiResponse::ok(of_issue(&state, request, &found, index)))
    }
}

impl Handler for ops::IssueReplaceLabels {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let body: IssueLabelsInput = request.json_as_with_status(422)?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let index = issue_of(&state, request, &found)?;
        let wanted = wanted_labels(&state, &found, body.labels.as_deref().unwrap_or_default())?;
        state
            .tracker_mut(&found.owner, &found.name)
            .issue_mut(index)
            .expect("the issue was found")
            .labels = wanted;
        Ok(ApiResponse::ok(of_issue(&state, request, &found, index)))
    }
}

impl Handler for ops::IssueClearLabels {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let index = issue_of(&state, request, &found)?;
        state
            .tracker_mut(&found.owner, &found.name)
            .issue_mut(index)
            .expect("the issue was found")
            .labels
            .clear();
        Ok(ApiResponse::no_content())
    }
}

impl Handler for ops::IssueRemoveLabel {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let identifier = request.param("identifier");
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let index = issue_of(&state, request, &found)?;
        let removed = identifier
            .parse()
            .ok()
            .or_else(|| {
                state
                    .tracker(&found.owner, &found.name)
                    .label_id(identifier)
            })
            .ok_or_else(ApiResponse::not_found)?;
        state
            .tracker_mut(&found.owner, &found.name)
            .issue_mut(index)
            .expect("the issue was found")
            .labels
            .retain(|id| *id != removed);
        Ok(ApiResponse::no_content())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tracker;
    use serde_json::json;

    #[test]
    fn a_colour_is_stored_the_way_forgejo_stores_it() {
        assert_eq!(wanted_color(PatchField::Missing).unwrap(), None);
        assert_eq!(
            wanted_color(PatchField::Value("00aabb".to_owned()))
                .unwrap()
                .unwrap(),
            "00aabb"
        );
        assert_eq!(
            wanted_color(PatchField::Value("#00aabb".to_owned()))
                .unwrap()
                .unwrap(),
            "00aabb"
        );
        assert_eq!(
            wanted_color(PatchField::Value("ABC".to_owned()))
                .unwrap()
                .unwrap(),
            "abc"
        );
        assert!(wanted_color(PatchField::Value("not a colour".to_owned())).is_err());
        assert!(wanted_color(PatchField::Value("#12345".to_owned())).is_err());
    }

    #[test]
    fn a_name_must_be_given_and_may_not_be_too_long() {
        assert_eq!(wanted_name(PatchField::Missing).unwrap(), None);
        assert_eq!(
            wanted_name(PatchField::Value(" bug ".to_owned()))
                .unwrap()
                .unwrap(),
            "bug"
        );
        assert!(wanted_name(PatchField::Value(" ".to_owned())).is_err());
        let long = "x".repeat(NAME_LIMIT + 1);
        assert!(wanted_name(PatchField::Value(long)).is_err());
        let longest = "x".repeat(NAME_LIMIT);
        assert_eq!(
            wanted_name(PatchField::Value(longest.clone()))
                .unwrap()
                .unwrap(),
            longest
        );
    }

    #[test]
    fn a_label_is_rendered_with_its_url() {
        let mut tracker = Tracker::default();
        tracker.add_label(
            3,
            NewLabel {
                name: "bug".to_owned(),
                color: "ff0000".to_owned(),
                description: String::new(),
                exclusive: false,
                is_archived: false,
            },
        );
        let rendered = render::label(
            "http://h/api/v1/repos/alice/tool",
            tracker.label(3).expect("the label was added"),
        );
        assert_eq!(rendered.id, 3);
        assert_eq!(rendered.name, "bug");
        assert_eq!(rendered.color, "ff0000");
        assert!(!rendered.is_archived);
        assert_eq!(rendered.url, "http://h/api/v1/repos/alice/tool/labels/3");
        assert_eq!(
            json!(rendered),
            json!({
                "id": 3,
                "name": "bug",
                "exclusive": false,
                "description": "",
                "color": "ff0000",
                "url": "http://h/api/v1/repos/alice/tool/labels/3",
                "is_archived": false,
            })
        );
    }
}
