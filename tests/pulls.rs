#![allow(dead_code, unused_imports)]
//! Pull requests, through the REST API.

mod common;

use std::path::{Path, PathBuf};

use common::{Api, Reply, TempDir, assert_date_times, clone_url, commit_file, git, git_ok};
use ninot_forgejo::Server;
use serde_json::{Value, json};

/// A fake with `admin` owning a public repository.
fn fake(name: &str) -> Server {
    let server = Server::start().unwrap();
    let created = Api::admin(&server).post("/user/repos", json!({ "name": name }));
    assert_eq!(created.status, 201, "{}", created.body);
    server
}

/// Creates a repository and pushes two branches.
fn setup_repo(server: &Server) -> (Api<'_>, String, String) {
    let admin = Api::admin(server);
    admin.post("/user/repos", json!({ "name": "tool" }));
    let work = TempDir::new();
    git_ok(
        &work.0,
        &[
            "clone",
            "-q",
            &clone_url(server, "admin", "tool", Some(server.admin_token())),
            "c",
        ],
    );
    let clone = work.join("c");
    commit_file(&clone, "a.txt", "a");
    git_ok(&clone, &["push", "-q", "origin", "main"]);
    git_ok(&clone, &["checkout", "-q", "-b", "feature"]);
    commit_file(&clone, "b.txt", "b");
    git_ok(&clone, &["push", "-q", "origin", "feature"]);
    (admin, "main".to_owned(), "feature".to_owned())
}

#[test]
fn creating_a_pull_request_returns_the_next_index_and_attaches_to_an_issue() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    let created = admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.body["number"], 1);
    assert_eq!(created.body["state"], "open");
    assert_eq!(created.body["base"]["ref"], "main");
    assert_eq!(created.body["head"]["ref"], "feature");
    assert!(
        created.body["mergeable"].as_bool().unwrap_or(false) || created.body["mergeable"].is_null()
    );

    // It also appears as an issue with pull_request field
    let issue = admin.get("/repos/admin/tool/issues/1");
    assert_eq!(issue.status, 200, "{}", issue.body);
    assert!(issue.body["pull_request"].is_object());
}

#[test]
fn listing_and_getting_a_pull_request_works() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    let list = admin.get("/repos/admin/tool/pulls");
    assert_eq!(list.status, 200, "{}", list.body);
    let arr = list.body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["number"], 1);

    let one = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(one.status, 200, "{}", one.body);
    assert_eq!(one.body["title"], "Add b");
}

#[test]
fn patching_a_pull_request_updates_title_body_state_and_base() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    let patched = admin.patch(
        "/repos/admin/tool/pulls/1",
        json!({ "title": "Add B", "body": "Updated" }),
    );
    assert_eq!(patched.status, 201, "{}", patched.body);
    assert_eq!(patched.body["title"], "Add B");
    assert_eq!(patched.body["body"], "Updated");
}

#[test]
fn commits_and_files_endpoints_return_data_for_pr() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    let commits = admin.get("/repos/admin/tool/pulls/1/commits");
    assert_eq!(commits.status, 200, "{}", commits.body);
    assert!(!commits.body.as_array().unwrap().is_empty());

    let files = admin.get("/repos/admin/tool/pulls/1/files");
    assert_eq!(files.status, 200, "{}", files.body);
}

#[test]
fn diff_and_patch_endpoints_return_text() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    let diff = admin.get("/repos/admin/tool/pulls/1.diff");
    assert_eq!(diff.status, 200, "{}", diff.body);
    let patch = admin.get("/repos/admin/tool/pulls/1.patch");
    assert_eq!(patch.status, 200, "{}", patch.body);
}

