//! Comments on the issues of a repository.
//!
//! Commenting needs the `write:issue` scope and read access to the repository. A comment is
//! changed by whoever wrote it, or by whoever may write to the repository.
//!
//! The comments of one issue come back whole, without the narrowing by the `since` and `before`
//! timestamps Forgejo takes.

use std::sync::Mutex;

use serde::Deserialize;

use super::Handler;
use super::issues::issue_index;
use super::repos::{Found, find_repo};
use super::{may_change, no_write};
use crate::api::{ApiRequest, ApiResponse, ApiResult, paginate};
use crate::operation::ops;
use crate::render;
use crate::state::{State, lock, now};
use crate::types;

/// Comment request fields with optional bodies to retain the handler's 422 validation response.
#[derive(Deserialize)]
struct CommentInput {
    /// Comment text.
    body: Option<String>,
}

/// The comment `request` addresses, which a path parameter that is not an id is a 404.
fn comment_id(request: &ApiRequest) -> Result<u64, ApiResponse> {
    request
        .param("id")
        .parse()
        .map_err(|_| ApiResponse::not_found())
}

/// The body of a comment, which may not be empty.
fn wanted_body(body: &str) -> Result<String, ApiResponse> {
    if body.is_empty() {
        return Err(ApiResponse::error(422, "a comment needs a body"));
    }
    Ok(body.to_owned())
}

/// Renders `comment` of `found`, which the caller may see.
fn rendered(state: &State, request: &ApiRequest, found: &Found, comment: u64) -> types::Comment {
    let owner = state
        .user(&found.owner)
        .expect("a visible repository has an owner");
    let repo = owner
        .repo(&found.name)
        .expect("a visible repository exists");
    let comment = repo
        .tracker
        .comment(comment)
        .expect("the comment was found");
    render::comment(state, request, owner, repo, comment)
}

/// Renders the comments `tracker` holds, of one issue or of the whole repository.
fn rendered_all(
    state: &State,
    request: &ApiRequest,
    found: &Found,
    index: Option<u64>,
) -> Vec<types::Comment> {
    let owner = state
        .user(&found.owner)
        .expect("a visible repository has an owner");
    let repo = owner
        .repo(&found.name)
        .expect("a visible repository exists");
    let comments = match index {
        Some(index) => repo.tracker.comments_of(index),
        None => repo.tracker.comments(),
    };
    comments
        .into_iter()
        .map(|comment| render::comment(state, request, owner, repo, comment))
        .collect()
}

impl Handler for ops::IssueGetComments {
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
        Ok(ApiResponse::ok(rendered_all(
            &state,
            request,
            &found,
            Some(index),
        )))
    }
}

impl Handler for ops::IssueCreateComment {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let poster = request.require_scope("write:issue")?.login.clone();
        let body: CommentInput = request.json_as_with_status(422)?;
        let body = wanted_body(body.body.as_deref().unwrap_or_default())?;
        let index = issue_index(request)?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        if state
            .tracker(&found.owner, &found.name)
            .issue(index)
            .is_none()
        {
            return Err(ApiResponse::not_found());
        }
        let id = state.next_id();
        let id = state
            .tracker_mut(&found.owner, &found.name)
            .add_comment(id, index, &poster, &body);
        Ok(ApiResponse::json(
            201,
            rendered(&state, request, &found, id),
        ))
    }
}

impl Handler for ops::IssueGetRepoComments {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        Ok(paginate(
            request,
            rendered_all(&state, request, &found, None),
        ))
    }
}

impl Handler for ops::IssueGetComment {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:issue")?;
        let id = comment_id(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        if state
            .tracker(&found.owner, &found.name)
            .comment(id)
            .is_none()
        {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::ok(rendered(&state, request, &found, id)))
    }
}

impl Handler for ops::IssueEditComment {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let id = comment_id(request)?;
        let body: CommentInput = request.json_as_with_status(422)?;
        let body = wanted_body(body.body.as_deref().unwrap_or_default())?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let poster = state
            .tracker(&found.owner, &found.name)
            .comment(id)
            .ok_or_else(ApiResponse::not_found)?
            .poster
            .clone();
        if !may_change(&found, request, &poster) {
            return Err(no_write());
        }
        let comment = state
            .tracker_mut(&found.owner, &found.name)
            .comment_mut(id)
            .expect("the comment was found");
        comment.body = body;
        comment.updated_at = now();
        Ok(ApiResponse::ok(rendered(&state, request, &found, id)))
    }
}

impl Handler for ops::IssueDeleteComment {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:issue")?;
        let id = comment_id(request)?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let poster = state
            .tracker(&found.owner, &found.name)
            .comment(id)
            .ok_or_else(ApiResponse::not_found)?
            .poster
            .clone();
        if !may_change(&found, request, &poster) {
            return Err(no_write());
        }
        state
            .tracker_mut(&found.owner, &found.name)
            .remove_comment(id)
            .expect("the comment was found");
        Ok(ApiResponse::no_content())
    }
}
