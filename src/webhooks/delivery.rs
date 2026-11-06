//! Recording, signing and sending webhook deliveries.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Mutex, Weak};
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha1::Sha1;
use sha2::Sha256;
use uuid::{Builder, Variant, Version};

use crate::state::{Delivery, State, lock, now};

const USER_AGENT: &str = "Forgejo-Hookshot/ninot-forgejo";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryHeaders {
    pub event: String,
    pub delivery: String,
    pub hook_id: u64,
    pub target_id: u64,
    pub target_type: &'static str,
    pub content_type: &'static str,
    /// Hex HMAC-SHA256 of the body, empty when the hook has no secret.
    pub signature_256: String,
    /// Hex HMAC-SHA1 of the body, empty when the hook has no secret.
    pub signature_1: String,
}

impl DeliveryHeaders {
    /// The headers of one delivery, as Forgejo sends them: the event, the delivery and the
    /// signature under the `X-Forgejo-`, `X-Gitea-` and `X-Gogs-` prefixes, plus the
    /// `X-GitHub-` and `X-Hub-` headers GitHub clients read. The signatures are sent even
    /// without a secret, empty.
    pub fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("User-Agent", USER_AGENT.to_owned()),
            ("Content-Type", self.content_type.to_owned()),
            ("X-Forgejo-Event", self.event.clone()),
            ("X-Gitea-Event", self.event.clone()),
            ("X-Forgejo-Event-Type", self.event.clone()),
            ("X-Gitea-Event-Type", self.event.clone()),
            ("X-Forgejo-Delivery", self.delivery.clone()),
            ("X-Gitea-Delivery", self.delivery.clone()),
            ("X-Gogs-Event", self.event.clone()),
            ("X-Gogs-Event-Type", self.event.clone()),
            ("X-Gogs-Delivery", self.delivery.clone()),
            ("X-GitHub-Event", self.event.clone()),
            ("X-GitHub-Event-Type", self.event.clone()),
            ("X-GitHub-Delivery", self.delivery.clone()),
            ("X-Forgejo-Signature", self.signature_256.clone()),
            ("X-Gitea-Signature", self.signature_256.clone()),
            ("X-Gogs-Signature", self.signature_256.clone()),
            ("X-Hub-Signature", format!("sha1={}", self.signature_1)),
            (
                "X-Hub-Signature-256",
                format!("sha256={}", self.signature_256),
            ),
            ("X-Forgejo-Hook-ID", self.hook_id.to_string()),
            ("X-Gitea-Hook-ID", self.hook_id.to_string()),
            (
                "X-Forgejo-Hook-Installation-Target-ID",
                self.target_id.to_string(),
            ),
            (
                "X-Gitea-Hook-Installation-Target-ID",
                self.target_id.to_string(),
            ),
            (
                "X-Forgejo-Hook-Installation-Target-Type",
                self.target_type.to_owned(),
            ),
            (
                "X-Gitea-Hook-Installation-Target-Type",
                self.target_type.to_owned(),
            ),
        ]
    }
}

