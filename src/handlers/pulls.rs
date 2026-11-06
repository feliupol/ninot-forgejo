//! Pull requests of a repository.

use std::sync::Mutex;

use serde::Deserialize;

use super::issues::LabelReference;
use super::repos::{Found, find_repo, find_writable_repo};
use super::{Handler, may_change, no_write};
use crate::api::{ApiRequest, ApiResponse, ApiResult, PatchField, paginate};
use crate::merge;
use crate::operation::ops;
use crate::render;
use crate::state::pull::MergeMethod;
use crate::state::{State, Status, lock, now};
use crate::types;
use ninot_common::git;

/// Pull-request creation fields, with semantic validation left to the handler.
#[derive(Deserialize)]
struct CreatePullRequestInput {
    /// Deprecated single-user assignee.
    #[serde(default)]
    assignee: PatchField<String>,
    /// Replacement list of assignees.
    #[serde(default)]
    assignees: PatchField<Vec<String>>,
    /// Base branch.
    base: Option<String>,
    /// Pull-request body.
    body: Option<String>,
    /// Head branch.
    head: Option<String>,
    /// Label ids; a missing or null list means no labels.
    labels: Option<Vec<LabelReference>>,
    /// Pull-request title.
    title: Option<String>,
}

/// Pull-request edit fields, including presence-aware assignee inputs.
#[derive(Deserialize)]
struct EditPullRequestInput {
    /// Deprecated single-user assignee.
    #[serde(default)]
    assignee: PatchField<String>,
    /// Replacement list of assignees.
    #[serde(default)]
    assignees: PatchField<Vec<String>>,
    /// New base branch.
    base: Option<String>,
    /// New pull-request body.
    body: Option<String>,
    /// New open or closed state.
    state: Option<String>,
    /// New pull-request title.
    title: Option<String>,
}

fn pr_index(request: &ApiRequest) -> Result<u64, ApiResponse> {
    request
        .param("index")
        .parse()
        .map_err(|_| ApiResponse::not_found())
}

/// Renders the pull request `index` of `found`, reading where its branches stand and whether
/// they can be merged from the repository every time, as a push to either of them has to show up
/// in the pull request the next time it is asked for.
fn rendered_pr(
    state: &State,
    request: &ApiRequest,
    found: &Found,
    index: u64,
) -> types::PullRequest {
    let owner = state
        .user(&found.owner)
        .expect("a visible repository has an owner");
    let repo = owner
        .repo(&found.name)
        .expect("a visible repository exists");
    let pr = repo.tracker.pull(index).expect("the pr was found");
    let branches = merge::Branches::of(&repo.git_dir, &pr.base, &pr.head);
    render::pull_request(state, request, owner, repo, pr, &branches)
}

impl Handler for ops::RepoListPullRequests {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let repo = state
            .user(&found.owner)
            .and_then(|user| user.repo(&found.name))
            .expect("a visible repository exists");
        let wanted = request.query("state").unwrap_or("open");
        let wanted: Vec<u64> = repo
            .tracker
            .pulls
            .values()
            .filter(|pr| request.query("base").is_none_or(|base| pr.base == base))
            .filter(|pr| request.query("head").is_none_or(|head| pr.head == head))
            .filter(|pr| match wanted {
                "open" => pr.state == Status::Open && !pr.merged,
                "closed" => pr.state == Status::Closed || pr.merged,
                _ => true,
            })
            .rev()
            .map(|pr| pr.index)
            .collect();
        let items: Vec<types::PullRequest> = wanted
            .into_iter()
            .map(|index| rendered_pr(&state, request, &found, index))
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoCreatePullRequest {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let caller = request.require_scope("write:repository")?.login.clone();
        let body: CreatePullRequestInput = request.json_as_with_status(422)?;
        let title = body
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ApiResponse::error(422, "title is required"))?
            .to_owned();
        let head = body
            .head
            .as_deref()
            .ok_or_else(|| ApiResponse::error(422, "head is required"))?
            .to_owned();
        let base = body
            .base
            .as_deref()
            .ok_or_else(|| ApiResponse::error(422, "base is required"))?
            .to_owned();
        if head == base {
            return Err(ApiResponse::error(422, "head and base must differ"));
        }
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let owner = found.owner.clone();
        let name = found.name.clone();
        let git_dir = state
            .user(&owner)
            .and_then(|u| u.repo(&name))
            .map(|r| r.git_dir.clone())
            .expect("repo exists");

