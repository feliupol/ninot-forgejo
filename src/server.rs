//! The HTTP server and its lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use axum::body::{Body as AxumBody, to_bytes};
use axum::extract::{Request, State as AxumState};
use axum::response::Response;
use headers::HeaderMapExt as _;
use headers::authorization::{Authorization, Basic, Bearer};
use http::{HeaderMap, HeaderValue, StatusCode, request::Parts};
use ninot_common::git;
use ninot_common::profile::{Profile, Profiler};
use ninot_common::tls::CA_FILE;
use ninot_common::token;
use serde_json::Value;

use crate::api::{ApiRequest, ApiResponse, Body, Caller};
use crate::handlers;
use crate::operation::{Method, OPERATIONS};
use crate::parse;
use crate::router::Router;
use crate::state::{State, WorkflowDispatch, lock, now};
use crate::webhooks;

/// Prefix of a fake's tokens, which Forgejo spells `fjo_`.
pub(crate) const TOKEN_PREFIX: &str = "fjo_";

/// Path prefix every API operation of the spec lives under.
pub(crate) const API_PREFIX: &str = "/api/v1";

/// Login of the user that owns the admin token.
pub(crate) const ADMIN_LOGIN: &str = "admin";

/// Every scope the spec defines, as the admin token holds all of them.
pub(crate) const ADMIN_SCOPES: [&str; 18] = [
    "read:activitypub",
    "write:activitypub",
    "read:admin",
    "write:admin",
    "read:issue",
    "write:issue",
    "read:misc",
    "write:misc",
    "read:notification",
    "write:notification",
    "read:organization",
    "write:organization",
    "read:package",
    "write:package",
    "read:repository",
    "write:repository",
    "read:user",
    "write:user",
];

/// Routes and state shared by request handlers.
pub(crate) struct App {
    profiler: Profiler,
    router: Router,
    pub state: Arc<Mutex<State>>,
    /// `http` or `https`, for the URLs the fake hands out.
    scheme: &'static str,
}

/// How to start a fake.
#[derive(Clone, Debug)]
pub struct Options {
    pub address: SocketAddr,
    /// Serves HTTPS with a certificate from a fresh local certificate authority, whose
    /// certificate [`Server::ca_cert_path`] points at.
    pub tls: bool,
    /// Which addresses a webhook delivery may contact (`ninot_forgejo::TargetPolicy`).
    /// The default, `ninot_forgejo::TargetPolicy::Any`, allows every address, including
    /// loopback, so local receivers keep working.
    pub webhook_targets: crate::webhooks::TargetPolicy,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            tls: false,
            webhook_targets: webhooks::TargetPolicy::Any,
        }
    }
}

/// A running fake, stopped when dropped.
pub struct Server {
    admin_token: String,
    app: Arc<App>,
    harness: ninot_common::server::Server,
    data_dir: PathBuf,
}

/// A fixture handle for a user in the running service.
pub struct User {
    app: Weak<App>,
    url: String,
    login: String,
}

/// A fixture handle for a repository in the running service.
#[derive(Clone)]
pub struct Repository {
    app: Weak<App>,
    url: String,
    owner: String,
    name: String,
}

/// A fixture handle for a pull request in the running service.
pub struct PullRequest {
    repository: Repository,
    index: u64,
}

/// A fixture handle for an Actions run in the running service.
pub struct ActionRun {
    repository: Repository,
    id: u64,
}

/// The merge strategy and branch-deletion policy for a scheduled merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledMergeInfo {
    pub merge_method: String,
    pub delete_branch: bool,
}

/// Typed seed for one supported pull-request timeline event.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineEventSeed {
    /// Forgejo event type.
    #[serde(rename = "type")]
    pub event_type: crate::state::pull::TimelineEventKind,
}

/// Supported states for a seeded commit status.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatusState {
    /// A status is still running.
    Pending,
    /// The commit passed this check.
    Success,
    /// A non-failure error occurred.
    Error,
    /// The commit failed this check.
    Failure,
    /// The check reported a warning.
    Warning,
    /// The check was skipped.
    Skipped,
}

impl CommitStatusState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Error => "error",
            Self::Failure => "failure",
            Self::Warning => "warning",
            Self::Skipped => "skipped",
        }
    }
}

/// Typed seed for a status attached to an existing commit.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitStatusSeed {
    /// A commit SHA or reference that resolves to a commit.
    pub sha: String,
    /// Status context shown to clients.
    pub context: String,
    /// Result of this check.
    pub state: CommitStatusState,
    /// Human-readable status description.
    #[serde(default)]
    pub description: Option<String>,
    /// Link associated with the check.
    #[serde(default)]
    pub target_url: Option<String>,
}

/// Seed data for a pull request backed by existing Git branches.
#[derive(Clone, Debug, Default)]
pub struct PullFixture {
    /// Pull request title.
    pub title: String,
    /// Markdown description.
    pub body: String,
    /// Head branch in the same repository.
    pub head: String,
    /// Target branch.
    pub base: String,
    /// Author login, defaulting to the fixture administrator when empty.
    pub author: String,
    /// Existing repository label names.
    pub labels: Vec<String>,
    /// Whether the pull request is a draft.
    pub draft: bool,
}

impl Server {
    /// Starts a fake on a free local port.
    pub fn start() -> io::Result<Self> {
        Self::start_with(Options::default())
    }

