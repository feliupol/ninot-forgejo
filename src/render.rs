//! Renders the spec's response bodies from state objects.
//!
//! Every body is a model generated from the pinned description's definitions, so what a client
//! reads is what that description says the operation answers with; the only shape Forgejo itself
//! departs from is what [`spec/overrides.json`](crate) records and why.
//!
//! Every field the spec declares as `format: date-time` is a [`DateTime<Utc>`], which is what
//! serde renders as a timestamp a Go client can parse. One the fake has no time for is
//! `null`, or [`crate::time::unset`], Forgejo's zero time; an empty string is not a date-time
//! and makes Go refuse the whole response.

use base64::Engine as _;
use url::Url;

use crate::api::ApiRequest;
use crate::merge;
use crate::state::pull::PullRequest;
use crate::state::{Comment, Issue, Label, Permission, Repo, State, User};
use crate::time;
use crate::types;
use ninot_common::git;

/// The Forgejo the pinned spec describes, as `GET /version` returns it.
pub fn version() -> types::ServerVersion {
    types::ServerVersion {
        version: crate::spec::VERSION.to_owned(),
    }
}

/// The host of `url` with the port it names, as an `ssh_url` addresses a host: `host:port`.
/// A URL that names no host is its own, which is what it looked like before.
pub fn ssh_host(url: &str) -> String {
    let host = Url::parse(url).ok().and_then(|url| {
        let host = url.host_str()?;
        Some(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        })
    });
    host.unwrap_or_else(|| url.to_owned())
}

/// A user, as `GET /user` returns them.
pub fn user(request: &ApiRequest, user: &User) -> types::User {
    types::User {
        id: user.id as i64,
        login: user.login.clone(),
        login_name: user.login.clone(),
        source_id: 0,
        full_name: user.full_name.clone(),
        email: user.email.clone(),
        avatar_url: format!("{}/assets/img/avatar_default.png", request.web_url),
        language: "en-US".to_owned(),
        is_admin: user.is_admin,
        last_login: user.created_at,
        created: user.created_at,
        restricted: user.restricted,
        active: user.active,
        prohibit_login: false,
        location: String::new(),
        website: String::new(),
        description: String::new(),
        visibility: "public".to_owned(),
        followers_count: 0,
        following_count: 0,
        starred_repos_count: 0,
        html_url: format!("{}/{}", request.web_url, user.login),
        pronouns: String::new(),
    }
}

/// A repository, as `GET /repos/{owner}/{repo}` and the repository lists return it. `empty`
/// comes from the bare repository, which the state does not read on its own.
pub fn repo(
    request: &ApiRequest,
    owner: &User,
    repo: &Repo,
    permission: Permission,
    empty: bool,
) -> types::Repository {
    let full_name = format!("{}/{}", owner.login, repo.name);
    let api = format!("{}/repos/{full_name}", request.api_url);
    let html = format!("{}/{full_name}", request.web_url);
    let host = ssh_host(&request.web_url);
    types::Repository {
        id: repo.id as i64,
        owner: user(request, owner),
        name: repo.name.clone(),
        full_name: full_name.clone(),
        description: repo.description.clone(),
        empty,
        private: repo.private,
        fork: false,
        template: false,
        parent: Box::new(None),
        mirror: false,
        internal: false,
        size: 0,
        object_format_name: types::RepositoryObjectFormatName::Sha1,
        html_url: html.clone(),
        url: api,
        link: String::new(),
        ssh_url: format!("git@{host}:{full_name}.git"),
        clone_url: format!("{html}.git"),
        original_url: String::new(),
        website: String::new(),
        stars_count: 0,
        forks_count: 0,
        forks: 0,
        watchers_count: 0,
        // Pull requests are open issues too, once the fake serves them.
        open_issues_count: repo.tracker.open_issues() as i64,
        open_pr_counter: repo
            .tracker
            .pulls
            .values()
            .filter(|pr| pr.state == crate::state::Status::Open && !pr.merged)
            .count() as i64,
        release_counter: 0,
        default_branch: repo.default_branch.clone(),
        default_delete_branch_after_merge: repo.delete_branch_after_merge,
        archived: false,
        archived_at: None,
        created_at: repo.created_at,
        updated_at: repo.updated_at,
        pushed_at: repo.pushed_at,
        has_issues: true,
        has_wiki: true,
        has_pull_requests: true,
        has_projects: true,
        has_releases: true,
        has_packages: true,
        has_actions: true,
        allow_merge_commits: true,
        allow_rebase: true,
        allow_rebase_explicit: true,
        allow_rebase_update: true,
        allow_fast_forward_only_merge: false,
        allow_squash_merge: true,
        ignore_whitespace_conflicts: false,
        mirror_interval: String::new(),
        // A repository that is no mirror has never been mirrored. Forgejo renders such a time as
        // its zero time, and the spec declares the field `date-time`, so anything else here
        // makes a Go client refuse to parse the whole repository.
        mirror_updated: time::unset(),
        repo_transfer: None,
        topics: Vec::new(),
        permissions: types::Permission {
            admin: permission.admin,
            push: permission.push,
            pull: permission.pull,
        },
        // The keys Forgejo leaves out of a repository the fake has nothing to say about.
        avatar_url: None,
        default_allow_maintainer_edit: None,
        default_merge_style: None,
        default_update_style: None,
        external_tracker: None,
        external_wiki: None,
        globally_editable_wiki: None,
        has_wiki_contents: None,
        internal_tracker: None,
        language: None,
        languages_url: None,
        wiki_branch: None,
        wiki_clone_url: None,
        wiki_ssh_url: None,
    }
}

