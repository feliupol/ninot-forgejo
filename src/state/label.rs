//! Labels of a repository: the labels an issue or a pull request can carry.

use super::{Tracker, now};
use chrono::{DateTime, Utc};

/// A label of a repository, which issues and pull requests of it may carry.
pub struct Label {
    /// Identifier of the label, unique across every object of the fake.
    pub id: u64,
    /// Name to show and to look the label up by.
    pub name: String,
    /// Colour as Forgejo stores it: hexadecimal digits without a leading `#`.
    pub color: String,
    pub description: String,
    /// Whether the label excludes the other exclusive labels of an issue.
    pub exclusive: bool,
    /// Whether the label is kept out of the lists of assignable labels.
    pub is_archived: bool,
    pub updated_at: DateTime<Utc>,
}

/// What a request asks for when it creates a label.
pub struct NewLabel {
    pub name: String,
    pub color: String,
    pub description: String,
    pub exclusive: bool,
    pub is_archived: bool,
}

impl Tracker {
    /// The label with id `id`.
    pub fn label(&self, id: u64) -> Option<&Label> {
        self.labels.get(&id)
    }

    /// The label with id `id`, to change it.
    pub fn label_mut(&mut self, id: u64) -> Option<&mut Label> {
        self.labels.get_mut(&id)
    }

    /// The id of the label named `name`, whatever its case, or `None` when there is none.
    pub fn label_id(&self, name: &str) -> Option<u64> {
        self.labels
            .values()
            .find(|label| label.name.eq_ignore_ascii_case(name))
            .map(|label| label.id)
    }

    /// Every label of the repository, by name.
    pub fn labels(&self) -> Vec<&Label> {
        let mut labels: Vec<&Label> = self.labels.values().collect();
        labels.sort_by_key(|label| label.name.to_lowercase());
        labels
    }

    /// Records a label, which the repository does not have yet.
    pub fn add_label(&mut self, id: u64, new: NewLabel) {
        self.labels.insert(
            id,
            Label {
                id,
                name: new.name,
                color: new.color,
                description: new.description,
                exclusive: new.exclusive,
                is_archived: new.is_archived,
                updated_at: now(),
            },
        );
    }

    /// Removes the label with id `id`; the issues of the repository stop carrying it.
    pub fn remove_label(&mut self, id: u64) -> Option<Label> {
        let label = self.labels.remove(&id)?;
        for issue in self.issues.values_mut() {
            issue.labels.retain(|carried| *carried != id);
        }
        Some(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::issue::NewIssue;

    /// A tracker with `name` as its first label, whose id is `id`.
    fn tracker(id: u64, name: &str) -> Tracker {
        let mut tracker = Tracker::default();
        tracker.add_label(
            id,
            NewLabel {
                name: name.to_owned(),
                color: "ff0000".to_owned(),
                description: String::new(),
                exclusive: false,
                is_archived: false,
            },
        );
        tracker
    }

    /// An issue filed as `id`, carrying the labels `labels`.
    fn filed(tracker: &mut Tracker, id: u64, labels: Vec<u64>) -> u64 {
        tracker.add_issue(
            id,
            NewIssue {
                title: "Broken".to_owned(),
                body: String::new(),
                poster: "alice".to_owned(),
                labels,
                assignees: Vec::new(),
                closed: false,
            },
        )
    }

    #[test]
    fn labels_are_found_by_id_and_by_name() {
        let mut tracker = tracker(7, "Bug");
        assert_eq!(
            tracker.label(7).map(|label| label.color.as_str()),
            Some("ff0000")
        );
        assert!(tracker.label(8).is_none());
        assert_eq!(tracker.label_id("bug"), Some(7));
        assert_eq!(tracker.label_id("BUG"), Some(7));
        assert_eq!(tracker.label_id("other"), None);
        tracker.add_label(
            8,
            NewLabel {
                name: "docs".to_owned(),
                color: "00aabb".to_owned(),
                description: String::new(),
                exclusive: false,
                is_archived: false,
            },
        );
        assert_eq!(
            tracker
                .labels()
                .iter()
                .map(|label| label.name.as_str())
                .collect::<Vec<_>>(),
            ["Bug", "docs"]
        );
    }

    #[test]
    fn removing_a_label_takes_it_off_the_issues_that_carry_it() {
        let mut tracker = tracker(7, "bug");
        let index = filed(&mut tracker, 1, vec![7]);
        let other = filed(&mut tracker, 2, Vec::new());
        tracker.remove_label(7).unwrap();
        assert!(tracker.label(7).is_none());
        assert!(tracker.issue(index).unwrap().labels.is_empty());
        assert!(tracker.issue(other).unwrap().labels.is_empty());
        assert!(tracker.remove_label(7).is_none());
    }
}