    /// Starts a fake as `options` describes.
    ///
    /// # Errors
    ///
    /// When the port cannot be bound, the data directory cannot be created or the listener
    /// cannot be served.
    pub fn start_with(options: Options) -> io::Result<Self> {
        let listener = ninot_common::server::Server::bind(options.address)?;
        let scheme = if options.tls { "https" } else { "http" };
        let admin_token = token::new_token(TOKEN_PREFIX);
        let name = token::new_token(TOKEN_PREFIX);
        let data_dir = std::env::temp_dir().join(format!("ninot-forgejo-{}", &name[4..]));
        std::fs::create_dir_all(&data_dir)?;
        let tls = if options.tls {
            let tls =
                ninot_common::tls::generate("ninot-forgejo local CA").map_err(io::Error::other)?;
            std::fs::write(data_dir.join(CA_FILE), &tls.ca_pem)?;
            Some(tls.config)
        } else {
            None
        };
        let mut state = State::default();
        state.git_root = Some(data_dir.clone());
        state.webhook_targets = options.webhook_targets;
        state.add_admin(ADMIN_LOGIN);
        state.tokens.insert(
            admin_token.clone(),
            Caller {
                login: ADMIN_LOGIN.to_owned(),
                scopes: ADMIN_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_owned())
                    .collect(),
            },
        );
        let state_arc = Arc::new(Mutex::new(state));
        let queue = webhooks::spawn_sender(Arc::downgrade(&state_arc));
        lock(&state_arc).webhook_queue = Some(queue);
        let app = Arc::new(App {
            profiler: Profiler::new(),
            router: Router::new(OPERATIONS),
            state: state_arc,
            scheme,
        });
        let router = axum::Router::new()
            .fallback(dispatch)
            .with_state(app.clone());
        let router = app.profiler.layer(router);
        let harness = match tls {
            None => ninot_common::server::Server::serve(listener, router)?,
            Some(config) => ninot_common::server::Server::serve_tls(listener, router, config)?,
        };
        Ok(Self {
            admin_token,
            app,
            harness,
            data_dir,
        })
    }

    /// Base URL, such as `http://127.0.0.1:1234`. The API is served under `/api/v1`, and git
    /// at the root.
    pub fn url(&self) -> &str {
        self.harness.url()
    }

    /// PEM certificate of the local certificate authority when serving HTTPS. Point clients at
    /// it, for example with `SSL_CERT_FILE` or git's `http.sslCAInfo`.
    pub fn ca_cert_path(&self) -> Option<PathBuf> {
        let path = self.data_dir.join(CA_FILE);
        path.exists().then_some(path)
    }

    /// Token of the `admin` user, which holds every scope.
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// Starts recording client requests, clearing the previous profile.
    pub fn start_profile(&self) {
        self.app.profiler.start();
    }

    /// Stops recording and returns the requests actually received over the network.
    pub fn stop_profile(&self) -> Profile {
        self.app.profiler.stop()
    }

    /// Enables bounded capture of real REST request bodies for security assertions.
    pub fn start_request_trace(&self) {
        lock(&self.app.state).start_request_trace();
    }

    /// Stops bounded request capture and returns its records and truncation status.
    pub fn stop_request_trace(&self) -> crate::state::RequestTrace {
        lock(&self.app.state).stop_request_trace()
    }

    /// Rejects the next matching request with a real Forgejo error response.
    ///
    /// # Errors
    /// Rejects status codes outside the HTTP error range.
    pub fn fail_next(&self, method: &str, path: &str, status: u16) -> io::Result<()> {
        if !(400..600).contains(&status) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fault status must be an HTTP error",
            ));
        }
        self.add_fault(method, path, crate::faults::Fault::Status(status));
        Ok(())
    }

    /// Adds deterministic latency to the next matching request.
    pub fn delay_next(&self, method: &str, path: &str, milliseconds: u64) {
        self.add_fault(method, path, crate::faults::Fault::Latency(milliseconds));
    }

    /// Removes all fault rules and their application history.
    pub fn clear_faults(&self) {
        lock(&self.app.state).faults.clear();
    }

    /// Installs a single-use request fault in the shared engine.
    fn add_fault(&self, method: &str, path: &str, fault: crate::faults::Fault) {
        let target = crate::faults::Target {
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
        };
        lock(&self.app.state)
            .faults
            .add(ninot_common::faults::Rule::new(target, fault).times(1));
    }

    /// Creates a user fixture and returns its resource handle.
    pub fn create_user(&self, login: &str) -> User {
        lock(&self.app.state).add_user(login);
        User {
            app: Arc::downgrade(&self.app),
            url: self.url().to_owned(),
            login: login.to_owned(),
        }
    }

    /// Looks up an existing user fixture.
    ///
    /// # Errors
    /// Returns `NotFound` when the user does not exist.
    pub fn user(&self, login: &str) -> io::Result<User> {
        if lock(&self.app.state).user(login).is_none() {
            return Err(not_found("user does not exist"));
        }
        Ok(User {
            app: Arc::downgrade(&self.app),
            url: self.url().to_owned(),
            login: login.to_owned(),
        })
    }

    /// Looks up an existing repository fixture.
    ///
    /// # Errors
    /// Returns `NotFound` when the owner or repository does not exist.
    pub fn repository(&self, owner: &str, name: &str) -> io::Result<Repository> {
        if lock(&self.app.state)
            .user(owner)
            .and_then(|user| user.repo(name))
            .is_none()
        {
            return Err(not_found("repository does not exist"));
        }
        Ok(Repository {
            app: Arc::downgrade(&self.app),
            url: self.url().to_owned(),
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// Blocks until the server stops.
    pub fn wait(mut self) {
        self.harness.wait();
    }
}

fn not_found(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}

impl User {
    fn app(&self) -> io::Result<Arc<App>> {
        self.app
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "server was stopped"))
    }

    fn repository_handle(&self, name: &str) -> Repository {
        Repository {
            app: self.app.clone(),
            url: self.url.clone(),
            owner: self.login.clone(),
            name: name.to_owned(),
        }
    }

    /// User login this handle refers to.
    pub fn login(&self) -> &str {
        &self.login
    }

    /// Stable numeric ID assigned to this user by the running service.
    ///
    /// # Errors
    /// Returns `BrokenPipe` after the server stops and `NotFound` if the user was removed.
    pub fn id(&self) -> io::Result<u64> {
        let app = self.app()?;
        let state = lock(&app.state);
        state
            .user(&self.login)
            .map(|user| user.id)
            .ok_or_else(|| not_found("user does not exist"))
    }

    /// Sets the display name and email, creating the user when absent.
    pub fn set_profile(&self, full_name: &str, email: &str) -> io::Result<()> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let user = state.add_user(&self.login);
        user.full_name = full_name.to_owned();
        user.email = email.to_owned();
        Ok(())
    }

    /// Issues a token acting as this user.
    pub fn create_token(&self, scopes: &[&str]) -> io::Result<String> {
        Ok(lock(&self.app()?.state).add_token(&self.login, scopes))
    }

    /// Creates a repository owned by this user.
    ///
    /// # Errors
    /// Returns invalid names, duplicate repositories or Git initialization failures.
    pub fn create_repository(
        &self,
        name: &str,
        private: bool,
        default_branch: &str,
        auto_init: bool,
    ) -> io::Result<Repository> {
        create_repository(
            &self.app()?,
            &self.login,
            name,
            private,
            default_branch,
            auto_init,
        )?;
        Ok(self.repository_handle(name))
    }
}

