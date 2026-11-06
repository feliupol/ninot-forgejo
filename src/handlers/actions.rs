//! Repository Actions task listing.

use std::sync::Mutex;

use super::Handler;
use super::repos::{find_repo, find_writable_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult};
use crate::operation::ops;
use crate::state::{State, lock};

/// Parses the identifier routed by the pinned Actions operation.
fn run_id(request: &ApiRequest) -> Result<i64, ApiResponse> {
    request
        .param("run_id")
        .parse()
        .map_err(|_| ApiResponse::error(400, "invalid action run identifier"))
}

/// Tests repeated query parameters as alternatives, as Forgejo status filters do.
fn matches_query(request: &ApiRequest, key: &str, value: Option<&str>) -> bool {
    request.query(key).is_none()
        || request
            .query
            .iter()
            .any(|(name, expected)| name == key && value == Some(expected.as_str()))
}

/// Adds an instance-relative web URL without persisting a server address in state.
fn render_run(request: &ApiRequest, run: &crate::types::ActionRun) -> crate::types::ActionRun {
    let mut run = run.clone();
    run.html_url = Some(format!(
        "{}/{}/{}/actions/runs/{}",
        request.web_url,
        request.param("owner"),
        request.param("repo"),
        run.index_in_repo.unwrap_or_default()
    ));
    run
}

impl Handler for ops::ListActionRuns {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("visible repository exists");
        for run in repo.action_runs.values_mut() {
            run.advance();
        }
        let limit = request
            .query("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(1, 50);
        let page = request
            .query("page")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let runs: Vec<_> = repo
            .action_runs
            .values()
            .rev()
            .filter(|run| {
                matches_query(request, "workflow_id", run.run.workflow_id.as_deref())
                    && matches_query(request, "event", run.run.event.as_deref())
                    && matches_query(request, "status", run.run.status.as_deref())
                    && matches_query(request, "branch", run.run.prettyref.as_deref())
                    && matches_query(request, "head_sha", run.run.commit_sha.as_deref())
            })
            .collect();
        Ok(ApiResponse::ok(crate::types::ListActionRunResponse {
            total_count: Some(runs.len() as i64),
            workflow_runs: runs
                .into_iter()
                .skip(page.saturating_sub(1).saturating_mul(limit))
                .take(limit)
                .map(|run| render_run(request, &run.run))
                .collect(),
        }))
    }
}

impl Handler for ops::ActionRun {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let id = run_id(request)?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("visible repository exists");
        let run = repo
            .action_runs
            .get_mut(&id)
            .ok_or_else(ApiResponse::not_found)?;
        run.advance();
        Ok(ApiResponse::ok(render_run(request, &run.run)))
    }
}

impl Handler for ops::ListActionRunJobs {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let id = run_id(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let run = state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .and_then(|repo| repo.action_runs.get(&id))
            .ok_or_else(ApiResponse::not_found)?;
        Ok(ApiResponse::ok(&run.jobs))
    }
}

impl Handler for ops::RepoGetActionJobLogs {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let id: i64 = request
            .param("job_id")
            .parse()
            .map_err(|_| ApiResponse::error(400, "invalid action job identifier"))?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let runs = &state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .expect("visible repository exists")
            .action_runs;
        let run = runs
            .values()
            .find(|run| run.jobs.iter().any(|job| job.id == Some(id)))
            .ok_or_else(|| ApiResponse::error(404, "resource does not exist"))?;
        // Forgejo answers this for a job no runner has picked up yet.
        let log = run
            .logs
            .get(&id)
            .ok_or_else(|| ApiResponse::error(404, "job has not been executed yet"))?;
        Ok(ApiResponse::bytes(
            200,
            "text/plain; charset=utf-8",
            log.clone().into_bytes(),
        ))
    }
}

impl Handler for ops::CancelActionRun {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let id = run_id(request)?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let run = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .and_then(|repo| repo.action_runs.get_mut(&id))
            .ok_or_else(ApiResponse::not_found)?;
        run.cancel();
        Ok(ApiResponse::no_content())
    }
}

impl Handler for ops::ListActionTasks {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let repo = state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .expect("visible repository exists");
        let limit = request
            .query("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(1, 50);
        let page = request
            .query("page")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let tasks = repo
            .action_tasks
            .iter()
            .rev()
            .skip(page.saturating_sub(1).saturating_mul(limit))
            .take(limit)
            .cloned()
            .collect();
        Ok(ApiResponse::ok(crate::types::ActionTaskResponse {
            total_count: Some(repo.action_tasks.len() as i64),
            workflow_runs: tasks,
        }))
    }
}
