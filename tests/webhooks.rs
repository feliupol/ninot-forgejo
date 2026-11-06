//! Webhook management and the deliveries Forgejo sends.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Api, Receiver, TempDir, assert_date_times, clone_url, commit_file, git_ok};
use hmac::{Hmac, KeyInit, Mac};
use ninot_forgejo::Server;
use serde_json::{Value, json};
use sha2::Sha256;

const SECRET: &str = "s3cret";
/// The sha a push reports for a ref that did not exist before it.
const ZERO_SHA: &str = "0000000000000000000000000000000000000000";
const REPO: &str = "tool";
const HOOKS: &str = "/repos/admin/tool/hooks";

/// The lowercase hex HMAC-SHA256 of `body` under `secret`, as Forgejo signs a delivery.
fn digest(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Asserts the signature headers Forgejo sends: the bare hex digest, plus the `sha256=` form
/// GitHub-compatible receivers expect.
fn verify_signed(headers: &HashMap<String, String>, body: &[u8]) {
    let hex = digest(SECRET, body);
    assert_eq!(headers["x-forgejo-signature"], hex, "no bare hex signature");
    assert_eq!(headers["x-gitea-signature"], hex, "no bare hex signature");
    assert_eq!(headers["x-hub-signature-256"], format!("sha256={hex}"));
}

/// Asserts the event headers, which name the event under every prefix Forgejo and GitHub
/// deliver it under.
fn verify_event(headers: &HashMap<String, String>, event: &str) {
    for name in [
        "x-forgejo-event",
        "x-gitea-event",
        "x-forgejo-event-type",
        "x-gitea-event-type",
        "x-github-event",
    ] {
        assert_eq!(headers[name], event, "{name} does not name the event");
    }
}

/// Whether `value` has the shape of a UUID, as the delivery headers carry.
fn is_uuid(value: &str) -> bool {
    let mut groups = value.split('-');
    [8, 4, 4, 4, 12].iter().all(|length| {
        groups.next().is_some_and(|group| {
            group.len() == *length && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
    }) && groups.next().is_none()
}

/// Asserts the delivery headers name one delivery by one UUID.
fn verify_delivery(headers: &HashMap<String, String>) {
    let uuid = &headers["x-forgejo-delivery"];
    assert!(is_uuid(uuid), "{uuid} is not a UUID");
    assert_eq!(headers["x-gitea-delivery"], *uuid);
    assert_eq!(headers["x-github-delivery"], *uuid);
}

/// A fake with an empty `admin/tool`.
fn server_with_repo() -> Server {
    let server = Server::start().unwrap();
    Api::admin(&server).post("/user/repos", json!({ "name": REPO }));
    server
}

/// Creates the hook every delivery test listens with, and returns it as the API rendered it.
fn create_hook(admin: &Api, receiver: &Receiver) -> Value {
    let created = admin.post(
        HOOKS,
        json!({ "config": { "url": receiver.url, "content_type": "json", "secret": SECRET } }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    created.body
}

/// The hook `id` of the rendered `hook`.
fn id_of(hook: &Value) -> String {
    hook["id"].as_i64().expect("a hook has an id").to_string()
}

/// A work tree holding two commits on `main`, and their shas oldest first.
fn work_with_two_commits() -> (TempDir, PathBuf, Vec<String>) {
    let work = TempDir::new();
    git_ok(&work.0, &["init", "-q", "-b", "main", "w"]);
    let dir = work.join("w");
    let mut shas = Vec::new();
    for (file, content) in [("a.txt", "a"), ("b.txt", "b")] {
        commit_file(&dir, file, content);
        shas.push(git_ok(&dir, &["rev-parse", "HEAD"]));
    }
    (work, dir, shas)
}

/// Pushes `main` of the work tree `dir` into `admin/tool` and returns the sha it pointed at.
fn push_main(server: &Server, dir: &Path) -> String {
    let url = clone_url(server, "admin", REPO, Some(server.admin_token()));
    git_ok(dir, &["push", "-q", &url, "main"]);
    git_ok(dir, &["rev-parse", "HEAD"])
}

#[test]
fn a_hook_is_created_listed_read_edited_and_deleted() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let hook = create_hook(&admin, &receiver);
    let id = id_of(&hook);
    assert_eq!(hook["active"], true);
    assert_eq!(hook["events"], json!(["push"]));
    assert_eq!(hook["type"], "forgejo");
    assert_eq!(hook["config"]["url"], json!(receiver.url));
    assert_eq!(hook["config"]["content_type"], "json");

    let listed = admin.get(HOOKS);
    assert_eq!(listed.status, 200);
    assert_eq!(listed.headers["x-total-count"], "1");
    assert_eq!(listed.body.as_array().map(Vec::len), Some(1));
    assert_eq!(listed.body[0]["id"], hook["id"]);
    assert_eq!(listed.body[0]["config"], hook["config"]);

    let got = admin.get(&format!("{HOOKS}/{id}"));
    assert_eq!(got.status, 200);
    assert_eq!(got.body["id"], hook["id"]);
    assert_eq!(got.body["events"], json!(["push"]));

    let edited = admin.patch(
        &format!("{HOOKS}/{id}"),
        json!({
            "active": false,
            "events": ["issues"],
            "config": { "url": "http://127.0.0.1:9/moved", "secret": SECRET }
        }),
    );
    assert_eq!(edited.status, 200, "{}", edited.body);
    assert_eq!(edited.body["active"], false);
    assert_eq!(edited.body["events"], json!(["issues"]));
    assert_eq!(edited.body["config"]["url"], "http://127.0.0.1:9/moved");
    assert_date_times("Hook", &edited.body);

    let reread = admin.get(&format!("{HOOKS}/{id}"));
    assert_eq!(reread.body["active"], false);
    assert_eq!(reread.body["events"], json!(["issues"]));

    assert_eq!(admin.delete(&format!("{HOOKS}/{id}")).status, 204);
    assert_eq!(admin.get(&format!("{HOOKS}/{id}")).status, 404);
    assert_eq!(admin.delete(&format!("{HOOKS}/{id}")).status, 404);
    assert_eq!(admin.get(HOOKS).body, json!([]));
    assert_eq!(
        admin.post(&format!("{HOOKS}/{id}/tests"), json!({})).status,
        404
    );
}

#[test]
fn no_hook_response_shows_the_secret() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let created = create_hook(&admin, &receiver);
    let id = id_of(&created);
    let edited = admin.patch(
        &format!("{HOOKS}/{id}"),
        json!({ "config": { "secret": SECRET } }),
    );
    assert_eq!(edited.status, 200, "{}", edited.body);

    let bodies = [
        created,
        edited.body,
        admin.get(HOOKS).body[0].clone(),
        admin.get(&format!("{HOOKS}/{id}")).body,
    ];
    for body in bodies {
        let config = body["config"].as_object().expect("a hook renders a config");
        assert!(
            !config.contains_key("secret"),
            "a hook reveals its secret: {body}"
        );
        assert_eq!(
            config.len(),
            2,
            "config is the url and nothing else: {body}"
        );
        assert!(config.contains_key("content_type"), "{body}");
        assert!(config.contains_key("url"), "{body}");
    }
}

#[test]
fn a_token_that_may_only_read_cannot_manage_hooks() {
    let server = server_with_repo();
    server.create_user("reader");
    let token = server
        .user("reader")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    let reader = Api::new(&server, Some(&token));
    let receiver = Receiver::start(200);
    let body = json!({ "config": { "url": receiver.url, "content_type": "json" } });

    let created = reader.post(HOOKS, body.clone());
    assert_eq!(created.status, 403, "{}", created.body);
    assert_eq!(
        created.body["message"],
        "token does not have at least one of required scopes: write:repository"
    );
    assert_eq!(reader.get(HOOKS).status, 403);
    assert_eq!(reader.patch(&format!("{HOOKS}/1"), body).status, 403);
    assert_eq!(reader.delete(&format!("{HOOKS}/1")).status, 403);
    assert_eq!(
        reader.post(&format!("{HOOKS}/1/tests"), json!({})).status,
        403
    );
    assert!(receiver.received().is_empty(), "no hook was created");
}

#[test]
fn a_user_who_does_not_admin_the_repository_cannot_manage_its_hooks() {
    let server = server_with_repo();
    Api::admin(&server).post("/user/repos", json!({ "name": "closed", "private": true }));
    server.create_user("bob");
    let token = server
        .user("bob")
        .unwrap()
        .create_token(&["read:repository", "write:repository"])
        .unwrap();
    let bob = Api::new(&server, Some(&token));
    let body = json!({ "config": { "url": "http://127.0.0.1:9/hook" } });

    // `admin/tool` is public, so Bob may read it; administering its hooks is another matter.
    assert_eq!(bob.get(HOOKS).status, 403);
    assert_eq!(bob.post(HOOKS, body.clone()).status, 403);
    assert_eq!(bob.get(&format!("{HOOKS}/1")).status, 403);
    assert_eq!(bob.patch(&format!("{HOOKS}/1"), json!({})).status, 403);
    assert_eq!(bob.delete(&format!("{HOOKS}/1")).status, 403);
    assert_eq!(bob.post(&format!("{HOOKS}/1/tests"), json!({})).status, 403);

    // A repository Bob cannot see is a 404, as everywhere else in Forgejo.
    assert_eq!(bob.get("/repos/admin/closed/hooks").status, 404);
    assert_eq!(bob.post("/repos/admin/closed/hooks", body).status, 404);
}

#[test]
fn an_inactive_hook_is_never_called() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let created = admin.post(
        HOOKS,
        json!({
            "active": false,
            "config": { "url": receiver.url, "content_type": "json", "secret": SECRET }
        }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.body["active"], false);

    let (_work, dir, _shas) = work_with_two_commits();
    push_main(&server, &dir);

    let deliveries = server.wait_for_deliveries(1, Duration::from_millis(500));
    assert!(deliveries.is_empty(), "an inactive hook is called");
    assert!(receiver.received().is_empty(), "an inactive hook is called");
}

#[test]
fn a_hook_subscribed_to_issues_only_hears_no_push() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let created = admin.post(
        HOOKS,
        json!({
            "events": ["issues"],
            "config": { "url": receiver.url, "content_type": "json", "secret": SECRET }
        }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.body["events"], json!(["issues"]));

    let (_work, dir, _shas) = work_with_two_commits();
    push_main(&server, &dir);

    let deliveries = server.wait_for_deliveries(1, Duration::from_millis(500));
    assert!(
        deliveries.is_empty(),
        "a hook hears only what it subscribed to"
    );
    assert!(
        receiver.received().is_empty(),
        "a hook hears only its events"
    );
}

#[test]
fn a_push_is_delivered_with_the_headers_forgejo_signs_it() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let hook = create_hook(&admin, &receiver);

    let (_work, dir, _shas) = work_with_two_commits();
    let head = push_main(&server, &dir);

    let push = receiver.next();
    verify_event(&push.headers, "push");
    verify_delivery(&push.headers);
    verify_signed(&push.headers, &push.body);
    assert_eq!(push.headers["content-type"], "application/json");
    assert_eq!(push.headers["x-forgejo-hook-id"], id_of(&hook));
    assert_eq!(push.headers["x-gitea-hook-id"], id_of(&hook));
    let payload: Value = serde_json::from_slice(&push.body).unwrap();
    assert_eq!(payload["after"], head);
}

#[test]
fn a_hook_without_a_secret_gets_empty_signatures() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    let created = admin.post(
        HOOKS,
        json!({ "config": { "url": receiver.url, "content_type": "json" } }),
    );
    assert_eq!(created.status, 201, "{}", created.body);

    let (_work, dir, _shas) = work_with_two_commits();
    push_main(&server, &dir);

    let push = receiver.next();
    verify_event(&push.headers, "push");
    verify_delivery(&push.headers);
    // Forgejo sends the signature headers regardless, empty without a secret.
    for (name, value) in [
        ("x-forgejo-signature", ""),
        ("x-gitea-signature", ""),
        ("x-gogs-signature", ""),
        ("x-hub-signature", "sha1="),
        ("x-hub-signature-256", "sha256="),
    ] {
        assert_eq!(push.headers[name], value, "{name}");
    }
}

#[test]
fn the_push_payload_describes_the_commits_that_were_pushed() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let receiver = Receiver::start(200);
    create_hook(&admin, &receiver);

    let (_work, dir, shas) = work_with_two_commits();
    let head = push_main(&server, &dir);

    let push = receiver.next();
    let payload: Value = serde_json::from_slice(&push.body).unwrap();
    assert_eq!(payload["ref"], "refs/heads/main");
    assert_eq!(payload["before"], ZERO_SHA, "the first push of a ref");
    assert_eq!(payload["after"], head);
    assert_eq!(payload["total_commits"], 2);
    let commits = payload["commits"].as_array().expect("commits");
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["id"], shas[0]);
    assert_eq!(commits[0]["message"], "Add a.txt");
    assert_eq!(commits[1]["id"], shas[1]);
    assert_eq!(commits[1]["message"], "Add b.txt");
    assert_eq!(payload["head_commit"]["id"], head);
    assert_eq!(payload["repository"]["full_name"], "admin/tool");
    assert_eq!(payload["pusher"]["login"], "admin");
    assert_eq!(payload["sender"]["login"], "admin");
}

