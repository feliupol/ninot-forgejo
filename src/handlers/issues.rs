//! Issues of a repository: filing, reading, listing and editing them.
//!
//! Filing and commenting on an issue needs the `write:issue` scope and read access to the
//! repository, as Forgejo lets anybody who may read a repository file issues on it. Changing an
//! issue that somebody else filed needs write access to the repository.

use std::sync::Mutex;

use serde::Deserialize;

use super::repos::{Found, find_repo};
use super::{Handler, may_change, no_write};
use crate::api::{ApiRequest, ApiResponse, ApiResult, PatchField, paginate};
use crate::operation::ops;
use crate::parse;
use crate::render;
use crate::state::{Issue, NewIssue, State, Status, Tracker, lock, now};
use crate::types;

/// The number of the issue the request addresses, which a path that is not one is a 404.
pub fn issue_index(request: &ApiRequest) -> Result<u64, ApiResponse> {
    request
        .param("index")
        .parse()
        .map_err(|_| ApiResponse::not_found())
}

/// Renders the issue `index` of `found`, which the caller may see.
fn rendered(state: &State, request: &ApiRequest, found: &Found, index: u64) -> types::Issue {
    let owner = state
        .user(&found.owner)
        .expect("a visible repository has an owner");
    let repo = owner
        .repo(&found.name)
        .expect("a visible repository exists");
    let issue = repo.tracker.issue(index).expect("the issue was found");
    render::issue(state, request, owner, repo, issue)
}

/// The state the issue list filters on, `all` standing for both states.
fn wanted_state(request: &ApiRequest) -> Result<Option<Status>, ApiResponse> {
    match request.query("state").unwrap_or("open") {
        "open" => Ok(Some(Status::Open)),
        "closed" => Ok(Some(Status::Closed)),
        "all" => Ok(None),
        state => Err(ApiResponse::error(422, &format!("unknown state {state}"))),
    }
}

/// The issues of `tracker` the request asks for: those of the state it names and of the labels
/// it lists, the newest first.
fn selected<'a>(tracker: &'a Tracker, request: &ApiRequest) -> Result<Vec<&'a Issue>, ApiResponse> {
    let state = wanted_state(request)?;
    let labels: Vec<String> = request
        .query("labels")
        .map(parse::label_names)
        .unwrap_or_default();
    Ok(tracker
        .issues()
        .rev()
        .filter(|issue| state.is_none_or(|state| issue.state == state))
        .filter(|issue| labels.is_empty() || tracker.has_any_label(issue, &labels))
        .collect())
}

/// A label request accepts either an id or name; the empty name and id zero mean no label.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum LabelReference {
    /// Numeric label identifier.
    Id(i64),
    /// Label name.
    Name(String),
}

/// The label ids a request names; zero and the empty name are no-label placeholders.
pub fn label_ids(tracker: &Tracker, values: &[LabelReference]) -> Result<Vec<u64>, ApiResponse> {
    values
        .iter()
        .filter(|label| match label {
            LabelReference::Id(id) => *id != 0,
            LabelReference::Name(name) => !name.is_empty(),
        })
        .map(|label| {
            let id = match label {
                LabelReference::Id(id) => u64::try_from(*id)
                    .ok()
                    .and_then(|id| tracker.label(id).map(|label| label.id)),
                LabelReference::Name(name) => tracker.label_id(name),
            };
            let named = match label {
                LabelReference::Id(id) => id.to_string(),
                LabelReference::Name(name) => name.clone(),
            };
            id.ok_or_else(|| ApiResponse::error(422, &format!("label {named} does not exist")))
        })
        .collect()
}

/// The logins an issue is assigned to by `body`, or `None` when the body leaves the assignees
/// alone. Every login must be a user the fake knows, except the empty name a client sends in
/// place of naming nobody, which Forgejo ignores.
pub fn assignees(
    state: &State,
    assignee: PatchField<String>,
    assignees: PatchField<Vec<String>>,
) -> Result<Option<Vec<String>>, ApiResponse> {
    if matches!(assignee, PatchField::Missing) && matches!(assignees, PatchField::Missing) {
        return Ok(None);
    }
    let mut wanted: Vec<String> = Vec::new();
    let name_of = |login: &str| -> Result<Option<String>, ApiResponse> {
        if login.is_empty() {
            return Ok(None);
        }
        if state.user(login).is_none() {
            return Err(ApiResponse::error(
                422,
                &format!("user {login} does not exist"),
            ));
        }
        Ok(Some(login.to_owned()))
    };
    if let PatchField::Value(login) = &assignee
        && let Some(login) = name_of(login)?
    {
        wanted.push(login);
    }
    if matches!(assignee, PatchField::Null) {
        return Err(ApiResponse::error(
            422,
            "assignees must be the names of users",
        ));
    }
    let requested = match assignees {
        PatchField::Value(values) => values,
        PatchField::Missing | PatchField::Null => Vec::new(),
    };
    for login in &requested {
        if let Some(login) = name_of(login)?
            && !wanted.contains(&login)
        {
            wanted.push(login);
        }
    }
    Ok(Some(wanted))
}