        // Check branches exist
        let head_ref = git::resolve_commit(&git_dir, &head)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?;
        let base_ref = git::resolve_commit(&git_dir, &base)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?;
        if head_ref.is_none() || base_ref.is_none() {
            return Err(ApiResponse::error(422, "head or base does not exist"));
        }

        // Check for duplicate open PR with same head/base
        let tracker = state.tracker(&owner, &name);
        for pr in tracker.pulls.values() {
            if pr.head == head && pr.base == base && pr.state == Status::Open && !pr.merged {
                return Err(ApiResponse::error(409, "pull request already exists"));
            }
        }

        // Get labels/assignees
        let labels = super::issues::label_ids(
            state.tracker(&owner, &name),
            body.labels.as_deref().unwrap_or_default(),
        )?;
        let assignees =
            super::issues::assignees(&state, body.assignee.clone(), body.assignees.clone())?
                .unwrap_or_default();
        let body_text = body.body.unwrap_or_default();

        // Create PR - takes next index
        let poster = caller;
        let id = state.next_id();
        let index = state.tracker_mut(&owner, &name).add_pull(
            id,
            crate::state::pull::NewPullRequest {
                title: title.clone(),
                body: body_text.clone(),
                poster: poster.clone(),
                labels,
                assignees,
                base: base.clone(),
                head: head.clone(),
            },
        );

        // Also create an issue with same index and pull_request field
        let issue_id = state.next_id();
        state.tracker_mut(&owner, &name).add_issue_at(
            issue_id,
            Some(index),
            crate::state::NewIssue {
                title: title.clone(),
                body: body_text,
                poster: poster.clone(),
                labels: Vec::new(),
                assignees: Vec::new(),
                closed: false,
            },
        );
        // overwrite issue labels/assignees to match? or just add again - better to edit
        {
            let labels_copy = super::issues::label_ids(
                state.tracker(&owner, &name),
                body.labels.as_deref().unwrap_or_default(),
            )?;
            let ass_copy = super::issues::assignees(&state, body.assignee, body.assignees)?
                .unwrap_or_default();
            if let Some(issue) = state.tracker_mut(&owner, &name).issue_mut(index) {
                issue.labels = labels_copy;
                issue.assignees = ass_copy;
            }
        }
        Ok(ApiResponse::json(
            201,
            rendered_pr(&state, request, &found, index),
        ))
    }
}

impl Handler for ops::RepoGetPullRequest {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let index = pr_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        if state.pull(&found.owner, &found.name, index).is_none() {
            return Err(ApiResponse::not_found());
        }
        Ok(ApiResponse::ok(rendered_pr(&state, request, &found, index)))
    }
}

impl Handler for ops::RepoEditPullRequest {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: EditPullRequestInput = request.json_as_with_status(422)?;
        let index = pr_index(request)?;
        let mut state = lock(state);
        let found = find_repo(&state, request)?;
        let poster = state
            .pull(&found.owner, &found.name, index)
            .ok_or_else(ApiResponse::not_found)?
            .poster
            .clone();
        if !may_change(&found, request, &poster) {
            return Err(no_write());
        }
        let assignees = super::issues::assignees(&state, body.assignee, body.assignees)?;
        let pr = state
            .pull_mut(&found.owner, &found.name, index)
            .expect("pr exists");
        if let Some(title) = body.title.as_deref() {
            pr.title = title.to_owned();
        }
        if let Some(b) = body.body.as_deref() {
            pr.body = b.to_owned();
        }
        if let Some(ass_ref) = assignees.as_ref() {
            pr.assignees = ass_ref.clone();
        }
        if let Some(state_str) = body.state.as_deref() {
            if let Some(st) = Status::parse(state_str) {
                pr.state = st;
                let time = now();
                if st == Status::Closed {
                    pr.closed_at = Some(time);
                }
                pr.updated_at = time;
            }
        } else {
            pr.updated_at = now();
        }
        if let Some(base) = body.base.as_deref() {
            pr.base = base.to_owned();
        }
        // sync to issue
        {
            let ass_copy = assignees.clone();
            if let Some(issue) = state
                .tracker_mut(&found.owner, &found.name)
                .issue_mut(index)
            {
                if let Some(title) = body.title.as_deref() {
                    issue.title = title.to_owned();
                }
                if let Some(b) = body.body.as_deref() {
                    issue.body = b.to_owned();
                }
                if let Some(ass) = ass_copy {
                    issue.assignees = ass;
                }
            }
        }
        Ok(ApiResponse::json(
            201,
            rendered_pr(&state, request, &found, index),
        ))
    }
}

