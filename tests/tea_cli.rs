//! The official Forgejo CLI, `tea`, against the fake.
//!
//! One test per check the conformance script used to run, each against a fake of its own and a
//! `tea` with a configuration of its own: `tea` keeps its logins in `$XDG_CONFIG_HOME/tea`, so
//! these tests neither read nor write the login of whoever runs them.
//!
//! Needs the pinned `tea` these checks were written against, named in [`PINNED`].

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use common::{Api, TempDir, assert_date_times, clone_url, commit_file, git_ok};
use ninot_forgejo::Server;
use serde_json::{Value, json};

/// The release these tests are written against, named whenever there is no binary to run.
const PINNED: &str = "tea 0.9.2 linux-amd64, from \
     https://dl.gitea.com/tea/0.9.2/tea-0.9.2-linux-amd64, with the sha256 to check it against \
     beside it at https://dl.gitea.com/tea/0.9.2/tea-0.9.2-linux-amd64.sha256; unpack it and \
     point $TEA at the binary, or put `tea` on $PATH.";

/// Name the tests give the login they add, which `tea` stores in its configuration.
const LOGIN: &str = "ninot";

/// The `tea` to run: `$TEA`, else `tea` from `$PATH`, looked up here so that a missing binary
/// names the pinned release instead of failing to spawn.
fn tea_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(
        || match std::env::var_os("TEA").filter(|tea| !tea.is_empty()) {
            Some(tea) => {
                let tea = PathBuf::from(tea);
                assert!(
                    tea.is_file(),
                    "$TEA names no tea binary: {}\n{PINNED}",
                    tea.display()
                );
                tea
            }
            None => on_path("tea").unwrap_or_else(|| panic!("no tea binary: {PINNED}")),
        },
    )
}

/// `name` in the first directory of `$PATH` that holds it.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// A `tea` with a home and a configuration of its own, holding one login.
struct Tea {
    bin: PathBuf,
    home: TempDir,
}

impl Tea {
    /// A `tea` whose configuration directory does not exist yet.
    fn new() -> Self {
        Self {
            bin: tea_bin().to_owned(),
            home: TempDir::new(),
        }
    }

    /// A `tea` logged into `server` as [`LOGIN`] with the token of its admin.
    fn logged_in(server: &Server) -> Self {
        let tea = Self::new();
        let added = tea.run(&[
            "login",
            "add",
            "--name",
            LOGIN,
            "--url",
            server.url(),
            "--token",
            server.admin_token(),
        ]);
        assert!(added.status.success(), "{}", both(&added));
        // The fake has no `GET /user/keys`, which tea asks for and warns about on the way.
        assert!(
            both(&added).contains(&format!("Login as admin on {} successful", server.url())),
            "{}",
            both(&added)
        );
        tea
    }

    /// Runs `tea args`, returning what it wrote; panics with that unless it exited 0.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "tea {} failed: {}",
            args.join(" "),
            both(&output)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// [`Self::ok`], with [`LOGIN`] named.
    fn logged_ok(&self, command: &[&str], args: &[&str]) -> String {
        self.ok(&with_login(command, args))
    }

    /// Runs `tea command` with [`LOGIN`] named, for the commands meant to fail.
    fn logged(&self, command: &[&str], args: &[&str]) -> Output {
        self.run(&with_login(command, args))
    }

    /// Runs `tea args` from a home of its own, so that no repository in the working tree makes
    /// `tea` pick a login of its own.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.bin)
            .args(args)
            .current_dir(&self.home.0)
            .env("HOME", &self.home.0)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("NO_COLOR", "1")
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "cannot run {} {}: {error}\n{PINNED}",
                    self.bin.display(),
                    args.join(" ")
                )
            })
    }
}

