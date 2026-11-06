//! The instants the fake records, as Forgejo records them.
//!
//! Every `date-time` the spec declares is a [`DateTime<Utc>`], which is what the generated
//! models hold and what serde renders as a timestamp a Go client parses. Nothing is kept as the
//! string a timestamp is written as, so no renderer has to read one back.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};

/// Seconds from the epoch to `0001-01-01T00:00:00Z`, the zero time of Forgejo's Go types.
const UNSET_SECONDS: i64 = -62_135_596_800;

/// The current time, to the second, as Forgejo records a moment.
pub fn now() -> DateTime<Utc> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    DateTime::from_timestamp(seconds as i64, 0).unwrap_or_else(unset)
}

/// What Forgejo renders a date-time it never set as: the zero time of its Go type.
///
/// The spec declares every date-time it renders as `format: date-time`, so a field may not be
/// left empty: a Go client parsing the response as a timestamp refuses `""` outright, which is
/// how `tea` came to reject a whole repository over its `mirror_updated`.
pub fn unset() -> DateTime<Utc> {
    DateTime::from_timestamp(UNSET_SECONDS, 0).expect("the zero time is a timestamp chrono holds")
}

/// The instant `date` names, in UTC as a `date-time` field renders it. git writes a signature
/// as strict ISO 8601, which is RFC 3339 with an offset, so the offset is what this reads.
pub fn timestamp(date: &str) -> DateTime<Utc> {
    match DateTime::parse_from_rfc3339(date) {
        Ok(date) => date.with_timezone(&Utc),
        Err(error) => panic!("{date} is not a timestamp: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_time_is_a_timestamp_to_the_second() {
        let rendered = serde_json::to_value(now()).unwrap();
        let rendered = rendered.as_str().expect("a timestamp renders as a string");
        assert_eq!(rendered.len(), 20, "{rendered}");
        assert!(rendered.ends_with('Z'), "{rendered}");
    }

    #[test]
    fn an_unset_timestamp_is_the_zero_time_forgejo_sends() {
        // It is a sentinel rather than an instant: it names a moment before the epoch, which
        // chrono holds but a Go client parses like any other timestamp.
        assert_eq!(
            serde_json::to_value(unset()).unwrap(),
            serde_json::json!("0001-01-01T00:00:00Z")
        );
        assert!(DateTime::parse_from_rfc3339("0001-01-01T00:00:00Z").is_ok());
    }

    #[test]
    fn a_git_signature_is_read_as_the_utc_timestamp_it_names() {
        assert_eq!(
            timestamp("2026-09-29T20:38:27Z"),
            DateTime::from_timestamp(1_790_714_307, 0).unwrap()
        );
        // An offset git wrote for a committer east of Greenwich names the same instant in UTC.
        assert_eq!(
            timestamp("2026-09-29T22:38:27+02:00"),
            DateTime::from_timestamp(1_790_714_307, 0).unwrap()
        );
    }
}
