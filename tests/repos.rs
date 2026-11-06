//! Users, tokens and repositories through the REST API.

mod common;

use common::{Api, UNSET_TIME, assert_date_times};
use ninot_forgejo::Server;
use serde_json::json;

#[test]
fn version_and_authenticated_user() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    let version = admin.get("/version");
    assert_eq!(version.status, 200);
    assert!(
        version.body["version"]
            .as_str()
            .unwrap()
            .starts_with("16.0.3")
    );
    let me = admin.get("/user");
    assert_eq!(me.status, 200);
    assert_eq!(me.body["login"], "admin");
    assert_eq!(me.body["is_admin"], true);
}

#[test]
fn requests_without_a_valid_token_are_unauthorized() {
    let server = Server::start().unwrap();
    assert_eq!(Api::new(&server, None).get("/user").status, 401);
    assert_eq!(Api::new(&server, Some("nope")).get("/user").status, 401);
}

#[test]
fn added_users_authenticate_with_their_tokens() {
    let server = Server::start().unwrap();
    server.create_user("alice");
    let token = server
        .user("alice")
        .unwrap()
        .create_token(&["write:repository", "write:user"])
        .unwrap();
    let me = Api::new(&server, Some(&token)).get("/user");
    assert_eq!(me.body["login"], "alice");
    assert_eq!(me.body["is_admin"], false);
}

#[test]
fn create_get_list_and_delete_a_repository() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    let created = admin.post(
        "/user/repos",
        json!({ "name": "tool", "private": true, "description": "A tool" }),
    );
    assert_eq!(created.status, 201);
    assert_eq!(created.body["full_name"], "admin/tool");
    assert_eq!(created.body["owner"]["login"], "admin");
    assert_eq!(created.body["private"], true);
    assert_eq!(created.body["empty"], true);
    assert_eq!(created.body["default_branch"], "main");
    assert_eq!(
        created.body["clone_url"],
        format!("{}/admin/tool.git", server.url())
    );

    assert_eq!(
        admin.post("/user/repos", json!({ "name": "tool" })).status,
        409
    );
    assert_eq!(admin.get("/repos/admin/tool").body["description"], "A tool");
    let mine = admin.get("/user/repos");
    assert_eq!(mine.body.as_array().unwrap().len(), 1);

    assert_eq!(admin.delete("/repos/admin/tool").status, 204);
    assert_eq!(admin.get("/repos/admin/tool").status, 404);
}

#[test]
fn private_repositories_are_hidden_from_other_users() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "secret", "private": true }));
    server.create_user("alice");
    let token = server
        .user("alice")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    assert_eq!(
        Api::new(&server, Some(&token))
            .get("/repos/admin/secret")
            .status,
        404
    );
}

#[test]
fn auto_init_creates_a_readme_commit_on_the_default_branch() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    let created = admin.post("/user/repos", json!({ "name": "tool", "auto_init": true }));
    assert_eq!(created.body["empty"], false);
    let branches = admin.get("/repos/admin/tool/branches");
    assert_eq!(branches.status, 200);
    assert_eq!(branches.body[0]["name"], "main");
    let readme = admin.get("/repos/admin/tool/contents/README.md");
    assert_eq!(readme.status, 200);
    assert_eq!(readme.body["type"], "file");
    assert_eq!(readme.body["encoding"], "base64");
}

#[test]
fn an_empty_default_branch_is_no_branch_at_all() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    // The spec declares `default_branch` as a plain string and an empty one means "unset" in
    // Forgejo, which then uses its default. `tea` sends one with every repository it creates.
    let created = admin.post(
        "/user/repos",
        json!({
            "name": "tool", "description": "", "private": false, "auto_init": false,
            "template": false, "default_branch": "", "trust_model": "",
        }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.body["default_branch"], "main");
    assert_eq!(created.body["full_name"], "admin/tool");
    assert_eq!(
        admin.get("/repos/admin/tool").body["default_branch"],
        "main"
    );
    assert_eq!(
        admin.get("/repos/admin/tool/branches").body,
        json!([]),
        "an empty default branch is not created as a branch"
    );

    // A branch that is named is still taken as it is named, and one that is left out is main.
    let other = admin.post(
        "/user/repos",
        json!({ "name": "gear", "default_branch": "trunk" }),
    );
    assert_eq!(other.status, 201, "{}", other.body);
    assert_eq!(other.body["default_branch"], "trunk");
    let plain = admin.post("/user/repos", json!({ "name": "gadget" }));
    assert_eq!(plain.status, 201, "{}", plain.body);
    assert_eq!(plain.body["default_branch"], "main");
}

#[test]
fn the_date_times_of_a_repository_and_of_a_user_are_the_ones_forgejo_sends() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "tool" }));
    let repository = admin.get("/repos/admin/tool");
    assert_date_times("Repository", &repository.body);
    assert_date_times("User", &repository.body["owner"]);
    assert_date_times("User", &admin.get("/user").body);
    // A repository that is no mirror has never been mirrored, and Forgejo says so with the zero
    // time of a date-time. The empty string the fake sent is what made `tea` 0.9.2 answer
    // `cannot parse "\"" as "2006"` and refuse to list the repository at all.
    assert_eq!(repository.body["mirror_updated"], UNSET_TIME);
    // `mirror_interval` is a plain string in the spec, and empty is what it is without a mirror.
    assert_eq!(repository.body["mirror_interval"], "");
}

#[test]
fn unimplemented_operations_answer_501_with_their_operation_id() {
    let server = Server::start().unwrap();
    let reply = Api::admin(&server).get("/repos/admin/tool/milestones");
    assert_eq!(reply.status, 501);
    assert!(reply.body.to_string().contains("issueGetMilestonesList"));
}

#[test]
fn unknown_paths_are_not_found() {
    let server = Server::start().unwrap();
    assert_eq!(Api::admin(&server).get("/no/such/thing").status, 404);
}

#[test]
fn a_new_repository_has_no_topics() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "tool" }));
    let topics = admin.get("/repos/admin/tool/topics");
    assert_eq!(topics.status, 200);
    assert_eq!(topics.body, json!({ "topics": [] }));
    assert_eq!(admin.get("/repos/admin/none/topics").status, 404);
}

#[test]
fn a_zero_limit_means_the_default_page_size() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "one" }));
    admin.post("/user/repos", json!({ "name": "two" }));
    let listed = admin.get("/user/repos?limit=0");
    assert_eq!(listed.body.as_array().unwrap().len(), 2);
}
