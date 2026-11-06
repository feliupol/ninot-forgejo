//! Issues of a repository: their numbering, their fields, and the labels and comments hanging
//! off them.
//!
//! [`Tracker`] is what one repository knows about its issues. Pull requests will live beside
//! them and share its numbering, as Forgejo counts both from one counter.

use std::collections::BTreeMap;

use super::{Comment, Label, State, now};
use crate::api::Caller;
use chrono::{DateTime, Utc};

/// Whether an issue is open or closed, as Forgejo's `StateType` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
}

impl Status {
    /// The state as the spec's `StateType` names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    /// The state `state` names, which is either of the two.
    pub fn parse(state: &str) -> Option<Self> {
        match state {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// An issue of a repository, numbered as that repository counts its issues.
pub struct Issue {
    /// Identifier of the issue, unique across every object of the fake.
    pub id: u64,
    /// The number the issue is known by within its repository, counted from one and shared with
    /// the repository's pull requests.
    pub index: u64,
    pub title: String,
    pub body: String,
    pub state: Status,
    /// Login of the user who filed the issue.
    pub poster: String,
    /// Ids of the labels the issue carries, in the order they were added.
    pub labels: Vec<u64>,
    /// Logins of the users the issue is assigned to, in the order they were named.
    pub assignees: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the issue was closed, and `None` while it is open.
    pub closed_at: Option<DateTime<Utc>>,
}

/// What a request asks for when it files an issue.
pub struct NewIssue {
    pub title: String,
    pub body: String,
    /// Login of the user filing the issue.
    pub poster: String,
    /// Ids of labels the repository has.
    pub labels: Vec<u64>,
    /// Logins of users that exist.
    pub assignees: Vec<String>,
    /// Whether the issue is filed already closed.
    pub closed: bool,
}

/// What one repository knows about its issues: its labels, its issues and their comments, and
/// the number the next issue will get.
#[derive(Default)]
pub struct Tracker {
    /// Labels by id, as the label operations name them.
    pub(super) labels: BTreeMap<u64, Label>,
    /// Issues by their number within the repository.
    pub(super) issues: BTreeMap<u64, Issue>,
    /// Comments by id, of every issue of the repository.
    pub(super) comments: BTreeMap<u64, Comment>,
    /// The number the last issue was given.
    pub(super) last_index: u64,
    /// Pull requests by their index, alongside issues.
    pub(crate) pulls: BTreeMap<u64, super::pull::PullRequest>,
}

impl Tracker {
    /// The issue numbered `index`.
    pub fn issue(&self, index: u64) -> Option<&Issue> {
        self.issues.get(&index)
    }

    /// The issue numbered `index`, to change it.
    pub fn issue_mut(&mut self, index: u64) -> Option<&mut Issue> {
        self.issues.get_mut(&index)
    }

    /// Every issue of the repository, oldest first.
    pub fn issues(&self) -> impl DoubleEndedIterator<Item = &Issue> {
        self.issues.values()
    }

    /// How many issues are open.
    pub fn open_issues(&self) -> usize {
        self.issues
            .values()
            .filter(|issue| issue.state == Status::Open)
            .count()
    }

    /// Records an issue as the next one of the repository, and returns its number.
    pub fn add_issue(&mut self, id: u64, new: NewIssue) -> u64 {
        self.add_issue_at(id, None, new)
    }

    /// Records an issue at a specific index (or next if None), returning the index.
    pub fn add_issue_at(&mut self, id: u64, index: Option<u64>, new: NewIssue) -> u64 {
        let index = index.unwrap_or_else(|| {
            self.last_index += 1;
            self.last_index
        });
        if index > self.last_index {
            self.last_index = index;
        }
        let time = now();
        let closed_at = new.closed.then_some(time);
        self.issues.insert(
            index,
            Issue {
                id,
                index,
                title: new.title,
                body: new.body,
                state: if new.closed {
                    Status::Closed
                } else {
                    Status::Open
                },
                poster: new.poster,
                labels: new.labels,
                assignees: new.assignees,
                created_at: time,
                updated_at: time,
                closed_at,
            },
        );
        index
    }

    /// Whether the issue carries any of the labels named `names`, whatever their case.
    pub fn has_any_label(&self, issue: &Issue, names: &[String]) -> bool {
        issue
            .labels
            .iter()
            .filter_map(|id| self.label(*id))
            .any(|label| names.contains(&label.name.to_lowercase()))
    }
}

impl State {
    /// The issue tracker of `owner/name`, which the caller can see.
    pub fn tracker(&self, owner: &str, name: &str) -> &Tracker {
        &self
            .user(owner)
            .and_then(|user| user.repo(name))
            .expect("a visible repository has a tracker")
            .tracker
    }

    /// The issue tracker of `owner/name`, to change it.
    pub fn tracker_mut(&mut self, owner: &str, name: &str) -> &mut Tracker {
        &mut self
            .user_mut(owner)
            .and_then(|user| user.repo_mut(name))
            .expect("a visible repository has a tracker")
            .tracker
    }
}

/// Whether `caller` may change what `author` wrote: the author itself, or whoever may write to
/// the repository.
pub fn author_or_writer(caller: Option<&Caller>, author: &str, may_write: bool) -> bool {
    may_write || caller.is_some_and(|caller| caller.login.eq_ignore_ascii_case(author))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(closed: bool) -> NewIssue {
        NewIssue {
            title: "Broken".to_owned(),
            body: String::new(),
            poster: "alice".to_owned(),
            labels: Vec::new(),
            assignees: Vec::new(),
            closed,
        }
    }

    #[test]
    fn issues_are_numbered_from_one_and_closed_ones_record_when() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.add_issue(1, issue(false)), 1);
        assert_eq!(tracker.add_issue(2, issue(true)), 2);
        assert_eq!(tracker.add_issue(3, issue(false)), 3);
        assert!(tracker.issue(1).unwrap().closed_at.is_none());
        let closed = tracker.issue(2).unwrap();
        assert_eq!(closed.state, Status::Closed);
        assert!(closed.closed_at.is_some());
        assert_eq!(tracker.open_issues(), 2);
        assert_eq!(
            tracker
                .issues()
                .map(|issue| issue.index)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(tracker.issue(4).is_none());
    }

    #[test]
    fn a_state_is_named_as_the_spec_names_it() {
        assert_eq!(Status::parse("open"), Some(Status::Open));
        assert_eq!(Status::parse("closed"), Some(Status::Closed));
        assert_eq!(Status::parse("sideways"), None);
        assert_eq!(Status::Open.as_str(), "open");
        assert_eq!(Status::Closed.as_str(), "closed");
    }

    #[test]
    fn an_author_may_change_what_they_wrote_and_a_writer_may_change_anything() {
        let alice = Caller {
            login: "alice".to_owned(),
            scopes: Vec::new(),
        };
        let bob = Caller {
            login: "bob".to_owned(),
            scopes: Vec::new(),
        };
        assert!(author_or_writer(Some(&alice), "Alice", false));
        assert!(!author_or_writer(Some(&bob), "alice", false));
        assert!(author_or_writer(Some(&bob), "alice", true));
        assert!(!author_or_writer(None, "alice", false));
        assert!(author_or_writer(None, "alice", true));
    }
}
