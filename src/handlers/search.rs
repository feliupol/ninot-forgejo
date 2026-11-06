//! Cross-repository issue and pull-request search over visible repository state.

use std::sync::Mutex;

use super::Handler;
use crate::api::{ApiRequest, ApiResponse, ApiResult, paginate};
use crate::operation::ops;
use crate::render;
use crate::state::{State, lock};

impl Handler for ops::IssueSearchIssues {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let caller = request.require_scope("read:issue")?;
        let wanted = request.query("state").unwrap_or("open");
        let kind = request.query("type");
        if !matches!(kind, None | Some("pulls" | "issues")) {
            return Err(ApiResponse::error(422, "type must be pulls or issues"));
        }
        let state = lock(state);
        let mut results = Vec::new();
        for owner in state.users() {
            for repo in owner.repos.values() {
                if state
                    .permission(Some(caller), &owner.login, &repo.name)
                    .is_none()
                {
                    continue;
                }
                for issue in repo.tracker.issues() {
                    let pull = repo.tracker.pull(issue.index);
                    if (kind == Some("pulls") && pull.is_none())
                        || (kind == Some("issues") && pull.is_some())
                    {
                        continue;
                    }
                    if wanted != "all" && issue.state.as_str() != wanted {
                        continue;
                    }
                    if request.query("created") == Some("true")
                        && !issue.poster.eq_ignore_ascii_case(&caller.login)
                    {
                        continue;
                    }
                    if request.query("review_requested") == Some("true")
                        && !pull.is_some_and(|pull| {
                            pull.reviewers
                                .iter()
                                .any(|reviewer| reviewer.eq_ignore_ascii_case(&caller.login))
                        })
                    {
                        continue;
                    }
                    if request.query("q").is_some_and(|query| {
                        !format!("{} {}", issue.title, issue.body)
                            .to_lowercase()
                            .contains(&query.to_lowercase())
                    }) {
                        continue;
                    }
                    results.push(render::issue(&state, request, owner, repo, issue));
                }
            }
        }
        Ok(paginate(request, results))
    }
}
