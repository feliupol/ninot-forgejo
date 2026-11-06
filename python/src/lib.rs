//! Python fixture bindings for the Forgejo protocol server.

use ninot_forgejo::server::{
    ActionRun as RustActionRun, CommitStatusSeed, PullFixture, PullRequest as RustPullRequest,
    Repository as RustRepository, ScheduledMergeInfo as RustScheduledMergeInfo,
    Server as RustServer, User as RustUser,
};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::path::Path;

/// A running Forgejo service with direct fixture operations.
#[pyclass]
pub struct Server {
    inner: Option<RustServer>,
}

#[pyclass]
pub struct User {
    inner: RustUser,
}

#[pyclass]
pub struct Repository {
    inner: RustRepository,
}

#[pyclass]
pub struct PullRequest {
    inner: RustPullRequest,
}

#[pyclass]
pub struct ActionRun {
    inner: RustActionRun,
}

#[pyclass]
pub struct ScheduledMergeInfo {
    inner: RustScheduledMergeInfo,
}

impl Server {
    /// Borrows a running server, rejecting use after stop.
    fn running(&self) -> PyResult<&RustServer> {
        self.inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("server was stopped"))
    }
}

#[pymethods]
impl User {
    #[getter]
    fn login(&self) -> &str {
        self.inner.login()
    }

    #[getter]
    fn id(&self) -> PyResult<u64> {
        self.inner.id().map_err(error)
    }

    /// Sets this user's display name and email.
    #[pyo3(signature = (full_name, email=""))]
    fn set_profile(&self, full_name: &str, email: &str) -> PyResult<()> {
        self.inner.set_profile(full_name, email).map_err(error)
    }

    /// Issues a token acting as this user.
    fn create_token(&self, scopes: Vec<String>) -> PyResult<String> {
        let scopes: Vec<_> = scopes.iter().map(String::as_str).collect();
        self.inner.create_token(&scopes).map_err(error)
    }

    /// Creates a repository owned by this user.
    #[pyo3(signature = (name, private=false, default_branch="main", auto_init=true))]
    fn create_repository(
        &self,
        name: &str,
        private: bool,
        default_branch: &str,
        auto_init: bool,
    ) -> PyResult<Repository> {
        Ok(Repository {
            inner: self
                .inner
                .create_repository(name, private, default_branch, auto_init)
                .map_err(error)?,
        })
    }
}

#[pymethods]
impl Repository {
    #[getter]
    fn owner(&self) -> &str {
        self.inner.owner()
    }

    #[getter]
    fn name(&self) -> &str {
        self.inner.name()
    }

    /// Sets the description returned by repository API responses.
    fn set_description(&self, description: &str) -> PyResult<()> {
        self.inner.set_description(description).map_err(error)
    }

    /// Commits UTF-8 fixture content and returns its real Git SHA.
    ///
    /// `None` preserves Git's file-mode behavior; `True` sets the executable bit and `False` clears it.
    #[pyo3(signature = (branch, path, content, message="Update fixture", executable=None))]
    fn commit_file(
        &self,
        branch: &str,
        path: &str,
        content: &str,
        message: &str,
        executable: Option<bool>,
    ) -> PyResult<String> {
        self.inner
            .commit_file_with_mode(
                branch,
                Path::new(path),
                content.as_bytes(),
                message,
                executable,
            )
            .map_err(error)
    }

    /// Seeds a branch from an existing branch or Git SHA.
    #[pyo3(signature = (branch, reference="main"))]
    fn create_branch(&self, branch: &str, reference: &str) -> PyResult<()> {
        self.inner.create_branch(branch, reference).map_err(error)
    }

    /// Creates a label in this repository.
    #[pyo3(signature = (label, color="00ff00"))]
    fn create_label(&self, label: &str, color: &str) -> PyResult<u64> {
        self.inner.create_label(label, color).map_err(error)
    }

