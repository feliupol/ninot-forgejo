//! Issues, their labels and their comments, through the REST API.

mod common;

use common::{Api, Reply, assert_date_times};
use ninot_forgejo::Server;
use serde_json::{Value, json};

/// A fake with `admin` owning a public repository.
fn fake(name: &str) -> Server {
    let server = Server::start().unwrap();
    let created = Api::admin(&server).post("/user/repos", json!({ "name": name }));
    assert_eq!(created.status, 201, "{}", created.body);
    server
}

/// Creates an issue of `repo` and returns the 201 that answers it.
fn create_in(api: &Api, repo: &str, body: Value) -> Reply {
    let created = api.post(&format!("/repos/admin/{repo}/issues"), body);
    assert_eq!(created.status, 201, "{}", created.body);
    created
}

/// Creates an issue of `tool` and returns the 201 that answers it.
fn create_issue(api: &Api, body: Value) -> Reply {
    create_in(api, "tool", body)
}

/// Creates a label of `repo` and returns it.
fn create_label(api: &Api, repo: &str, name: &str) -> Value {
    let created = api.post(
        &format!("/repos/admin/{repo}/labels"),
        json!({ "name": name, "color": "ff0000" }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    created.body
}

/// The `number` of every issue of a list reply.
fn issue_numbers(reply: &Reply) -> Vec<u64> {
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply
        .body
        .as_array()
        .expect("an issue list is an array")
        .iter()
        .map(|issue| issue["number"].as_u64().expect("an issue is numbered"))
        .collect()
}

/// The `name` of every label of a list reply.
fn label_names(reply: &Reply) -> Vec<String> {
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply
        .body
        .as_array()
        .expect("a label list is an array")
        .iter()
        .map(|label| {
            label["name"]
                .as_str()
                .expect("a label has a name")
                .to_owned()
        })
        .collect()
}

/// The `name` of every label the issue of `reply` carries.
fn labels_of(reply: &Reply) -> Vec<String> {
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body["labels"]
        .as_array()
        .expect("an issue carries a list of labels")
        .iter()
        .map(|label| {
            label["name"]
                .as_str()
                .expect("a label has a name")
                .to_owned()
        })
        .collect()
}

/// The `body` of every comment of a list reply.
fn bodies(reply: &Reply) -> Vec<String> {
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply
        .body
        .as_array()
        .expect("a comment list is an array")
        .iter()
        .map(|comment| {
            comment["body"]
                .as_str()
                .expect("a comment has a body")
                .to_owned()
        })
        .collect()
}

/// The id of `label`.
fn id_of(label: &Value) -> String {
    label["id"]
        .as_u64()
        .expect("a label is identified")
        .to_string()
}

#[test]
fn a_created_issue_looks_the_way_the_spec_says() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let issue = create_issue(&admin, json!({ "title": "Broken", "body": "It breaks." })).body;
    assert_eq!(issue["number"], 1);
    assert_eq!(issue["title"], "Broken");
    assert_eq!(issue["body"], "It breaks.");
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["user"]["login"], "admin");
    assert_eq!(issue["labels"], json!([]));
    assert_eq!(issue["assignees"], json!([]));
    assert_eq!(issue["assignee"], Value::Null);
    assert_eq!(issue["comments"], 0);
    assert_eq!(issue["closed_at"], Value::Null);
    assert_eq!(issue["is_locked"], false);
    assert_eq!(issue["pin_order"], 0);
    assert_eq!(issue["ref"], "");
    assert_eq!(issue["milestone"], Value::Null);
    assert_eq!(issue["pull_request"], Value::Null);
    assert_eq!(issue["assets"], json!([]));
    assert_eq!(issue["original_author"], "");
    assert_eq!(issue["original_author_id"], 0);
    assert_eq!(
        issue["repository"]["id"],
        admin.get("/repos/admin/tool").body["id"]
    );
    assert_eq!(issue["repository"]["name"], "tool");
    assert_eq!(issue["repository"]["owner"], "admin");
    assert_eq!(issue["repository"]["full_name"], "admin/tool");
    assert_eq!(
        issue["url"],
        format!("{}/api/v1/repos/admin/tool/issues/1", server.url())
    );
    assert_eq!(
        issue["html_url"],
        format!("{}/admin/tool/issues/1", server.url())
    );
    assert!(issue["id"].as_u64().unwrap() > 0);
    assert!(issue["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(issue["created_at"], issue["updated_at"]);
    assert_eq!(admin.get("/repos/admin/tool").body["open_issues_count"], 1);
    assert_eq!(admin.get("/repos/admin/tool").body["open_pr_counter"], 0);
}

#[test]
fn issues_are_numbered_per_repository_from_one() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "gear" }));
    let first = create_issue(&admin, json!({ "title": "One" })).body;
    let second = create_issue(&admin, json!({ "title": "Two" })).body;
    assert_eq!(first["number"], 1);
    assert_eq!(second["number"], 2);
    // Another repository counts from one on its own, with ids of its own.
    let elsewhere = create_in(&admin, "gear", json!({ "title": "One" })).body;
    assert_eq!(elsewhere["number"], 1);
    assert_ne!(elsewhere["id"], first["id"]);

    let one = admin.get("/repos/admin/tool/issues/1");
    assert_eq!(one.status, 200);
    assert_eq!(one.body["id"], first["id"]);
    assert_eq!(one.body["title"], "One");
    assert_eq!(admin.get("/repos/admin/tool/issues/2").body["title"], "Two");
    assert_eq!(admin.get("/repos/admin/tool/issues/3").status, 404);
    assert_eq!(admin.get("/repos/admin/tool/issues/abc").status, 404);
    assert_eq!(admin.get("/repos/admin/gear/issues/2").status, 404);
}