/// The lowercase hex HMAC-SHA256 of `body` under `secret`, as Forgejo signs a delivery: the
/// digest alone, with no algorithm in front of it.
pub fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// The lowercase hex HMAC-SHA1 of `body` under `secret`, for `X-Hub-Signature`.
pub fn signature_sha1(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Queues one delivery of `event` to every hook of `owner/name` that wants it, or to `only_hook`
/// alone when a hook test names one. `payload` is the generated model of what the spec says the
/// event carries, serialized once per hook so its signature covers the body as it is sent.
pub fn enqueue<T: Serialize>(
    state: &mut State,
    owner: &str,
    name: &str,
    event: &str,
    payload: &T,
    only_hook: Option<u64>,
) {
    let Some(repo) = state.user(owner).and_then(|u| u.repo(name)) else {
        return;
    };
    let repository_id = repo.id;
    let targets: Vec<(u64, String, String, Option<String>)> = repo
        .hooks
        .iter()
        .filter(|hook| only_hook.is_none_or(|id| id == hook.id) && hook.wants(event))
        .map(|hook| {
            (
                hook.id,
                hook.url.clone(),
                hook.content_type.clone(),
                hook.secret.clone(),
            )
        })
        .collect();
    let json_body = serde_json::to_vec(payload).expect("an event payload always serializes");
    for (hook_id, url, content_type, secret) in targets {
        let (body, mime) = if content_type == "form" {
            let form: String = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("payload", &String::from_utf8_lossy(&json_body))
                .finish();
            (form.into_bytes(), "application/x-www-form-urlencoded")
        } else {
            (json_body.clone(), "application/json")
        };
        let id = state.next_id();
        let headers = DeliveryHeaders {
            event: event.to_owned(),
            delivery: guid(id),
            hook_id,
            target_id: repository_id,
            target_type: "repository",
            content_type: mime,
            signature_256: secret
                .as_deref()
                .filter(|secret| !secret.is_empty())
                .map_or_else(String::new, |secret| signature(secret, &body)),
            signature_1: secret
                .as_deref()
                .filter(|secret| !secret.is_empty())
                .map_or_else(String::new, |secret| signature_sha1(secret, &body)),
        };
        state.deliveries.push(Delivery {
            id,
            guid: headers.delivery.clone(),
            hook_id,
            repository_id,
            event: event.to_owned(),
            url,
            headers,
            body,
            delivered_at: now(),
            status_code: None,
            error: None,
            duration_ms: 0,
        });
        if let Some(queue) = &state.webhook_queue {
            let _ = queue.send(id);
        }
    }
}

/// The UUID naming one delivery, which is all `X-Forgejo-Delivery` carries. The counter the
/// fake keeps with the fake's own name in it, stamped with the version and the variant that
/// make a UUID a UUID.
fn guid(id: u64) -> String {
    let counter = u128::from(id) | (0x6e69_6e6f_7400_0000u128 << 64);
    Builder::from_u128(counter)
        .with_version(Version::Random)
        .with_variant(Variant::RFC4122)
        .into_uuid()
        .to_string()
}

/// Which addresses a webhook delivery may contact.
///
/// [`TargetPolicy::Any`] is the default, so a delivery to `127.0.0.1` keeps working.
/// [`TargetPolicy::PublicOnly`] refuses loopback, link-local and private addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetPolicy {
    /// Contact any resolved address, including loopback and private networks.
    #[default]
    Any,
    /// Contact an address only when it is publicly routable.
    PublicOnly,
}

impl TargetPolicy {
    /// Whether a delivery may contact `addr`.
    ///
    /// [`TargetPolicy::Any`] allows every address. [`TargetPolicy::PublicOnly`] refuses
    /// every address that is not globally routable: loopback, unspecified, private,
    /// carrier-grade NAT (`100.64/10`, which includes Tailscale), link-local, multicast,
    /// reserved and documentation ranges, IPv6 unique-local (`fc00::/7`, which includes
    /// Tailscale's `fd7a:115c:a1e0::/48`), and IPv4-mapped forms of any of these. 6to4
    /// (`2002::/16`) and Teredo (`2001::/32`) are refused wholesale, since they tunnel IPv4.
    pub fn allows(self, addr: std::net::IpAddr) -> bool {
        match self {
            Self::Any => true,
            Self::PublicOnly => !is_denied(addr),
        }
    }
}

/// Whether `addr` is not globally routable, including an IPv4-mapped IPv6 address whose
/// embedded IPv4 is not.
fn is_denied(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(addr) => is_denied_v4(addr),
        IpAddr::V6(addr) => match addr.to_ipv4_mapped() {
            Some(mapped) => is_denied_v4(mapped),
            None => {
                let [first, second, ..] = addr.segments();
                addr.is_loopback()
                    || addr.is_unspecified()
                    || addr.is_multicast()
                    || (first & 0xffc0) == 0xfe80
                    || (first & 0xfe00) == 0xfc00
                    || (first, second) == (0x2001, 0x0db8)
                    || first == 0x2002
                    || (first, second) == (0x2001, 0x0000)
            }
        },
    }
}

/// Whether `addr` is in a range that is not globally routable.
fn is_denied_v4(addr: Ipv4Addr) -> bool {
    let [a, b, c, _] = addr.octets();
    addr.is_loopback()
        || addr.is_unspecified()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_multicast()
        || (a == 100 && (b & 0xc0) == 64)
        || (a == 198 && (b & 0xfe) == 18)
        || (a & 0xf0) == 240
        || (a, b, c) == (192, 0, 2)
        || (a, b, c) == (198, 51, 100)
        || (a, b, c) == (203, 0, 113)
}

/// Webhook HTTP client settings: do not follow redirects, wait five seconds to connect,
/// and ten seconds for the whole request.
fn client_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
}