    /// Seeds a typed commit status on an existing commit.
    fn create_commit_status(&self, py: Python<'_>, seed: &Bound<'_, PyAny>) -> PyResult<u64> {
        let json = PyModule::import(py, "json")?
            .call_method1("dumps", (seed,))?
            .extract::<String>()?;
        let seed: CommitStatusSeed = serde_json::from_str(&json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        self.inner.create_commit_status(seed).map_err(error)
    }

    /// Looks up an existing pull request by service index.
    fn pull_request(&self, index: u64) -> PyResult<PullRequest> {
        Ok(PullRequest {
            inner: self.inner.pull_request(index).map_err(error)?,
        })
    }

    /// Creates a pull request and its paired issue against real branch commits.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (head, base="main", title="Fixture pull request", body="", author="admin", labels=None, draft=false))]
    fn create_pull_request(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        author: &str,
        labels: Option<Vec<String>>,
        draft: bool,
    ) -> PyResult<PullRequest> {
        Ok(PullRequest {
            inner: self
                .inner
                .create_pull_request(PullFixture {
                    head: head.to_owned(),
                    base: base.to_owned(),
                    title: title.to_owned(),
                    body: body.to_owned(),
                    author: author.to_owned(),
                    labels: labels.unwrap_or_default(),
                    draft,
                })
                .map_err(error)?,
        })
    }

    /// Seeds an Actions task through the generated protocol model.
    fn create_action_task(&self, py: Python<'_>, task: &Bound<'_, PyAny>) -> PyResult<u64> {
        let json = PyModule::import(py, "json")?
            .call_method1("dumps", (task,))?
            .extract::<String>()?;
        let task = serde_json::from_str(&json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        self.inner.create_action_task(task).map_err(error)
    }

    /// Seeds an existing write-only Actions secret by name.
    fn create_action_secret(&self, secret_name: &str) -> PyResult<()> {
        self.inner.create_action_secret(secret_name).map_err(error)
    }

    /// Returns Actions secret names without exposing their values.
    fn action_secret_names(&self) -> PyResult<Vec<String>> {
        self.inner.action_secret_names().map_err(error)
    }

    /// Creates an Actions run and its jobs through typed generated models.
    fn create_action_run(
        &self,
        py: Python<'_>,
        run: &Bound<'_, PyAny>,
        jobs: &Bound<'_, PyAny>,
    ) -> PyResult<ActionRun> {
        let json_module = PyModule::import(py, "json")?;
        let run_json = json_module
            .call_method1("dumps", (run,))?
            .extract::<String>()?;
        let jobs_json = json_module
            .call_method1("dumps", (jobs,))?
            .extract::<String>()?;
        let run = serde_json::from_str(&run_json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        let jobs = serde_json::from_str(&jobs_json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        Ok(ActionRun {
            inner: self.inner.create_action_run(run, jobs).map_err(error)?,
        })
    }

    /// Queues a dispatch which returns the specified seeded workflow run.
    fn script_workflow_dispatch(
        &self,
        workflow_filename: &str,
        reference: &str,
        inputs: std::collections::HashMap<String, String>,
        run: &ActionRun,
    ) -> PyResult<()> {
        self.inner
            .script_workflow_dispatch(
                workflow_filename,
                reference,
                inputs.into_iter().collect(),
                &run.inner,
            )
            .map_err(error)
    }

    /// Sets the default for deleting merged pull request branches.
    fn set_delete_branch_after_merge(&self, enabled: bool) -> PyResult<()> {
        self.inner
            .set_delete_branch_after_merge(enabled)
            .map_err(error)
    }

    /// Advances ready scheduled merges once without background timing.
    fn advance(&self) -> PyResult<usize> {
        self.inner.advance().map_err(error)
    }
}

#[pymethods]
impl PullRequest {
    #[getter]
    fn index(&self) -> u64 {
        self.inner.index()
    }

    /// Requests a review from a user on this pull request.
    fn request_review(&self, login: &str) -> PyResult<()> {
        self.inner.request_review(login).map_err(error)
    }

    /// Adds a supported event to this pull request's timeline.
    fn create_timeline_event(&self, py: Python<'_>, seed: &Bound<'_, PyAny>) -> PyResult<()> {
        let json = PyModule::import(py, "json")?
            .call_method1("dumps", (seed,))?
            .extract::<String>()?;
        let seed = serde_json::from_str(&json)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        self.inner.create_timeline_event(seed).map_err(error)
    }

    /// Returns pending merge configuration, if the pull request has one.
    fn scheduled_merge(&self) -> PyResult<Option<ScheduledMergeInfo>> {
        Ok(self
            .inner
            .scheduled_merge()
            .map_err(error)?
            .map(|inner| ScheduledMergeInfo { inner }))
    }
}

#[pymethods]
impl ScheduledMergeInfo {
    #[getter]
    fn merge_method(&self) -> &str {
        &self.inner.merge_method
    }

    #[getter]
    fn delete_branch(&self) -> bool {
        self.inner.delete_branch
    }
}

#[pymethods]
impl ActionRun {
    #[getter]
    fn id(&self) -> u64 {
        self.inner.id()
    }

    /// Scripts successive job-status frames for workflow-run observations.
    fn script_status_frames(&self, frames: Vec<Vec<String>>) -> PyResult<()> {
        self.inner.script_status_frames(frames).map_err(error)
    }

    /// Records the log text of the job at `job_index`, in workflow order.
    fn set_job_log(&self, job_index: usize, log: &str) -> PyResult<()> {
        self.inner.set_job_log(job_index, log).map_err(error)
    }
}

/// Converts fixture failures into Python exceptions.
fn error(error: std::io::Error) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[pymethods]
impl Server {
    /// Starts a loopback service on a free port, over HTTPS when `tls` is set.
    #[new]
    #[pyo3(signature = (tls=false))]
    fn new(tls: bool) -> PyResult<Self> {
        Ok(Self {
            inner: Some(
                RustServer::start_with(ninot_forgejo::server::Options {
                    tls,
                    ..Default::default()
                })
                .map_err(error)?,
            ),
        })
    }

    /// Root URL (`http`, or `https` when TLS is on), including Git transport.
    #[getter]
    fn url(&self) -> PyResult<String> {
        Ok(self.running()?.url().to_owned())
    }

    /// PEM path of the local certificate authority, absent unless the service speaks HTTPS.
    #[getter]
    fn ca_cert_path(&self) -> PyResult<Option<String>> {
        Ok(self
            .running()?
            .ca_cert_path()
            .map(|path| path.to_string_lossy().into_owned()))
    }

    /// Forgejo REST API base URL.
    #[getter]
    fn api_url(&self) -> PyResult<String> {
        Ok(format!("{}/api/v1", self.running()?.url()))
    }

    /// Administrator credential for the client under test.
    #[getter]
    fn admin_token(&self) -> PyResult<String> {
        Ok(self.running()?.admin_token().to_owned())
    }

    /// Creates a user fixture and returns its resource handle.
    fn create_user(&self, login: &str) -> PyResult<User> {
        Ok(User {
            inner: self.running()?.create_user(login),
        })
    }

    /// Looks up an existing user fixture.
    fn user(&self, login: &str) -> PyResult<User> {
        Ok(User {
            inner: self.running()?.user(login).map_err(error)?,
        })
    }

    /// Looks up an existing repository fixture.
    fn repository(&self, owner: &str, name: &str) -> PyResult<Repository> {
        Ok(Repository {
            inner: self.running()?.repository(owner, name).map_err(error)?,
        })
    }

    /// Starts recording requests actually received by the HTTP service.
    fn start_profile(&self) -> PyResult<()> {
        self.running()?.start_profile();
        Ok(())
    }

    /// Stops recording and returns each request's native route label and outcome.
    fn stop_profile(&self, py: Python<'_>) -> PyResult<Vec<Py<PyDict>>> {
        self.running()?
            .stop_profile()
            .hits()
            .iter()
            .map(|hit| {
                let record = PyDict::new(py);
                record.set_item("method", &hit.method)?;
                record.set_item("path", &hit.path)?;
                record.set_item("label", &hit.label)?;
                record.set_item("query", &hit.query)?;
                record.set_item("status", hit.status)?;
                Ok(record.unbind())
            })
            .collect()
    }

    /// Enables a bounded direct-fixture trace of request bodies, without auth headers.
    fn start_request_trace(&self) -> PyResult<()> {
        self.running()?.start_request_trace();
        Ok(())
    }

    /// Stops request tracing and returns captured requests plus an explicit truncation flag.
    fn stop_request_trace(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let trace = self.running()?.stop_request_trace();
        let result = PyDict::new(py);
        let requests = PyList::empty(py);
        for hit in trace.requests {
            let request = PyDict::new(py);
            request.set_item("method", hit.method)?;
            request.set_item("path", hit.path)?;
            request.set_item("query", hit.query)?;
            request.set_item("body", PyBytes::new(py, &hit.body))?;
            requests.append(request)?;
        }
        result.set_item("requests", requests)?;
        result.set_item("truncated", trace.truncated)?;
        Ok(result.unbind())
    }

    /// Rejects the next matching request before it changes service state.
    fn fail_next(&self, method: &str, path: &str, status: u16) -> PyResult<()> {
        self.running()?
            .fail_next(method, path, status)
            .map_err(error)
    }

    /// Delays the next request matching an exact method and absolute URL path.
    fn delay_next(&self, method: &str, path: &str, milliseconds: u64) -> PyResult<()> {
        self.running()?.delay_next(method, path, milliseconds);
        Ok(())
    }

    /// Removes every injected fault.
    fn clear_faults(&self) -> PyResult<()> {
        self.running()?.clear_faults();
        Ok(())
    }

    /// Stops the service and removes its temporary Git storage.
    fn stop(&mut self) {
        self.inner = None;
    }

    /// Enters the fixture context.
    fn __enter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    /// Stops the service on context exit.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) {
        self.stop();
    }
}

/// Loads the native fixture API.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Server>()?;
    module.add_class::<User>()?;
    module.add_class::<Repository>()?;
    module.add_class::<PullRequest>()?;
    module.add_class::<ActionRun>()?;
    module.add_class::<ScheduledMergeInfo>()?;
    Ok(())
}
