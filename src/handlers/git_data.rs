//! Branches and file contents read from the hosted git repositories.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;

use super::Handler;
use super::repos::find_writable_repo;
use super::repos::{Found, find_repo};
use crate::api::{ApiRequest, ApiResponse, ApiResult, paginate};
use crate::operation::ops;
use crate::parse;
use crate::render;
use crate::state::{State, lock};
use base64::Engine as _;
use ninot_common::git::{self, PathContent};

/// A visible repository and its bare git repository, with the state lock released.
struct Located {
    found: Found,
    git_dir: PathBuf,
    default_branch: String,
}

impl Located {
    fn api(&self, request: &ApiRequest) -> String {
        format!(
            "{}/repos/{}/{}",
            request.api_url, self.found.owner, self.found.name
        )
    }

    fn html(&self, request: &ApiRequest) -> String {
        format!(
            "{}/{}/{}",
            request.web_url, self.found.owner, self.found.name
        )
    }
}

/// Resolves the repository of the request and clones what the handlers need from it, so the
/// state lock is not held while git runs.
fn locate(state: &Mutex<State>, request: &ApiRequest) -> Result<Located, ApiResponse> {
    let state = lock(state);
    let found = find_repo(&state, request)?;
    let repo = state
        .user(&found.owner)
        .and_then(|user| user.repo(&found.name))
        .expect("a visible repository exists");
    Ok(Located {
        git_dir: repo.git_dir.clone(),
        default_branch: repo.default_branch.clone(),
        found,
    })
}

fn internal(error: std::io::Error) -> ApiResponse {
    ApiResponse::error(500, &error.to_string())
}

/// Branch creation fields, optional here to retain the handler's 422 for missing names.
#[derive(Deserialize)]
struct CreateBranchInput {
    /// New branch name.
    new_branch_name: Option<String>,
    /// Existing branch from which to create the new branch.
    old_branch_name: Option<String>,
    /// Existing branch, tag, or commit from which to create the new branch.
    old_ref_name: Option<String>,
}

/// File creation fields, optional here to retain the handler's 422 for missing content.
#[derive(Deserialize)]
struct CreateFileInput {
    /// Base64-encoded file contents.
    content: Option<String>,
    /// Branch to update; omitted means the repository default branch.
    branch: Option<String>,
    /// New branch to create from `branch`; omitted means update `branch` directly.
    new_branch: Option<String>,
    /// Commit message.
    message: Option<String>,
}

impl Handler for ops::RepoListBranches {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let located = locate(state, request)?;
        let api = located.api(request);
        let mut items = Vec::new();
        for found in git::refs(&located.git_dir, "refs/heads/").map_err(internal)? {
            let commit = git::commit(&located.git_dir, &found.sha).map_err(internal)?;
            let name = parse::branch_of(&found.name).unwrap_or(&found.name);
            items.push(render::branch(&api, name, &commit));
        }
        Ok(paginate(request, items))
    }
}

impl Handler for ops::RepoGetBranch {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let located = locate(state, request)?;
        let branch = request.param("branch");
        let sha = git::resolve_commit(&located.git_dir, &format!("refs/heads/{branch}"))
            .map_err(internal)?
            .ok_or_else(ApiResponse::not_found)?;
        let commit = git::commit(&located.git_dir, &sha).map_err(internal)?;
        Ok(ApiResponse::ok(render::branch(
            &located.api(request),
            branch,
            &commit,
        )))
    }
}

impl Handler for ops::RepoCreateBranch {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: CreateBranchInput = request.json_as_with_status(422)?;
        let name = body.new_branch_name.as_deref().unwrap_or_default();
        if name.is_empty() {
            return Err(ApiResponse::error(422, "new_branch_name is required"));
        }
        {
            let state = lock(state);
            find_writable_repo(&state, request)?;
        }
        let located = locate(state, request)?;
        let reference = format!("refs/heads/{name}");
        if git::resolve_commit(&located.git_dir, &reference)
            .map_err(internal)?
            .is_some()
        {
            return Err(ApiResponse::error(409, "branch already exists"));
        }
        let source = body
            .old_branch_name
            .as_deref()
            .or(body.old_ref_name.as_deref())
            .unwrap_or(&located.default_branch);
        let sha = git::resolve_commit(&located.git_dir, &format!("refs/heads/{source}"))
            .map_err(internal)?
            .ok_or_else(ApiResponse::not_found)?;
        git::create_ref(&located.git_dir, &reference, &sha, &super::BOT)
            .map_err(|error| ApiResponse::error(422, &error.to_string()))?;
        let commit = git::commit(&located.git_dir, &sha).map_err(internal)?;
        Ok(ApiResponse::json(
            201,
            render::branch(&located.api(request), name, &commit),
        ))
    }
}

