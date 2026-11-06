//! Commit statuses share the hosted repository's real Git identities.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde::Deserialize;

use ninot_common::git;

use super::Handler;
use super::repos::{find_repo, find_writable_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult, paginate};
use crate::operation::ops;
use crate::render;
use crate::state::{State, lock, now};
use crate::types;

/// Status input keeps the status string open for Forgejo's 422 validation response.
#[derive(Deserialize)]
struct CreateStatusInput {
    /// Commit status value.
    state: Option<String>,
    /// Status context.
    context: Option<String>,
    /// Human-readable status description.
    description: Option<String>,
    /// URL associated with the status.
    target_url: Option<String>,
}

/// Resolves a status reference to an existing Git commit.
fn sha(state: &State, request: &ApiRequest) -> Result<String, ApiResponse> {
    let found = find_repo(state, request)?;
    let repo = state
        .user(&found.owner)
        .expect("validated repository and authenticated user exist")
        .repo(&found.name)
        .expect("validated repository and authenticated user exist");
    let reference = request
        .params
        .get("sha")
        .or_else(|| request.params.get("ref"))
        .expect("validated repository and authenticated user exist");
    git::resolve_commit(&repo.git_dir, reference)
        .map_err(|error| ApiResponse::error(500, &error.to_string()))?
        .ok_or_else(ApiResponse::not_found)
}

/// Reads newest statuses first, preserving older entries for history listing.
fn history(state: &State, request: &ApiRequest) -> Result<Vec<types::CommitStatus>, ApiResponse> {
    let found = find_repo(state, request)?;
    let sha = sha(state, request)?;
    let repo = state
        .user(&found.owner)
        .expect("validated repository and authenticated user exist")
        .repo(&found.name)
        .expect("validated repository and authenticated user exist");
    Ok(repo
        .statuses
        .get(&sha)
        .into_iter()
        .flatten()
        .rev()
        .cloned()
        .collect())
}

impl Handler for ops::RepoCreateStatus {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let caller = request.require_scope("write:repository")?;
        let body: CreateStatusInput = request.json_as_with_status(422)?;
        let status = body
            .state
            .as_deref()
            .filter(|value| {
                matches!(
                    *value,
                    "pending" | "success" | "error" | "failure" | "warning" | "skipped"
                )
            })
            .ok_or_else(|| ApiResponse::error(422, "invalid commit status state"))?
            .to_owned();
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let sha = sha(&state, request)?;
        let id = state.next_id();
        let at = now();
        let creator = render::user(
            request,
            state
                .user(&caller.login)
                .expect("validated repository and authenticated user exist"),
        );
        let status = types::CommitStatus {
            id: Some(id as i64),
            context: Some(body.context.unwrap_or_else(|| "default".to_owned())),
            status: Some(status.into()),
            description: Some(body.description.unwrap_or_default()),
            target_url: Some(body.target_url.unwrap_or_default()),
            created_at: Some(at),
            updated_at: Some(at),
            creator: Some(creator),
            url: Some(format!(
                "{}/repos/{}/{}/statuses/{sha}",
                request.api_url, found.owner, found.name
            )),
        };
        state
            .user_mut(&found.owner)
            .expect("validated repository and authenticated user exist")
            .repo_mut(&found.name)
            .expect("validated repository and authenticated user exist")
            .statuses
            .entry(sha)
            .or_default()
            .push(status.clone());
        Ok(ApiResponse::json(201, status))
    }
}

impl Handler for ops::RepoListStatuses {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        Ok(paginate(request, history(&lock(state), request)?))
    }
}

impl Handler for ops::RepoListStatusesByRef {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        <ops::RepoListStatuses as Handler>::handle(state, request)
    }
}

impl Handler for ops::RepoGetCombinedStatusByRef {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let sha = sha(&state, request)?;
        let mut seen = BTreeSet::new();
        let statuses: Vec<_> = history(&state, request)?
            .into_iter()
            .filter(|status| seen.insert(status.context.clone()))
            .collect();
        let values: Vec<_> = statuses
            .iter()
            .filter_map(|status| status.status.as_ref())
            .map(|s| s.as_str())
            .collect();
        let combined = [
            "error", "failure", "warning", "pending", "success", "skipped",
        ]
        .into_iter()
        .find(|candidate| values.contains(candidate))
        .unwrap_or("");
        let combined = if combined == "error" {
            "failure"
        } else {
            combined
        };
        if statuses.is_empty() {
            return Ok(ApiResponse::ok(types::CombinedStatus {
                commit_url: Some(String::new()),
                url: Some(String::new()),
                sha: Some(String::new()),
                state: Some(String::new().into()),
                total_count: Some(0),
                statuses: None,
                repository: None,
            }));
        }
        let owner = state
            .user(&found.owner)
            .expect("validated repository and authenticated user exist");
        let repo = owner
            .repo(&found.name)
            .expect("validated repository and authenticated user exist");
        let url = format!("{}/repos/{}/{}", request.api_url, found.owner, found.name);
        Ok(ApiResponse::ok(types::CombinedStatus {
            commit_url: Some(format!("{url}/git/commits/{sha}")),
            url: Some(format!("{url}/commits/{sha}/status")),
            sha: Some(sha),
            state: Some(combined.to_owned().into()),
            total_count: Some(statuses.len() as i64),
            statuses: Some(statuses),
            repository: Some(render::repo(
                request,
                owner,
                repo,
                found.permission,
                repo.is_empty(),
            )),
        }))
    }
}