/// An issue of `owner/name`, as `GET /repos/{owner}/{repo}/issues/{index}` and the issue lists
/// return it. Its labels and comments live in the repository's tracker.
pub fn issue(
    state: &State,
    request: &ApiRequest,
    owner: &User,
    repo: &Repo,
    issue: &Issue,
) -> types::Issue {
    let full_name = format!("{}/{}", owner.login, repo.name);
    let api = format!("{}/repos/{full_name}", request.api_url);
    let html = format!("{}/{full_name}", request.web_url);
    let labels = issue
        .labels
        .iter()
        .filter_map(|id| repo.tracker.label(*id))
        .map(|carried| self::label(&api, carried))
        .collect();
    let assignees: Vec<types::User> = issue
        .assignees
        .iter()
        .filter_map(|login| state.user(login))
        .map(|assignee| self::user(request, assignee))
        .collect();
    types::Issue {
        id: issue.id as i64,
        url: format!("{api}/issues/{}", issue.index),
        html_url: format!("{html}/issues/{}", issue.index),
        number: issue.index as i64,
        user: user(
            request,
            state.user(&issue.poster).expect("an issue has a poster"),
        ),
        original_author: String::new(),
        original_author_id: 0,
        title: issue.title.clone(),
        body: issue.body.clone(),
        ref_: String::new(),
        assets: Vec::new(),
        created_at: issue.created_at,
        updated_at: issue.updated_at,
        closed_at: issue.closed_at,
        labels,
        milestone: None,
        assignee: assignees.first().cloned(),
        assignees,
        state: types::StateType(issue.state.as_str().to_owned()),
        is_locked: false,
        comments: repo.tracker.comment_count(issue.index) as i64,
        due_date: None,
        pin_order: 0,
        repository: meta(&repo.id, &repo.name, &owner.login, &full_name),
        pull_request: repo
            .tracker
            .pull(issue.index)
            .map(|pr| types::PullRequestMeta {
                draft: Some(pr.draft),
                html_url: Some(format!("{html}/pulls/{}", pr.index)),
                merged: Some(pr.merged),
                merged_at: pr.merged_at,
            }),
    }
}

/// A label of the repository whose API URL is `api`, as the label operations return it.
pub fn label(api: &str, label: &Label) -> types::Label {
    types::Label {
        id: label.id as i64,
        name: label.name.clone(),
        exclusive: label.exclusive,
        description: label.description.clone(),
        color: label.color.clone(),
        url: format!("{api}/labels/{}", label.id),
        is_archived: label.is_archived,
    }
}

