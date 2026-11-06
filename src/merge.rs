//! Bringing the head of a pull request into its base branch with git.
//!
//! Everything here runs against the bare repository a repository is stored in. A merge itself
//! needs a working tree, so one is checked out into a scratch directory that shares the bare
//! repository's objects and is thrown away afterwards; the branch a pull request is merged
//! into moves only once git has made the commit, and only if it still points where it did.
//!
//! Whether a pull request can be merged is answered by looking at the repository and never from
//! what a request recorded: a branch may have been pushed to since, which is what turns a pull
//! request that was mergeable a moment ago into one that conflicts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::ApiResponse;
use crate::state::pull::MergeMethod;

/// Files changed by the head since its common ancestor with the base.
///
/// # Errors
/// Returns Git failures, including histories without a common ancestor.
pub(crate) fn comparison_files(
    git_dir: &Path,
    base: &str,
    head: &str,
) -> io::Result<Vec<crate::types::CommitAffectedFiles>> {
    let range = format!("{base}...{head}");
    let result = checked(in_repo(
        git_dir,
        &["diff", "--no-renames", "--name-status", "-z", &range, "--"],
    ))?;
    let mut fields = result
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let path = fields
            .next()
            .ok_or_else(|| io::Error::other("git diff omitted a path"))?;
        let status = match status {
            b"A" => "added",
            b"D" => "removed",
            _ => "modified",
        };
        files.push(crate::types::CommitAffectedFiles {
            filename: Some(String::from_utf8(path.to_vec()).map_err(io::Error::other)?),
            status: Some(status.to_owned()),
        });
    }
    Ok(files)
}

/// Commits fixture content and atomically advances the requested branch.
///
/// # Errors
/// Returns invalid paths or Git failures without advancing the branch.
pub(crate) fn commit_file(
    git_dir: &Path,
    branch: &str,
    base: &str,
    path: &Path,
    content: &[u8],
    message: &str,
) -> Result<String, Error> {
    commit_file_with_executable_mode(git_dir, branch, base, path, content, message, None)
}

/// Commits fixture content while optionally preserving or explicitly setting its executable bit.
///
/// # Errors
/// Returns invalid paths or Git failures without advancing the branch.
pub(crate) fn commit_file_with_executable_mode(
    git_dir: &Path,
    branch: &str,
    base: &str,
    path: &Path,
    content: &[u8],
    message: &str,
    executable: Option<bool>,
) -> Result<String, Error> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || path
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err(Error::Git(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture path must stay inside the repository",
        )));
    }
    checked(in_repo(
        git_dir,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    ))
    .map_err(Error::Git)?;
    if ninot_common::git::is_empty(git_dir).map_err(Error::Git)? {
        return initial_file_commit(git_dir, branch, path, content, message, executable);
    }
    let previous = commit(git_dir, branch);
    let start = previous.as_deref().unwrap_or(base);
    let tree = WorkTree::add(git_dir, start, "admin")?;
    let target = tree.dir.join(path);
    // Reject symlinks in every existing path component before writing fixture bytes.
    let mut parent = tree.dir.clone();
    for part in path.components() {
        parent.push(part);
        if fs::symlink_metadata(&parent).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::Git(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixture path contains a symlink",
            )));
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(Error::Git)?;
    }
    fs::write(&target, content).map_err(Error::Git)?;
    let path = path
        .to_str()
        .ok_or_else(|| Error::Git(io::Error::other("fixture path is not UTF-8")))?;
    match executable {
        Some(true) => tree.merge(&["add", "--chmod=+x", "--", path])?,
        Some(false) => tree.merge(&["add", "--chmod=-x", "--", path])?,
        None => tree.merge(&["add", "--", path])?,
    };
    tree.merge(&["commit", "--allow-empty", "-m", message])?;
    let sha = tree.head()?;
    move_ref(
        git_dir,
        branch,
        &sha,
        previous
            .as_deref()
            .unwrap_or("0000000000000000000000000000000000000000"),
    )?;
    Ok(sha)
}

