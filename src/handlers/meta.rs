//! Server metadata operations.

use std::sync::Mutex;

use super::Handler;
use crate::api::{ApiRequest, ApiResponse, ApiResult};
use crate::operation::ops;
use crate::render;
use crate::state::State;

/// The version of Forgejo the pinned spec describes, which anyone may read.
impl Handler for ops::GetVersion {
    fn handle(_: &Mutex<State>, _: &ApiRequest) -> ApiResult {
        Ok(ApiResponse::ok(render::version()))
    }
}
