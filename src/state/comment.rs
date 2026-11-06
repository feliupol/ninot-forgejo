//! Comments on the issues of a repository.

use super::{Tracker, now};
use chrono::{DateTime, Utc};

/// A comment on an issue, which the issues and the repository list by id.
pub struct Comment {
    /// Identifier of the comment, unique across every object of the fake.
    pub id: u64,
    /// Number of the issue the comment hangs on.
    pub index: u64,
    /// Login of the user who wrote the comment.
    pub poster: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Tracker {
    /// The comment with id `id`, whichever issue of the repository it hangs on.
    pub fn comment(&self, id: u64) -> Option<&Comment> {
        self.comments.get(&id)
    }

    /// The comment with id `id`, to change it.
    pub fn comment_mut(&mut self, id: u64) -> Option<&mut Comment> {
        self.comments.get_mut(&id)
    }

    /// Every comment of the repository, oldest first.
    pub fn comments(&self) -> Vec<&Comment> {
        self.comments.values().collect()
    }

    /// The comments of issue `index`, oldest first.
    pub fn comments_of(&self, index: u64) -> Vec<&Comment> {
        self.comments
            .values()
            .filter(|comment| comment.index == index)
            .collect()
    }

    /// How many comments issue `index` has.
    pub fn comment_count(&self, index: u64) -> usize {
        self.comments
            .values()
            .filter(|comment| comment.index == index)
            .count()
    }

    /// Records a comment on issue `index` and returns its id.
    pub fn add_comment(&mut self, id: u64, index: u64, poster: &str, body: &str) -> u64 {
        let time = now();
        self.comments.insert(
            id,
            Comment {
                id,
                index,
                poster: poster.to_owned(),
                body: body.to_owned(),
                created_at: time,
                updated_at: time,
            },
        );
        id
    }

    /// Removes the comment with id `id`, which the issues stop counting as well.
    pub fn remove_comment(&mut self, id: u64) -> Option<Comment> {
        self.comments.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.add_comment(1, 3, "alice", "first"), 1);
        assert_eq!(tracker.add_comment(2, 1, "bob", "second"), 2);
        tracker
    }

    #[test]
    fn comments_are_listed_by_issue_and_by_repository_oldest_first() {
        let tracker = tracker();
        assert_eq!(tracker.comment(1).unwrap().body, "first");
        assert_eq!(tracker.comment(1).unwrap().index, 3);
        assert!(tracker.comment(3).is_none());
        assert_eq!(
            tracker
                .comments_of(3)
                .iter()
                .map(|comment| comment.body.as_str())
                .collect::<Vec<_>>(),
            ["first"]
        );
        assert!(tracker.comments_of(2).is_empty());
        assert_eq!(tracker.comment_count(1), 1);
        assert_eq!(
            tracker
                .comments()
                .iter()
                .map(|comment| comment.body.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn a_removed_comment_is_gone_from_the_issue_as_well() {
        let mut tracker = tracker();
        assert!(tracker.remove_comment(1).is_some());
        assert!(tracker.comment(1).is_none());
        assert!(tracker.comments_of(3).is_empty());
        assert_eq!(tracker.comment_count(3), 0);
        assert!(tracker.remove_comment(1).is_none());
        assert_eq!(tracker.comments().len(), 1);
    }
}
