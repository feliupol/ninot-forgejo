//! Repository lifecycle: create, read, list, update and delete.

use std::path::Path;
use std::sync::Mutex;

use super::Handler;
use super::no_write;
use crate::api::{ApiRequest, ApiResponse, ApiResult, PatchField, paginate};
use crate::operation::ops;
use crate::parse;
use crate::render;
use crate::state::{Permission, State, lock, now};
use crate::types;
use ninot_common::git;

/// A repository the caller can see, under its current owner and name.
pub struct Found {
    pub owner: String,
    pub name: String,
    pub permission: Permission,
}

/// The branch a new repository starts on when the request names none. The spec declares
/// `default_branch` as a plain string, so an empty one means "unset" here as it does in Forgejo;
/// taking it for a branch name of its own left the initialisation without a reference name.
const DEFAULT_BRANCH: &str = "main";

/// Resolves `{owner}/{repo}` for the caller. A repository renamed away answers under its new
/// name, as Forgejo follows the old URL silently, and one invisible to the caller is a 404.
pub fn find_repo(state: &State, request: &ApiRequest) -> Result<Found, ApiResponse> {
    let (owner, name) = (request.param("owner"), request.param("repo"));
    let caller = request.caller.as_ref();
    if let Some(permission) = state.permission(caller, owner, name) {
        let user = state
            .user(owner)
            .expect("a visible repository has an owner");
        return Ok(Found {
            owner: user.login.clone(),
            name: user
                .repo(name)
                .expect("a visible repository exists")
                .name
                .clone(),
            permission,
        });
    }
    if let Some((to_owner, to_name)) = state.redirect(owner, name)
        && let Some(permission) = state.permission(caller, to_owner, to_name)
    {
        let user = state
            .user(to_owner)
            .expect("a redirected repository has an owner");
        return Ok(Found {
            owner: user.login.clone(),
            name: user
                .repo(to_name)
                .expect("a redirected repository exists")
                .name
                .clone(),
            permission,
        });
    }
    Err(ApiResponse::not_found())
}

/// Like [`find_repo`], but the caller must administer the repository.
pub fn find_admin_repo(state: &State, request: &ApiRequest) -> Result<Found, ApiResponse> {
    let found = find_repo(state, request)?;
    if found.permission.admin {
        Ok(found)
    } else {
        Err(ApiResponse::error(
            403,
            "Must have admin rights to repository.",
        ))
    }
}

/// Like [`find_repo`], but the caller must be able to write to the repository, as Forgejo's
/// `reqRepoWriter` requires.
pub fn find_writable_repo(state: &State, request: &ApiRequest) -> Result<Found, ApiResponse> {
    let found = find_repo(state, request)?;
    if found.permission.push {
        Ok(found)
    } else {
        Err(no_write())
    }
}

/// Renders `found`, which the caller can see.
fn rendered(state: &State, request: &ApiRequest, found: &Found) -> types::Repository {
    let owner = state
        .user(&found.owner)
        .expect("a visible repository has an owner");
    let repo = owner
        .repo(&found.name)
        .expect("a visible repository exists");
    let empty = repo.is_empty();
    render::repo(request, owner, repo, found.permission, empty)
}

/// Creates the bare git repository of the newly recorded `owner/name`, with `HEAD` on `branch`
/// and, when `readme` is set, a README committed there. Runs with the lock released so other
/// requests proceed; on failure the repository is deleted again, freeing its name.
///
/// # Errors
///
/// The git error that made creating the repository fail.
fn init_storage(
    state: &Mutex<State>,
    owner: &str,
    name: &str,
    git_dir: &Path,
    branch: &str,
    readme: bool,
) -> std::io::Result<()> {
    let created = git::init_bare(git_dir, branch).and_then(|()| {
        if readme {
            git::commit_readme(git_dir, branch, name, &super::BOT)
        } else {
            Ok(())
        }
    });
    if created.is_err() {
        let mut state = lock(state);
        let still_ours = state
            .user(owner)
            .and_then(|user| user.repo(name))
            .is_some_and(|repo| repo.git_dir == git_dir);
        if still_ours {
            state.delete_repo(owner, name);
        }
        drop(state);
        let _ = std::fs::remove_dir_all(git_dir);
    }
    created
}