/// A comment on an issue of `owner/name`, as the comment operations return it.
pub fn comment(
    state: &State,
    request: &ApiRequest,
    owner: &User,
    repo: &Repo,
    comment: &Comment,
) -> types::Comment {
    let html = format!("{}/{}/{}", request.web_url, owner.login, repo.name);
    types::Comment {
        id: comment.id as i64,
        html_url: format!(
            "{html}/issues/{}#issuecomment-{}",
            comment.index, comment.id
        ),
        issue_url: format!("{html}/issues/{}", comment.index),
        pull_request_url: String::new(),
        user: user(
            request,
            state.user(&comment.poster).expect("a comment has a poster"),
        ),
        original_author: String::new(),
        original_author_id: 0,
        body: comment.body.clone(),
        assets: Vec::new(),
        created_at: comment.created_at,
        updated_at: comment.updated_at,
    }
}

/// The signature git recorded for a commit, as the spec's commit bodies spell it.
fn commit_user(signature: &git::Signature) -> types::CommitUser {
    types::CommitUser {
        name: Some(signature.name.clone()),
        email: Some(signature.email.clone()),
        date: Some(signature.date.clone()),
    }
}

/// One branch, as `GET /repos/{owner}/{repo}/branches` returns it.
pub fn branch(api: &str, name: &str, commit: &git::Commit) -> types::Branch {
    types::Branch {
        name: name.to_owned(),
        commit: types::BranchCommit {
            id: commit.sha.clone(),
            message: commit.message.clone(),
            url: format!("{api}/git/commits/{}", commit.sha),
            author: commit_user(&commit.author),
            committer: commit_user(&commit.committer),
            verification: None,
            timestamp: time::timestamp(&commit.committer.date),
        },
        protected: false,
        required_approvals: 0,
        enable_status_check: false,
        status_check_contexts: Vec::new(),
        user_can_push: true,
        user_can_merge: true,
        effective_branch_protection_name: String::new(),
    }
}

/// A file's contents, as `GET /repos/{owner}/{repo}/contents/{filepath}` returns them.
pub fn file(
    api: &str,
    html: &str,
    reference: &str,
    entry: &git::TreeEntry,
    content: &[u8],
) -> types::ContentsResponse {
    let self_url = format!("{api}/contents/{}?ref={reference}", entry.path);
    let html_url = format!("{html}/src/branch/{reference}/{}", entry.path);
    let git_url = format!("{api}/git/blobs/{}", entry.sha);
    let download_url = format!("{html}/raw/branch/{reference}/{}", entry.path);
    types::ContentsResponse {
        name: name_of(&entry.path).to_owned(),
        path: entry.path.clone(),
        sha: entry.sha.clone(),
        last_commit_sha: entry.sha.clone(),
        type_: "file".to_owned(),
        size: entry.size as i64,
        encoding: Some("base64".to_owned()),
        content: Some(base64::engine::general_purpose::STANDARD.encode(content)),
        target: None,
        submodule_git_url: None,
        url: self_url.clone(),
        html_url: html_url.clone(),
        git_url: git_url.clone(),
        download_url: Some(download_url.clone()),
        links: types::FileLinksResponse {
            self_: self_url,
            git: git_url,
            html: html_url,
            download: Some(download_url),
        },
        is_submodule: None,
        last_commit_when: None,
    }
}

/// The entries of a directory, as the contents operation returns for one.
pub fn directory(
    api: &str,
    html: &str,
    reference: &str,
    entries: &[git::TreeEntry],
) -> Vec<types::ContentsResponse> {
    entries
        .iter()
        .map(|entry| {
            let directory = entry.kind == "tree";
            let self_url = format!("{api}/contents/{}?ref={reference}", entry.path);
            let html_url = format!("{html}/src/branch/{reference}/{}", entry.path);
            let git_url = format!(
                "{api}/git/{}s/{}",
                if directory { "tree" } else { "blob" },
                entry.sha
            );
            let download_url =
                (!directory).then(|| format!("{html}/raw/branch/{reference}/{}", entry.path));
            types::ContentsResponse {
                name: name_of(&entry.path).to_owned(),
                path: entry.path.clone(),
                sha: entry.sha.clone(),
                last_commit_sha: entry.sha.clone(),
                type_: if directory { "dir" } else { "file" }.to_owned(),
                size: entry.size as i64,
                is_submodule: Some(entry.kind == "commit"),
                encoding: None,
                content: None,
                target: None,
                submodule_git_url: None,
                url: self_url.clone(),
                html_url: html_url.clone(),
                git_url: git_url.clone(),
                download_url,
                links: types::FileLinksResponse {
                    self_: self_url,
                    git: git_url,
                    html: html_url,
                    download: None,
                },
                last_commit_when: None,
            }
        })
        .collect()
}