/// Makes the first commit in an empty bare repository without seeding an extra commit.
///
/// The commit is created in an isolated temporary repository, imported into the bare object
/// store, then published with the same compare-and-swap used for later commits.
fn initial_file_commit(
    git_dir: &Path,
    branch: &str,
    path: &Path,
    content: &[u8],
    message: &str,
    executable: Option<bool>,
) -> Result<String, Error> {
    let temp = TemporaryDirectory::new("initial")?;
    checked(in_tree(&temp.0, &["init", "--quiet"])).map_err(Error::Git)?;

    let target = temp.0.join(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(Error::Git)?;
    }
    fs::write(&target, content).map_err(Error::Git)?;
    let relative_path = path
        .to_str()
        .ok_or_else(|| Error::Git(io::Error::other("fixture path is not UTF-8")))?;
    let (name, email) = identity("admin");
    let add_args = match executable {
        Some(true) => vec!["add", "--chmod=+x", "--", relative_path],
        Some(false) => vec!["add", "--chmod=-x", "--", relative_path],
        None => vec!["add", "--", relative_path],
    };
    for args in [add_args, vec!["commit", "--allow-empty", "-m", message]] {
        let mut command = in_tree(&temp.0, &args);
        command
            .env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_COMMITTER_EMAIL", &email);
        checked(command).map_err(Error::Git)?;
    }

    let sha = checked(in_tree(&temp.0, &["rev-parse", "HEAD"]))
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .map_err(Error::Git)?;
    let source = temp
        .0
        .to_str()
        .ok_or_else(|| Error::Git(io::Error::other("temporary repository path is not UTF-8")))?;
    checked(in_repo(
        git_dir,
        &["fetch", "--no-write-fetch-head", "--no-tags", source, &sha],
    ))
    .map_err(Error::Git)?;
    move_ref(
        git_dir,
        branch,
        &sha,
        "0000000000000000000000000000000000000000",
    )?;
    Ok(sha)
}

/// A scratch directory removed even when creating or publishing the commit fails.
struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(kind: &str) -> Result<Self, Error> {
        let path = scratch(kind);
        fs::create_dir_all(&path).map_err(Error::Git)?;
        Ok(Self(path))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The branches of a pull request as the repository holds them at this moment.
pub struct Branches {
    /// The commit the base points at, empty when the branch does not exist.
    pub base: String,
    /// The commit the head points at, empty when the branch does not exist.
    pub head: String,
    /// Whether the head can be brought into the base.
    pub mergeable: bool,
}

impl Branches {
    /// Reads both branches of a pull request and tells whether they can be merged, which is
    /// what every rendering of a pull request is built on.
    pub fn of(git_dir: &Path, base: &str, head: &str) -> Self {
        let base = commit(git_dir, base).unwrap_or_default();
        let head = commit(git_dir, head).unwrap_or_default();
        let mergeable = mergeable(git_dir, &base, &head);
        Self {
            base,
            head,
            mergeable,
        }
    }
}

/// Why a merge did not happen. A conflict is what the caller may be told about; a failure of git
/// is what only the fake can answer, so the two are kept apart for [`Error::into_response`].
#[derive(Debug)]
pub enum Error {
    /// The branches cannot be brought together, either because they conflict or because the
    /// merge style does not suit them.
    Conflict(String),
    /// git could not be run at all.
    Git(io::Error),
}

impl Error {
    /// What a request that could not merge is answered with: a conflict is the caller's doing,
    /// a failure of git is the fake's.
    pub fn into_response(self) -> ApiResponse {
        match self {
            Self::Conflict(message) => ApiResponse::error(405, &message),
            Self::Git(error) => ApiResponse::error(500, &error.to_string()),
        }
    }
}

/// Merges `head` into `base` as `method` asks, on behalf of `by`, with `message` as the message
/// of the commit the style makes, and answers the commit the pull request records as its merge.
///
/// That commit is the one the base branch points at afterwards, except for a pull request merged
/// by hand, which moves nothing and answers nothing. The working tree is gone by the time this
/// returns, whether the merge happened or not, and the base branch moves only after git has made
/// the commit and only if nothing else moved it in the meantime.
pub fn merge(
    git_dir: &Path,
    base: &str,
    head: &str,
    method: MergeMethod,
    by: &str,
    message: &str,
) -> Result<String, Error> {
    let base_sha = branch(git_dir, base)?;
    let head_sha = branch(git_dir, head)?;
    // A pull request merged by hand changed nothing the fake can do to the repository: whoever
    // merged it says so, and the base branch is left as it is.
    if method == MergeMethod::ManuallyMerged {
        return Ok(String::new());
    }
    let tree = WorkTree::add(git_dir, &base_sha, by)?;
    let merged = match method {
        MergeMethod::Merge => {
            tree.merge(&["merge", "--no-ff", "--no-edit", "-m", message, &head_sha])?;
            tree.head()?
        }
        MergeMethod::Squash => {
            tree.merge(&["merge", "--squash", &head_sha])?;
            tree.merge(&["commit", "--allow-empty", "-m", message])?;
            tree.head()?
        }
        MergeMethod::FastForwardOnly => {
            tree.merge(&["merge", "--ff-only", &head_sha])?;
            tree.head()?
        }
        MergeMethod::Rebase => tree.rebase(&base_sha, &head_sha)?,
        MergeMethod::RebaseMerge => {
            let rebased = tree.rebase(&base_sha, &head_sha)?;
            // The replayed head is merged into the base it was replayed on: that is what tells
            // this style from a rebase, the commits are new and a merge commit says so.
            tree.merge(&["checkout", "--detach", &base_sha])?;
            tree.merge(&["merge", "--no-ff", "--no-edit", "-m", message, &rebased])?;
            tree.head()?
        }
        MergeMethod::ManuallyMerged => unreachable!("a hand-merged pull request returns above"),
    };
    // The commit the merge made is reachable from the base branch once the base points at it.
    move_ref(git_dir, base, &merged, &base_sha)?;
    drop(tree);
    Ok(merged)
}

/// Deletes the branch `name` of the bare repository, as a pull request merged with
/// `delete_branch_after_merge` asks for.
pub fn delete_branch(git_dir: &Path, name: &str) -> Result<(), Error> {
    let reference = format!("refs/heads/{name}");
    checked(in_repo(git_dir, &["update-ref", "-d", &reference]))
        .map(drop)
        .map_err(Error::Git)
}

/// The commit the branch `name` points at, or a conflict, as a branch the fake cannot find.
fn branch(git_dir: &Path, name: &str) -> Result<String, Error> {
    commit(git_dir, name).ok_or_else(|| Error::Conflict(format!("branch {name} does not exist")))
}

/// The commit `reference` points at in the bare repository, `None` when it points at nothing.
///
/// A reference beginning with a dash is refused rather than handed to git, which would read it
/// as an option.
fn commit(git_dir: &Path, reference: &str) -> Option<String> {
    if reference.starts_with('-') {
        return None;
    }
    let output = in_repo(
        git_dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .output()
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|sha| !sha.is_empty())
}

/// Whether `head` can be brought into `base`: already there, ahead of it to be fast-forwarded,
/// or apart from it but never having changed the same thing differently.
fn mergeable(git_dir: &Path, base: &str, head: &str) -> bool {
    if base.is_empty() || head.is_empty() {
        return false;
    }
    if succeeds(in_repo(
        git_dir,
        &["merge-base", "--is-ancestor", head, base],
    )) {
        // There is nothing left of the head to bring in.
        return true;
    }
    let upstream = match output(in_repo(git_dir, &["merge-base", base, head])) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => return false,
    };
    unmerged(git_dir, &upstream, base, head).is_empty()
}