#[test]
fn testing_a_hook_delivers_the_head_of_the_default_branch() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let (_work, dir, _shas) = work_with_two_commits();
    let head = push_main(&server, &dir);
    let receiver = Receiver::start(200);
    let hook = create_hook(&admin, &receiver);

    let tested = admin.post(&format!("{HOOKS}/{}/tests", id_of(&hook)), json!({}));
    assert_eq!(tested.status, 204, "{}", tested.body);

    let deliveries = receiver.wait_for(1, Duration::from_secs(5));
    assert_eq!(deliveries.len(), 1, "testing a hook delivers it once");
    let delivery = &deliveries[0];
    verify_event(&delivery.headers, "push");
    verify_delivery(&delivery.headers);
    verify_signed(&delivery.headers, &delivery.body);
    let payload: Value = serde_json::from_slice(&delivery.body).unwrap();
    assert_eq!(payload["ref"], "refs/heads/main");
    assert_eq!(payload["after"], head);
    assert_eq!(payload["head_commit"]["id"], head);
    assert_eq!(payload["total_commits"], 2);
    assert_eq!(payload["repository"]["full_name"], "admin/tool");
}

#[test]
fn a_redirect_from_the_target_is_recorded_and_not_followed() {
    let server = server_with_repo();
    let admin = Api::admin(&server);
    let (_work, dir, _shas) = work_with_two_commits();
    push_main(&server, &dir);
    let elsewhere = Receiver::start(200);
    let redirector = Receiver::redirecting(&format!("{}/moved", elsewhere.url));
    let created = admin.post(
        HOOKS,
        json!({ "config": { "url": redirector.url, "content_type": "json", "secret": SECRET } }),
    );
    assert_eq!(created.status, 201, "{}", created.body);

    let tested = admin.post(
        &format!("{HOOKS}/{}/tests", id_of(&created.body)),
        json!({}),
    );
    assert_eq!(tested.status, 204, "{}", tested.body);

    let started = std::time::Instant::now();
    let delivery = loop {
        let finished = server
            .deliveries()
            .into_iter()
            .find(|delivery| delivery.status_code.is_some() || delivery.error.is_some());
        if let Some(delivery) = finished {
            break delivery;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no delivery finished"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(delivery.status_code, Some(302), "{:?}", delivery.error);
    assert_eq!(redirector.received().len(), 1, "the target was called once");
    assert!(elsewhere.received().is_empty(), "the redirect was followed");
}

#[test]
fn a_public_only_server_refuses_to_deliver_to_a_loopback_receiver() {
    let server = Server::start_with(ninot_forgejo::server::Options {
        webhook_targets: ninot_forgejo::TargetPolicy::PublicOnly,
        ..Default::default()
    })
    .unwrap();
    Api::admin(&server).post("/user/repos", json!({ "name": REPO }));
    let admin = Api::admin(&server);
    let (_work, dir, _shas) = work_with_two_commits();
    push_main(&server, &dir);
    let receiver = Receiver::start(200);
    // `localhost` is refused by name at creation, so the trailing-dot spelling of the same
    // name is what reaches delivery, where the resolved address is checked.
    let port = receiver.url.rsplit(':').next().unwrap();
    let created = admin.post(
        HOOKS,
        json!({ "config": { "url": format!("http://localhost.:{port}"), "content_type": "json", "secret": SECRET } }),
    );
    assert_eq!(created.status, 201, "{}", created.body);

    let tested = admin.post(
        &format!("{HOOKS}/{}/tests", id_of(&created.body)),
        json!({}),
    );
    assert_eq!(tested.status, 204, "{}", tested.body);

    let started = std::time::Instant::now();
    let delivery = loop {
        let finished = server
            .deliveries()
            .into_iter()
            .find(|delivery| delivery.status_code.is_some() || delivery.error.is_some());
        if let Some(delivery) = finished {
            break delivery;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no delivery finished"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(delivery.status_code, None);
    assert_eq!(
        delivery.error.as_deref(),
        Some("webhook target not allowed")
    );
    assert!(
        receiver.received().is_empty(),
        "a refused target was called"
    );
}