impl Handler for ops::RepoCreateFile {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("write:repository")?;
        let body: CreateFileInput = request.json_as_with_status(422)?;
        let encoded = body
            .content
            .as_deref()
            .ok_or_else(|| ApiResponse::error(422, "content is required"))?;
        let content = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ApiResponse::error(422, "content must be base64"))?;
        {
            let state = lock(state);
            find_writable_repo(&state, request)?;
        }
        let located = locate(state, request)?;
        let base = body.branch.as_deref().unwrap_or(&located.default_branch);
        let branch = body.new_branch.as_deref().unwrap_or(base);
        if body.new_branch.as_deref().is_some_and(str::is_empty) {
            return Err(ApiResponse::error(422, "new_branch must not be empty"));
        }
        let empty = git::is_empty(&located.git_dir).map_err(internal)?;
        let branch_ref = format!("refs/heads/{branch}");
        let existing_branch =
            git::resolve_commit(&located.git_dir, &branch_ref).map_err(internal)?;
        if body.new_branch.is_some() && existing_branch.is_some() {
            return Err(ApiResponse::error(409, "branch already exists"));
        }
        let base_sha = if empty {
            None
        } else {
            Some(
                git::resolve_commit(&located.git_dir, &format!("refs/heads/{base}"))
                    .map_err(internal)?
                    .ok_or_else(ApiResponse::not_found)?,
            )
        };
        let sha = existing_branch.as_ref().or(base_sha.as_ref());
        let path = request.param("filepath");
        if let Some(sha) = &sha
            && git::path_content(&located.git_dir, sha, path)
                .map_err(internal)?
                .is_some()
        {
            return Err(ApiResponse::error(422, "repository file already exists"));
        }
        let message = body.message.as_deref().unwrap_or("Create file");
        let sha = crate::merge::commit_file(
            &located.git_dir,
            branch,
            base_sha.as_deref().unwrap_or_default(),
            std::path::Path::new(path),
            &content,
            message,
        )
        .map_err(crate::merge::Error::into_response)?;
        if empty {
            let mut state = lock(state);
            state
                .user_mut(&located.found.owner)
                .and_then(|user| user.repo_mut(&located.found.name))
                .expect("the located repository remains present")
                .set_default_branch(branch)
                .map_err(internal)?;
        }
        let commit = git::commit(&located.git_dir, &sha).map_err(internal)?;
        let Some(PathContent::File(entry)) =
            git::path_content(&located.git_dir, &sha, path).map_err(internal)?
        else {
            return Err(ApiResponse::error(500, "committed file is missing"));
        };
        Ok(ApiResponse::json(
            201,
            crate::types::FileResponse {
                content: Some(render::file(
                    &located.api(request),
                    &located.html(request),
                    branch,
                    &entry,
                    &content,
                )),
                commit: Some(crate::types::FileCommitResponse {
                    sha: Some(sha),
                    message: Some(commit.message),
                    ..Default::default()
                }),
                verification: None,
            },
        ))
    }
}

impl Handler for ops::RepoCompareDiff {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let located = locate(state, request)?;
        let (base, head) = request
            .param("basehead")
            .split_once("...")
            .ok_or_else(|| ApiResponse::error(422, "expected base...head"))?;
        let resolve = |reference: &str| {
            git::resolve_commit(&located.git_dir, reference)
                .map_err(internal)?
                .ok_or_else(ApiResponse::not_found)
        };
        let (base, head) = (resolve(base)?, resolve(head)?);
        let shas = git::new_commits(&located.git_dir, &head, &[&base]).map_err(internal)?;
        let commits = shas
            .iter()
            .map(|sha| git::commit(&located.git_dir, sha).map(|commit| render::commit(&commit)))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(internal)?;
        let files =
            crate::merge::comparison_files(&located.git_dir, &base, &head).map_err(internal)?;
        Ok(ApiResponse::ok(crate::types::Compare {
            total_commits: Some(commits.len() as i64),
            commits,
            files,
        }))
    }
}

impl Handler for ops::RepoGetContents {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let located = locate(state, request)?;
        let reference = request
            .query("ref")
            .unwrap_or(&located.default_branch)
            .to_owned();
        let sha = git::resolve_commit(&located.git_dir, &reference)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiResponse::error(404, &format!("commit does not exist [ref: {reference}]"))
            })?;
        let content = git::path_content(&located.git_dir, &sha, request.param("filepath"))
            .map_err(internal)?
            .ok_or_else(ApiResponse::not_found)?;
        let (api, html) = (located.api(request), located.html(request));
        Ok(match content {
            PathContent::Directory(entries) => {
                ApiResponse::ok(render::directory(&api, &html, &reference, &entries))
            }
            PathContent::File(entry) => {
                let bytes = git::blob(&located.git_dir, &entry.sha).map_err(internal)?;
                ApiResponse::ok(render::file(&api, &html, &reference, &entry, &bytes))
            }
        })
    }
}