fn create_repository(
    app: &Arc<App>,
    owner: &str,
    name: &str,
    private: bool,
    default_branch: &str,
    auto_init: bool,
) -> io::Result<()> {
    if !parse::repository_name(name) || !parse::repository_name(owner) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid repository owner or name",
        ));
    }
    let mut state = lock(&app.state);
    state.add_user(owner);
    state
        .create_repo(owner, name, private, None, default_branch)
        .map_err(io::Error::other)?;
    let git_dir = state
        .user(owner)
        .and_then(|user| user.repo(name))
        .expect("new repository exists")
        .git_dir
        .clone();
    let result = git::init_bare(&git_dir, default_branch).and_then(|()| {
        if auto_init {
            git::commit_readme(&git_dir, default_branch, name, &handlers::BOT)
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        state.delete_repo(owner, name);
        let _ = std::fs::remove_dir_all(git_dir);
    }
    result
}

impl Repository {
    fn app(&self) -> io::Result<Arc<App>> {
        self.app
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "server was stopped"))
    }

    /// Repository owner login.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Repository name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets the repository description returned by repository API responses.
    ///
    /// # Errors
    /// Returns `BrokenPipe` after the server stops and `NotFound` if the repository was removed.
    pub fn set_description(&self, description: &str) -> io::Result<()> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        repo.description = Some(description.to_owned());
        Ok(())
    }

    /// Commits fixture bytes on a branch, creating it from the default branch if absent.
    pub fn commit_file(
        &self,
        branch: &str,
        path: &Path,
        content: &[u8],
        message: &str,
    ) -> io::Result<String> {
        self.commit_file_with_mode(branch, path, content, message, None)
    }

    /// Commits fixture bytes on a branch, optionally setting or clearing its executable bit.
    /// `None` preserves Git's file-mode behavior.
    pub fn commit_file_with_mode(
        &self,
        branch: &str,
        path: &Path,
        content: &[u8],
        message: &str,
        executable: Option<bool>,
    ) -> io::Result<String> {
        let app = self.app()?;
        let state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        crate::merge::commit_file_with_executable_mode(
            &repo.git_dir,
            branch,
            &repo.default_branch,
            path,
            content,
            message,
            executable,
        )
        .map_err(|error| match error {
            crate::merge::Error::Git(error) => error,
            crate::merge::Error::Conflict(message) => io::Error::other(message),
        })
    }

    /// Seeds a branch at an existing commit or branch.
    pub fn create_branch(&self, branch: &str, reference: &str) -> io::Result<()> {
        let app = self.app()?;
        let state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let sha = git::resolve_commit(&repo.git_dir, reference)?
            .ok_or_else(|| not_found("commit does not exist"))?;
        git::create_ref(
            &repo.git_dir,
            &format!("refs/heads/{branch}"),
            &sha,
            &handlers::BOT,
        )
    }

    /// Seeds a commit status on an existing Git commit.
    pub fn create_commit_status(&self, seed: CommitStatusSeed) -> io::Result<u64> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let sha = git::resolve_commit(&repo.git_dir, &seed.sha)?
            .ok_or_else(|| not_found("commit does not exist"))?;
        let id = state.next_id();
        let timestamp = crate::time::now();
        let status = crate::types::CommitStatus {
            id: Some(id as i64),
            context: Some(seed.context),
            status: Some(crate::types::CommitStatusState(
                seed.state.as_str().to_owned(),
            )),
            description: Some(seed.description.unwrap_or_default()),
            target_url: Some(seed.target_url.unwrap_or_default()),
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
            creator: None,
            url: Some(format!(
                "{}/api/v1/repos/{}/{}/statuses/{sha}",
                self.url, self.owner, self.name
            )),
        };
        state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?
            .statuses
            .entry(sha)
            .or_default()
            .push(status);
        Ok(id)
    }

    /// Creates a pull request and its paired issue against actual Git branch state.
    pub fn create_pull_request(&self, fixture: PullFixture) -> io::Result<PullRequest> {
        let app = self.app()?;
        let index = create_pull_request(&app, &self.owner, &self.name, fixture)?;
        Ok(PullRequest {
            repository: self.clone(),
            index,
        })
    }

    /// Looks up an existing pull request by its service index.
    pub fn pull_request(&self, index: u64) -> io::Result<PullRequest> {
        let app = self.app()?;
        let state = lock(&app.state);
        if state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .and_then(|repo| repo.tracker.pull(index))
            .is_none()
        {
            return Err(not_found("pull request does not exist"));
        }
        Ok(PullRequest {
            repository: self.clone(),
            index,
        })
    }

    /// Seeds an Actions task and returns its assigned identifier.
    pub fn create_action_task(&self, mut task: crate::types::ActionTask) -> io::Result<u64> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        if state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .is_none()
        {
            return Err(not_found("repository does not exist"));
        }
        let id = state.next_id();
        task.id = Some(id as i64);
        task.created_at.get_or_insert_with(now);
        task.updated_at.get_or_insert_with(now);
        state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .expect("repository exists")
            .action_tasks
            .push(task);
        Ok(id)
    }

    /// Seeds an existing write-only Actions secret by name.
    pub fn create_action_secret(&self, secret_name: &str) -> io::Result<()> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        repo.action_secrets.insert(secret_name.to_owned());
        Ok(())
    }

    /// Returns secret names without exposing their values.
    pub fn action_secret_names(&self) -> io::Result<Vec<String>> {
        let app = self.app()?;
        let state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        Ok(repo.action_secrets.iter().cloned().collect())
    }

    /// Creates a workflow run and its jobs, assigning globally unique identifiers.
    pub fn create_action_run(
        &self,
        mut run: crate::types::ActionRun,
        mut jobs: Vec<crate::types::ActionRunJob>,
    ) -> io::Result<ActionRun> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let repo_id = repo.id as i64;
        let index = repo.action_runs.len() as i64 + 1;
        let owner_id = state.user(&self.owner).expect("repository owner exists").id as i64;
        let id = state.next_id();
        run.id = Some(id as i64);
        run.index_in_repo = Some(index);
        run.event.get_or_insert_with(|| "push".to_owned());
        run.workflow_id
            .get_or_insert_with(|| "workflow.yml".to_owned());
        run.created.get_or_insert_with(now);
        run.updated.get_or_insert_with(now);
        for job in &mut jobs {
            job.id = Some(state.next_id() as i64);
            job.run_id = Some(id as i64);
            job.owner_id = Some(owner_id);
            job.repo_id = Some(repo_id);
            job.attempt.get_or_insert(1);
            job.task_id.get_or_insert(0);
            job.status.get_or_insert_with(|| "waiting".to_owned());
        }
        run.status = Some(crate::state::aggregate_action_jobs(&jobs).to_owned());
        state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .expect("repository exists")
            .action_runs
            .insert(
                id as i64,
                crate::state::ActionRun {
                    run,
                    jobs,
                    script: Default::default(),
                    logs: Default::default(),
                },
            );
        Ok(ActionRun {
            repository: self.clone(),
            id,
        })
    }

    /// Queues a dispatch that returns a seeded workflow run.
    pub fn script_workflow_dispatch(
        &self,
        workflow_filename: &str,
        reference: &str,
        inputs: BTreeMap<String, String>,
        run: &ActionRun,
    ) -> io::Result<()> {
        if !self.app.ptr_eq(&run.repository.app)
            || run.repository.owner != self.owner
            || run.repository.name != self.name
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workflow run belongs to a different repository",
            ));
        }
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let Some(action_run) = repo.action_runs.get(&(run.id as i64)) else {
            return Err(not_found("workflow dispatch run does not exist"));
        };
        if action_run.run.workflow_id.as_deref() != Some(workflow_filename) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workflow dispatch filename does not match the seeded run",
            ));
        }
        repo.workflow_dispatches.push_back(WorkflowDispatch {
            workflow_filename: workflow_filename.to_owned(),
            reference: reference.to_owned(),
            inputs,
            run_id: run.id as i64,
        });
        Ok(())
    }

    /// Sets the repository default used when scheduling a merge.
    pub fn set_delete_branch_after_merge(&self, enabled: bool) -> io::Result<()> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user_mut(&self.owner)
            .and_then(|user| user.repo_mut(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        repo.delete_branch_after_merge = enabled;
        Ok(())
    }

    /// Seeds a label and returns its service-wide identifier.
    pub fn create_label(&self, label: &str, color: &str) -> io::Result<u64> {
        let app = self.app()?;
        let mut state = lock(&app.state);
        let repo = state
            .user(&self.owner)
            .and_then(|user| user.repo(&self.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        if repo.tracker.label_id(label).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "label already exists",
            ));
        }
        let id = state.next_id();
        state.tracker_mut(&self.owner, &self.name).add_label(
            id,
            crate::state::NewLabel {
                name: label.to_owned(),
                color: color.trim_start_matches('#').to_owned(),
                description: String::new(),
                exclusive: false,
                is_archived: false,
            },
        );
        Ok(id)
    }

    /// Advances scheduled merges whose latest checks are successful or skipped.
    pub fn advance(&self) -> io::Result<usize> {
        advance(&self.app()?, &self.url, &self.owner, &self.name)
    }
}