/// Issue creation fields with optional `title` so its missing-value validation remains a 422.
#[derive(Deserialize)]
struct CreateIssueInput {
    /// Issue title.
    title: Option<String>,
    /// Issue description.
    body: Option<String>,
    /// Label ids.
    labels: Option<Vec<LabelReference>>,
    /// Whether the new issue is closed.
    closed: Option<bool>,
    /// Deprecated single-user assignee input.
    #[serde(default)]
    assignee: PatchField<String>,
    /// Replacement list of assigned users.
    #[serde(default)]
    assignees: PatchField<Vec<String>>,
}

/// Issue edit fields used by this implementation.
#[derive(Deserialize)]
struct EditIssueInput {
    /// New issue title.
    title: Option<String>,
    /// New issue description.
    body: Option<String>,
    /// Requested open or closed state.
    state: Option<String>,
    /// Deprecated single-user assignee input.
    #[serde(default)]
    assignee: PatchField<String>,
    /// Replacement list of assigned users.
    #[serde(default)]
    assignees: PatchField<Vec<String>>,
}

/// The state an edit asks for, which the edit option spells `state`.
fn edited_state(state: Option<&str>) -> Result<Option<Status>, ApiResponse> {
    match state {
        None => Ok(None),
        Some(state) => Status::parse(state)
            .map(Some)
            .ok_or_else(|| ApiResponse::error(422, &format!("unknown state {state}"))),
    }
}

impl Handler for ops::IssueListIssues {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let owner = state
            .user(&found.owner)
            .expect("a visible repository has an owner");
        let repo = owner
            .repo(&found.name)
            .expect("a visible repository exists");
        let items = selected(&repo.tracker, request)?
            .into_iter()
            .map(|issue| render::issue(&state, request, owner, repo, issue))
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::IssueCreateIssue {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let poster = request.require_scope("write:issue")?.login.clone();
        let body: CreateIssueInput = request.json_as_with_status(422)?;
        let title = body.title.as_deref().unwrap_or_default().trim();
        if title.is_empty() {
            return Err(ApiResponse::error(422, "title is required"));
        }
        let title = title.to_owned();
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let assignees = assignees(&state, body.assignee, body.assignees)?.unwrap_or_default();
        let labels = label_ids(
            state.tracker(&found.owner, &found.name),
            body.labels.as_deref().unwrap_or_default(),
        )?;
        let id = state.next_id();
        let index = state.tracker_mut(&found.owner, &found.name).add_issue(
            id,
            NewIssue {
                title,
                body: body.body.unwrap_or_default(),
                poster,
                labels,
                assignees,
                closed: body.closed.unwrap_or(false),
            },
        );
        Ok(ApiResponse::json(
            201,
            rendered(&state, request, &found, index),
        ))
    }
}

impl Handler for ops::IssueGetIssue {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let index = issue_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        if state
            .tracker(&found.owner, &found.name)
            .issue(index)
            .is_none()
        {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::ok(rendered(&state, request, &found, index)))
    }
}

impl Handler for ops::IssueGetCommentsAndTimeline {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let index = issue_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let owner = state
            .user(&found.owner)
            .expect("a visible repository has an owner");
        let repo = owner
            .repo(&found.name)
            .expect("a visible repository exists");
        let tracker = &repo.tracker;
        if tracker.issue(index).is_none() {
            return Err(ApiResponse::not_found());
        }
        let mut events: Vec<types::TimelineComment> = tracker
            .pull(index)
            .into_iter()
            .flat_map(|pull| pull.timeline.iter())
            .map(|event| types::TimelineComment {
                id: Some(event.id as i64),
                type_: Some(event.kind.as_str().to_owned()),
                created_at: Some(event.created_at),
                ..Default::default()
            })
            .collect();
        events.extend(tracker.comments_of(index).into_iter().map(|comment| {
            types::TimelineComment {
                id: Some(comment.id as i64),
                type_: Some("commented".to_owned()),
                created_at: Some(comment.created_at),
                updated_at: Some(comment.updated_at),
                ref_comment: Some(render::comment(&state, request, owner, repo, comment)),
                ..Default::default()
            }
        }));
        events.sort_by_key(|event| event.created_at);
        Ok(paginate(request, events))
    }
}

impl Handler for ops::IssueEditIssue {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let body: EditIssueInput = request.json_as_with_status(422)?;
        let index = issue_index(request)?;
        let closed_to = edited_state(body.state.as_deref())?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let poster = state
            .tracker(&found.owner, &found.name)
            .issue(index)
            .ok_or_else(ApiResponse::not_found)?
            .poster
            .clone();
        if !may_change(&found, request, &poster) {
            return Err(no_write());
        }
        let assignees = assignees(&state, body.assignee, body.assignees)?;
        let time = now();
        let issue = state
            .tracker_mut(&found.owner, &found.name)
            .issue_mut(index)
            .expect("the issue was found");
        if let Some(title) = body.title {
            issue.title = title.to_owned();
        }
        if let Some(body) = body.body {
            issue.body = body.to_owned();
        }
        if let Some(assignees) = assignees {
            issue.assignees = assignees;
        }
        if let Some(closed) = closed_to {
            issue.state = closed;
            issue.closed_at = (closed == Status::Closed).then_some(time);
        }
        issue.updated_at = time;
        Ok(ApiResponse::json(
            201,
            rendered(&state, request, &found, index),
        ))
    }
}
