//! The payload fields every event of a repository shares.
//!
//! A payload names the repository an event happened in, and the user it happened on behalf of, as
//! the generated models of the spec's `PushPayload` spell them.

use crate::api::ApiRequest;
use crate::render;
use crate::server::ADMIN_LOGIN;
use crate::state::State;
use crate::types;

pub mod push;

/// The repository an event happened in, as a payload renders it: the same body the repository
/// operations answer with.
pub fn repository(
    request: &ApiRequest,
    state: &State,
    owner: &str,
    name: &str,
) -> types::Repository {
    let owner_user = state.user(owner).expect("owner exists");
    let repo = owner_user.repo(name).expect("repo exists");
    let empty = repo.is_empty();
    let permission = state
        .permission(request.caller.as_ref(), owner, name)
        .expect("a push came to a repository the caller may see");
    render::repo(request, owner_user, repo, permission, empty)
}

/// The user an event happened on behalf of: the one whose token made the request, which a
/// payload names in both `pusher` and `sender`, or the `admin` the fake pushes its own
/// commits as. A caller the fake no longer knows is `null`, as the spec's `PushPayload`
/// declares both of them nullable.
pub fn doer(request: &ApiRequest, state: &State) -> Option<types::User> {
    let login = request
        .caller
        .as_ref()
        .map_or(ADMIN_LOGIN, |caller| caller.login.as_str());
    state.user(login).map(|user| render::user(request, user))
}
