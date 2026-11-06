#![allow(dead_code)]
//! Pull requests alongside issues, sharing the same numbering.

use super::{State, Status, now};
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// A timeline event kind supported by the fixture.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimelineEventKind {
    /// A merge was scheduled after required checks pass.
    #[serde(rename = "pull_scheduled_merge")]
    ScheduledMerge,
    /// A push changed the pull request head.
    #[serde(rename = "pull_push")]
    Push,
    /// A scheduled merge was cancelled.
    #[serde(rename = "pull_cancel_scheduled_merge")]
    CancelScheduledMerge,
}

impl TimelineEventKind {
    /// The event name Forgejo returns in timeline responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScheduledMerge => "pull_scheduled_merge",
            Self::Push => "pull_push",
            Self::CancelScheduledMerge => "pull_cancel_scheduled_merge",
        }
    }
}

/// One persisted pull-request timeline event.
#[derive(Clone)]
pub struct TimelineEvent {
    /// Identifier unique across service resources.
    pub id: u64,
    /// Event kind returned in the REST timeline.
    pub kind: TimelineEventKind,
    /// Event creation time.
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
    RebaseMerge,
    FastForwardOnly,
    ManuallyMerged,
}

impl MergeMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(Self::Merge),
            "squash" => Some(Self::Squash),
            "rebase" => Some(Self::Rebase),
            "rebase-merge" | "rebase_merge" => Some(Self::RebaseMerge),
            "fast-forward-only" | "fast_forward_only" => Some(Self::FastForwardOnly),
            "manually-merged" | "manually_merged" => Some(Self::ManuallyMerged),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
            Self::RebaseMerge => "rebase-merge",
            Self::FastForwardOnly => "fast-forward-only",
            Self::ManuallyMerged => "manually-merged",
        }
    }
}

/// A pull request.
#[derive(Clone)]
pub struct PullRequest {
    /// Users explicitly requested to review this PR.
    pub reviewers: Vec<String>,
    /// A deterministic pending merge, completed by the fixture's next advance.
    pub scheduled_merge: Option<ScheduledMerge>,
    /// Timeline events in their creation order.
    pub timeline: Vec<TimelineEvent>,
    /// Identifier unique across objects.
    pub id: u64,
    /// Shared index with issues.
    pub index: u64,
    pub title: String,
    pub body: String,
    pub state: Status,
    /// Login of the user who opened the PR.
    pub poster: String,
    /// Ids of labels.
    pub labels: Vec<u64>,
    /// Assignee logins.
    pub assignees: Vec<String>,
    pub base: String,
    pub head: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by: Option<String>,
    pub merge_commit_sha: Option<String>,
    pub merged: bool,
    pub draft: bool,
    /// Whether head branch was deleted after merge.
    pub head_deleted: bool,
}

/// Options retained while a merge waits for checks.
#[derive(Clone)]
pub struct ScheduledMerge {
    /// Check contexts that must report on the current head before completion.
    pub required_contexts: Vec<String>,
    /// Requested Git merge strategy.
    pub method: MergeMethod,
    /// User who authorized the merge.
    pub caller: String,
    /// Commit message for the eventual merge.
    pub message: String,
    /// Whether successful completion deletes the source branch.
    pub delete_branch: bool,
}

pub struct NewPullRequest {
    pub title: String,
    pub body: String,
    pub poster: String,
    pub labels: Vec<u64>,
    pub assignees: Vec<String>,
    pub base: String,
    pub head: String,
}

impl super::Tracker {
    /// Add a pull request and return its index.
    pub fn add_pull(&mut self, id: u64, new: NewPullRequest) -> u64 {
        self.last_index += 1;
        let index = self.last_index;
        let time = now();
        self.pulls.insert(
            index,
            PullRequest {
                reviewers: Vec::new(),
                scheduled_merge: None,
                timeline: Vec::new(),
                id,
                index,
                title: new.title,
                body: new.body,
                state: Status::Open,
                poster: new.poster,
                labels: new.labels,
                assignees: new.assignees,
                base: new.base.clone(),
                head: new.head.clone(),
                created_at: time,
                updated_at: time,
                closed_at: None,
                merged_at: None,
                merged_by: None,
                merge_commit_sha: None,
                merged: false,
                draft: false,
                head_deleted: false,
            },
        );
        index
    }

    pub fn pull(&self, index: u64) -> Option<&PullRequest> {
        self.pulls.get(&index)
    }

    pub fn pull_mut(&mut self, index: u64) -> Option<&mut PullRequest> {
        self.pulls.get_mut(&index)
    }
}

impl State {
    pub fn pull(&self, owner: &str, name: &str, index: u64) -> Option<&PullRequest> {
        self.tracker(owner, name).pull(index)
    }

    pub fn pull_mut(&mut self, owner: &str, name: &str, index: u64) -> Option<&mut PullRequest> {
        self.tracker_mut(owner, name).pull_mut(index)
    }

    /// Records an event on an existing pull request.
    pub fn add_pull_timeline_event(
        &mut self,
        owner: &str,
        name: &str,
        index: u64,
        kind: TimelineEventKind,
    ) -> bool {
        if self.pull(owner, name, index).is_none() {
            return false;
        }
        let event = TimelineEvent {
            id: self.next_id(),
            kind,
            created_at: now(),
        };
        self.pull_mut(owner, name, index)
            .expect("pull request was checked above")
            .timeline
            .push(event);
        true
    }
}