/// The paths a merge of `head` into `base` cannot decide, which `git read-tree` leaves behind as
/// unmerged entries in an index written to a scratch file.
///
/// This is the merge `git merge` would perform, told apart with the same three trees, without
/// checking anything out. A repository that cannot even be read that way is reported as
/// unmergeable, which is what the caller can do about it either way.
fn unmerged(git_dir: &Path, upstream: &str, base: &str, head: &str) -> Vec<String> {
    let index = scratch("index");
    let with_index = |args: &[&str]| {
        let mut command = in_repo(git_dir, args);
        command.env("GIT_INDEX_FILE", &index);
        command
    };
    let undecidable = || vec![format!("{head} cannot be merged into {base}")];
    let paths = if !output(with_index(&[
        "read-tree",
        "-m",
        "-i",
        "--aggressive",
        upstream,
        base,
        head,
    ]))
    .is_ok_and(|output| output.status.success())
    {
        undecidable()
    } else {
        match output(with_index(&["ls-files", "-u"])) {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect(),
            _ => undecidable(),
        }
    };
    // The index was a place to read the merge from and is of no use to anyone now.
    fs::remove_file(index).ok();
    paths
}

/// A working tree of the bare repository `git_dir`, sharing its objects and nothing else, whose
/// `HEAD` is detached so that a merge in it moves no branch on its own.
struct WorkTree {
    git_dir: PathBuf,
    dir: PathBuf,
    /// The login of the user the commits made here belong to.
    by: String,
}