#[test]
fn editing_an_issue_changes_its_text_its_assignees_and_its_state() {
    let server = fake("tool");
    server.create_user("alice");
    let admin = Api::admin(&server);
    create_issue(&admin, json!({ "title": "Broken", "body": "It breaks." }));

    let edited = admin.patch(
        "/repos/admin/tool/issues/1",
        json!({ "title": "Broken badly", "assignees": ["alice"] }),
    );
    assert_eq!(edited.status, 201);
    assert_eq!(edited.body["title"], "Broken badly");
    assert_eq!(edited.body["body"], "It breaks.");
    assert_eq!(edited.body["assignees"][0]["login"], "alice");
    assert_eq!(edited.body["assignee"]["login"], "alice");

    let cleared = admin.patch("/repos/admin/tool/issues/1", json!({ "assignees": [] }));
    assert_eq!(cleared.body["assignees"], json!([]));
    assert_eq!(cleared.body["assignee"], Value::Null);
    let one = admin.patch("/repos/admin/tool/issues/1", json!({ "assignee": "alice" }));
    assert_eq!(one.body["assignees"][0]["login"], "alice");

    let closed = admin.patch("/repos/admin/tool/issues/1", json!({ "state": "closed" }));
    assert_eq!(closed.body["state"], "closed");
    assert!(closed.body["closed_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(admin.get("/repos/admin/tool").body["open_issues_count"], 0);

    let reopened = admin.patch("/repos/admin/tool/issues/1", json!({ "state": "open" }));
    assert_eq!(reopened.body["state"], "open");
    assert_eq!(reopened.body["closed_at"], Value::Null);
    assert_eq!(admin.get("/repos/admin/tool").body["open_issues_count"], 1);
    assert_eq!(
        admin.get("/repos/admin/tool/issues/1").body["title"],
        "Broken badly"
    );
}

#[test]
fn an_issue_is_validated_when_it_is_created_and_when_it_is_edited() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    assert_eq!(
        admin
            .post("/repos/admin/tool/issues", json!({ "body": "No title" }))
            .status,
        422
    );
    assert_eq!(
        admin
            .post_raw("/repos/admin/tool/issues", "not json")
            .status,
        400
    );
    assert_eq!(
        admin
            .post(
                "/repos/admin/tool/issues",
                json!({ "title": "One", "labels": [404] })
            )
            .status,
        422
    );
    assert_eq!(
        admin
            .post(
                "/repos/admin/tool/issues",
                json!({ "title": "One", "assignees": ["nobody"] })
            )
            .status,
        422
    );
    let created = create_issue(&admin, json!({ "title": "One" }));
    assert_eq!(
        admin
            .patch("/repos/admin/tool/issues/1", json!({ "state": "sideways" }))
            .status,
        422
    );
    assert_eq!(
        admin.get("/repos/admin/tool/issues/1").body["id"],
        created.body["id"]
    );
    assert_eq!(
        admin
            .patch(
                "/repos/admin/tool/issues/1",
                json!({ "assignees": ["nobody"] })
            )
            .status,
        422
    );
}

#[test]
fn the_issue_list_filters_by_state_and_labels() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let bug = create_label(&admin, "tool", "bug");
    create_issue(&admin, json!({ "title": "One" }));
    create_issue(&admin, json!({ "title": "Two", "labels": [bug["id"]] }));
    let closed = create_issue(&admin, json!({ "title": "Three", "closed": true }));
    assert!(closed.body["closed_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(closed.body["state"], "closed");

    // Without a state the list holds the open issues, the newest first.
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues")),
        [2, 1]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?state=open")),
        [2, 1]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?state=closed")),
        [3]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?state=all")),
        [3, 2, 1]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?labels=bug")),
        [2]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?labels=bug,nothing")),
        [2]
    );
    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?state=all&labels=nothing")),
        Vec::<u64>::new()
    );
    assert_eq!(admin.get("/repos/admin/tool").body["open_issues_count"], 2);
}