#[test]
fn merge_and_is_merged_workflow() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": base, "head": head }),
    );
    let merged = admin.get("/repos/admin/tool/pulls/1/merge");
    assert_eq!(merged.status, 404, "{}", merged.body);

    let merge = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({ "Do": "merge", "MergeMessageField": "merge" }),
    );
    assert_eq!(merge.status, 200, "{}", merge.body);

    let merged2 = admin.get("/repos/admin/tool/pulls/1/merge");
    assert_eq!(merged2.status, 204, "{}", merged2.body);

    let pr = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(pr.status, 200, "{}", pr.body);
    assert!(pr.body["merged"].as_bool().unwrap_or(false));
}

/// Creates `admin/{name}`, private when asked, and a local repository `a` whose `origin` is
/// it, with `a.txt` committed on `main` and pushed. The scratch directory keeps `a` alive.
fn repository(server: &Server, name: &str, private: bool) -> (TempDir, PathBuf) {
    let admin = Api::admin(server);
    let created = admin.post("/user/repos", json!({ "name": name, "private": private }));
    assert_eq!(created.status, 201, "{}", created.body);
    let work = TempDir::new();
    git_ok(&work.0, &["init", "-q", "-b", "main", "a"]);
    let clone = work.join("a");
    git_ok(
        &clone,
        &[
            "remote",
            "add",
            "origin",
            &clone_url(server, "admin", name, Some(server.admin_token())),
        ],
    );
    commit_file(&clone, "a.txt", "one");
    git_ok(&clone, &["push", "-q", "origin", "main"]);
    (work, clone)
}

/// The public `admin/tool` the pull request tests share.
fn tool(server: &Server) -> (TempDir, PathBuf) {
    repository(server, "tool", false)
}

/// A fresh clone of `admin/name`, to read the history the fake wrote.
fn clone_of(server: &Server, work: &TempDir, name: &str) -> PathBuf {
    git_ok(
        &work.0,
        &[
            "clone",
            "-q",
            &clone_url(server, "admin", name, Some(server.admin_token())),
            "c",
        ],
    );
    work.join("c")
}

/// Commits `file` with `content` on `branch` and pushes it, creating the branch at the
/// commit the clone has checked out when it has none.
fn push_commit(clone: &Path, branch: &str, file: &str, content: &str) {
    if git(clone, &["rev-parse", "--verify", "--quiet", branch])
        .status
        .success()
    {
        git_ok(clone, &["checkout", "-q", branch]);
    } else {
        git_ok(clone, &["checkout", "-q", "-b", branch]);
    }
    commit_file(clone, file, content);
    git_ok(clone, &["push", "-q", "origin", branch]);
}