impl WorkTree {
    /// Checks `commit` out into a scratch directory.
    fn add(git_dir: &Path, commit: &str, by: &str) -> Result<Self, Error> {
        let tree = Self {
            git_dir: git_dir.to_owned(),
            dir: scratch("merge"),
            by: by.to_owned(),
        };
        let path = tree.dir.to_str().ok_or_else(|| {
            Error::Git(io::Error::other(format!(
                "{} is no path",
                tree.dir.display()
            )))
        })?;
        checked(in_repo(
            git_dir,
            &["worktree", "add", "--detach", path, commit],
        ))
        .map_err(Error::Git)?;
        Ok(tree)
    }

    /// Runs git in the working tree as the user merging, and answers its output.
    ///
    /// A merge that does not come out is a conflict, which is what git says on its error output
    /// and the only thing the caller can be told about it.
    fn merge(&self, args: &[&str]) -> Result<String, Error> {
        let (name, email) = identity(&self.by);
        let mut command = in_tree(&self.dir, args);
        command
            .env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_COMMITTER_EMAIL", &email);
        match output(command) {
            Ok(output) if output.status.success() => {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            }
            Ok(output) => Err(Error::Conflict(conflict(&output))),
            Err(error) => Err(Error::Git(error)),
        }
    }

    /// The commit the working tree has checked out.
    fn head(&self) -> Result<String, Error> {
        self.merge(&["rev-parse", "HEAD"])
    }

    /// Replays the commits of `head` that `base` does not have onto `base`, and answers the
    /// commit they end at. They are named by sha so that git leaves `HEAD` detached instead of
    /// checking out the branch they belong to, which must not move.
    fn rebase(&self, base: &str, head: &str) -> Result<String, Error> {
        let upstream = self.merge(&["merge-base", base, head])?;
        self.merge(&["rebase", "--onto", base, &upstream, head])?;
        self.head()
    }
}