#[test]
fn the_issue_list_pages_with_total_count_and_link_headers() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    for title in ["One", "Two", "Three", "Four", "Five"] {
        create_issue(&admin, json!({ "title": title }));
    }

    let first = admin.get("/repos/admin/tool/issues?limit=2");
    assert_eq!(issue_numbers(&first), [5, 4]);
    assert_eq!(first.header("x-total-count"), Some("5"));
    let link = first
        .header("link")
        .expect("a paged list has a Link header");
    assert!(link.contains("page=2>; rel=\"next\""), "{link}");
    assert!(link.contains("page=3>; rel=\"last\""), "{link}");
    assert!(!link.contains("rel=\"prev\""), "{link}");

    let middle = admin.get("/repos/admin/tool/issues?limit=2&page=2");
    assert_eq!(issue_numbers(&middle), [3, 2]);
    assert_eq!(middle.header("x-total-count"), Some("5"));
    let link = middle.header("link").unwrap();
    assert!(link.contains("page=1>; rel=\"prev\""), "{link}");
    assert!(link.contains("page=3>; rel=\"next\""), "{link}");
    assert!(link.contains("page=3>; rel=\"last\""), "{link}");

    let last = admin.get("/repos/admin/tool/issues?limit=2&page=3");
    assert_eq!(issue_numbers(&last), [1]);
    let link = last.header("link").unwrap();
    assert!(link.contains("page=2>; rel=\"prev\""), "{link}");
    assert!(link.contains("page=1>; rel=\"first\""), "{link}");
    assert!(!link.contains("rel=\"next\""), "{link}");

    assert_eq!(
        issue_numbers(&admin.get("/repos/admin/tool/issues?limit=2&page=9")),
        Vec::<u64>::new()
    );
    // A list that fits in one page needs no Link header, but always a total count.
    let whole = admin.get("/repos/admin/tool/issues");
    assert_eq!(issue_numbers(&whole), [5, 4, 3, 2, 1]);
    assert_eq!(whole.header("x-total-count"), Some("5"));
    assert_eq!(whole.header("link"), None);
    assert_eq!(
        admin
            .get("/repos/admin/tool/issues?state=closed")
            .header("x-total-count"),
        Some("0")
    );
}

