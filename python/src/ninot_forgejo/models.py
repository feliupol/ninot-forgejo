"""Typed values used by the Forgejo direct fixture API."""

from typing import Literal, TypedDict

TimelineEventKind = Literal["pull_scheduled_merge", "pull_push", "pull_cancel_scheduled_merge"]
CommitStatusState = Literal["pending", "success", "error", "failure", "warning", "skipped"]


class TimelineEventSeed(TypedDict):
    """Fields accepted when adding a pull-request timeline event."""

    type: TimelineEventKind


class _CommitStatusDetails(TypedDict, total=False):
    """Optional commit status details."""

    description: str
    target_url: str


class CommitStatusSeed(_CommitStatusDetails):
    """Fields accepted when seeding a commit status."""

    sha: str
    context: str
    state: CommitStatusState


class ProfileHit(TypedDict):
    """A request observed by the service profiler."""

    method: str
    path: str
    query: str | None
    status: int
    label: str


class ActionTaskSeed(TypedDict, total=False):
    """Fields accepted when seeding an Actions task."""

    created_at: str
    updated_at: str
    run_started_at: str
    name: str
    display_title: str
    event: str
    head_branch: str
    head_sha: str
    run_number: int
    status: str
    url: str
    workflow_id: str


class ActionRunSeed(TypedDict, total=False):
    """Fields accepted when seeding an Actions workflow run."""

    workflow_id: str
    event: str
    commit_sha: str
    prettyref: str
    title: str
    status: str


class ActionJobSeed(TypedDict, total=False):
    """Fields accepted when seeding a workflow run job."""

    name: str
    status: str
    runs_on: list[str]


class RequestTraceHit(TypedDict):
    """One request captured by the opt-in request trace."""

    method: str
    path: str
    query: str
    body: bytes


class RequestTrace(TypedDict):
    """Captured requests and whether the trace reached its size limit."""

    requests: list[RequestTraceHit]
    truncated: bool