/// A client pinned to the addresses `url` resolves to, after [`TargetPolicy::PublicOnly`]
/// has accepted every one of them.
///
/// A resolution failure keeps the resolver's error text. A URL that cannot be checked, no
/// addresses, or any disallowed address is `webhook target not allowed`, and nothing is sent.
fn pinned_client(url: &str) -> Result<reqwest::blocking::Client, String> {
    let not_allowed = || "webhook target not allowed".to_owned();
    let parsed = url::Url::parse(url).map_err(|_| not_allowed())?;
    let Some(raw_host) = parsed.host_str() else {
        return Err(not_allowed());
    };
    // `host_str` keeps the brackets around an IPv6 literal; the resolver does not.
    let host = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(raw_host);
    let Some(port) = parsed.port_or_known_default() else {
        return Err(not_allowed());
    };
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .collect();
    if addrs.is_empty()
        || addrs
            .iter()
            .any(|addr| !TargetPolicy::PublicOnly.allows(addr.ip()))
    {
        return Err(not_allowed());
    }
    client_builder()
        .no_proxy()
        .resolve_to_addrs(host, &addrs)
        .build()
        .map_err(|error| error.to_string())
}

/// POSTs `body` to `url` with `headers` and returns the status code, or the client error text.
fn post(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
) -> Result<u16, String> {
    let mut request = client.post(url).body(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request
        .send()
        .map(|response| response.status().as_u16())
        .map_err(|error| error.to_string())
}

pub fn spawn_sender(state: Weak<Mutex<State>>) -> mpsc::Sender<u64> {
    let (sender, receiver) = mpsc::channel::<u64>();
    std::thread::spawn(move || {
        let client = client_builder()
            .build()
            .expect("cannot build webhook client");
        for id in receiver {
            let Some(state) = state.upgrade() else {
                return;
            };
            let job = {
                let state = lock(&state);
                let policy = state.webhook_targets;
                state
                    .deliveries
                    .iter()
                    .find(|delivery| delivery.id == id)
                    .map(|delivery| {
                        (
                            delivery.url.clone(),
                            delivery.headers.pairs(),
                            delivery.body.clone(),
                            policy,
                        )
                    })
            };
            let Some((url, headers, body, policy)) = job else {
                continue;
            };
            let started = Instant::now();
            let outcome = match policy {
                TargetPolicy::Any => post(&client, &url, headers, body),
                TargetPolicy::PublicOnly => match pinned_client(&url) {
                    Ok(pinned) => post(&pinned, &url, headers, body),
                    Err(error) => Err(error),
                },
            };
            let mut state = lock(&state);
            if let Some(delivery) = state
                .deliveries
                .iter_mut()
                .find(|delivery| delivery.id == id)
            {
                delivery.duration_ms = started.elapsed().as_millis() as u64;
                match outcome {
                    Ok(status) => delivery.status_code = Some(status),
                    Err(error) => delivery.error = Some(error),
                }
            }
        }
    });
    sender
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// The headers of one `push` delivery, signed (with a stand-in SHA-1) when `signature`
    /// is given.
    fn headers(signature: Option<&str>) -> DeliveryHeaders {
        DeliveryHeaders {
            event: "push".to_owned(),
            delivery: guid(7),
            hook_id: 3,
            target_id: 2,
            target_type: "repository",
            content_type: "application/json",
            signature_256: signature.unwrap_or_default().to_owned(),
            signature_1: if signature.is_some() { "cafe" } else { "" }.to_owned(),
        }
    }

    /// The value of the header `name`.
    fn value<'a>(pairs: &'a [(&'a str, String)], name: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(header, _)| *header == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_signature_is_the_bare_hex_digest() {
        // Test case 2 of RFC 4231, which Forgejo's HMAC-SHA256 also reproduces.
        assert_eq!(
            signature("Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn a_delivery_names_its_event_under_every_prefix_forgejo_sends_it_under() {
        let pairs = headers(Some("deadbeef")).pairs();
        for name in [
            "X-Forgejo-Event",
            "X-Gitea-Event",
            "X-Forgejo-Event-Type",
            "X-Gitea-Event-Type",
            "X-GitHub-Event",
        ] {
            assert_eq!(value(&pairs, name), Some("push"), "{name}");
        }
        let delivery = guid(7);
        for name in [
            "X-Forgejo-Delivery",
            "X-Gitea-Delivery",
            "X-GitHub-Delivery",
        ] {
            assert_eq!(value(&pairs, name), Some(delivery.as_str()), "{name}");
        }
        for name in ["X-Gogs-Event", "X-Gogs-Event-Type", "X-GitHub-Event-Type"] {
            assert_eq!(value(&pairs, name), Some("push"), "{name}");
        }
        assert_eq!(value(&pairs, "X-Gogs-Delivery"), Some(delivery.as_str()));
        for name in [
            "X-Forgejo-Signature",
            "X-Gitea-Signature",
            "X-Gogs-Signature",
        ] {
            assert_eq!(value(&pairs, name), Some("deadbeef"), "{name}");
        }
        assert_eq!(
            value(&pairs, "X-Hub-Signature-256"),
            Some("sha256=deadbeef")
        );
        assert_eq!(value(&pairs, "X-Hub-Signature"), Some("sha1=cafe"));
    }

    #[test]
    fn a_hook_without_a_secret_gets_empty_signatures() {
        let pairs = headers(None).pairs();
        for name in [
            "X-Forgejo-Signature",
            "X-Gitea-Signature",
            "X-Gogs-Signature",
        ] {
            assert_eq!(value(&pairs, name), Some(""), "{name}");
        }
        assert_eq!(value(&pairs, "X-Hub-Signature"), Some("sha1="));
        assert_eq!(value(&pairs, "X-Hub-Signature-256"), Some("sha256="));
    }

    #[test]
    fn a_delivery_is_named_by_a_uuid() {
        let delivery = guid(7);
        let uuid = Uuid::parse_str(&delivery).unwrap_or_else(|_| panic!("{delivery} is no uuid"));
        assert_eq!(uuid.get_version_num(), 4, "{delivery} is not version 4");
        assert_eq!(uuid.get_variant(), Variant::RFC4122, "{delivery}");
        assert_ne!(guid(7), guid(8), "one counter, one name");
    }

    /// [`TargetPolicy::PublicOnly`] refuses every non-routable address in the table, and
    /// [`TargetPolicy::Any`] allows every one of them.
    #[test]
    fn a_public_only_policy_allows_only_public_addresses() {
        let cases = [
            // IPv4 private, loopback, link-local, unspecified, broadcast, and their edges.
            ("127.0.0.1", false),
            ("10.1.2.3", false),
            ("192.168.0.1", false),
            ("169.254.169.254", false),
            ("0.0.0.0", false),
            ("255.255.255.255", false),
            ("172.16.0.1", false),
            ("172.31.255.255", false),
            ("172.32.0.1", true),
            // Carrier-grade NAT 100.64/10, which holds the Tailscale range.
            ("100.64.0.0", false),
            ("100.100.100.100", false),
            ("100.127.255.255", false),
            ("100.63.255.255", true),
            ("100.128.0.0", true),
            // Multicast 224/4 and reserved 240/4.
            ("224.0.0.1", false),
            ("239.255.255.255", false),
            ("240.0.0.0", false),
            ("255.255.255.254", false),
            // Benchmarking 198.18/15.
            ("198.18.0.0", false),
            ("198.19.255.255", false),
            ("198.17.255.255", true),
            ("198.20.0.0", true),
            // Documentation ranges.
            ("192.0.2.1", false),
            ("192.0.1.255", true),
            ("198.51.100.7", false),
            ("198.51.101.0", true),
            ("203.0.113.9", false),
            ("203.0.114.0", true),
            // IPv6 loopback, unspecified, link-local, unique-local and Tailscale's prefix.
            ("::1", false),
            ("::", false),
            ("fe80::1", false),
            ("febf::1", false),
            ("fd00::1", false),
            ("fc00::1", false),
            ("fdff::1", false),
            ("fbff::1", true),
            ("fd7a:115c:a1e0::1", false),
            // IPv6 multicast, documentation, 6to4 and Teredo.
            ("ff00::1", false),
            ("ff02::1", false),
            ("2001:db8::1", false),
            ("2001:db9::1", true),
            ("2002::1", false),
            ("2001::1", false),
            ("2001:1::1", true),
            // IPv4-mapped forms are judged by their embedded IPv4.
            ("::ffff:127.0.0.1", false),
            ("::ffff:10.0.0.1", false),
            ("::ffff:169.254.169.254", false),
            ("::ffff:100.100.100.100", false),
            ("::ffff:93.184.216.34", true),
            // Public addresses.
            ("93.184.216.34", true),
            ("2606:4700::1111", true),
        ];
        for (text, allowed) in cases {
            let addr = text
                .parse::<std::net::IpAddr>()
                .unwrap_or_else(|_| panic!("{text} is not an address"));
            assert_eq!(
                TargetPolicy::PublicOnly.allows(addr),
                allowed,
                "{text} under PublicOnly"
            );
            assert!(TargetPolicy::Any.allows(addr), "{text} under Any");
        }
    }
}