impl Handler for ops::CreateCurrentUserRepo {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let login = request.require_scope("write:repository")?.login.clone();
        let body: CreateRepoInput = request.json_as()?;
        let name = body
            .name
            .ok_or_else(|| ApiResponse::error(422, "repository name is required"))?;
        if !parse::repository_name(&name) {
            return Err(ApiResponse::error(
                422,
                &format!("invalid repository name {name}"),
            ));
        }
        let private = body.private.unwrap_or(false);
        let description = body.description;
        let auto_init = body.auto_init.unwrap_or(false);
        let branch = body
            .default_branch
            .as_deref()
            .filter(|branch| !branch.is_empty())
            .unwrap_or(DEFAULT_BRANCH)
            .to_owned();
        let (git_dir, branch) = {
            let mut state = lock(state);
            state
                .create_repo(&login, &name, private, description, &branch)
                .map_err(|message| ApiResponse::error(409, &message))?;
            let repo = state
                .user(&login)
                .expect("the caller exists")
                .repo(&name)
                .expect("the repository was recorded");
            (repo.git_dir.clone(), repo.default_branch.clone())
        };
        init_storage(state, &login, &name, &git_dir, &branch, auto_init)
            .map_err(|error| ApiResponse::error(500, &error.to_string()))?;
        let mut state = lock(state);
        if auto_init {
            state
                .user_mut(&login)
                .and_then(|user| user.repo_mut(&name))
                .expect("the repository was recorded")
                .pushed_at = Some(now());
        }
        let user = state.user(&login).expect("the caller exists");
        let repo = user.repo(&name).expect("the repository was recorded");
        let empty = repo.is_empty();
        let value = render::repo(request, user, repo, Permission::ADMIN, empty);
        Ok(ApiResponse::json(201, value))
    }
}

impl Handler for ops::UserCurrentListRepos {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let login = request.require_scope("read:repository")?.login.clone();
        let state = lock(state);
        let user = state.user(&login).ok_or_else(ApiResponse::not_found)?;
        let items = user
            .repos
            .values()
            .map(|repo| {
                let empty = repo.is_empty();
                render::repo(request, user, repo, Permission::ADMIN, empty)
            })
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoGet {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        Ok(ApiResponse::ok(rendered(&state, request, &found)))
    }
}

/// Topics are not modelled, so every repository has none.
impl Handler for ops::RepoListTopics {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        find_repo(&lock(state), request)?;
        // The fake models no topics, so every repository has none.
        Ok(ApiResponse::ok(types::TopicName { topics: Vec::new() }))
    }
}

impl Handler for ops::RepoEdit {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: RepoEditInput = request.json_as()?;
        let new_name = body.name.clone();
        if new_name
            .as_ref()
            .is_some_and(|name| !parse::repository_name(name))
        {
            return Err(ApiResponse::error(422, "invalid repository name"));
        }
        let mut state = lock(state);
        let mut found = find_admin_repo(&state, request)?;
        if let Some(new_name) = new_name {
            state
                .rename_repo(&found.owner, &found.name, &new_name)
                .map_err(|message| ApiResponse::error(409, &message))?;
            found.name = new_name;
        }
        let repo = state
            .user_mut(&found.owner)
            .and_then(|user| user.repo_mut(&found.name))
            .expect("the repository exists");
        if let Some(private) = body.private {
            repo.private = private;
        }
        match body.description {
            PatchField::Missing => {}
            PatchField::Null => repo.description = None,
            PatchField::Value(description) => repo.description = Some(description),
        }
        if let Some(branch) = body.default_branch.as_deref() {
            repo.set_default_branch(branch)
                .map_err(|error| ApiResponse::error(500, &error.to_string()))?;
        }
        repo.updated_at = now();
        Ok(ApiResponse::ok(rendered(&state, request, &found)))
    }
}

/// Repository creation fields handled by this service, with optional name validation.
#[derive(serde::Deserialize)]
struct CreateRepoInput {
    /// Name of the repository.
    name: Option<String>,
    /// Whether the repository is private.
    private: Option<bool>,
    /// Repository description.
    description: Option<String>,
    /// Whether to create an initial README.
    auto_init: Option<bool>,
    /// Default branch name.
    default_branch: Option<String>,
}

/// Repository update fields, with a presence-aware description patch.
#[derive(serde::Deserialize)]
struct RepoEditInput {
    /// New repository name.
    name: Option<String>,
    /// Whether the repository is private.
    private: Option<bool>,
    /// Omitted descriptions are preserved; null clears the description.
    #[serde(default)]
    description: PatchField<String>,
    /// New default branch.
    default_branch: Option<String>,
}

impl Handler for ops::RepoDelete {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let mut state = lock(state);
        let found = find_admin_repo(&state, request)?;
        let deleted = state.delete_repo(&found.owner, &found.name);
        drop(state);
        if let Some(repo) = deleted {
            let _ = std::fs::remove_dir_all(repo.git_dir);
        }
        Ok(ApiResponse::no_content())
    }
}
