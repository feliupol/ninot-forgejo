//! Server state shared by every request: users own repositories, tokens act as users.
//!
//! [`State`] and its user lookups live here; each kind of object has its own submodule.

mod action;
mod comment;
mod hook;
mod issue;
mod label;
mod repo;
mod user;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub(crate) use action::aggregate as aggregate_action_jobs;
pub use action::{ActionRun, WorkflowDispatch};
pub use comment::Comment;
pub use hook::{Delivery, Hook};
pub use issue::{Issue, NewIssue, Status, Tracker, author_or_writer};
pub use label::{Label, NewLabel};
pub use repo::{Permission, Repo};
pub use user::User;

pub use crate::time::now;

use crate::api::Caller;

/// One API request captured by the opt-in direct fixture trace.
#[derive(Clone, Debug)]
pub struct RequestTraceHit {
    /// HTTP method used by the request.
    pub method: String,
    /// URL path without the query string.
    pub path: String,
    /// Raw query string without the leading question mark.
    pub query: String,
    /// Request body bytes captured by the opt-in trace.
    pub body: Vec<u8>,
}

/// Bounded request-body trace returned by the fixture API.
#[derive(Clone, Debug, Default)]
pub struct RequestTrace {
    /// Captured API requests in arrival order.
    pub requests: Vec<RequestTraceHit>,
    /// True when the 8 MiB total capture limit omitted one or more requests.
    pub truncated: bool,
    /// Bytes retained for captured requests, including a small per-request metadata allowance.
    captured_bytes: usize,
}

/// Maximum retained bytes for one explicitly enabled request trace.
const REQUEST_TRACE_LIMIT: usize = 8 * 1024 * 1024;

/// Locks `state`, recovering it when a thread panicked while holding the lock, so one failing
/// request leaves later requests and test seeding working.
pub fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything the fake knows, guarded by one lock taken through [`lock`].
#[derive(Default)]
pub struct State {
    /// Fixture-defined failures, applied before normal protocol transitions.
    pub(crate) faults: crate::faults::Faults,
    /// Access tokens by their secret value.
    pub tokens: HashMap<String, Caller>,
    /// Users by lowercase login.
    users: BTreeMap<String, User>,
    /// Old `owner/name` (lowercase) of renamed repositories, to the current one.
    redirects: HashMap<String, String>,
    last_id: u64,
    /// Directory holding one bare git repository per repository; without it, nothing is on disk.
    pub git_root: Option<PathBuf>,
    /// Recorded webhook deliveries.
    pub deliveries: Vec<Delivery>,
    /// Queue for async webhook sending.
    pub webhook_queue: Option<std::sync::mpsc::Sender<u64>>,
    /// Which addresses a webhook delivery may contact. Defaults to allowing every address.
    pub webhook_targets: crate::webhooks::TargetPolicy,
    /// Optional bounded request trace; never records authorization headers.
    pub(crate) request_trace: Option<RequestTrace>,
}

impl State {
    /// Enables body capture for API requests received after this call.
    pub fn start_request_trace(&mut self) {
        self.request_trace = Some(RequestTrace::default());
    }

    /// Stops capture and returns its records and truncation state.
    pub fn stop_request_trace(&mut self) -> RequestTrace {
        self.request_trace.take().unwrap_or_default()
    }

    /// Records a request only while direct fixture capture is enabled, up to 8 MiB total.
    pub fn record_request(&mut self, method: &str, path: &str, query: &str, body: &[u8]) {
        let Some(trace) = self.request_trace.as_mut() else {
            return;
        };
        let size = 64 + method.len() + path.len() + query.len() + body.len();
        if trace.captured_bytes.saturating_add(size) > REQUEST_TRACE_LIMIT {
            trace.truncated = true;
            return;
        }
        trace.captured_bytes += size;
        trace.requests.push(RequestTraceHit {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            body: body.to_vec(),
        });
    }

    /// A new id, unique across every object of the fake.
    pub fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// Adds a user; an existing login is left unchanged.
    pub fn add_user(&mut self, login: &str) -> &mut User {
        let key = login.to_lowercase();
        if !self.users.contains_key(&key) {
            let id = self.next_id();
            self.users.insert(
                key.clone(),
                User {
                    id,
                    login: login.to_owned(),
                    full_name: String::new(),
                    email: String::new(),
                    is_admin: false,
                    created_at: now(),
                    active: true,
                    restricted: false,
                    repos: BTreeMap::new(),
                },
            );
        }
        self.users.get_mut(&key).unwrap()
    }

    /// Adds a user who administers every repository, as the fake's `admin` does.
    pub fn add_admin(&mut self, login: &str) -> &mut User {
        let user = self.add_user(login);
        user.is_admin = true;
        user
    }

    /// Issues a token acting as `login`, which must be an existing user.
    pub fn add_token(&mut self, login: &str, scopes: &[&str]) -> String {
        let login = self
            .user(login)
            .unwrap_or_else(|| panic!("no user {login}"))
            .login
            .clone();
        let token = ninot_common::token::new_token(crate::server::TOKEN_PREFIX);
        self.tokens.insert(
            token.clone(),
            Caller {
                login,
                scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            },
        );
        token
    }

    pub fn user(&self, login: &str) -> Option<&User> {
        self.users.get(&login.to_lowercase())
    }

    /// Iterates the users whose repositories can be searched.
    pub fn users(&self) -> impl Iterator<Item = &User> {
        self.users.values()
    }

    pub fn user_mut(&mut self, login: &str) -> Option<&mut User> {
        self.users.get_mut(&login.to_lowercase())
    }
}

pub mod pull;
