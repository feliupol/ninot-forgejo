//! Repository webhooks and their deliveries.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use url::Url;

use super::Handler;
use super::repos::{Found, find_admin_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult, PatchField, paginate};
use crate::operation::ops;
use crate::state::{Hook, State, lock, now};
use crate::types;
use crate::webhooks;

/// The repository whose webhooks `request` may manage. Managing them needs a token that may
/// write to repositories and a caller who administers this one, as Forgejo's `reqRepoAdmin`
/// does: a repository the caller cannot see is a 404, one they may see but not administer a
/// 403.
fn find_admin(state: &State, request: &ApiRequest) -> Result<Found, ApiResponse> {
    request.require_scope("write:repository")?;
    find_admin_repo(state, request)
}

fn hooks<'a>(state: &'a State, found: &Found) -> &'a Vec<Hook> {
    state
        .user(&found.owner)
        .and_then(|user| user.repo(&found.name))
        .map(|repo| &repo.hooks)
        .expect("repository exists")
}

fn hooks_mut<'a>(state: &'a mut State, found: &Found) -> &'a mut Vec<Hook> {
    state
        .user_mut(&found.owner)
        .and_then(|user| user.repo_mut(&found.name))
        .map(|repo| &mut repo.hooks)
        .expect("repository exists")
}

fn hook_id(request: &ApiRequest) -> Result<u64, ApiResponse> {
    request
        .param("id")
        .parse()
        .map_err(|_| ApiResponse::not_found())
}

/// A hook, as the spec's `Hook` renders it. `config` holds the url and the content type only:
/// Forgejo never hands the secret back, and neither does the fake, however it was set.
fn render_hook(request: &ApiRequest, found: &Found, hook: &Hook, state: &State) -> types::Hook {
    let api = format!(
        "{}/repos/{}/{}/hooks/{}",
        request.api_url, found.owner, found.name, hook.id
    );
    let last = state
        .deliveries
        .iter()
        .rev()
        .find(|delivery| delivery.hook_id == hook.id);
    let last_response = match last {
        None => types::HookLastResponse {
            code: None,
            message: None,
            status: types::HookLastResponseStatus::Unused,
        },
        Some(delivery) => match delivery.status_code {
            Some(code) => types::HookLastResponse {
                code: Some(code as i64),
                message: None,
                status: if (200..300).contains(&code) {
                    types::HookLastResponseStatus::Active
                } else {
                    types::HookLastResponseStatus::Failed
                },
            },
            None => types::HookLastResponse {
                code: None,
                message: delivery.error.clone(),
                status: types::HookLastResponseStatus::Pending,
            },
        },
    };
    types::Hook {
        active: hook.active,
        config: HashMap::from([
            ("content_type".to_owned(), hook.content_type.clone()),
            ("url".to_owned(), hook.url.clone()),
        ]),
        created_at: hook.created_at,
        deliveries_url: format!("{api}/deliveries"),
        events: hook.events.clone(),
        id: hook.id as i64,
        last_response,
        name: "web".to_owned(),
        ping_url: format!("{api}/pings"),
        test_url: format!("{api}/tests"),
        type_: "forgejo".to_owned(),
        updated_at: hook.updated_at,
        url: api,
        // The keys Forgejo leaves out of a web hook the fake has nothing to say about.
        authorization_header: None,
        branch_filter: None,
        content_type: None,
        metadata: None,
    }
}

/// Applies `config` to `hook`. The URL has to be `http` or `https`. When `policy` is
/// [`webhooks::TargetPolicy::PublicOnly`], an IP literal that policy refuses, or the name
/// `localhost`, is rejected; other hostnames are checked only when the hook fires.
fn apply_config(
    hook: &mut Hook,
    config: &HashMap<String, String>,
    policy: webhooks::TargetPolicy,
) -> Result<(), ApiResponse> {
    if let Some(url) = config.get("url") {
        // Forgejo only delivers to a URL it can fetch, which is an `http` or `https` one.
        let parsed = Url::parse(url)
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https"));
        let Some(parsed) = parsed else {
            return Err(ApiResponse::error(422, "invalid url"));
        };
        if policy == webhooks::TargetPolicy::PublicOnly && host_refused(policy, &parsed) {
            return Err(ApiResponse::error(422, "invalid url"));
        }
        hook.url = url.to_owned();
    }
    if let Some(content_type) = config.get("content_type").map(String::as_str) {
        if !matches!(content_type, "json" | "form") {
            hook.content_type = "json".to_owned();
        } else {
            hook.content_type = content_type.to_owned();
        }
    }
    if let Some(secret) = config.get("secret") {
        hook.secret = Some(secret.clone()).filter(|s| !s.is_empty());
    }
    if let Some(insecure_ssl) = config.get("insecure_ssl") {
        hook.insecure_ssl = insecure_ssl.clone();
    }
    Ok(())
}

/// Whether `url`'s host is refused by [`webhooks::TargetPolicy::PublicOnly`] before delivery.
///
/// IP literals are refused when [`webhooks::TargetPolicy::allows`] refuses them, and
/// `localhost` is refused in any letter case. Other names are not resolved here.
fn host_refused(policy: webhooks::TargetPolicy, url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(addr)) => !policy.allows(std::net::IpAddr::from(addr)),
        Some(url::Host::Ipv6(addr)) => !policy.allows(std::net::IpAddr::from(addr)),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Optional request fields accepted when editing a hook, including fixture-supported event