impl Handler for ops::RepoGetRawFile {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult {
        request.require_scope("read:repository")?;
        let located = locate(state, request)?;
        let reference = request
            .query("ref")
            .unwrap_or(&located.default_branch)
            .to_owned();
        let sha = git::resolve_commit(&located.git_dir, &reference)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiResponse::error(404, &format!("commit does not exist [ref: {reference}]"))
            })?;
        let content = git::path_content(&located.git_dir, &sha, request.param("filepath"))
            .map_err(internal)?
            .ok_or_else(ApiResponse::not_found)?;
        let PathContent::File(entry) = content else {
            return Err(ApiResponse::not_found());
        };
        let bytes = git::blob(&located.git_dir, &entry.sha).map_err(internal)?;
        Ok(ApiResponse::bytes(200, "application/octet-stream", bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ninot_common::git::TreeEntry;
    use serde_json::json;

    #[test]
    fn a_file_is_base64_encoded_with_its_urls() {
        let entry = TreeEntry {
            kind: "blob".to_owned(),
            mode: "100644".to_owned(),
            sha: "0".repeat(40),
            size: 4,
            path: "README.md".to_owned(),
        };
        let rendered = render::file(
            "http://h/api/v1/repos/a/b",
            "http://h/a/b",
            "main",
            &entry,
            b"# b\n",
        );
        assert_eq!(rendered.name, "README.md");
        assert_eq!(rendered.path, "README.md");
        assert_eq!(rendered.type_, "file");
        assert_eq!(rendered.size, 4);
        assert_eq!(rendered.encoding.as_deref(), Some("base64"));
        assert_eq!(rendered.content.as_deref(), Some("IyBiCg=="));
        assert_eq!(
            rendered.download_url.as_deref(),
            Some("http://h/a/b/raw/branch/main/README.md")
        );
        assert_eq!(
            rendered.url,
            "http://h/api/v1/repos/a/b/contents/README.md?ref=main"
        );
        assert_eq!(
            json!(rendered),
            json!({
                "name": "README.md",
                "path": "README.md",
                "sha": "0000000000000000000000000000000000000000",
                "last_commit_sha": "0000000000000000000000000000000000000000",
                "type": "file",
                "size": 4,
                "encoding": "base64",
                "content": "IyBiCg==",
                "target": null,
                "submodule_git_url": null,
                "url": "http://h/api/v1/repos/a/b/contents/README.md?ref=main",
                "html_url": "http://h/a/b/src/branch/main/README.md",
                "git_url": "http://h/api/v1/repos/a/b/git/blobs/0000000000000000000000000000000000000000",
                "download_url": "http://h/a/b/raw/branch/main/README.md",
                "_links": {
                    "self": "http://h/api/v1/repos/a/b/contents/README.md?ref=main",
                    "html": "http://h/a/b/src/branch/main/README.md",
                    "git": "http://h/api/v1/repos/a/b/git/blobs/0000000000000000000000000000000000000000",
                    "download": "http://h/a/b/raw/branch/main/README.md",
                },
            })
        );
    }

    #[test]
    fn a_directory_lists_its_entries() {
        let entries = vec![
            TreeEntry {
                kind: "tree".to_owned(),
                mode: "040000".to_owned(),
                sha: "1".repeat(40),
                size: 0,
                path: "dir".to_owned(),
            },
            TreeEntry {
                kind: "blob".to_owned(),
                mode: "100644".to_owned(),
                sha: "2".repeat(40),
                size: 3,
                path: "one.txt".to_owned(),
            },
        ];
        let rendered = render::directory(
            "http://h/api/v1/repos/a/b",
            "http://h/a/b",
            "main",
            &entries,
        );
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].name, "dir");
        assert_eq!(rendered[0].type_, "dir");
        assert_eq!(rendered[0].download_url, None);
        assert_eq!(rendered[1].name, "one.txt");
        assert_eq!(rendered[1].type_, "file");
        assert_eq!(rendered[1].size, 3);
        // A directory is no file, so what it has to download is nothing.
        assert_eq!(
            json!(rendered[0]),
            json!({
                "name": "dir",
                "path": "dir",
                "sha": "1111111111111111111111111111111111111111",
                "last_commit_sha": "1111111111111111111111111111111111111111",
                "type": "dir",
                "size": 0,
                "content": null,
                "encoding": null,
                "target": null,
                "submodule_git_url": null,
                "download_url": null,
                "is_submodule": false,
                "url": "http://h/api/v1/repos/a/b/contents/dir?ref=main",
                "html_url": "http://h/a/b/src/branch/main/dir",
                "git_url": "http://h/api/v1/repos/a/b/git/trees/1111111111111111111111111111111111111111",
                "_links": {
                    "self": "http://h/api/v1/repos/a/b/contents/dir?ref=main",
                    "html": "http://h/a/b/src/branch/main/dir",
                    "git": "http://h/api/v1/repos/a/b/git/trees/1111111111111111111111111111111111111111",
                },
            })
        );
    }
}