impl PullRequest {
    /// Pull request index assigned by the service.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// Requests a review from a user on this pull request.
    pub fn request_review(&self, login: &str) -> io::Result<()> {
        let app = self.repository.app()?;
        let mut state = lock(&app.state);
        if state
            .user(&self.repository.owner)
            .and_then(|user| user.repo(&self.repository.name))
            .and_then(|repo| repo.tracker.pull(self.index))
            .is_none()
        {
            return Err(not_found("pull request does not exist"));
        }
        state.add_user(login);
        let pull = state
            .pull_mut(&self.repository.owner, &self.repository.name, self.index)
            .expect("pull request exists");
        if !pull
            .reviewers
            .iter()
            .any(|reviewer| reviewer.eq_ignore_ascii_case(login))
        {
            pull.reviewers.push(login.to_owned());
        }
        Ok(())
    }

    /// Adds a typed event to this pull request's timeline.
    pub fn create_timeline_event(&self, seed: TimelineEventSeed) -> io::Result<()> {
        let app = self.repository.app()?;
        let mut state = lock(&app.state);
        if !state.add_pull_timeline_event(
            &self.repository.owner,
            &self.repository.name,
            self.index,
            seed.event_type,
        ) {
            return Err(not_found("pull request does not exist"));
        }
        Ok(())
    }

