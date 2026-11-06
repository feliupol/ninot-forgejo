//! The `push` event, sent once per ref an accepted push created, moved or deleted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::api::ApiRequest;
use crate::state::State;
use crate::time;
use crate::types;
use crate::webhooks;
use ninot_common::git;

/// The sha GitHub and Forgejo both report for a ref that does not exist before or after a
/// push.
pub const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Commits a push payload includes at most: "The array includes a maximum of 2048 commits"
/// (<https://forgejo.org/docs/latest/user/webhooks/>).
const MAX_PUSH_COMMITS: usize = 2048;

/// Queues one `push` event per ref a push changed. `git_dir` is read for the commits each
/// payload carries.
pub fn enqueue_push(
    state: &mut State,
    request: &ApiRequest,
    owner: &str,
    name: &str,
    git_dir: Option<&Path>,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) {
    for (reference, old, new) in changed_refs(before, after) {
        let commits = match git_dir {
            // A deleted ref has no commits to report.
            Some(git_dir) if new != ZERO_SHA => commits(git_dir, request, owner, name, before, new),
            _ => Vec::new(),
        };
        let payload = build_push_payload(request, state, owner, name, reference, old, new, commits);
        webhooks::enqueue(state, owner, name, "push", &payload, None);
    }
}

/// Queues the `push` event a hook test sends: the default branch's current head, delivered only
/// to `hook`.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_test(
    state: &mut State,
    request: &ApiRequest,
    owner: &str,
    name: &str,
    git_dir: &Path,
    branch: &str,
    head: &str,
    hook: Option<u64>,
) {
    // Nothing was pushed, so no ref was created: the head sits at the zero sha.
    let before = BTreeMap::new();
    let after = BTreeMap::from([(format!("refs/heads/{branch}"), head.to_owned())]);
    for (reference, old, new) in changed_refs(&before, &after) {
        let commits = commits(git_dir, request, owner, name, &before, new);
        let payload = build_push_payload(request, state, owner, name, reference, old, new, commits);
        webhooks::enqueue(state, owner, name, "push", &payload, hook);
    }
}

/// The refs a push changed, as `(ref, before, after)`, skipping those both sides agree on.
fn changed_refs<'a>(
    before: &'a BTreeMap<String, String>,
    after: &'a BTreeMap<String, String>,
) -> Vec<(&'a str, &'a str, &'a str)> {
    let references: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    references
        .into_iter()
        .map(|reference| {
            let old = before.get(reference).map_or(ZERO_SHA, String::as_str);
            let new = after.get(reference).map_or(ZERO_SHA, String::as_str);
            (reference.as_str(), old, new)
        })
        .filter(|(_, old, new)| old != new)
        .collect()
}

/// The commits `new` reached that no ref held before the push, oldest first, as payload
/// objects. Excluding every earlier ref's sha keeps a push of the same commits to a second ref
/// from reporting them twice.
fn commits(
    git_dir: &Path,
    request: &ApiRequest,
    owner: &str,
    name: &str,
    before: &BTreeMap<String, String>,
    new: &str,
) -> Vec<types::PayloadCommit> {
    let excluded: Vec<&str> = before.values().map(String::as_str).collect();
    let Ok(shas) = git::new_commits(git_dir, new, &excluded) else {
        return Vec::new();
    };
    let html = format!("{}/{owner}/{name}", request.web_url);
    let first = shas.len().saturating_sub(MAX_PUSH_COMMITS);
    shas[first..]
        .iter()
        .filter_map(|sha| git::commit(git_dir, sha).ok())
        .map(|commit| {
            let (added, removed, modified) =
                git::changed_paths(git_dir, &commit.sha).unwrap_or_default();
            types::PayloadCommit {
                added,
                author: person(&commit.author),
                committer: person(&commit.committer),
                distinct: true,
                id: commit.sha.clone(),
                message: commit.message.clone(),
                modified,
                removed,
                timestamp: time::timestamp(&commit.committer.date),
                tree_id: commit.tree.clone(),
                url: format!("{html}/commit/{}", commit.sha),
                verification: None,
            }
        })
        .collect()
}

/// The author or committer of a commit, as a push payload renders them: git knows their name and
/// address, and no account, which the spec's `PayloadUser` declares nullable.
fn person(signature: &git::Signature) -> types::PayloadUser {
    types::PayloadUser {
        name: signature.name.clone(),
        email: signature.email.clone(),
        username: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_push_payload(
    request: &ApiRequest,
    state: &State,
    owner: &str,
    name: &str,
    reference: &str,
    before: &str,
    after: &str,
    commits: Vec<types::PayloadCommit>,
) -> types::PushPayload {
    let doer = super::doer(request, state);
    let total = commits.len();
    types::PushPayload {
        ref_: reference.to_owned(),
        before: before.to_owned(),
        after: after.to_owned(),
        compare_url: format!(
            "{}/{owner}/{name}/compare/{before}...{after}",
            request.web_url
        ),
        head_commit: commits.last().cloned(),
        commits,
        total_commits: total as i64,
        repository: super::repository(request, state, owner, name),
        pusher: doer.clone(),
        sender: doer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(names: &[&str], sha: &str) -> BTreeMap<String, String> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), sha.to_owned()))
            .collect()
    }

    #[test]
    fn unchanged_refs_are_ignored() {
        let before = refs(&["refs/heads/main"], "1");
        assert!(changed_refs(&before, &before).is_empty());
    }

    #[test]
    fn a_created_ref_reports_the_zero_sha() {
        let before: BTreeMap<String, String> = BTreeMap::new();
        let after = refs(&["refs/heads/main"], "1");
        let changed = changed_refs(&before, &after);
        assert_eq!(changed, [("refs/heads/main", ZERO_SHA, "1")]);
    }

    #[test]
    fn a_deleted_ref_reports_the_zero_sha() {
        let before = refs(&["refs/heads/main", "refs/heads/gone"], "1");
        let after = refs(&["refs/heads/main"], "1");
        assert_eq!(
            changed_refs(&before, &after),
            [("refs/heads/gone", "1", ZERO_SHA)]
        );
    }

    #[test]
    fn every_changed_ref_is_reported_once() {
        let before = refs(&["refs/heads/main"], "1");
        let after = refs(&["refs/heads/main", "refs/tags/v1"], "2");
        assert_eq!(
            changed_refs(&before, &after),
            [
                ("refs/heads/main", "1", "2"),
                ("refs/tags/v1", ZERO_SHA, "2"),
            ]
        );
    }
}