impl Drop for WorkTree {
    fn drop(&mut self) {
        if let Some(path) = self.dir.to_str() {
            // A merge that stopped halfway leaves the working tree busy, which only a forced
            // removal takes away.
            let _ = checked(in_repo(
                &self.git_dir,
                &["worktree", "remove", "--force", path],
            ));
        }
        let _ = checked(in_repo(&self.git_dir, &["worktree", "prune"]));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Points the branch `name` at `sha`, but only while it still points at `expected`. The
/// reference is spelled out in full so that no branch name can be read as an option.
fn move_ref(git_dir: &Path, name: &str, sha: &str, expected: &str) -> Result<(), Error> {
    let reference = format!("refs/heads/{name}");
    checked(in_repo(git_dir, &["update-ref", &reference, sha, expected]))
        .map(drop)
        .map_err(|_| Error::Conflict(format!("branch {name} moved while merging into it")))
}

/// The name and email a merge is attributed to.
fn identity(login: &str) -> (String, String) {
    (login.to_owned(), format!("{login}@localhost"))
}

/// What git said when it refused, in one line.
fn conflict(output: &Output) -> String {
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.lines().next().unwrap_or_default().trim();
    if said.is_empty() {
        "the branches cannot be merged".to_owned()
    } else {
        said.to_owned()
    }
}

/// A path no other call can be using, as a fake serves tests that merge at the same time.
fn scratch(kind: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "ninot-forgejo-{kind}-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    ))
}

/// A git command in the bare repository `git_dir`, isolated from the user's and system's
/// configuration.
fn in_repo(git_dir: &Path, args: &[&str]) -> Command {
    let mut command = in_tree(git_dir, args);
    command.env("GIT_DIR", git_dir);
    command
}

/// A git command in `dir`, isolated from the user's and system's configuration.
fn in_tree(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

/// Runs `command` and answers its output whatever it is.
fn output(mut command: Command) -> io::Result<Output> {
    command.output()
}

/// Whether `command` runs and succeeds.
fn succeeds(command: Command) -> bool {
    output(command).is_ok_and(|output| output.status.success())
}

/// Runs `command`, failing with its error output when it does not succeed.
fn checked(mut command: Command) -> io::Result<Output> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(io::Error::other(conflict(&output)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ninot_common::git::{self, PathContent};

    /// Runs git in `dir` as `test`, failing when git does.
    fn git_in(dir: &Path, args: &[&str]) {
        let (name, email) = identity("test");
        let output = in_tree(dir, args)
            .env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_COMMITTER_EMAIL", &email)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A bare repository with one commit on `main` and one more on `feature`, which changes the
    /// line of `a.txt` that `main` has and adds `b.txt`. The scratch working tree it was built
    /// with is returned with it; both are left for the caller to remove.
    fn repository() -> (PathBuf, PathBuf) {
        let dir = scratch("test");
        git::init_bare(&dir, "main").unwrap();
        let work = scratch("test");
        git_in(
            &dir,
            &["clone", "-q", dir.to_str().unwrap(), work.to_str().unwrap()],
        );
        std::fs::write(work.join("a.txt"), "one").unwrap();
        git_in(&work, &["add", "a.txt"]);
        git_in(&work, &["commit", "-q", "-m", "Add a.txt"]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        git_in(&work, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(work.join("a.txt"), "two").unwrap();
        std::fs::write(work.join("b.txt"), "b").unwrap();
        git_in(&work, &["add", "a.txt", "b.txt"]);
        git_in(&work, &["commit", "-q", "-m", "Change a and add b"]);
        git_in(&work, &["push", "-q", "origin", "feature"]);
        (dir, work)
    }

    fn remove(dir: PathBuf, work: PathBuf) {
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn a_pull_request_reads_the_branches_the_repository_has() {
        let (dir, work) = repository();
        let branches = Branches::of(&dir, "main", "feature");
        assert_eq!(branches.base, commit(&dir, "main").unwrap());
        assert_eq!(branches.head, commit(&dir, "feature").unwrap());
        assert!(branches.mergeable);
        let missing = Branches::of(&dir, "main", "nowhere");
        assert_eq!(missing.head, "");
        assert!(!missing.mergeable);
        remove(dir, work);
    }

    #[test]
    fn every_merge_style_brings_the_head_into_the_base_its_own_way() {
        for (method, parents) in [
            (MergeMethod::Merge, 2),
            (MergeMethod::Squash, 1),
            (MergeMethod::Rebase, 1),
            (MergeMethod::RebaseMerge, 2),
            (MergeMethod::FastForwardOnly, 1),
        ] {
            let (dir, work) = repository();
            let feature = commit(&dir, "feature").unwrap();
            let merged = merge(&dir, "main", "feature", method.clone(), "test", "Merged").unwrap();
            assert_eq!(commit(&dir, "main").unwrap(), merged, "{method:?}");
            assert_eq!(
                git::commit(&dir, &merged).unwrap().parents.len(),
                parents,
                "{method:?} makes a commit with {parents} parents"
            );
            assert!(
                git::path_content(&dir, &merged, "b.txt").unwrap().is_some(),
                "{method:?} brings what the head added"
            );
            let a = match git::path_content(&dir, &merged, "a.txt").unwrap().unwrap() {
                PathContent::File(entry) => git::blob(&dir, &entry.sha).unwrap(),
                PathContent::Directory(_) => panic!("a.txt is a file"),
            };
            assert_eq!(a, b"two", "{method:?} brings what the head changed");
            assert_eq!(
                commit(&dir, "feature").unwrap(),
                feature,
                "{method:?} leaves the head branch where it was"
            );
            remove(dir, work);
        }
    }

    #[test]
    fn a_hand_merged_pull_request_moves_nothing() {
        let (dir, work) = repository();
        let main = commit(&dir, "main").unwrap();
        let merged = merge(
            &dir,
            "main",
            "feature",
            MergeMethod::ManuallyMerged,
            "test",
            "Merged",
        )
        .unwrap();
        assert_eq!(merged, "");
        assert_eq!(commit(&dir, "main").unwrap(), main);
        remove(dir, work);
    }

    #[test]
    fn a_conflicting_merge_moves_nothing_and_leaves_no_working_tree() {
        let (dir, work) = repository();
        // main changes the line of a.txt the head changed, so the two conflict.
        git_in(&work, &["checkout", "-q", "main"]);
        std::fs::write(work.join("a.txt"), "three").unwrap();
        git_in(&work, &["commit", "-q", "-a", "-m", "Change a"]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        let main = commit(&dir, "main").unwrap();
        assert!(!Branches::of(&dir, "main", "feature").mergeable);

        let error = merge(
            &dir,
            "main",
            "feature",
            MergeMethod::Merge,
            "test",
            "Merged",
        )
        .unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error:?}");
        assert_eq!(commit(&dir, "main").unwrap(), main);
        let trees = git::refs(&dir, "refs/worktree").unwrap_or_default();
        assert_eq!(trees.len(), 0, "the working tree of the merge is gone");

        // What the branch holds of the head is still there to be merged another way.
        assert!(delete_branch(&dir, "feature").is_ok());
        assert_eq!(commit(&dir, "feature"), None);
        remove(dir, work);
    }
}