#[test]
fn an_issue_ignores_the_empty_assignee_and_the_label_zero_clients_send() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    // `tea` 0.9.2 sends an empty assignee name and the label id 0 in place of naming nobody and
    // no label at all. Neither names a user or a label, and Forgejo ignores both.
    let filed = admin.post(
        "/repos/admin/tool/issues",
        json!({
            "title": "hi", "body": "", "ref": "", "assignees": [""],
            "due_date": Value::Null, "milestone": 0, "labels": [0], "closed": false,
        }),
    );
    assert_eq!(filed.status, 201, "{}", filed.body);
    assert_eq!(filed.body["number"], 1);
    assert_eq!(filed.body["assignees"], json!([]));
    assert_eq!(filed.body["assignee"], Value::Null);
    assert_eq!(filed.body["labels"], json!([]));

    // The same placeholders are ignored when an issue is edited.
    let edited = admin.patch(
        "/repos/admin/tool/issues/1",
        json!({ "assignees": [""], "assignee": "", "state": "closed" }),
    );
    assert_eq!(edited.status, 201, "{}", edited.body);
    assert_eq!(edited.body["state"], "closed");
    assert_eq!(edited.body["assignees"], json!([]));

    // A label list is read the same way wherever it is written, and a zero adds nothing.
    let labels = "/repos/admin/tool/issues/1/labels";
    let bug = create_label(&admin, "tool", "bug");
    let added = admin.post(labels, json!({ "labels": [0, bug["id"], ""] }));
    assert_eq!(added.status, 200, "{}", added.body);
    assert_eq!(label_names(&added), ["bug"]);
    let replaced = admin.put(labels, json!({ "labels": [0, ""] }));
    assert_eq!(label_names(&replaced), Vec::<String>::new());

    // A single empty `assignee` names nobody as well.
    let singly = admin.post(
        "/repos/admin/tool/issues",
        json!({ "title": "y", "assignee": "" }),
    );
    assert_eq!(singly.status, 201, "{}", singly.body);
    assert_eq!(singly.body["assignees"], json!([]));

    // A name or an id that is not empty is looked up as before, and refused when unknown.
    for body in [
        json!({ "title": "y", "assignees": ["nobody"] }),
        json!({ "title": "y", "labels": [404] }),
    ] {
        assert_eq!(
            admin.post("/repos/admin/tool/issues", body.clone()).status,
            422,
            "{body}"
        );
    }
    assert_eq!(
        admin
            .patch(
                "/repos/admin/tool/issues/1",
                json!({ "assignees": ["nobody"] })
            )
            .status,
        422
    );
    // A refused request is a refusal, not a silent clearing of what the issue carries.
    admin.post(labels, json!({ "labels": [bug["id"]] }));
    assert_eq!(admin.put(labels, json!({ "labels": [404] })).status, 422);
    assert_eq!(
        admin.post(labels, json!({ "labels": ["nothing"] })).status,
        422
    );
    assert_eq!(labels_of(&admin.get("/repos/admin/tool/issues/1")), ["bug"]);
}

#[test]
fn the_date_times_of_issues_labels_and_comments_are_the_ones_forgejo_sends() {
    let server = fake("tool");
    server.create_user("alice");
    let admin = Api::admin(&server);
    let bug = create_label(&admin, "tool", "bug");
    let filed = create_issue(
        &admin,
        json!({ "title": "One", "labels": [bug["id"]], "assignees": ["alice"] }),
    );
    assert_date_times("Issue", &filed.body);
    assert_date_times("User", &filed.body["user"]);
    assert_date_times("User", &filed.body["assignees"][0]);
    // A label declares no date-time of its own, but the audit follows the spec should it gain one.
    for label in filed.body["labels"]
        .as_array()
        .expect("the issue carries its labels")
    {
        assert_date_times("Label", label);
    }

    let commented = admin.post(
        "/repos/admin/tool/issues/1/comments",
        json!({ "body": "Me too." }),
    );
    assert_eq!(commented.status, 201, "{}", commented.body);
    assert_date_times("Comment", &commented.body);
    assert_date_times("User", &commented.body["user"]);

    // A closed issue records when it was closed; an open one has no such time.
    let closed = admin.patch("/repos/admin/tool/issues/1", json!({ "state": "closed" }));
    assert_date_times("Issue", &closed.body);
    assert!(closed.body["closed_at"].as_str().unwrap().ends_with('Z'));
    let reopened = admin.patch("/repos/admin/tool/issues/1", json!({ "state": "open" }));
    assert_date_times("Issue", &reopened.body);
    assert_date_times("Issue", &admin.get("/repos/admin/tool/issues/1").body);
}

