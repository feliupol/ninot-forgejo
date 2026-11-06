//! Deterministic protocol failures arranged through the fixture API.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// An exact HTTP method and path to fault.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Target {
    /// Uppercase HTTP method.
    pub method: String,
    /// Absolute URL path, including `/api/v1`.
    pub path: String,
}

/// Failures supported by the shared fault engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Fault {
    /// Reject a request before it changes service state.
    Status(u16),
    /// Delay a request by an explicit number of milliseconds.
    Latency(u64),
}

impl ninot_common::faults::Fault for Fault {
    fn latency(&self) -> Option<Duration> {
        match self {
            Self::Status(_) => None,
            Self::Latency(milliseconds) => Some(Duration::from_millis(*milliseconds)),
        }
    }
}

/// The service's shared fault engine.
pub(crate) type Faults = ninot_common::faults::Faults<Target, Fault>;
