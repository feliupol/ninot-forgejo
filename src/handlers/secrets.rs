//! Repository Actions secrets expose names, accept write-only values, and delete values.

use std::sync::Mutex;

use serde::Deserialize;

use super::Handler;
use super::repos::{find_repo, find_writable_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult, Body, paginate};
use crate::operation::ops;
use crate::state::{State, lock};
use crate::types;

/// Secret update input keeps `data` optional so invalid or missing values retain the same 400.
#[derive(Deserialize)]
struct SecretInput {
    /// Secret value. The service deliberately stores only the secret's name.
    data: Option<String>,
}

impl Handler for ops::RepoListActionsSecrets {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let repo = state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .expect("visible repository exists");
        let secrets = repo
            .action_secrets
            .iter()
            .map(|name| types::Secret {
                created_at: None,
                name: Some(name.clone()),
            })
            .collect();
        Ok(paginate(request, secrets))
    }
}

impl Handler for ops::UpdateRepoSecret {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: SecretInput = request.json_as()?;
        if body.data.is_none() {
            return Err(ApiResponse::error(400, "secret data must be a string"));
        }
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("writable repository exists");
        let created = repo
            .action_secrets
            .insert(request.param("secretname").to_owned());
        if created {
            Ok(ApiResponse {
                status: 201,
                headers: Vec::new(),
                body: Body::Empty,
            })
        } else {
            Ok(ApiResponse::no_content())
        }
    }
}

impl Handler for ops::DeleteRepoSecret {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("writable repository exists");
        if repo.action_secrets.remove(request.param("secretname")) {
            Ok(ApiResponse::no_content())
        } else {
            Err(ApiResponse::not_found())
        }
    }
}
