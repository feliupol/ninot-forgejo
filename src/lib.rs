//! In-memory fake of the Forgejo REST API and git hosting for end-to-end tests.
//!
//! The public surface:
//!
//! - [`Server`] starts a fake, seeds users and tokens, and prints its admin token;
//!   [`server::Options`] configures it.
//! - [`OPERATIONS`], [`ops`], [`Operation`], [`OperationInfo`] and [`Method`] describe the
//!   pinned spec's operations, and [`IMPLEMENTED`] lists those the fake serves.
//! - [`types`] holds the generated models of every response body and webhook payload the fake
//!   sends, which the renderers, the handlers and the payload builders name instead of building
//!   JSON by hand.
//! - [`TargetPolicy`] decides which addresses a webhook delivery may contact. The default
//!   allows every address, including loopback.

pub(crate) mod api;
pub(crate) mod faults;
pub(crate) mod handlers;
pub(crate) mod merge;
pub(crate) mod operation;
pub(crate) mod parse;
pub(crate) mod render;
pub(crate) mod router;
pub mod server;
pub(crate) mod spec;
pub(crate) mod state;
pub(crate) mod time;

pub use handlers::IMPLEMENTED;
pub use operation::{Method, OPERATIONS, Operation, OperationInfo, ops};
pub use server::{
    ActionRun, PullFixture, PullRequest, Repository, ScheduledMergeInfo, Server, User,
};
pub use state::{RequestTrace, RequestTraceHit};
pub use webhooks::TargetPolicy;

pub mod types;
pub(crate) mod webhooks;