impl Handler for ops::RepoDownloadPullDiffOrPatch {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let index = pr_index(request)?;
        let diff_type = request.param("diffType");
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let pr = state
            .pull(&found.owner, &found.name, index)
            .ok_or_else(ApiResponse::not_found)?
            .clone();
        let git_dir = state
            .user(&found.owner)
            .and_then(|u| u.repo(&found.name))
            .map(|r| r.git_dir.clone())
            .expect("repo exists");
        drop(state);

        let head_commit = git::resolve_commit(&git_dir, &pr.head)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?
            .ok_or_else(|| ApiResponse::error(422, "head does not exist"))?;
        let base_commit = git::resolve_commit(&git_dir, &pr.base)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?
            .ok_or_else(|| ApiResponse::error(422, "base does not exist"))?;

        let output = std::process::Command::new("git")
            .current_dir(&git_dir)
            .args({
                if diff_type == "diff" {
                    vec![
                        "diff".to_string(),
                        "--binary".to_string(),
                        base_commit.clone(),
                        head_commit.clone(),
                    ]
                } else {
                    vec![
                        "format-patch".to_string(),
                        "--stdout".to_string(),
                        "--no-signature".to_string(),
                        format!("{}..{}", base_commit, head_commit),
                    ]
                }
            })
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?;
        if !output.status.success() {
            return Err(ApiResponse::error(
                500,
                &String::from_utf8_lossy(&output.stderr),
            ));
        }
        Ok(ApiResponse {
            status: 200,
            headers: vec![(
                "content-type".to_owned(),
                "text/plain; charset=utf-8".to_owned(),
            )],
            body: crate::api::Body::Bytes(output.stdout),
        })
    }
}

impl Handler for ops::RepoGetPullRequestCommits {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let index = pr_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let pr = state
            .pull(&found.owner, &found.name, index)
            .ok_or_else(ApiResponse::not_found)?
            .clone();
        let git_dir = state
            .user(&found.owner)
            .and_then(|u| u.repo(&found.name))
            .map(|r| r.git_dir.clone())
            .expect("repo exists");
        drop(state);

        let head_commit = git::resolve_commit(&git_dir, &pr.head)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?
            .ok_or_else(|| ApiResponse::error(422, "head does not exist"))?;
        let base_commit = git::resolve_commit(&git_dir, &pr.base)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?
            .ok_or_else(|| ApiResponse::error(422, "base does not exist"))?;

