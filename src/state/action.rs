//! Actions runs, their jobs, and explicit fixture-defined lifecycle transitions.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// One workflow run and the jobs whose aggregate status it exposes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRun {
    /// The generated Forgejo run response.
    pub run: crate::types::ActionRun,
    /// Jobs in workflow order.
    pub jobs: Vec<crate::types::ActionRunJob>,
    /// Successive job-status frames, advanced by run-list reads.
    pub script: VecDeque<Vec<String>>,
    /// Log text of each job that has executed, keyed by job identifier.
    #[serde(default)]
    pub logs: std::collections::BTreeMap<i64, String>,
}

/// One fixture-defined workflow dispatch that consumes a pre-seeded real run.
#[derive(Clone, Debug)]
pub struct WorkflowDispatch {
    /// Workflow filename requested by the client.
    pub workflow_filename: String,
    /// Required ref from the dispatch payload.
    pub reference: String,
    /// Required workflow inputs from the dispatch payload.
    pub inputs: std::collections::BTreeMap<String, String>,
    /// Existing run identifier returned by the dispatch response.
    pub run_id: i64,
}

impl ActionRun {
    /// Applies one explicit lifecycle frame before the next run-list observation.
    pub fn advance(&mut self) {
        if let Some(statuses) = self.script.pop_front() {
            for (job, status) in self.jobs.iter_mut().zip(statuses) {
                job.status = Some(status);
            }
            self.run.status = Some(aggregate(&self.jobs).to_owned());
        }
    }

    /// Cancels unfinished jobs and leaves their completed predecessors intact.
    pub fn cancel(&mut self) {
        self.script.clear();
        for job in &mut self.jobs {
            if matches!(
                job.status.as_deref(),
                Some("waiting" | "blocked" | "running")
            ) {
                job.status = Some("cancelled".to_owned());
            }
        }
        self.run.status = Some(aggregate(&self.jobs).to_owned());
    }
}

/// Forgejo's aggregate status precedence for the supported job lifecycle states.
pub fn aggregate(jobs: &[crate::types::ActionRunJob]) -> &'static str {
    for status in ["cancelled", "failure", "running", "waiting", "blocked"] {
        if jobs.iter().any(|job| job.status.as_deref() == Some(status)) {
            return status;
        }
    }
    if !jobs.is_empty()
        && jobs
            .iter()
            .all(|job| job.status.as_deref() == Some("skipped"))
    {
        "skipped"
    } else {
        "success"
    }
}
