//! Webhook deliveries: queueing, signing and sending in [`delivery`], one payload builder
//! per event in [`events`].

pub mod delivery;
pub mod events;

pub use delivery::{DeliveryHeaders, TargetPolicy, enqueue, spawn_sender};