/// `command` and `args` as one command line with [`LOGIN`] named after the command words, which
/// every command but `whoami` takes: naming it keeps `tea` from announcing on stdout that it
/// fell back to that same login. `--login` is no global flag of `tea`, and `tea` 0.9.2 reads no
/// flag past the positional arguments, so it has to go here rather than at the end.
fn with_login<'a>(command: &[&'a str], args: &[&'a str]) -> Vec<&'a str> {
    let (last, words) = command.split_last().expect("a tea command");
    let mut named = words.to_vec();
    named.extend_from_slice(&[*last, "--login", LOGIN]);
    named.extend_from_slice(args);
    named
}

/// Everything `tea` wrote: it reports errors on stderr and its output on stdout.
fn both(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    format!("{stdout}{}", String::from_utf8_lossy(&output.stderr))
}

/// The rows `tea` printed, one per line, without the trailing spaces it pads its columns with.
fn rows(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// The cells of one `|`-framed line of a table `tea` printed, trimmed of the padding it aligns
/// its columns with and of the border on either end.
fn cells(line: &str) -> Vec<&str> {
    let mut cells: Vec<&str> = line.split('|').skip(1).map(str::trim).collect();
    cells.pop();
    cells
}

/// The column names of a table `tea` printed, in order. Its lines of `+---+` are no cells.
fn header(table: &str) -> Vec<&str> {
    table
        .lines()
        .find(|line| line.starts_with('|'))
        .map(cells)
        .unwrap_or_default()
}

/// The cells of every row of a table `tea` printed, in order, without its header, which is the
/// first of its `|`-framed lines.
fn rows_of(table: &str) -> Vec<Vec<&str>> {
    table
        .lines()
        .filter(|line| line.starts_with("| "))
        .skip(1)
        .map(cells)
        .collect()
}

/// The SSH clone URL of `full_name`, as the fake renders it and `tea` prints it.
fn ssh_url(server: &Server, full_name: &str) -> String {
    format!(
        "git@{}:{full_name}.git",
        server.url().trim_start_matches("http://")
    )
}

/// A fake and a `tea` logged into it, which is how every check here starts.
fn fake() -> (Server, Tea) {
    let server = Server::start().unwrap();
    let tea = Tea::logged_in(&server);
    (server, tea)
}

/// The public repository `admin/pub`, which the checks that need one create themselves.
fn pub_repo(tea: &Tea) {
    tea.logged_ok(&["repos", "create"], &["--name", "pub"]);
}

#[test]
fn whoami_reports_the_user_the_token_authenticates_as() {
    let (server, tea) = fake();

    let whoami = tea.ok(&["whoami"]);
    // `whoami` takes no `--login`, so tea names the one login it falls back to.
    assert!(
        whoami.contains(&format!("falling back to login '{LOGIN}'")),
        "{whoami}"
    );
    assert!(whoami.contains("# admin (admin)"), "{whoami}");
    assert_eq!(Api::admin(&server).get("/user").body["login"], "admin");
}

#[test]
fn repos_create_makes_a_public_repository() {
    let (server, tea) = fake();

    let created = tea.logged_ok(&["repos", "create"], &["--name", "pub"]);
    assert!(created.contains("# admin/pub (empty)"), "{created}");
    assert!(
        created.contains(&format!("{}/admin/pub", server.url())),
        "{created}"
    );

    let repo = Api::admin(&server).get("/repos/admin/pub");
    assert_eq!(repo.status, 200, "{}", repo.body);
    assert_eq!(repo.body["full_name"], "admin/pub");
    assert_eq!(repo.body["owner"]["login"], "admin");
    assert_eq!(repo.body["private"], false);
    assert_eq!(repo.body["empty"], true);
    assert_eq!(repo.body["default_branch"], "main");
    assert_eq!(repo.body["html_url"], format!("{}/admin/pub", server.url()));
    assert_eq!(
        repo.body["clone_url"],
        format!("{}/admin/pub.git", server.url())
    );
    assert_eq!(repo.body["ssh_url"], ssh_url(&server, "admin/pub"));
    // Every timestamp of the repository renders, which `tea` reads whole.
    assert_date_times("Repository", &repo.body);
}

#[test]
fn repos_create_makes_a_private_repository() {
    let (server, tea) = fake();
    tea.logged_ok(&["repos", "create"], &["--name", "pub"]);

    let created = tea.logged_ok(&["repos", "create"], &["--name", "priv", "--private"]);
    assert!(created.contains("# admin/priv (empty)"), "{created}");

    let repo = Api::admin(&server).get("/repos/admin/priv");
    assert_eq!(repo.status, 200, "{}", repo.body);
    assert_eq!(repo.body["full_name"], "admin/priv");
    assert_eq!(repo.body["private"], true);
    assert_eq!(repo.body["permissions"]["push"], true);
    // The repository created before it is the public one, which `--private` did not touch.
    assert_eq!(
        Api::admin(&server).get("/repos/admin/pub").body["private"],
        false
    );
}

#[test]
fn repos_ls_lists_the_repositories_of_the_login() {
    let (server, tea) = fake();
    tea.logged_ok(&["repos", "create"], &["--name", "pub"]);
    tea.logged_ok(&["repos", "create"], &["--name", "priv", "--private"]);

    let mut listed = rows(&tea.logged_ok(
        &["repos", "ls"],
        &["--fields", "name,url", "--output", "simple"],
    ));
    listed.sort_unstable();
    assert_eq!(
        listed,
        [
            format!("priv {}/admin/priv", server.url()),
            format!("pub {}/admin/pub", server.url()),
        ]
    );

    let table = tea.logged_ok(&["repos", "ls"], &[]);
    assert_eq!(header(&table), ["OWNER", "NAME", "TYPE", "SSH"]);
    let mut rows: Vec<Vec<&str>> = rows_of(&table);
    rows.sort_unstable();
    assert_eq!(
        rows,
        [
            [
                "admin",
                "priv",
                "source",
                ssh_url(&server, "admin/priv").as_str()
            ],
            [
                "admin",
                "pub",
                "source",
                ssh_url(&server, "admin/pub").as_str()
            ],
        ]
    );

    let mine = Api::admin(&server).get("/user/repos");
    assert_eq!(mine.status, 200, "{}", mine.body);
    let mut names: Vec<&str> = mine
        .body
        .as_array()
        .expect("a repository list is an array")
        .iter()
        .map(|repo| repo["name"].as_str().expect("a repository is named"))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["priv", "pub"]);
}

#[test]
fn issues_create_opens_an_issue() {
    let (server, tea) = fake();
    pub_repo(&tea);

    let created = tea.logged_ok(
        &["issues", "create"],
        &["--repo", "admin/pub", "--title", "hi"],
    );
    assert!(created.contains("# #1 hi (open)"), "{created}");
    assert!(
        created.contains(&format!("{}/admin/pub/issues/1", server.url())),
        "{created}"
    );

    let issue = Api::admin(&server).get("/repos/admin/pub/issues/1");
    assert_eq!(issue.status, 200, "{}", issue.body);
    assert_eq!(issue.body["number"], 1);
    assert_eq!(issue.body["title"], "hi");
    assert_eq!(issue.body["body"], "");
    assert_eq!(issue.body["state"], "open");
    assert_eq!(issue.body["user"]["login"], "admin");
    assert_eq!(issue.body["closed_at"], Value::Null);
    assert_eq!(issue.body["labels"], json!([]));
    assert_eq!(issue.body["pull_request"], Value::Null);
    assert_date_times("Issue", &issue.body);
}

#[test]
fn issues_ls_lists_the_open_issues_of_a_repository() {
    let (server, tea) = fake();
    pub_repo(&tea);
    tea.logged_ok(
        &["issues", "create"],
        &["--repo", "admin/pub", "--title", "hi"],
    );
    tea.logged_ok(
        &["issues", "create"],
        &["--repo", "admin/pub", "--title", "second"],
    );

    // The issues come newest first.
    let listed = rows(&tea.logged_ok(
        &["issues", "ls"],
        &[
            "--repo",
            "admin/pub",
            "--fields",
            "index,title,state",
            "--output",
            "simple",
        ],
    ));
    assert_eq!(listed, ["2 second open", "1 hi open"]);

    // The table is what `tea` prints by default; its columns are padded to the widest field, so
    // what it holds is checked rather than how it is spaced.
    let table = tea.logged_ok(&["issues", "ls"], &["--repo", "admin/pub"]);
    assert_eq!(
        header(&table),
        ["INDEX", "TITLE", "STATE", "AUTHOR", "MILESTONE", "LABELS"]
    );
    assert_eq!(
        rows_of(&table),
        [
            ["2", "second", "open", "admin", "", ""],
            ["1", "hi", "open", "admin", "", ""],
        ],
        "{table}"
    );

    let open = Api::admin(&server).get("/repos/admin/pub/issues");
    assert_eq!(open.status, 200, "{}", open.body);
    let numbers: Vec<u64> = open
        .body
        .as_array()
        .expect("an issue list is an array")
        .iter()
        .map(|issue| issue["number"].as_u64().expect("an issue is numbered"))
        .collect();
    assert_eq!(numbers, [2, 1]);
    assert_eq!(
        Api::admin(&server).get("/repos/admin/pub").body["open_issues_count"],
        2
    );
}

#[test]
fn issues_close_closes_an_issue_and_leaves_the_open_list() {
    let (server, tea) = fake();
    pub_repo(&tea);
    tea.logged_ok(
        &["issues", "create"],
        &["--repo", "admin/pub", "--title", "hi"],
    );
    tea.logged_ok(
        &["issues", "create"],
        &["--repo", "admin/pub", "--title", "second"],
    );

    // `tea` 0.9.2 reads no flag past the positional index: `issues close 1 --repo` is refused by
    // tea itself, before it sends anything.
    let closed = tea.logged_ok(&["issues", "close"], &["--repo", "admin/pub", "1"]);
    assert!(closed.contains("# #1"), "{closed}");
    assert!(closed.contains("(closed)"), "{closed}");

    let issue = Api::admin(&server).get("/repos/admin/pub/issues/1");
    assert_eq!(issue.status, 200, "{}", issue.body);
    assert_eq!(issue.body["state"], "closed");
    assert!(issue.body["closed_at"].is_string(), "{}", issue.body);
    assert_date_times("Issue", &issue.body);
    // `tea` sends the edit with an empty title, which the fake applies as it was asked to; the
    // title tea prints after closing shows the same. A `{"state": "closed"}` on its own keeps it.
    assert_eq!(issue.body["title"], "");
    assert_eq!(
        Api::admin(&server).get("/repos/admin/pub/issues/2").body["title"],
        "second"
    );

    let states = |state: &str| {
        rows(&tea.logged_ok(
            &["issues", "ls"],
            &[
                "--repo",
                "admin/pub",
                "--state",
                state,
                "--fields",
                "index,state",
                "--output",
                "simple",
            ],
        ))
    };
    assert_eq!(states("open"), ["2 open"]);
    assert_eq!(states("closed"), ["1 closed"]);
    // Every state lists the issues newest first.
    assert_eq!(states("all"), ["2 open", "1 closed"]);
    assert_eq!(
        Api::admin(&server).get("/repos/admin/pub").body["open_issues_count"],
        1
    );
}

#[test]
fn labels_create_adds_a_label() {
    let (server, tea) = fake();
    pub_repo(&tea);

    // `tea` says nothing about a label it added.
    let created = tea.logged_ok(
        &["labels", "create"],
        &["--repo", "admin/pub", "--name", "bug", "--color", "ff0000"],
    );
    assert!(created.trim().is_empty(), "{created}");

    let labels = Api::admin(&server).get("/repos/admin/pub/labels");
    assert_eq!(labels.status, 200, "{}", labels.body);
    let listed = labels.body.as_array().expect("a label list is an array");
    assert_eq!(listed.len(), 1, "{}", labels.body);
    assert_eq!(listed[0]["name"], "bug");
    // Forgejo stores a colour as hexadecimal digits without its leading hash.
    assert_eq!(listed[0]["color"], "ff0000");
    assert_eq!(listed[0]["description"], "");
}

#[test]
fn labels_ls_lists_the_labels_of_a_repository() {
    let (server, tea) = fake();
    pub_repo(&tea);
    tea.logged_ok(
        &["labels", "create"],
        &["--repo", "admin/pub", "--name", "bug", "--color", "ff0000"],
    );

    let labels = Api::admin(&server).get("/repos/admin/pub/labels");
    assert_eq!(labels.status, 200, "{}", labels.body);
    let id = labels.body[0]["id"]
        .as_u64()
        .expect("a label is identified")
        .to_string();

    let listed = rows(&tea.logged_ok(
        &["labels", "ls"],
        &["--repo", "admin/pub", "--output", "simple"],
    ));
    assert_eq!(listed, [format!("{id} ff0000 bug")]);

    // As with the issue table, the columns are padded to their widest field.
    let table = tea.logged_ok(&["labels", "ls"], &["--repo", "admin/pub"]);
    assert_eq!(header(&table), ["INDEX", "COLOR", "NAME", "DESCRIPTION"]);
    assert_eq!(
        rows_of(&table),
        [[id.as_str(), "ff0000", "bug", ""]],
        "{table}"
    );
}

#[test]
fn git_clone_commit_and_push_reaches_the_api() {
    let (server, tea) = fake();
    pub_repo(&tea);

    // The token is the password; any user name works.
    let url = clone_url(&server, "admin", "pub", Some(server.admin_token()));
    let work = TempDir::new();
    git_ok(&work.0, &["clone", "-q", &url, "clone"]);
    let clone = work.join("clone");
    git_ok(&clone, &["checkout", "-q", "-B", "main"]);
    commit_file(&clone, "hello.txt", "hello\n");
    git_ok(&clone, &["push", "-q", "origin", "main"]);
    let head = git_ok(&clone, &["rev-parse", "HEAD"]);

    assert_eq!(
        git_ok(&clone, &["ls-remote", "origin", "refs/heads/main"]),
        format!("{head}\trefs/heads/main")
    );

    let api = Api::admin(&server);
    let repo = api.get("/repos/admin/pub");
    assert_eq!(repo.status, 200, "{}", repo.body);
    assert_eq!(repo.body["empty"], false);

    let branches = api.get("/repos/admin/pub/branches");
    assert_eq!(branches.status, 200, "{}", branches.body);
    let branches = branches.body.as_array().expect("a branch list is an array");
    assert_eq!(branches.len(), 1, "{branches:?}");
    assert_eq!(branches[0]["name"], "main");
    assert_eq!(branches[0]["commit"]["id"], head);

    let contents = api.get("/repos/admin/pub/contents/hello.txt");
    assert_eq!(contents.status, 200, "{}", contents.body);
    assert_eq!(contents.body["encoding"], "base64");
    assert_eq!(contents.body["content"], "aGVsbG8K");

    // `tea` still lists the repository it created.
    assert!(
        tea.logged_ok(
            &["repos", "ls"],
            &["--fields", "name,url", "--output", "simple"]
        )
        .contains(&format!("pub {}/admin/pub", server.url()))
    );
}

/// `tea pulls ls` over a repository that has no pull requests.
///
/// The list stays empty: `tea` 0.9.2 cannot read a pull request of the fake, which renders
/// `base.repo.owner` as a login where `definitions.Repository` declares a `User` and the
/// `gitea.PRBranchInfo` of tea expects an object. Filling the list would fail the test over a
/// defect of the fake rather than over one of `tea`.
#[test]
fn pulls_ls_lists_no_pull_requests_of_a_repository_without_any() {
    let (server, tea) = fake();
    pub_repo(&tea);

    let listed = tea.logged_ok(
        &["pulls", "ls"],
        &["--repo", "admin/pub", "--output", "simple"],
    );
    assert!(listed.trim().is_empty(), "{listed}");
    let closed = tea.logged_ok(
        &["pulls", "ls"],
        &[
            "--repo",
            "admin/pub",
            "--state",
            "closed",
            "--output",
            "simple",
        ],
    );
    assert!(closed.trim().is_empty(), "{closed}");

    // The table of an empty list holds its header and no row.
    let table = tea.logged_ok(&["pulls", "ls"], &["--repo", "admin/pub"]);
    assert_eq!(
        header(&table),
        [
            "INDEX",
            "TITLE",
            "STATE",
            "AUTHOR",
            "MILESTONE",
            "UPDATED",
            "LABELS",
        ],
        "{table}"
    );
    assert!(rows_of(&table).is_empty(), "{table}");

    let pulls = Api::admin(&server).get("/repos/admin/pub/pulls");
    assert_eq!(pulls.status, 200, "{}", pulls.body);
    assert_eq!(pulls.body.as_array().map(Vec::len), Some(0));

    // An empty list is not silence: a repository that is not there fails.
    let missing = tea.logged(&["pulls", "ls"], &["--repo", "admin/nope"]);
    assert!(!missing.status.success(), "{}", both(&missing));
    assert!(
        both(&missing).contains("Error: Not Found"),
        "{}",
        both(&missing)
    );
}