/// mutations that Forgejo does not declare in `EditHookOption`.
#[derive(Deserialize)]
struct EditHookInput {
    /// Whether the hook is active.
    #[serde(default)]
    active: PatchField<bool>,
    /// Hook configuration values declared by Forgejo.
    #[serde(default)]
    config: PatchField<HashMap<String, String>>,
    /// Events to subscribe to.
    #[serde(default)]
    events: PatchField<Vec<String>>,
    /// Additional event names to enable, a fixture-supported extension.
    #[serde(default)]
    add_events: PatchField<Vec<String>>,
    /// Event names to disable, a fixture-supported extension.
    #[serde(default)]
    remove_events: PatchField<Vec<String>>,
}

/// Typed creation input that preserves the handler's defaults and validation response.
#[derive(Deserialize)]
struct CreateHookInput {
    /// Whether the hook is active; missing and null default to true.
    #[serde(default)]
    active: PatchField<bool>,
    /// Hook configuration values.
    config: Option<HashMap<String, String>>,
    /// Events to subscribe to; missing and null default to push.
    #[serde(default)]
    events: PatchField<Vec<String>>,
}

impl Handler for ops::RepoListHooks {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let state = lock(state);
        let found = find_admin(&state, request)?;
        let items: Vec<types::Hook> = hooks(&state, &found)
            .iter()
            .map(|hook| render_hook(request, &found, hook, &state))
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoCreateHook {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let body: CreateHookInput = request.json_as_with_status(422)?;
        let mut state = lock(state);
        let found = find_admin(&state, request)?;
        let config = body.config.unwrap_or_default();
        if !config.contains_key("url") {
            return Err(ApiResponse::error(422, "url required"));
        }
        let created = now();
        let mut hook = Hook {
            id: state.next_id(),
            active: match body.active {
                PatchField::Value(active) => active,
                PatchField::Missing | PatchField::Null => true,
            },
            events: match body.events {
                PatchField::Value(events) => events,
                PatchField::Missing | PatchField::Null => vec!["push".to_owned()],
            },
            url: String::new(),
            content_type: "json".to_owned(),
            secret: None,
            insecure_ssl: "0".to_owned(),
            created_at: created,
            updated_at: created,
        };
        apply_config(&mut hook, &config, state.webhook_targets)?;
        // Forgejo records the `type` a request names, but the fake only ever delivers the
        // payload of a `forgejo` hook, and renders the one it has.
        hooks_mut(&mut state, &found).push(hook);
        let rendered = render_hook(
            request,
            &found,
            hooks(&state, &found).last().unwrap(),
            &state,
        );
        Ok(ApiResponse::json(201, rendered))
    }
}

impl Handler for ops::RepoGetHook {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let state = lock(state);
        let found = find_admin(&state, request)?;
        let id = hook_id(request)?;
        let hook = hooks(&state, &found)
            .iter()
            .find(|h| h.id == id)
            .ok_or_else(ApiResponse::not_found)?;
        Ok(ApiResponse::ok(render_hook(request, &found, hook, &state)))
    }
}

impl Handler for ops::RepoDeleteHook {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let mut state = lock(state);
        let found = find_admin(&state, request)?;
        let id = hook_id(request)?;
        let hooks = hooks_mut(&mut state, &found);
        let idx = hooks.iter().position(|h| h.id == id);
        if let Some(idx) = idx {
            hooks.remove(idx);
        } else {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::no_content())
    }
}

impl Handler for ops::RepoEditHook {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let body: EditHookInput = request.json_as_with_status(422)?;
        let mut state = lock(state);
        let found = find_admin(&state, request)?;
        let id = hook_id(request)?;
        let policy = state.webhook_targets;
        {
            let hooks = hooks_mut(&mut state, &found);
            let hook = hooks
                .iter_mut()
                .find(|h| h.id == id)
                .ok_or_else(ApiResponse::not_found)?;
            if let PatchField::Value(active) = body.active {
                hook.active = active;
            }
            if let PatchField::Value(config) = body.config {
                apply_config(hook, &config, policy)?;
            }
            if let PatchField::Value(events) = body.events {
                hook.events = events;
            }
            if let PatchField::Value(add_events) = body.add_events {
                for ev in add_events {
                    if !hook.events.contains(&ev) {
                        hook.events.push(ev);
                    }
                }
            }
            if let PatchField::Value(remove_events) = body.remove_events {
                hook.events.retain(|e| !remove_events.contains(e));
            }
            hook.updated_at = now();
        }
        let hook = hooks(&state, &found)
            .iter()
            .find(|h| h.id == id)
            .ok_or_else(ApiResponse::not_found)?;
        Ok(ApiResponse::ok(render_hook(request, &found, hook, &state)))
    }
}

impl Handler for ops::RepoTestHook {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let mut state = lock(state);
        let found = find_admin(&state, request)?;
        let id = hook_id(request)?;
        if !hooks(&state, &found).iter().any(|hook| hook.id == id) {
            return Err(ApiResponse::not_found());
        }
        // Forgejo tests a hook by delivering a push of the default branch's current head, which
        // is what a subscriber would see.
        let repo = state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .expect("repository exists");
        let git_dir = repo.git_dir.clone();
        let branch = repo.default_branch.clone();
        let head = ninot_common::git::resolve_commit(&git_dir, &branch)
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::webhooks::events::push::ZERO_SHA.to_owned());
        webhooks::events::push::enqueue_test(
            &mut state,
            request,
            &found.owner,
            &found.name,
            &git_dir,
            &branch,
            &head,
            Some(id),
        );
        Ok(ApiResponse::no_content())
    }
}
