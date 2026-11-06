//! Operation handlers and their registry.

use std::sync::Mutex;

use crate::api::{ApiRequest, ApiResponse, ApiResult};
use crate::operation::{Operation, OperationInfo, ops};
use crate::state::{State, author_or_writer};
use ninot_common::git;

/// Author and committer of the commits the fake makes itself, such as a new repository's
/// README.
pub(crate) const BOT: git::Bot = git::Bot {
    name: "ninot-forgejo",
    email: "ninot-forgejo@localhost",
};

mod actions;
mod comments;
mod git_data;
mod hooks;
mod issues;
mod labels;
mod meta;
mod pulls;
mod repos;
mod search;
mod secrets;
mod statuses;
mod users;
mod workflows;

/// Whether the caller may change what `author` wrote: the author itself, or whoever may write
/// to the repository, as Forgejo lets a repository's writer edit the issues of others.
pub fn may_change(found: &repos::Found, request: &ApiRequest, author: &str) -> bool {
    author_or_writer(request.caller.as_ref(), author, found.permission.push)
}

/// The answer Forgejo gives a caller that may not write to a repository.
pub fn no_write() -> ApiResponse {
    ApiResponse::error(403, "Must have write access to repository.")
}

/// Implemented by the generated type of every operation the fake supports.
pub trait Handler: Operation {
    fn handle(state: &Mutex<State>, request: &ApiRequest) -> ApiResult;
}

/// Function pointer to a handler.
pub type HandlerFn = fn(&Mutex<State>, &ApiRequest) -> ApiResult;

macro_rules! registry {
    ($($op:ty),* $(,)?) => {
        /// Operations that have a handler.
        pub static IMPLEMENTED: &[&OperationInfo] = &[$(<$op as Operation>::INFO),*];

        /// The handler of an operation, if it has one.
        pub fn handler(info: &OperationInfo) -> Option<HandlerFn> {
            $(if info == <$op as Operation>::INFO {
                return Some(<$op as Handler>::handle);
            })*
            None
        }
    };
}

registry![
    ops::GetVersion,
    ops::IssueSearchIssues,
    ops::ListActionTasks,
    ops::ListActionRuns,
    ops::ActionRun,
    ops::ListActionRunJobs,
    ops::RepoGetActionJobLogs,
    ops::CancelActionRun,
    ops::DispatchWorkflow,
    ops::RepoListActionsSecrets,
    ops::UpdateRepoSecret,
    ops::DeleteRepoSecret,
    ops::UserGetCurrent,
    ops::CreateCurrentUserRepo,
    ops::UserCurrentListRepos,
    ops::RepoGet,
    ops::RepoListTopics,
    ops::RepoEdit,
    ops::RepoDelete,
    ops::RepoListBranches,
    ops::RepoGetBranch,
    ops::RepoCreateBranch,
    ops::RepoGetContents,
    ops::RepoGetRawFile,
    ops::RepoCreateFile,
    ops::RepoCompareDiff,
    ops::RepoCreateStatus,
    ops::RepoListStatuses,
    ops::RepoListStatusesByRef,
    ops::RepoGetCombinedStatusByRef,
    ops::IssueListIssues,
    ops::IssueCreateIssue,
    ops::IssueGetIssue,
    ops::IssueEditIssue,
    ops::IssueListLabels,
    ops::IssueCreateLabel,
    ops::IssueGetLabel,
    ops::IssueEditLabel,
    ops::IssueDeleteLabel,
    ops::IssueGetLabels,
    ops::IssueAddLabel,
    ops::IssueReplaceLabels,
    ops::IssueClearLabels,
    ops::IssueRemoveLabel,
    ops::IssueGetComments,
    ops::IssueGetCommentsAndTimeline,
    ops::IssueCreateComment,
    ops::IssueGetRepoComments,
    ops::IssueGetComment,
    ops::IssueEditComment,
    ops::IssueDeleteComment,
    ops::RepoListPullRequests,
    ops::RepoCreatePullRequest,
    ops::RepoGetPullRequest,
    ops::RepoEditPullRequest,
    ops::RepoDownloadPullDiffOrPatch,
    ops::RepoGetPullRequestCommits,
    ops::RepoGetPullRequestFiles,
    ops::RepoPullRequestIsMerged,
    ops::RepoMergePullRequest,
    ops::RepoCancelScheduledAutoMerge,
    ops::RepoListHooks,
    ops::RepoCreateHook,
    ops::RepoGetHook,
    ops::RepoEditHook,
    ops::RepoDeleteHook,
    ops::RepoTestHook,
];