    /// Returns the scheduled merge method and branch-deletion policy, if any.
    pub fn scheduled_merge(&self) -> io::Result<Option<ScheduledMergeInfo>> {
        let app = self.repository.app()?;
        let state = lock(&app.state);
        let repo = state
            .user(&self.repository.owner)
            .and_then(|user| user.repo(&self.repository.name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let pull = repo
            .tracker
            .pull(self.index)
            .ok_or_else(|| not_found("pull request does not exist"))?;
        Ok(pull
            .scheduled_merge
            .as_ref()
            .map(|merge| ScheduledMergeInfo {
                merge_method: merge.method.as_str().to_owned(),
                delete_branch: merge.delete_branch,
            }))
    }
}

impl ActionRun {
    /// Run identifier assigned by the service.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Records the log a job produced, making it readable through the job-log endpoint.
    pub fn set_job_log(&self, job_index: usize, log: &str) -> io::Result<()> {
        let app = self.repository.app()?;
        let mut state = lock(&app.state);
        let run = state
            .user_mut(&self.repository.owner)
            .and_then(|user| user.repo_mut(&self.repository.name))
            .and_then(|repo| repo.action_runs.get_mut(&(self.id as i64)))
            .ok_or_else(|| not_found("action run does not exist"))?;
        let job_id = run
            .jobs
            .get(job_index)
            .and_then(|job| job.id)
            .ok_or_else(|| not_found("action run job does not exist"))?;
        run.logs.insert(job_id, log.to_owned());
        Ok(())
    }

    /// Scripts successive job-status frames consumed by workflow-run observations.
    pub fn script_status_frames(&self, frames: Vec<Vec<String>>) -> io::Result<()> {
        let app = self.repository.app()?;
        let mut state = lock(&app.state);
        let run = state
            .user_mut(&self.repository.owner)
            .and_then(|user| user.repo_mut(&self.repository.name))
            .and_then(|repo| repo.action_runs.get_mut(&(self.id as i64)))
            .ok_or_else(|| not_found("action run does not exist"))?;
        for statuses in &frames {
            if statuses.len() != run.jobs.len()
                || statuses.iter().any(|status| {
                    !matches!(
                        status.as_str(),
                        "waiting"
                            | "blocked"
                            | "running"
                            | "success"
                            | "failure"
                            | "cancelled"
                            | "skipped"
                    )
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "each lifecycle frame must contain one supported status per job",
                ));
            }
        }
        run.script = frames.into();
        Ok(())
    }
}

fn create_pull_request(
    app: &Arc<App>,
    owner: &str,
    name: &str,
    fixture: PullFixture,
) -> io::Result<u64> {
    let mut state = lock(&app.state);
    let repo = state
        .user(owner)
        .and_then(|user| user.repo(name))
        .ok_or_else(|| not_found("repository does not exist"))?;
    for branch in [&fixture.head, &fixture.base] {
        if git::resolve_commit(&repo.git_dir, &format!("refs/heads/{branch}"))?.is_none() {
            return Err(not_found("pull request branch does not exist"));
        }
    }
    if fixture.head == fixture.base
        || repo.tracker.pulls.values().any(|pull| {
            pull.head == fixture.head
                && pull.base == fixture.base
                && pull.state == crate::state::Status::Open
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "pull request already exists or has identical branches",
        ));
    }
    let labels = fixture
        .labels
        .iter()
        .map(|label| {
            repo.tracker
                .label_id(label)
                .ok_or_else(|| not_found("label does not exist"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let author = if fixture.author.is_empty() {
        ADMIN_LOGIN
    } else {
        &fixture.author
    };
    state.add_user(author);
    let id = state.next_id();
    let index = state.tracker_mut(owner, name).add_pull(
        id,
        crate::state::pull::NewPullRequest {
            title: fixture.title.clone(),
            body: fixture.body.clone(),
            poster: author.to_owned(),
            labels: labels.clone(),
            assignees: Vec::new(),
            head: fixture.head,
            base: fixture.base,
        },
    );
    state
        .pull_mut(owner, name, index)
        .expect("new pull request exists")
        .draft = fixture.draft;
    let id = state.next_id();
    state.tracker_mut(owner, name).add_issue_at(
        id,
        Some(index),
        crate::state::NewIssue {
            title: fixture.title,
            body: fixture.body,
            poster: author.to_owned(),
            labels,
            assignees: Vec::new(),
            closed: false,
        },
    );
    Ok(index)
}

fn advance(app: &Arc<App>, url: &str, owner: &str, name: &str) -> io::Result<usize> {
    let ready = {
        let state = lock(&app.state);
        let repo = state
            .user(owner)
            .and_then(|user| user.repo(name))
            .ok_or_else(|| not_found("repository does not exist"))?;
        let mut ready = Vec::new();
        for pull in repo.tracker.pulls.values() {
            let Some(merge) = &pull.scheduled_merge else {
                continue;
            };
            if pull.merged || pull.state != crate::state::Status::Open || pull.draft {
                continue;
            }
            let Some(sha) = git::resolve_commit(&repo.git_dir, &pull.head)? else {
                continue;
            };
            let mut seen = std::collections::BTreeSet::new();
            let checks_pass = repo
                .statuses
                .get(&sha)
                .into_iter()
                .flatten()
                .rev()
                .filter(|status| seen.insert(status.context.clone()))
                .all(|status| {
                    status
                        .status
                        .as_ref()
                        .is_some_and(|status| matches!(status.as_str(), "success" | "skipped"))
                });
            let required_present = merge.required_contexts.iter().all(|required| {
                repo.statuses
                    .get(&sha)
                    .into_iter()
                    .flatten()
                    .any(|status| status.context.as_ref() == Some(required))
            });
            if checks_pass && required_present {
                ready.push((pull.index, merge.clone()));
            }
        }
        ready
    };
    let mut completed = 0;
    for (index, merge) in ready {
        let body = serde_json::to_vec(&serde_json::json!({
            "Do": merge.method.as_str(), "MergeTitleField": merge.message,
            "delete_branch_after_merge": merge.delete_branch,
        }))
        .map_err(io::Error::other)?;
        let request = ApiRequest {
            path: format!("/repos/{owner}/{name}/pulls/{index}/merge"),
            params: HashMap::from([
                ("owner".to_owned(), owner.to_owned()),
                ("repo".to_owned(), name.to_owned()),
                ("index".to_owned(), index.to_string()),
            ]),
            query: Vec::new(),
            body: body.into(),
            caller: Some(Caller {
                login: merge.caller,
                scopes: vec!["write:repository".to_owned()],
            }),
            api_url: format!("{url}{API_PREFIX}"),
            web_url: url.to_owned(),
        };
        match <crate::ops::RepoMergePullRequest as handlers::Handler>::handle(&app.state, &request)
        {
            Ok(_) => completed += 1,
            Err(response) if response.status == 405 => {}
            Err(response) => {
                return Err(io::Error::other(format!(
                    "scheduled merge failed with HTTP {}",
                    response.status
                )));
            }
        }
    }
    Ok(completed)
}

impl Drop for Server {
    fn drop(&mut self) {
        self.harness.stop();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

async fn dispatch(AxumState(app): AxumState<Arc<App>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(_) => {
            return into_response(ApiResponse::error(400, "cannot read request body"), "", "");
        }
    };
    tokio::task::spawn_blocking(move || app.handle(parts, body))
        .await
        .unwrap_or_else(|_| into_response(ApiResponse::error(500, "handler panicked"), "", ""))
}

impl App {
    /// Applies fixture fault rules around the normal protocol transition.
    fn handle(&self, parts: Parts, body: bytes::Bytes) -> Response {
        if parts
            .uri
            .path()
            .strip_prefix(API_PREFIX)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        {
            lock(&self.state).record_request(
                parts.method.as_str(),
                parts.uri.path(),
                parts.uri.query().unwrap_or_default(),
                &body,
            );
        }
        let decision =
            lock(&self.state)
                .faults
                .decide(parts.method.as_str(), parts.uri.path(), |target| {
                    target.method == parts.method.as_str() && target.path == parts.uri.path()
                });
        if !decision.latency.is_zero() {
            // Sleeping is the deliberately injected transport delay, not scenario progression.
            std::thread::sleep(decision.latency);
        }
        if let Some(crate::faults::Fault::Status(status)) = decision.terminal {
            return into_response(
                ApiResponse::error(status, "Injected service failure"),
                "",
                parts.uri.path(),
            );
        }
        self.serve(parts, body)
    }

    /// Serves one request, the whole body of which has been read.
    fn serve(&self, parts: Parts, body: bytes::Bytes) -> Response {
        let full_path = parts.uri.path();
        let host = parts
            .headers
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("localhost")
            .to_owned();
        let web_url = format!("{}://{host}", self.scheme);
        let (api_url, path) = match full_path.strip_prefix(API_PREFIX) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => (
                format!("{web_url}{API_PREFIX}"),
                if rest.is_empty() {
                    "/".to_owned()
                } else {
                    rest.to_owned()
                },
            ),
            _ => (web_url.clone(), full_path.to_owned()),
        };
        if let Some(git_path) = git::parse_path(full_path) {
            return self.serve_git(&parts, &body, &git_path, &api_url, &web_url, &path);
        }
        let caller = match self.authenticate(&parts.headers) {
            Ok(caller) => caller,
            Err(response) => return into_response(response, &api_url, &path),
        };
        let found = method(&parts.method)
            .and_then(|method| self.router.find(method, full_path))
            .ok_or_else(ApiResponse::not_found);
        let found = match found {
            Ok(found) => found,
            Err(response) => return into_response(response, &api_url, &path),
        };
        let Some(handler) = handlers::handler(found.info) else {
            let response = ApiResponse::error(
                501,
                &format!("Not implemented by ninot-forgejo: {}", found.info.id),
            );
            return into_response(response, &api_url, &path);
        };
        let request = ApiRequest {
            path,
            params: found.params,
            query: parts
                .uri
                .query()
                .map(|query| {
                    url::form_urlencoded::parse(query.as_bytes())
                        .into_owned()
                        .collect()
                })
                .unwrap_or_default(),
            body,
            caller,
            api_url,
            web_url,
        };
        let response = handler(&self.state, &request).unwrap_or_else(|error| error);
        into_response(response, &request.api_url, &request.path)
    }

    /// Serves git's smart HTTP protocol, enforcing the repository's visibility and permissions.
    fn serve_git(
        &self,
        parts: &Parts,
        body: &bytes::Bytes,
        path: &git::GitPath,
        api_url: &str,
        web_url: &str,
        request_path: &str,
    ) -> Response {
        let query = parts.uri.query().unwrap_or("");
        let push = git::is_push(path.service_path, query);
        let caller = self.authenticate(&parts.headers).ok().flatten();
        let resolved = {
            let state = lock(&self.state);
            let target = state
                .permission(caller.as_ref(), path.owner, path.repo)
                .is_some()
                .then(|| (path.owner.to_owned(), path.repo.to_owned()))
                .or_else(|| {
                    state
                        .redirect(path.owner, path.repo)
                        .map(|(owner, name)| (owner.to_owned(), name.to_owned()))
                });
            target.and_then(|(owner, name)| {
                let permission = state.permission(caller.as_ref(), &owner, &name)?;
                let git_dir = state.user(&owner)?.repo(&name)?.git_dir.clone();
                Some((owner, name, permission, git_dir))
            })
        };
        // Git asks for credentials when the request is anonymous, and reads the status when it
        // is not.
        let denied = |status: u16, text: &str| {
            if caller.is_none() {
                git_text(401, "Authentication required\n")
                    .with_header("www-authenticate", "Basic realm=\"Forgejo\"".to_owned())
                    .into_axum(api_url, request_path)
            } else {
                git_text(status, text).into_axum(api_url, request_path)
            }
        };
        let (owner, name, git_dir) = match resolved {
            Some((owner, name, permission, git_dir)) if !push || permission.push => {
                (owner, name, git_dir)
            }
            Some(_) => return denied(403, "Permission denied\n"),
            None => return denied(404, "Repository not found.\n"),
        };
        let header = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        let request = git::CgiRequest {
            method: parts.method.as_str(),
            service_path: path.service_path,
            query,
            content_type: header("content-type"),
            content_encoding: header("content-encoding"),
            git_protocol: header("git-protocol"),
            remote_user: caller.as_ref().map(|caller| caller.login.as_str()),
            export_all: true,
            body,
        };
        let receive = parts.method == http::Method::POST && path.service_path == "git-receive-pack";
        let before = receive.then(|| ref_snapshot(&git_dir));
        let response = match git::http_backend(&git_dir, &request) {
            Ok(response) => response,
            Err(error) => {
                return git_text(500, &format!("{error}\n")).into_axum(api_url, request_path);
            }
        };
        if let Some(before) = before.filter(|_| response.status == 200) {
            let request = ApiRequest {
                path: format!("/repos/{owner}/{name}"),
                params: HashMap::new(),
                query: Vec::new(),
                body: bytes::Bytes::new(),
                caller,
                api_url: api_url.to_owned(),
                web_url: web_url.to_owned(),
            };
            self.record_push(&owner, &name, &git_dir, &before, &request);
        }
        let mut reply = ApiResponse::bytes(response.status, "", response.body);
        reply.headers = response.headers;
        into_response(reply, api_url, request_path)
    }

    /// Notes a completed push, and points a repository that had no branches at the branch the
    /// push created.
    fn record_push(
        &self,
        owner: &str,
        name: &str,
        git_dir: &Path,
        before: &BTreeMap<String, String>,
        request: &ApiRequest,
    ) {
        let after = ref_snapshot(git_dir);
        let mut state = lock(&self.state);
        let Some(repo) = state.user_mut(owner).and_then(|user| user.repo_mut(name)) else {
            return;
        };
        repo.pushed_at = Some(now());
        if let Some(branch) = first_default_branch(&repo.default_branch, before, &after)
            && let Err(error) = repo.set_default_branch(&branch)
        {
            eprintln!("ninot-forgejo: cannot set the default branch to {branch}: {error}");
        }
        webhooks::events::push::enqueue_push(
            &mut state,
            request,
            owner,
            name,
            Some(git_dir),
            before,
            &after,
        );
    }

    /// Resolves the `Authorization` header; an unknown token is a 401.
    fn authenticate(&self, headers: &HeaderMap) -> Result<Option<Caller>, ApiResponse> {
        if !headers.contains_key(http::header::AUTHORIZATION) {
            return Ok(None);
        }
        let state = lock(&self.state);
        token_of(headers)
            .and_then(|token| state.tokens.get(&token).cloned())
            .map(Some)
            .ok_or_else(ApiResponse::unauthorized)
    }
}

/// The token the `Authorization` header of `headers` carries, or `None` when it carries none
/// the fake knows, which is a 401. Basic auth and `Bearer` are the credentials the `headers`
/// crate has types for, and carry the token as the password and as the credentials themselves;
/// `token` is Forgejo's own scheme, which it has no type for.
fn token_of(headers: &HeaderMap) -> Option<String> {
    if let Some(Authorization(basic)) = headers.typed_get::<Authorization<Basic>>() {
        return Some(basic.password().to_owned());
    }
    if let Some(Authorization(bearer)) = headers.typed_get::<Authorization<Bearer>>() {
        return Some(bearer.token().to_owned());
    }
    token_credentials(headers).map(str::to_owned)
}

/// The credentials of Forgejo's own `token` scheme, `token X`, which the `headers` crate has no
/// type for. Schemes are case-insensitive, as RFC 7235 has them.
fn token_credentials(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let (scheme, credentials) = parse::credentials(value)?;
    (scheme.eq_ignore_ascii_case("token")).then_some(credentials)
}

fn method(method: &http::Method) -> Option<Method> {
    Some(match *method {
        http::Method::GET => Method::Get,
        http::Method::POST => Method::Post,
        http::Method::PUT => Method::Put,
        http::Method::PATCH => Method::Patch,
        http::Method::DELETE => Method::Delete,
        _ => return None,
    })
}

/// Every ref of a bare repository and the sha it points at.
fn ref_snapshot(git_dir: &std::path::Path) -> BTreeMap<String, String> {
    git::refs(git_dir, "refs/")
        .unwrap_or_default()
        .into_iter()
        .map(|found| (found.name, found.sha))
        .collect()
}

/// The default branch a push into a repository without branches establishes: the current
/// default, then `main`, then `master`, then the first pushed branch by name. `None` when the
/// repository already had branches or the push created none.
fn first_default_branch(
    current: &str,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Option<String> {
    if before.keys().any(|name| parse::branch_of(name).is_some()) {
        return None;
    }
    let pushed: Vec<&str> = after
        .keys()
        .filter_map(|name| parse::branch_of(name))
        .collect();
    [current, "main", "master"]
        .into_iter()
        .find(|branch| pushed.contains(branch))
        .or_else(|| pushed.first().copied())
        .map(str::to_owned)
}

fn git_text(status: u16, text: &str) -> ApiResponse {
    ApiResponse::bytes(status, "text/plain", text.as_bytes().to_vec())
}

impl ApiResponse {
    fn into_axum(self, api_url: &str, path: &str) -> Response {
        into_response(self, api_url, path)
    }
}

/// Forgejo names the API URL of the request in every error body, which handlers do not know;
/// they leave it null and it is filled in here.
fn name_error_url(response: &mut ApiResponse, api_url: &str, path: &str) {
    if let Body::Json(Value::Object(body)) = &mut response.body
        && body.contains_key("message")
        && body.get("url") == Some(&Value::Null)
    {
        body.insert("url".to_owned(), format!("{api_url}{path}").as_str().into());
    }
}

fn into_response(response: ApiResponse, api_url: &str, path: &str) -> Response {
    let mut response = response;
    name_error_url(&mut response, api_url, path);
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    let headers = builder.headers_mut().unwrap();
    let body = match response.body {
        Body::Empty => AxumBody::empty(),
        Body::Json(value) => {
            headers.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            AxumBody::from(serde_json::to_vec(&value).unwrap())
        }
        Body::Bytes(bytes) => AxumBody::from(bytes),
    };
    for (name, value) in response.headers {
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    }
    builder.body(body).unwrap()
}

impl Server {
    /// A snapshot of every webhook delivery the fake has recorded, in the order they were sent.
    pub fn deliveries(&self) -> Vec<crate::state::Delivery> {
        lock(&self.app.state).deliveries.clone()
    }

    /// Waits until at least `count` deliveries are recorded, or `timeout` passes, and returns
    /// whatever was recorded by then.
    pub fn wait_for_deliveries(
        &self,
        count: usize,
        timeout: std::time::Duration,
    ) -> Vec<crate::state::Delivery> {
        let start = std::time::Instant::now();
        loop {
            let deliveries = self.deliveries();
            if deliveries.len() >= count || start.elapsed() > timeout {
                return deliveries;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token the `Authorization` header `value` carries.
    fn token_from_authorization(value: &str) -> Option<String> {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_str(value).ok()?,
        );
        token_of(&headers)
    }

    #[test]
    fn tokens_come_from_either_scheme_and_from_git_s_basic_auth() {
        assert_eq!(
            token_from_authorization("token abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            token_from_authorization("Bearer abc").as_deref(),
            Some("abc")
        );
        // `b2F1dGgyOmFiYw==` is `oauth2:abc`, which is what git sends for a token.
        assert_eq!(
            token_from_authorization("Basic b2F1dGgyOmFiYw==").as_deref(),
            Some("abc")
        );
        assert_eq!(token_from_authorization("basic"), None);
        assert_eq!(token_from_authorization("Digest abc"), None);
        assert_eq!(token_from_authorization("Basic bm9jb2xvbg=="), None);
    }

    #[test]
    fn the_default_branch_of_an_empty_repository_follows_the_first_push() {
        let after = BTreeMap::from([
            ("refs/heads/master".to_owned(), "abc".to_owned()),
            ("refs/tags/v1".to_owned(), "abc".to_owned()),
        ]);
        assert_eq!(
            first_default_branch("main", &BTreeMap::new(), &after).as_deref(),
            Some("master")
        );
        assert_eq!(first_default_branch("main", &after, &after), None);
        let both = BTreeMap::from([
            ("refs/heads/main".to_owned(), "abc".to_owned()),
            ("refs/heads/topic".to_owned(), "abc".to_owned()),
        ]);
        assert_eq!(
            first_default_branch("main", &BTreeMap::new(), &both).as_deref(),
            Some("main")
        );
        assert_eq!(
            first_default_branch("trunk", &BTreeMap::new(), &BTreeMap::new()),
            None
        );
    }
}