#[test]
fn labels_are_created_listed_edited_and_deleted() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let bug = create_label(&admin, "tool", "bug");
    assert_eq!(bug["color"], "ff0000");
    assert_eq!(bug["description"], "");
    assert_eq!(bug["exclusive"], false);
    assert_eq!(bug["is_archived"], false);
    assert_eq!(
        bug["url"],
        format!(
            "{}/api/v1/repos/admin/tool/labels/{}",
            server.url(),
            id_of(&bug)
        )
    );
    assert_eq!(
        admin
            .get(&format!("/repos/admin/tool/labels/{}", id_of(&bug)))
            .body,
        bug
    );

    // A color given with its leading hash is stored the way Forgejo stores it, without it.
    let described = admin.post(
        "/repos/admin/tool/labels",
        json!({
            "name": "docs",
            "color": "#00aabb",
            "description": "About the documentation",
            "exclusive": true,
        }),
    );
    assert_eq!(described.status, 201);
    assert_eq!(described.body["color"], "00aabb");
    assert_eq!(described.body["description"], "About the documentation");
    assert_eq!(described.body["exclusive"], true);

    let listed = admin.get("/repos/admin/tool/labels");
    assert_eq!(label_names(&listed), ["bug", "docs"]);
    assert_eq!(listed.header("x-total-count"), Some("2"));

    let edited = admin.patch(
        &format!("/repos/admin/tool/labels/{}", id_of(&bug)),
        json!({ "name": "defect", "color": "123456", "is_archived": true }),
    );
    assert_eq!(edited.status, 200);
    assert_eq!(edited.body["name"], "defect");
    assert_eq!(edited.body["color"], "123456");
    assert_eq!(edited.body["is_archived"], true);

    assert_eq!(
        admin
            .delete(&format!("/repos/admin/tool/labels/{}", id_of(&bug)))
            .status,
        204
    );
    assert_eq!(
        admin
            .get(&format!("/repos/admin/tool/labels/{}", id_of(&bug)))
            .status,
        404
    );
    assert_eq!(
        label_names(&admin.get("/repos/admin/tool/labels")),
        ["docs"]
    );

    // The same name may be used in another repository, with a label id of its own.
    admin.post("/user/repos", json!({ "name": "gear" }));
    let elsewhere = create_label(&admin, "gear", "bug");
    assert_ne!(id_of(&elsewhere), id_of(&bug));
    assert_eq!(
        admin
            .get(&format!("/repos/admin/tool/labels/{}", id_of(&elsewhere)))
            .status,
        404
    );
    assert_eq!(label_names(&admin.get("/repos/admin/gear/labels")), ["bug"]);
}

#[test]
fn labels_are_validated_and_unique_by_name_within_a_repository() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let labels = "/repos/admin/tool/labels";
    assert_eq!(admin.post(labels, json!({ "name": "" })).status, 422);
    assert_eq!(
        admin
            .post(labels, json!({ "name": "bug", "color": "not a color" }))
            .status,
        422
    );
    let bug = create_label(&admin, "tool", "bug");
    assert_eq!(admin.post(labels, json!({ "name": "Bug" })).status, 422);
    assert_eq!(admin.post(labels, json!({ "color": "00aabb" })).status, 422);
    assert_eq!(
        admin
            .patch(&format!("{labels}/{}", id_of(&bug)), json!({ "name": "" }))
            .status,
        422
    );
    assert_eq!(
        admin.get(&format!("{labels}/{}", id_of(&bug))).body["name"],
        "bug"
    );
}

#[test]
fn deleting_a_label_takes_it_off_the_issues_that_carry_it() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let bug = create_label(&admin, "tool", "bug");
    create_issue(&admin, json!({ "title": "One", "labels": [bug["id"]] }));
    assert_eq!(labels_of(&admin.get("/repos/admin/tool/issues/1")), ["bug"]);

    assert_eq!(
        admin
            .delete(&format!("/repos/admin/tool/labels/{}", id_of(&bug)))
            .status,
        204
    );
    assert_eq!(
        labels_of(&admin.get("/repos/admin/tool/issues/1")),
        Vec::<String>::new()
    );
    assert_eq!(
        label_names(&admin.get("/repos/admin/tool/issues/1/labels")),
        Vec::<String>::new()
    );
    assert_eq!(admin.get("/repos/admin/tool").body["open_issues_count"], 1);
}

