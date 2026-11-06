//! Protocol contracts for commit status history and real Git comparisons.

mod common;

use common::Api;
use ninot_forgejo::Server;
use serde_json::json;
use std::path::Path;

#[test]
fn status_history_uses_the_latest_context_and_forgejo_priority() {
    let server = Server::start().unwrap();
    let repository = server
        .create_user("alice")
        .create_repository("app", true, "main", true)
        .unwrap();
    let sha = repository
        .commit_file(
            "feature",
            Path::new("module.py"),
            b"VALUE = 1\n",
            "Add module",
        )
        .unwrap();
    let api = Api::admin(&server);
    let root = "/repos/alice/app";
    for (context, status) in [
        ("check", "failure"),
        ("check", "success"),
        ("optional", "skipped"),
    ] {
        let reply = api.post(
            &format!("{root}/statuses/{sha}"),
            json!({"context":context,"state":status}),
        );
        assert_eq!(reply.status, 201, "{}", reply.body);
    }
    let combined = api.get(&format!("{root}/commits/feature/status"));
    assert_eq!(combined.body["state"], "success");
    assert_eq!(combined.body["total_count"], 2);
    assert_eq!(
        api.get(&format!("{root}/statuses/{sha}"))
            .body
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        api.post(
            &format!("{root}/statuses/{sha}"),
            json!({"state":"warning","context":"optional"})
        )
        .status,
        201
    );
    assert_eq!(
        api.get(&format!("{root}/commits/feature/status")).body["state"],
        "warning"
    );
    let comparison = api.get(&format!("{root}/compare/main...feature"));
    assert_eq!(comparison.status, 200, "{}", comparison.body);
    assert_eq!(comparison.body["total_commits"], 1);
    assert_eq!(comparison.body["commits"][0]["sha"], sha);
    assert_eq!(comparison.body["files"][0]["filename"], "module.py");
    let empty = api.get(&format!("{root}/compare/main...main"));
    assert_eq!(empty.body["commits"], json!([]));
    assert_eq!(empty.body["files"], json!([]));
    assert_eq!(
        Api::new(&server, None)
            .get(&format!("{root}/commits/feature/status"))
            .status,
        401
    );
    let read_token = server
        .user("admin")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    assert_eq!(
        Api::new(&server, Some(&read_token))
            .post(
                &format!("{root}/statuses/{sha}"),
                json!({"state":"success"})
            )
            .status,
        403
    );
    assert_eq!(
        api.post(
            &format!("{root}/statuses/{sha}"),
            json!({"state":"invalid"})
        )
        .status,
        422
    );
    assert_eq!(
        api.get(&format!("{root}/compare/missing...main")).status,
        404
    );
}

#[test]
fn fixture_content_cannot_escape_the_git_worktree() {
    let server = Server::start().unwrap();
    let repository = server
        .create_user("alice")
        .create_repository("app", false, "main", true)
        .unwrap();
    for path in ["../escape", "/tmp/escape", ".git/config"] {
        assert!(
            repository
                .commit_file("feature", Path::new(path), b"unsafe", "Invalid")
                .is_err()
        );
    }
    assert!(
        server
            .user("alice")
            .unwrap()
            .create_repository("app", false, "main", true)
            .is_err()
    );
}
