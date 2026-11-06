//! Repository webhooks and the deliveries sent to them.

use crate::webhooks::DeliveryHeaders;
use chrono::{DateTime, Utc};

/// A repository webhook.
#[derive(Clone)]
pub struct Hook {
    pub id: u64,
    pub active: bool,
    /// Events delivered; `*` means all.
    pub events: Vec<String>,
    pub url: String,
    /// `json` or `form`.
    pub content_type: String,
    pub secret: Option<String>,
    pub insecure_ssl: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Hook {
    /// Whether this hook is delivered `event`: it must be active, and subscribed to the event
    /// or to every event. Forgejo sends nothing to an inactive hook, and nothing an event it
    /// did not subscribe to; creating one sends nothing at all.
    pub fn wants(&self, event: &str) -> bool {
        self.active
            && self
                .events
                .iter()
                .any(|wanted| wanted == event || wanted == "*")
    }
}

/// One webhook delivery attempt, recorded for inspection.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub id: u64,
    pub guid: String,
    pub hook_id: u64,
    pub repository_id: u64,
    pub event: String,
    pub url: String,
    pub headers: DeliveryHeaders,
    pub body: Vec<u8>,
    pub delivered_at: DateTime<Utc>,
    /// Set once the attempt finished with an HTTP response.
    pub status_code: Option<u16>,
    /// Set once the attempt failed without an HTTP response.
    pub error: Option<String>,
    pub duration_ms: u64,
}

impl Delivery {
    pub fn finished(&self) -> bool {
        self.status_code.is_some() || self.error.is_some()
    }

    /// The JSON payload, whether it was sent as JSON or as the `payload` field of a form.
    pub fn payload(&self) -> serde_json::Value {
        let body = String::from_utf8_lossy(&self.body);
        let form = url::form_urlencoded::parse(body.as_bytes())
            .find(|(field, _)| field == "payload")
            .map(|(_, payload)| payload.into_owned());
        let json = form.unwrap_or_else(|| body.into_owned());
        serde_json::from_str(&json).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::now;
    use serde_json::Value;

    /// A hook that is `active` and subscribed to `events`.
    fn hook(active: bool, events: &[&str]) -> Hook {
        Hook {
            id: 1,
            active,
            events: events.iter().map(|event| (*event).to_owned()).collect(),
            url: "http://127.0.0.1:9/hook".to_owned(),
            content_type: "json".to_owned(),
            secret: None,
            insecure_ssl: "0".to_owned(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn a_hook_hears_the_events_it_subscribed_to_and_no_other() {
        let hook = hook(true, &["push", "issues"]);
        assert!(hook.wants("push"));
        assert!(hook.wants("issues"));
        assert!(!hook.wants("create"));
    }

    #[test]
    fn a_hook_subscribed_to_every_event_hears_every_event() {
        assert!(hook(true, &["*"]).wants("push"));
        assert!(hook(true, &["*"]).wants("ping"));
    }

    #[test]
    fn an_inactive_hook_hears_nothing() {
        let hook = hook(false, &["*"]);
        assert!(!hook.wants("push"));
        assert!(!hook.wants("ping"));
    }

    #[test]
    fn a_delivery_is_read_back_whether_it_was_sent_as_json_or_as_a_form() {
        let payload = serde_json::json!({ "ref": "refs/heads/main", "note": "a b&c=d" });
        let json = serde_json::to_string(&payload).unwrap();
        let delivery = |body: String| Delivery {
            id: 1,
            guid: String::new(),
            hook_id: 1,
            repository_id: 1,
            event: "push".to_owned(),
            url: "http://127.0.0.1:9/hook".to_owned(),
            headers: DeliveryHeaders {
                event: "push".to_owned(),
                delivery: String::new(),
                hook_id: 1,
                target_id: 1,
                target_type: "repository",
                content_type: "application/json",
                signature_256: String::new(),
                signature_1: String::new(),
            },
            body: body.into_bytes(),
            delivered_at: now(),
            status_code: None,
            error: None,
            duration_ms: 0,
        };
        assert_eq!(delivery(json.clone()).payload(), payload);
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("payload", &json)
            .finish();
        assert_eq!(delivery(form).payload(), payload);
        assert_eq!(delivery("not json".to_owned()).payload(), Value::Null);
    }
}
