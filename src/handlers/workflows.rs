//! Workflow dispatch requests consume fixture-defined runs through Forgejo's REST protocol.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::Handler;
use super::repos::find_writable_repo;
use crate::api::{ApiRequest, ApiResponse, ApiResult};
use crate::operation::ops;
use crate::state::{State, lock};
use crate::types;

impl Handler for ops::DispatchWorkflow {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: types::DispatchWorkflowOption = request.json_as()?;
        let reference = body.ref_.as_str();
        let inputs: BTreeMap<String, String> = body.inputs.into_iter().collect();
        let return_run_info = body.return_run_info;
        let found = {
            let state = lock(state);
            find_writable_repo(&state, request)?
        };
        let mut state = lock(state);
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("writable repository exists");
        let workflow_filename = request.param("workflowfilename");
        let Some(index) = repo.workflow_dispatches.iter().position(|dispatch| {
            dispatch.workflow_filename == workflow_filename
                && dispatch.reference == reference
                && dispatch.inputs == inputs
        }) else {
            return Err(ApiResponse::error(
                400,
                "workflow dispatch has no matching scripted run",
            ));
        };
        let dispatch = repo
            .workflow_dispatches
            .remove(index)
            .expect("dispatch was found above");
        let run = repo
            .action_runs
            .get(&dispatch.run_id)
            .expect("fixture only queues existing runs");
        if return_run_info {
            Ok(ApiResponse::json(
                201,
                types::DispatchWorkflowRun {
                    id: run.run.id,
                    run_number: run.run.index_in_repo,
                    jobs: run.jobs.iter().filter_map(|job| job.name.clone()).collect(),
                },
            ))
        } else {
            Ok(ApiResponse::no_content())
        }
    }
}