/// A pull request, as the pull request operations return it. `branches` is where its base and
/// head stand right now, read from the repository rather than from what the request recorded, so
/// that a push to either of them shows in the pull request as soon as it is asked for again.
pub fn pull_request(
    state: &State,
    request: &ApiRequest,
    owner: &User,
    repo: &Repo,
    pr: &PullRequest,
    branches: &merge::Branches,
) -> types::PullRequest {
    let full_name = format!("{}/{}", owner.login, repo.name);
    let api = format!("{}/repos/{full_name}", request.api_url);
    let html = format!("{}/{full_name}", request.web_url);
    let labels = repo
        .tracker
        .issue(pr.index)
        .expect("a PR has a paired issue")
        .labels
        .iter()
        .filter_map(|id| repo.tracker.label(*id))
        .map(|carried| self::label(&api, carried))
        .collect();
    let assignees: Vec<types::User> = pr
        .assignees
        .iter()
        .filter_map(|login| state.user(login))
        .map(|assignee| self::user(request, assignee))
        .collect();
    let meta = meta(&repo.id, &repo.name, &owner.login, &full_name);
    let branch_info = |name: &str, sha: &str| types::PrBranchInfo {
        label: name.to_owned(),
        ref_: name.to_owned(),
        sha: sha.to_owned(),
        repo: meta.clone(),
        repo_id: None,
    };
    types::PullRequest {
        id: pr.id as i64,
        url: format!("{api}/pulls/{}", pr.index),
        html_url: format!("{html}/pulls/{}", pr.index),
        number: pr.index as i64,
        user: user(request, state.user(&pr.poster).expect("a pr has a poster")),
        title: pr.title.clone(),
        body: pr.body.clone(),
        created_at: pr.created_at,
        updated_at: pr.updated_at,
        closed_at: pr.closed_at,
        merged_at: pr.merged_at,
        merged_by: pr
            .merged_by
            .as_ref()
            .and_then(|login| state.user(login))
            .map(|merged| self::user(request, merged)),
        merge_commit_sha: pr.merge_commit_sha.clone(),
        state: types::StateType(pr.state.as_str().to_owned()),
        labels,
        assignee: assignees.first().cloned(),
        assignees,
        milestone: None,
        due_date: None,
        base: branch_info(&pr.base, &branches.base),
        head: branch_info(&pr.head, &branches.head),
        merged: pr.merged,
        mergeable: branches.mergeable,
        draft: pr.draft,
        comments: repo.tracker.comment_count(pr.index) as i64,
        review_comments: 0,
        additions: 0,
        deletions: 0,
        changed_files: 0,
        patch_url: format!("{api}/pulls/{}.patch", pr.index),
        diff_url: format!("{api}/pulls/{}.diff", pr.index),
        // The keys Forgejo leaves out of a pull request the fake has nothing to say about.
        allow_maintainer_edit: None,
        flow: None,
        is_locked: None,
        merge_base: None,
        pin_order: None,
        requested_reviewers: Vec::new(),
        requested_reviewers_teams: Vec::new(),
    }
}

/// One commit of a pull request, as the commit list of a pull request returns it. Git alone
/// knows who made it, so the author and committer it names are `null`, as the override of
/// `Commit` records, and the signatures are read from its commit.
pub fn commit(commit: &git::Commit) -> types::Commit {
    types::Commit {
        sha: commit.sha.clone(),
        commit: types::RepoCommit {
            author: commit_user(&commit.author),
            committer: commit_user(&commit.committer),
            message: commit.message.clone(),
            tree: None,
            url: None,
            verification: None,
        },
        author: None,
        committer: None,
        parents: commit
            .parents
            .iter()
            .map(|sha| types::CommitMeta {
                created: None,
                sha: Some(sha.clone()),
                url: None,
            })
            .collect(),
        url: None,
        created: None,
        html_url: None,
        stats: None,
        files: Vec::new(),
    }
}

/// A change category reported by the compare operation for a pull request file.
#[derive(Clone, Copy)]
pub enum ChangedFileStatus {
    /// The file was added by the pull request.
    Added,
    /// The file was removed by the pull request.
    Removed,
    /// The file existed before and after the pull request.
    Modified,
}

