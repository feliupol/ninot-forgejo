//! The user a request is authenticated as.

use std::sync::Mutex;

use super::Handler;
use crate::api::{ApiRequest, ApiResponse, ApiResult};
use crate::operation::ops;
use crate::render;
use crate::state::{State, lock};

impl Handler for ops::UserGetCurrent {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let login = request.require_caller()?.login.clone();
        let state = lock(state);
        let user = state.user(&login).ok_or_else(ApiResponse::not_found)?;
        Ok(ApiResponse::ok(render::user(request, user)))
    }
}