#[test]
fn the_labels_of_an_issue_are_added_replaced_and_removed() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    let bug = create_label(&admin, "tool", "bug");
    let docs = create_label(&admin, "tool", "docs");
    create_issue(&admin, json!({ "title": "One" }));
    let labels = "/repos/admin/tool/issues/1/labels";
    assert_eq!(label_names(&admin.get(labels)), Vec::<String>::new());

    // Labels are named either by their id or by their name.
    let added = admin.post(labels, json!({ "labels": [bug["id"], "docs"] }));
    assert_eq!(added.status, 200);
    assert_eq!(label_names(&added), ["bug", "docs"]);
    assert_eq!(
        labels_of(&admin.get("/repos/admin/tool/issues/1")),
        ["bug", "docs"]
    );

    let replaced = admin.put(labels, json!({ "labels": [docs["id"]] }));
    assert_eq!(label_names(&replaced), ["docs"]);

    assert_eq!(
        admin.delete(&format!("{labels}/{}", id_of(&docs))).status,
        204
    );
    assert_eq!(label_names(&admin.get(labels)), Vec::<String>::new());
    admin.post(labels, json!({ "labels": ["bug"] }));
    assert_eq!(
        admin.delete(&format!("{labels}/{}", id_of(&bug))).status,
        204
    );
    assert_eq!(label_names(&admin.get(labels)), Vec::<String>::new());

    admin.post(labels, json!({ "labels": ["bug", "docs"] }));
    assert_eq!(admin.delete(&format!("{labels}/bug")).status, 204);
    assert_eq!(label_names(&admin.get(labels)), ["docs"]);
    assert_eq!(admin.delete(labels).status, 204);
    assert_eq!(label_names(&admin.get(labels)), Vec::<String>::new());

    // Labels that do not exist are refused, and so is an issue that does not.
    assert_eq!(
        admin.post(labels, json!({ "labels": ["nothing"] })).status,
        422
    );
    assert_eq!(admin.post(labels, json!({ "labels": [9999] })).status, 422);
    assert_eq!(admin.get("/repos/admin/tool/issues/9/labels").status, 404);
    assert_eq!(
        admin
            .post(
                "/repos/admin/tool/issues/9/labels",
                json!({ "labels": ["bug"] })
            )
            .status,
        404
    );
}

#[test]
fn comments_are_created_listed_edited_and_deleted() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    create_issue(&admin, json!({ "title": "One" }));
    create_issue(&admin, json!({ "title": "Two" }));

    let posted = admin.post(
        "/repos/admin/tool/issues/1/comments",
        json!({ "body": "I see it too." }),
    );
    assert_eq!(posted.status, 201);
    let comment = posted.body;
    assert_eq!(comment["body"], "I see it too.");
    assert_eq!(comment["user"]["login"], "admin");
    assert_eq!(comment["assets"], json!([]));
    assert_eq!(comment["original_author"], "");
    assert_eq!(comment["original_author_id"], 0);
    assert_eq!(comment["pull_request_url"], "");
    assert!(comment["id"].as_u64().unwrap() > 0);
    assert!(comment["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(comment["created_at"], comment["updated_at"]);
    assert_eq!(
        comment["issue_url"],
        format!("{}/admin/tool/issues/1", server.url())
    );
    assert_eq!(
        comment["html_url"],
        format!(
            "{}/admin/tool/issues/1#issuecomment-{}",
            server.url(),
            comment["id"].as_u64().unwrap()
        )
    );

    admin.post(
        "/repos/admin/tool/issues/2/comments",
        json!({ "body": "Not here." }),
    );
    assert_eq!(
        bodies(&admin.get("/repos/admin/tool/issues/1/comments")),
        ["I see it too."]
    );
    let of_repo = admin.get("/repos/admin/tool/issues/comments");
    assert_eq!(of_repo.header("x-total-count"), Some("2"));
    assert_eq!(bodies(&of_repo).len(), 2);
    assert_eq!(admin.get("/repos/admin/tool/issues/1").body["comments"], 1);
    assert_eq!(
        bodies(&admin.get("/repos/admin/tool/issues/comments?limit=1")),
        ["I see it too."]
    );

    let id = comment["id"].as_u64().unwrap();
    let by_id = format!("/repos/admin/tool/issues/comments/{id}");
    assert_eq!(admin.get(&by_id).body, comment);
    let edited = admin.patch(&by_id, json!({ "body": "Same here." }));
    assert_eq!(edited.status, 200);
    assert_eq!(edited.body["body"], "Same here.");
    assert_eq!(admin.get(&by_id).body["body"], "Same here.");

    assert_eq!(admin.delete(&by_id).status, 204);
    assert_eq!(admin.get(&by_id).status, 404);
    assert_eq!(
        bodies(&admin.get("/repos/admin/tool/issues/comments")).len(),
        1
    );
    assert_eq!(admin.get("/repos/admin/tool/issues/1").body["comments"], 0);

    // A comment is only found in the repository it belongs to.
    admin.post("/user/repos", json!({ "name": "gear" }));
    assert_eq!(
        admin
            .get(&format!("/repos/admin/gear/issues/comments/{id}"))
            .status,
        404
    );
}

#[test]
fn a_comment_needs_a_body_and_an_issue_to_hang_on() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    create_issue(&admin, json!({ "title": "One" }));
    let comments = "/repos/admin/tool/issues/1/comments";
    assert_eq!(admin.post(comments, json!({ "body": "" })).status, 422);
    assert_eq!(admin.post_raw(comments, "{").status, 400);
    assert_eq!(
        admin
            .post(
                "/repos/admin/tool/issues/9/comments",
                json!({ "body": "Hello" })
            )
            .status,
        404
    );
    let id = admin
        .post(comments, json!({ "body": "I see it too." }))
        .body["id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        admin
            .patch(
                &format!("/repos/admin/tool/issues/comments/{id}"),
                json!({ "body": "" })
            )
            .status,
        422
    );
    assert_eq!(
        admin.get("/repos/admin/tool/issues/comments/9999").status,
        404
    );
}