/// Opens a pull request of `admin/name` from `head` into `base`, and answers its number.
fn open_pull(admin: &Api<'_>, name: &str, base: &str, head: &str) -> u64 {
    let created = admin.post(
        &format!("/repos/admin/{name}/pulls"),
        json!({ "title": "Work", "body": "Work", "base": base, "head": head }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    created.body["number"].as_u64().unwrap()
}

/// The branches `admin/name` has, as `(name, commit id)` pairs.
fn listed_branches(admin: &Api<'_>, name: &str) -> Vec<(String, String)> {
    let listed = admin.get(&format!("/repos/admin/{name}/branches"));
    assert_eq!(listed.status, 200, "{}", listed.body);
    listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|branch| {
            (
                branch["name"].as_str().unwrap().to_owned(),
                branch["commit"]["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The names of the branches `admin/name` has.
fn branches(admin: &Api<'_>, name: &str) -> Vec<String> {
    listed_branches(admin, name)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The commit `branch` of `admin/name` points at.
fn branch_sha(admin: &Api<'_>, name: &str, branch: &str) -> String {
    listed_branches(admin, name)
        .into_iter()
        .find(|(listed, _)| listed == branch)
        .unwrap_or_else(|| panic!("{branch} is not a branch of admin/{name}"))
        .1
}

/// The parents of the commit `branch` of `clone` points at, as `git rev-list --parents` lists
/// them, without the commit itself.
fn parents(clone: &Path, branch: &str) -> Vec<String> {
    let line = git_ok(clone, &["rev-list", "--parents", "-n", "1", branch]);
    line.split_whitespace().skip(1).map(str::to_owned).collect()
}

/// The commit `branch` of `clone` points at.
fn head(clone: &Path, branch: &str) -> String {
    git_ok(clone, &["rev-parse", branch])
}

#[test]
fn a_conflicting_pull_request_is_not_mergeable_and_merging_it_changes_nothing() {
    let server = Server::start().unwrap();
    let (_work, clone) = tool(&server);
    // Both branches change the same line of a.txt.
    push_commit(&clone, "feature", "a.txt", "two");
    push_commit(&clone, "main", "a.txt", "three");

    let admin = Api::admin(&server);
    let number = open_pull(&admin, "tool", "main", "feature");
    let pulls = format!("/repos/admin/tool/pulls/{number}");
    assert_eq!(admin.get(&pulls).body["mergeable"], false);

    let main = branch_sha(&admin, "tool", "main");
    let merge = admin.post(&format!("{pulls}/merge"), json!({ "Do": "merge" }));
    assert_eq!(merge.status, 405, "{}", merge.body);
    assert_eq!(
        branch_sha(&admin, "tool", "main"),
        main,
        "a refused merge moves the base branch"
    );

    // Nothing about the pull request changed either.
    let pull = admin.get(&pulls);
    assert_eq!(pull.status, 200, "{}", pull.body);
    assert_eq!(pull.body["merged"], false);
    assert_eq!(pull.body["mergeable"], false);
    assert_eq!(pull.body["merged_at"], Value::Null);
    assert_eq!(pull.body["merge_commit_sha"], Value::Null);
    assert_eq!(admin.get(&format!("{pulls}/merge")).status, 404);
    assert_eq!(
        admin.get("/repos/admin/tool/issues/1").body["state"],
        "open"
    );
}

#[test]
fn mergeability_is_recomputed_when_the_base_branch_moves() {
    let server = Server::start().unwrap();
    let (_work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "from the head");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    assert_eq!(
        admin.get("/repos/admin/tool/pulls/1").body["mergeable"],
        true
    );

    // A commit on the base branch changing the same line of b.txt conflicts with the head,
    // which the pull request can only tell after looking again.
    push_commit(&clone, "main", "b.txt", "from the base");
    assert_eq!(
        admin.get("/repos/admin/tool/pulls/1").body["mergeable"],
        false
    );
}

#[test]
fn merging_makes_a_commit_with_two_parents() {
    let server = Server::start().unwrap();
    let (work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    let (main, feature) = (
        branch_sha(&admin, "tool", "main"),
        branch_sha(&admin, "tool", "feature"),
    );

    let merge = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({
            "Do": "merge",
            "MergeTitleField": "Merge pull request #1",
            "MergeMessageField": "Adds b.",
        }),
    );
    assert_eq!(merge.status, 200, "{}", merge.body);

    let merged = clone_of(&server, &work, "tool");
    let base = head(&merged, "main");
    assert_eq!(
        parents(&merged, "main"),
        [main.clone(), feature.clone()],
        "a merged pull request has one parent per branch"
    );
    assert_eq!(
        git_ok(&merged, &["log", "-1", "--format=%s", "main"]),
        "Merge pull request #1"
    );
    assert!(
        git_ok(&merged, &["log", "-1", "--format=%b", "main"]).contains("Adds b."),
        "the merge message is the body of the merge commit"
    );

    // The API reports the commit it made, and where the branches stand.
    let pull = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(pull.body["merge_commit_sha"], base);
    assert_eq!(pull.body["base"]["sha"], base);
    assert_eq!(pull.body["head"]["sha"], feature);
    assert_eq!(branch_sha(&admin, "tool", "main"), base);
}

#[test]
fn squashing_makes_one_commit_with_the_changes_of_the_head() {
    let server = Server::start().unwrap();
    let (work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    let main = branch_sha(&admin, "tool", "main");

    let merge = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({ "Do": "squash", "MergeTitleField": "Add b" }),
    );
    assert_eq!(merge.status, 200, "{}", merge.body);

    let merged = clone_of(&server, &work, "tool");
    assert_eq!(
        parents(&merged, "main"),
        [main],
        "a squashed pull request has one parent"
    );
    assert_eq!(
        git_ok(&merged, &["log", "-1", "--format=%s", "main"]),
        "Add b"
    );
    assert_eq!(
        git_ok(&merged, &["diff", "--name-only", "main^", "main"]),
        "b.txt",
        "the squashed commit carries what the head changed"
    );
    assert_eq!(std::fs::read_to_string(merged.join("b.txt")).unwrap(), "b");
    // The head keeps the commit it had.
    assert_ne!(
        branch_sha(&admin, "tool", "main"),
        branch_sha(&admin, "tool", "feature")
    );
}

#[test]
fn rebasing_replays_the_commits_of_the_head_onto_the_base() {
    let server = Server::start().unwrap();
    let (work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    // The base moves on, so the commit of the head is replayed instead of kept.
    push_commit(&clone, "main", "c.txt", "c");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    let (main, feature) = (
        branch_sha(&admin, "tool", "main"),
        branch_sha(&admin, "tool", "feature"),
    );

    let merge = admin.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "rebase" }));
    assert_eq!(merge.status, 200, "{}", merge.body);

    let merged = clone_of(&server, &work, "tool");
    let base = head(&merged, "main");
    assert_ne!(base, feature, "the commit of the head is replayed");
    assert_eq!(
        parents(&merged, "main"),
        [main],
        "a rebased pull request is not merged"
    );
    assert_eq!(
        git_ok(&merged, &["log", "-1", "--format=%s", "main"]),
        "Add b.txt",
        "the replayed commit keeps the message of the head"
    );
    assert_eq!(
        git_ok(&merged, &["rev-list", "--count", "main"]),
        "3",
        "the head contributes one commit on top of the base"
    );
    // The head branch is left alone.
    assert_eq!(branch_sha(&admin, "tool", "feature"), feature);
}

#[test]
fn fast_forward_only_merges_only_when_the_base_is_an_ancestor_of_the_head() {
    let server = Server::start().unwrap();
    let (work, clone) = tool(&server);
    let root = head(&clone, "main");
    push_commit(&clone, "feature", "b.txt", "b");
    push_commit(&clone, "main", "c.txt", "c");
    git_ok(&clone, &["branch", "behind", &root]);
    git_ok(&clone, &["push", "-q", "origin", "behind"]);
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");

    // The base moved past the point the two branches share, so it is no longer an ancestor
    // of the head and only a real merge commit can bring the head in.
    let main = branch_sha(&admin, "tool", "main");
    let refused = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({ "Do": "fast-forward-only" }),
    );
    assert_eq!(refused.status, 405, "{}", refused.body);
    assert_eq!(branch_sha(&admin, "tool", "main"), main);

    // Against a base the head is ahead of, the same style fast-forwards.
    let number = open_pull(&admin, "tool", "behind", "feature");
    let merged = admin.post(
        &format!("/repos/admin/tool/pulls/{number}/merge"),
        json!({ "Do": "fast-forward-only" }),
    );
    assert_eq!(merged.status, 200, "{}", merged.body);
    let feature = branch_sha(&admin, "tool", "feature");
    assert_eq!(branch_sha(&admin, "tool", "behind"), feature);
    // A clone has only the default branch checked out; the others arrive as remote branches.
    let history = clone_of(&server, &work, "tool");
    assert_eq!(parents(&history, "origin/behind"), [root]);
    assert_eq!(head(&history, "origin/behind"), feature);
}

#[test]
fn merging_with_delete_branch_after_merge_removes_the_head_branch() {
    let server = Server::start().unwrap();
    let (_work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    assert!(branches(&admin, "tool").contains(&"feature".to_owned()));

    let merge = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({ "Do": "merge", "delete_branch_after_merge": true }),
    );
    assert_eq!(merge.status, 200, "{}", merge.body);
    assert_eq!(branches(&admin, "tool"), ["main"]);
    assert_eq!(admin.get("/repos/admin/tool/pulls/1").body["merged"], true);
}

#[test]
fn a_token_that_only_reads_repositories_cannot_change_a_pull_request() {
    let server = Server::start().unwrap();
    let (_work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");

    server.create_user("bob");
    let token = server
        .user("bob")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    let bob = Api::new(&server, Some(&token));
    let created = bob.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "t", "base": "main", "head": "feature" }),
    );
    assert_eq!(created.status, 403, "{}", created.body);
    let edited = bob.patch("/repos/admin/tool/pulls/1", json!({ "title": "t" }));
    assert_eq!(edited.status, 403, "{}", edited.body);
    let merge = bob.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "merge" }));
    assert_eq!(merge.status, 403, "{}", merge.body);

    // Reading it is all bob may do, and none of the three changed anything.
    assert_eq!(bob.get("/repos/admin/tool/pulls/1").status, 200);
    let pull = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(pull.body["title"], "Work");
    assert_eq!(pull.body["merged"], false);
}

