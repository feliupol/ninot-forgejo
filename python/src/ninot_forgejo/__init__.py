"""Direct fixture interface for a real loopback Forgejo service."""

from ._native import ActionRun, PullRequest, Repository, ScheduledMergeInfo, Server, User
from .models import (
    ActionJobSeed,
    ActionRunSeed,
    ActionTaskSeed,
    CommitStatusSeed,
    CommitStatusState,
    ProfileHit,
    RequestTrace,
    RequestTraceHit,
    TimelineEventKind,
    TimelineEventSeed,
)

__all__ = [
    "ActionJobSeed",
    "ActionRun",
    "ActionRunSeed",
    "ActionTaskSeed",
    "CommitStatusSeed",
    "CommitStatusState",
    "ProfileHit",
    "PullRequest",
    "Repository",
    "RequestTrace",
    "RequestTraceHit",
    "ScheduledMergeInfo",
    "Server",
    "TimelineEventKind",
    "TimelineEventSeed",
    "User",
]