#[test]
fn reading_issues_needs_the_read_issue_scope_and_writing_the_write_one() {
    let server = fake("tool");
    create_issue(&Api::admin(&server), json!({ "title": "One" }));
    server.create_user("alice");
    let repository = server
        .user("alice")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    let reader = server
        .user("alice")
        .unwrap()
        .create_token(&["read:issue"])
        .unwrap();
    let writer = server
        .user("alice")
        .unwrap()
        .create_token(&["write:issue"])
        .unwrap();

    assert_eq!(
        Api::new(&server, None)
            .get("/repos/admin/tool/issues")
            .status,
        401
    );
    assert_eq!(
        Api::new(&server, Some(&repository))
            .get("/repos/admin/tool/issues")
            .status,
        403
    );
    for path in [
        "/repos/admin/tool/issues",
        "/repos/admin/tool/issues/1",
        "/repos/admin/tool/labels",
        "/repos/admin/tool/issues/1/comments",
        "/repos/admin/tool/issues/comments",
    ] {
        assert_eq!(
            Api::new(&server, Some(&reader)).get(path).status,
            200,
            "{path}"
        );
    }
    let reader = Api::new(&server, Some(&reader));
    assert_eq!(
        reader
            .post(
                "/repos/admin/tool/issues",
                json!({ "title": "Not mine to write" })
            )
            .status,
        403
    );
    assert_eq!(
        reader
            .post("/repos/admin/tool/labels", json!({ "name": "bug" }))
            .status,
        403
    );
    assert_eq!(reader.get("/repos/admin/tool/issues/1/labels").status, 200);
    assert_eq!(
        reader
            .post(
                "/repos/admin/tool/issues/1/labels",
                json!({ "labels": ["bug"] })
            )
            .status,
        403
    );
    assert_eq!(
        reader
            .put(
                "/repos/admin/tool/issues/1/labels",
                json!({ "labels": ["bug"] })
            )
            .status,
        403
    );
    assert_eq!(
        reader
            .delete("/repos/admin/tool/issues/1/labels/docs")
            .status,
        403
    );
    assert_eq!(
        reader.delete("/repos/admin/tool/issues/1/labels").status,
        403
    );
    assert_eq!(
        reader.patch("/repos/admin/tool/issues/1", json!({})).status,
        403
    );
    assert_eq!(
        reader.delete("/repos/admin/tool/issues/comments/1").status,
        403
    );

    let writer = Api::new(&server, Some(&writer));
    assert_eq!(
        writer
            .post("/repos/admin/tool/issues", json!({ "title": "Filed" }))
            .status,
        201
    );
    assert_eq!(writer.get("/repos/admin/tool/issues/9").status, 404);
    // A writer of issues still needs write access to the repository itself.
    assert_eq!(
        writer
            .post("/repos/admin/tool/labels", json!({ "name": "bug" }))
            .status,
        403
    );
}