#[test]
fn a_user_without_access_to_a_private_repository_sees_nothing_of_it() {
    let server = Server::start().unwrap();
    let (_work, clone) = repository(&server, "secret", true);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    let number = open_pull(&admin, "secret", "main", "feature");
    let pulls = format!("/repos/admin/secret/pulls/{number}");

    // Mallory's token may do everything a token can, so what answers is what she may not
    // see rather than what she may not do.
    server.create_user("mallory");
    let token = server
        .user("mallory")
        .unwrap()
        .create_token(&[
            "read:repository",
            "write:repository",
            "read:issue",
            "write:issue",
        ])
        .unwrap();
    let mallory = Api::new(&server, Some(&token));
    for status in [
        mallory.get("/repos/admin/secret").status,
        mallory.get("/repos/admin/secret/pulls").status,
        mallory.get("/repos/admin/secret/issues/1").status,
        mallory.get(&pulls).status,
        mallory.get(&format!("{pulls}/merge")).status,
        mallory
            .post(
                "/repos/admin/secret/pulls",
                json!({ "title": "t", "base": "main", "head": "feature" }),
            )
            .status,
        mallory.patch(&pulls, json!({ "title": "t" })).status,
        mallory
            .post(&format!("{pulls}/merge"), json!({ "Do": "merge" }))
            .status,
    ] {
        assert_eq!(status, 404, "a repository mallory cannot see is a 404");
    }
    // None of it touched the pull request.
    let pull = admin.get(&pulls);
    assert_eq!(pull.body["title"], "Work");
    assert_eq!(pull.body["merged"], false);
}