/// One file a pull request changes, of which the fake counts no lines or content changes.
pub fn changed_file(filename: String, status: ChangedFileStatus) -> types::ChangedFile {
    types::ChangedFile {
        filename,
        status: match status {
            ChangedFileStatus::Added => "added",
            ChangedFileStatus::Removed => "removed",
            ChangedFileStatus::Modified => "modified",
        }
        .to_owned(),
        additions: 0,
        deletions: 0,
        changes: 0,
        blob_url: Some(String::new()),
        raw_url: String::new(),
        contents_url: String::new(),
        html_url: None,
        previous_filename: None,
    }
}

/// The last segment of a path from the repository root.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The repository `name` of `owner`, as the spec's `RepositoryMeta` names it beside an issue and
/// beside the base and head of a pull request.
fn meta(id: &u64, name: &str, owner: &str, full_name: &str) -> types::RepositoryMeta {
    types::RepositoryMeta {
        id: Some(*id as i64),
        name: Some(name.to_owned()),
        owner: Some(owner.to_owned()),
        full_name: Some(full_name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn request() -> ApiRequest {
        ApiRequest {
            path: "/repos/alice/tool".to_owned(),
            params: Default::default(),
            query: Vec::new(),
            body: bytes::Bytes::new(),
            caller: None,
            api_url: "http://h/api/v1".to_owned(),
            web_url: "http://h".to_owned(),
        }
    }

    /// A state with one repository, and that repository.
    fn repository() -> (State, Permission) {
        let mut state = State::default();
        state.git_root = Some(std::env::temp_dir().join("ninot-forgejo-render-test"));
        state.add_user("alice");
        state
            .create_repo("alice", "tool", true, Some("A tool".to_owned()), "main")
            .unwrap();
        (state, Permission::ADMIN)
    }

    #[test]
    fn a_repository_names_its_urls_after_the_server() {
        let (state, permission) = repository();
        let owner = state.user("alice").unwrap();
        let repository = owner.repo("tool").unwrap();
        let rendered = repo(&request(), owner, repository, permission, true);
        assert_eq!(rendered.full_name, "alice/tool");
        assert_eq!(rendered.description.as_deref(), Some("A tool"));
        assert!(rendered.empty);
        assert!(rendered.private);
        assert_eq!(rendered.default_branch, "main");
        assert_eq!(rendered.clone_url, "http://h/alice/tool.git");
        assert_eq!(rendered.ssh_url, "git@h:alice/tool.git");
        assert_eq!(rendered.html_url, "http://h/alice/tool");
        assert_eq!(rendered.url, "http://h/api/v1/repos/alice/tool");
        assert_eq!(rendered.owner.login, "alice");
        assert!(rendered.permissions.admin);
        assert!(rendered.permissions.push);
        assert!(rendered.permissions.pull);
        // The keys the fake has nothing to say about stay out of the body.
        let body = serde_json::to_value(&rendered).unwrap();
        assert!(body.get("avatar_url").is_none(), "{body}");
        assert!(body.get("default_merge_style").is_none(), "{body}");
    }

    #[test]
    fn an_ssh_url_names_the_host_with_the_port_it_serves_on() {
        assert_eq!(ssh_host("http://127.0.0.1:3000"), "127.0.0.1:3000");
        assert_eq!(ssh_host("https://forgejo.example"), "forgejo.example");
        // A host of no URL is its own host, as it was before the fake read one.
        assert_eq!(ssh_host("localhost:3000"), "localhost:3000");
    }

    #[test]
    fn a_user_is_reported_as_an_administrator_only_when_one_is() {
        let (mut state, _) = repository();
        state.add_user("bob");
        let request = request();
        let alice = state.user("alice").unwrap();
        let bob = state.user("bob").unwrap();
        assert!(!user(&request, alice).is_admin);
        assert!(!user(&request, bob).is_admin);
        state.add_admin("root");
        let root = state.user("root").unwrap();
        assert!(user(&request, root).is_admin);
    }

    #[test]
    fn the_version_comes_from_the_pinned_spec() {
        assert_eq!(version().version, crate::spec::VERSION);
    }
}