#[test]
fn only_the_author_or_somebody_with_write_access_may_change_an_issue() {
    let server = fake("tool");
    let admin = Api::admin(&server);
    create_issue(&admin, json!({ "title": "One" }));
    server.create_user("bob");
    let token = server
        .user("bob")
        .unwrap()
        .create_token(&["read:issue", "write:issue"])
        .unwrap();
    let bob = Api::new(&server, Some(&token));

    // A reader of a public repository may file issues and comment on them.
    let own = bob.post("/repos/admin/tool/issues", json!({ "title": "Mine" }));
    assert_eq!(own.status, 201);
    assert_eq!(own.body["number"], 2);
    assert_eq!(own.body["user"]["login"], "bob");
    let commented = bob.post(
        "/repos/admin/tool/issues/2/comments",
        json!({ "body": "On it." }),
    );
    assert_eq!(commented.status, 201);

    // But not touch what somebody else wrote.
    assert_eq!(
        bob.patch("/repos/admin/tool/issues/1", json!({ "state": "closed" }))
            .status,
        403
    );
    assert_eq!(
        bob.post(
            "/repos/admin/tool/issues/1/labels",
            json!({ "labels": ["bug"] })
        )
        .status,
        403
    );
    assert_eq!(
        bob.post("/repos/admin/tool/labels", json!({ "name": "bug" }))
            .status,
        403
    );
    let foreign = admin
        .post(
            "/repos/admin/tool/issues/1/comments",
            json!({ "body": "Mine." }),
        )
        .body["id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        bob.patch(
            &format!("/repos/admin/tool/issues/comments/{foreign}"),
            json!({ "body": "Yours now." })
        )
        .status,
        403
    );
    assert_eq!(
        bob.delete(&format!("/repos/admin/tool/issues/comments/{foreign}"))
            .status,
        403
    );

    // Its own issue and its own comment are its own to change.
    let mine = bob.patch("/repos/admin/tool/issues/2", json!({ "state": "closed" }));
    assert_eq!(mine.status, 201);
    assert_eq!(mine.body["state"], "closed");
    let id = commented.body["id"].as_u64().unwrap();
    assert_eq!(
        bob.patch(
            &format!("/repos/admin/tool/issues/comments/{id}"),
            json!({ "body": "Done." })
        )
        .status,
        200
    );
    assert_eq!(
        bob.delete(&format!("/repos/admin/tool/issues/comments/{id}"))
            .status,
        204
    );
    // Whoever may write to the repository may label and close issues of others.
    let bug = create_label(&admin, "tool", "bug");
    admin.post(
        "/repos/admin/tool/issues/1/labels",
        json!({ "labels": [bug["id"]] }),
    );
    assert_eq!(labels_of(&admin.get("/repos/admin/tool/issues/1")), ["bug"]);
    let closed = admin.patch("/repos/admin/tool/issues/2", json!({ "state": "closed" }));
    assert_eq!(closed.body["state"], "closed");
}

#[test]
fn the_issues_of_a_private_repository_stay_invisible_to_outsiders() {
    let server = Server::start().unwrap();
    let admin = Api::admin(&server);
    admin.post("/user/repos", json!({ "name": "secret", "private": true }));
    create_in(&admin, "secret", json!({ "title": "One" }));
    admin.post("/repos/admin/secret/labels", json!({ "name": "bug" }));
    admin.post(
        "/repos/admin/secret/issues/1/comments",
        json!({ "body": "Secret." }),
    );
    assert_eq!(
        admin.get("/repos/admin/secret").body["open_issues_count"],
        1
    );

    server.create_user("alice");
    let token = server
        .user("alice")
        .unwrap()
        .create_token(&["read:issue", "write:issue"])
        .unwrap();
    let alice = Api::new(&server, Some(&token));
    for path in [
        "/repos/admin/secret/issues",
        "/repos/admin/secret/issues/1",
        "/repos/admin/secret/issues/1/labels",
        "/repos/admin/secret/issues/1/comments",
        "/repos/admin/secret/issues/comments",
        "/repos/admin/secret/issues/comments/1",
        "/repos/admin/secret/labels",
    ] {
        assert_eq!(alice.get(path).status, 404, "{path}");
    }
    assert_eq!(
        alice
            .post("/repos/admin/secret/issues", json!({ "title": "Sneaky" }))
            .status,
        404
    );
    assert_eq!(
        alice
            .post("/repos/admin/secret/labels", json!({ "name": "sneaky" }))
            .status,
        404
    );
    assert_eq!(
        alice
            .patch("/repos/admin/secret/issues/1", json!({ "state": "closed" }))
            .status,
        404
    );
    assert_eq!(
        alice
            .post(
                "/repos/admin/secret/issues/1/comments",
                json!({ "body": "Sneaky" })
            )
            .status,
        404
    );
    assert_eq!(
        admin.get("/repos/admin/secret").body["open_issues_count"],
        1
    );

    // A token without the scope is told so before the repository is looked for.
    let stranger = server
        .user("alice")
        .unwrap()
        .create_token(&["read:repository"])
        .unwrap();
    assert_eq!(
        Api::new(&server, Some(&stranger))
            .get("/repos/admin/secret/issues")
            .status,
        403
    );
}
