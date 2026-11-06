//! Types for supported API request and response bodies and webhook payloads.
//!
//! The models come from the definitions of the pinned Forgejo Swagger description that
//! `build.rs` generates with [`typify`]; [`spec/overrides.json`](crate) says what was corrected
//! in them and why. Request models deserialize the shape a Forgejo client sends; response and
//! webhook models serialize the shape a Forgejo client reads. Handlers name these models
//! instead of indexing JSON fields by hand.

include!(concat!(env!("OUT_DIR"), "/forgejo.rs"));