        let commits = git::new_commits(&git_dir, &head_commit, &[&base_commit])
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?;
        let mut items = Vec::new();
        for sha in commits {
            let c =
                git::commit(&git_dir, &sha).map_err(|e| ApiResponse::error(500, &e.to_string()))?;
            items.push(render::commit(&c));
        }
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoGetPullRequestFiles {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let index = pr_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let pr = state
            .pull(&found.owner, &found.name, index)
            .ok_or_else(ApiResponse::not_found)?
            .clone();
        let git_dir = state
            .user(&found.owner)
            .and_then(|u| u.repo(&found.name))
            .map(|r| r.git_dir.clone())
            .expect("repo exists");
        drop(state);

        let head_commit = git::resolve_commit(&git_dir, &pr.head)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?
            .ok_or_else(|| ApiResponse::error(422, "head does not exist"))?;
        let (added, removed, modified) = git::changed_paths(&git_dir, &head_commit)
            .map_err(|e| ApiResponse::error(500, &e.to_string()))?;
        let mut files: Vec<(String, render::ChangedFileStatus)> = added
            .into_iter()
            .map(|path| (path, render::ChangedFileStatus::Added))
            .collect();
        files.extend(
            removed
                .into_iter()
                .map(|path| (path, render::ChangedFileStatus::Removed)),
        );
        files.extend(
            modified
                .into_iter()
                .map(|path| (path, render::ChangedFileStatus::Modified)),
        );
        files.sort_by(|(left, _), (right, _)| left.cmp(right));
        files.dedup_by(|(left, _), (right, _)| left == right);
        let items: Vec<_> = files
            .into_iter()
            .map(|(path, status)| render::changed_file(path, status))
            .collect();
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoPullRequestIsMerged {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let index = pr_index(request)?;
        let state = lock(state);
        let found = find_repo(&state, request)?;
        let pr = state.pull(&found.owner, &found.name, index);
        match pr {
            Some(p) if p.merged => Ok(ApiResponse {
                status: 204,
                headers: Vec::new(),
                body: crate::api::Body::Empty,
            }),
            _ => Err(ApiResponse::not_found()),
        }
    }
}

/// The merge style a request asks for, as the spec's `Do` names it. A request that names none
/// merges the way Forgejo does when nothing is asked for.
#[derive(Deserialize)]
struct MergeRequestInput {
    /// Requested merge method, accepting the historical lower-case spelling.
    #[serde(alias = "do")]
    #[serde(rename = "Do")]
    do_: Option<String>,
    /// Optional commit title.
    #[serde(alias = "merge_title_field")]
    #[serde(rename = "MergeTitleField")]
    merge_title_field: Option<String>,
    /// Optional commit message.
    #[serde(alias = "merge_message_field")]
    #[serde(rename = "MergeMessageField")]
    merge_message_field: Option<String>,
    /// Whether to delete the source branch after merging.
    #[serde(alias = "DeleteBranchAfterMerge")]
    delete_branch_after_merge: Option<bool>,
    /// Whether to queue the merge until required checks succeed.
    merge_when_checks_succeed: Option<bool>,
}

/// The merge style a request asks for. Missing styles retain Forgejo's default merge behavior.
fn merge_style(body: &MergeRequestInput) -> Result<MergeMethod, ApiResponse> {
    let Some(asked) = body.do_.as_deref() else {
        return Ok(MergeMethod::Merge);
    };
    MergeMethod::parse(asked)
        .ok_or_else(|| ApiResponse::error(422, &format!("unknown merge style {asked}")))
}

/// The message of the commit a merge makes: its title, with the message the request brought
/// under it when it brought one.
///
/// A missing title is the pull request's title and index for a squash, and a summary naming
/// the pull request and both branches for every other style.
fn merge_message(
    body: &MergeRequestInput,
    pr: &crate::state::pull::PullRequest,
    style: &MergeMethod,
) -> String {
    let title = body
        .merge_title_field
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match style {
            MergeMethod::Squash => format!("{} (#{})", pr.title, pr.index),
            MergeMethod::Merge
            | MergeMethod::Rebase
            | MergeMethod::RebaseMerge
            | MergeMethod::FastForwardOnly
            | MergeMethod::ManuallyMerged => format!(
                "Merge pull request '{}' (#{}) from {} into {}",
                pr.title, pr.index, pr.head, pr.base
            ),
        });
    match body
        .merge_message_field
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        Some(message) => format!("{title}\n\n{message}"),
        None => title,
    }
}

impl Handler for ops::RepoMergePullRequest {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        let caller = request.require_scope("write:repository")?.login.clone();
        let index = pr_index(request)?;
        let body: MergeRequestInput = request.json_as()?;
        let style = merge_style(&body)?;
        let delete_branch = body.delete_branch_after_merge.unwrap_or(false);