#[test]
fn a_writer_of_another_repository_cannot_merge_someone_elses_pull_request() {
    let server = Server::start().unwrap();
    let (_work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    server.create_user("bob");
    let token = server
        .user("bob")
        .unwrap()
        .create_token(&["read:repository", "write:repository"])
        .unwrap();
    let bob = Api::new(&server, Some(&token));
    // Bob may write to a repository of his own, which is not what makes him able to write to
    // someone else's.
    assert_eq!(
        bob.post("/user/repos", json!({ "name": "tool" })).status,
        201
    );

    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");
    let merge = bob.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "merge" }));
    assert_eq!(merge.status, 403, "{}", merge.body);
    assert_eq!(
        bob.patch("/repos/admin/tool/pulls/1", json!({ "title": "t" }))
            .status,
        403
    );
    assert_eq!(
        bob.post(
            "/repos/admin/tool/pulls",
            json!({ "title": "t", "base": "main", "head": "feature" })
        )
        .status,
        403
    );
    let pull = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(pull.body["merged"], false);
    assert_eq!(pull.body["title"], "Work");
}

#[test]
fn a_merged_pull_request_names_its_merger_and_closes_its_issue() {
    let server = Server::start().unwrap();
    let (work, clone) = tool(&server);
    push_commit(&clone, "feature", "b.txt", "b");
    let admin = Api::admin(&server);
    open_pull(&admin, "tool", "main", "feature");

    let merge = admin.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "merge" }));
    assert_eq!(merge.status, 200, "{}", merge.body);

    let pull = admin.get("/repos/admin/tool/pulls/1");
    assert_eq!(pull.status, 200, "{}", pull.body);
    assert_eq!(pull.body["merged"], true);
    assert_eq!(pull.body["state"], "closed");
    assert_eq!(pull.body["merged_by"]["login"], "admin");
    assert_eq!(pull.body["merged_at"], pull.body["closed_at"]);
    assert_date_times("PullRequest", &pull.body);
    assert_eq!(admin.get("/repos/admin/tool/pulls/1/merge").status, 204);

    // The issue the pull request was filed as is closed with it.
    let issue = admin.get("/repos/admin/tool/issues/1");
    assert_eq!(issue.status, 200, "{}", issue.body);
    assert_eq!(issue.body["state"], "closed");
    assert_eq!(issue.body["closed_at"], pull.body["closed_at"]);
    assert_eq!(issue.body["pull_request"]["merged"], true);
    assert_eq!(
        issue.body["pull_request"]["merged_at"],
        pull.body["merged_at"]
    );

    // Nobody asked for the head branch to go, so it stays.
    assert!(branches(&admin, "tool").contains(&"feature".to_owned()));
    let merged = clone_of(&server, &work, "tool");
    assert_eq!(
        git_ok(&merged, &["log", "-1", "--format=%an", "main"]),
        "admin",
        "the merge commit is the merger's"
    );
}