        let (owner, name, git_dir, pr) = {
            let state = lock(state);
            let found = find_writable_repo(&state, request)?;
            let git_dir = state
                .user(&found.owner)
                .and_then(|user| user.repo(&found.name))
                .map(|repo| repo.git_dir.clone())
                .expect("a visible repository exists");
            let pr = state
                .pull(&found.owner, &found.name, index)
                .cloned()
                .ok_or_else(ApiResponse::not_found)?;
            (found.owner, found.name, git_dir, pr)
        };
        if pr.merged {
            return Err(ApiResponse::error(405, "Pull request is merged"));
        }
        if pr.state == Status::Closed || pr.draft {
            return Err(ApiResponse::error(405, "Pull request is closed or a draft"));
        }
        let message = merge_message(&body, &pr, &style);
        if body.merge_when_checks_succeed == Some(true) {
            let mut state = lock(state);
            if pr.scheduled_merge.is_some() {
                return Err(ApiResponse::error(409, "merge already scheduled"));
            }
            let sha = git::resolve_commit(&git_dir, &pr.head)
                .map_err(|error| ApiResponse::error(500, &error.to_string()))?
                .ok_or_else(ApiResponse::not_found)?;
            let repo = state
                .user(&owner)
                .and_then(|user| user.repo(&name))
                .expect("repository exists");
            let mut seen = std::collections::BTreeSet::new();
            let passed = repo
                .statuses
                .get(&sha)
                .into_iter()
                .flatten()
                .rev()
                .filter(|status| seen.insert(status.context.clone()))
                .all(|status| {
                    status
                        .status
                        .as_ref()
                        .is_some_and(|status| matches!(status.as_str(), "success" | "skipped"))
                });
            if !passed {
                let required_contexts = repo
                    .statuses
                    .get(&sha)
                    .into_iter()
                    .flatten()
                    .filter_map(|status| status.context.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                state
                    .pull_mut(&owner, &name, index)
                    .ok_or_else(ApiResponse::not_found)?
                    .scheduled_merge = Some(crate::state::pull::ScheduledMerge {
                    required_contexts,
                    method: style,
                    caller,
                    message,
                    delete_branch,
                });
                state.add_pull_timeline_event(
                    &owner,
                    &name,
                    index,
                    crate::state::pull::TimelineEventKind::ScheduledMerge,
                );
                return Ok(ApiResponse::ok_empty());
            }
        }

        // git runs with the lock released, so that other requests proceed while it works.
        let merge_commit = merge::merge(&git_dir, &pr.base, &pr.head, style, &caller, &message)
            .map_err(merge::Error::into_response)?;
        if delete_branch {
            merge::delete_branch(&git_dir, &pr.head).map_err(merge::Error::into_response)?;
        }

        let time = now();
        let mut state = lock(state);
        if state.pull(&owner, &name, index).is_none() {
            return Err(ApiResponse::not_found());
        }
        if let Some(pr) = state.pull_mut(&owner, &name, index) {
            pr.scheduled_merge = None;
            pr.merged = true;
            pr.state = Status::Closed;
            pr.merged_at = Some(time);
            pr.merged_by = Some(caller);
            pr.merge_commit_sha = Some(merge_commit);
            pr.closed_at = Some(time);
            pr.updated_at = time;
            pr.head_deleted = delete_branch;
        }
        // The issue the pull request was filed as is closed with it.
        if let Some(issue) = state.tracker_mut(&owner, &name).issue_mut(index) {
            issue.state = Status::Closed;
            issue.closed_at = Some(time);
            issue.updated_at = time;
        }

        Ok(ApiResponse::ok_empty())
    }
}

impl Handler for ops::RepoCancelScheduledAutoMerge {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let index = pr_index(request)?;
        let mut state = lock(state);
        let found = find_writable_repo(&state, request)?;
        let pr = state
            .pull_mut(&found.owner, &found.name, index)
            .ok_or_else(ApiResponse::not_found)?;
        if pr.scheduled_merge.take().is_none() {
            return Err(ApiResponse::not_found());
        }
        state.add_pull_timeline_event(
            &found.owner,
            &found.name,
            index,
            crate::state::pull::TimelineEventKind::CancelScheduledMerge,
        );
        Ok(ApiResponse::no_content())
    }
}