#[test]
fn validation_and_duplicates() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    let same = admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "t", "base": base, "head": base }),
    );
    assert_eq!(same.status, 422, "{}", same.body);

    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "t1", "base": base, "head": head }),
    );
    let dup = admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "t2", "base": base, "head": head }),
    );
    assert_eq!(dup.status, 409, "{}", dup.body);
}

/// The subject of the tip commit `branch` points at on `admin/tool`.
fn tip_subject(server: &Server, branch: &str) -> String {
    let work = TempDir::new();
    let clone = clone_of(server, &work, "tool");
    git_ok(&clone, &["log", "-1", "--format=%s", branch])
}

#[test]
fn merge_without_a_title_uses_forgejos_default_merge_message() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": &base, "head": &head }),
    );
    let merge = admin.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "merge" }));
    assert_eq!(merge.status, 200, "{}", merge.body);
    assert_eq!(
        tip_subject(&server, &base),
        format!("Merge pull request 'Add b' (#1) from {head} into {base}")
    );
}

#[test]
fn an_explicit_merge_title_overrides_the_default() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": &base, "head": &head }),
    );
    let merge = admin.post(
        "/repos/admin/tool/pulls/1/merge",
        json!({ "Do": "merge", "MergeTitleField": "custom" }),
    );
    assert_eq!(merge.status, 200, "{}", merge.body);
    assert_eq!(tip_subject(&server, &base), "custom");
}

#[test]
fn squash_without_a_title_uses_the_pull_request_title_and_index() {
    let server = Server::start().unwrap();
    let (admin, base, head) = setup_repo(&server);
    admin.post(
        "/repos/admin/tool/pulls",
        json!({ "title": "Add b", "body": "Adds b", "base": &base, "head": &head }),
    );
    let merge = admin.post("/repos/admin/tool/pulls/1/merge", json!({ "Do": "squash" }));
    assert_eq!(merge.status, 200, "{}", merge.body);
    assert_eq!(tip_subject(&server, &base), "Add b (#1)");
}
