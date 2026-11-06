//! A small REST client shared by the integration tests.

#![allow(dead_code)]

use std::sync::OnceLock;

use ninot_forgejo::Server;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::redirect::Policy;
use serde_json::Value;

/// The time Forgejo renders a date-time it never set as, which a Go client parses like any
/// other. The empty string the fake used to send is not a date-time and makes Go refuse the
/// whole response, as `tea` did over a repository's `mirror_updated`.
pub const UNSET_TIME: &str = "0001-01-01T00:00:00Z";

/// The pinned Swagger description the fake is generated from.
fn spec() -> &'static Value {
    static SPEC: OnceLock<Value> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_json::from_slice(&std::fs::read("spec/forgejo.v1.json").unwrap()).unwrap()
    })
}

/// Asserts that `body` renders every date-time the spec declares for `definition` as one a Go
/// client can parse: a timestamp, [`UNSET_TIME`] for one Forgejo never set, or `null`. A field
/// left out, or rendered as an empty string, fails.
pub fn assert_date_times(definition: &str, body: &Value) {
    let declared = spec()["definitions"][definition]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("the spec has no definition {definition}"))
        .iter()
        .filter(|(_, property)| property["format"] == "date-time")
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    for name in declared {
        let field = body
            .get(name)
            .unwrap_or_else(|| panic!("{definition} does not render {name}, which is a date-time"));
        if field.is_null() {
            continue;
        }
        let time = field
            .as_str()
            .unwrap_or_else(|| panic!("{definition}.{name} is not a date-time: {field}"));
        assert!(
            is_timestamp(time),
            "{definition}.{name} is not a date-time: {time:?}"
        );
    }
}

/// Whether `time` has the shape of the timestamps Forgejo renders: RFC 3339 in UTC, to the
/// second. Its length alone tells `""` and `UNSET_TIME` apart, and the rest keeps a malformed
/// one out. `humantime` will not confirm the zero time, having no way to name a moment before
/// 1970, which is exactly what makes the zero time a sentinel here.
fn is_timestamp(time: &str) -> bool {
    let bytes = time.as_bytes();
    bytes.len() == 20
        && (0..20).all(|index| match index {
            4 | 7 => bytes[index] == b'-',
            10 => bytes[index] == b'T',
            13 | 16 => bytes[index] == b':',
            19 => bytes[index] == b'Z',
            _ => bytes[index].is_ascii_digit(),
        })
}

/// Calls the API of `server` as the holder of `token`.
pub struct Api<'a> {
    pub server: &'a Server,
    pub token: Option<String>,
    client: Client,
}

/// Status, headers and JSON body of a response.
pub struct Reply {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Value,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

impl<'a> Api<'a> {
    pub fn new(server: &'a Server, token: Option<&str>) -> Self {
        Self {
            server,
            token: token.map(str::to_owned),
            client: Client::builder().redirect(Policy::none()).build().unwrap(),
        }
    }

    pub fn admin(server: &'a Server) -> Self {
        Self::new(server, Some(server.admin_token()))
    }

    fn send(&self, request: RequestBuilder) -> Reply {
        let request = match &self.token {
            Some(token) => request.header("Authorization", format!("token {token}")),
            None => request,
        };
        let response = request.send().unwrap();
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let text = response.text().unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.server.url())
    }

    pub fn get(&self, path: &str) -> Reply {
        self.send(self.client.get(self.url(path)))
    }

    pub fn delete(&self, path: &str) -> Reply {
        self.send(self.client.delete(self.url(path)))
    }

    pub fn post(&self, path: &str, body: Value) -> Reply {
        self.send(self.client.post(self.url(path)).json(&body))
    }

    /// Posts `body` as is, JSON or not.
    pub fn post_raw(&self, path: &str, body: &str) -> Reply {
        self.send(
            self.client
                .post(self.url(path))
                .header("content-type", "application/json")
                .body(body.to_owned()),
        )
    }

    pub fn patch(&self, path: &str, body: Value) -> Reply {
        self.send(self.client.patch(self.url(path)).json(&body))
    }

    pub fn put(&self, path: &str, body: Value) -> Reply {
        self.send(self.client.put(self.url(path)).json(&body))
    }
}

/// A scratch directory removed when dropped.
pub struct TempDir(pub std::path::PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ninot-forgejo-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn join(&self, path: &str) -> std::path::PathBuf {
        self.0.join(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs git in `dir`, isolated from the user's configuration and never prompting.
pub fn git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap()
}

/// Runs git and panics with its stderr unless it succeeds; returns trimmed stdout.
pub fn git_ok(dir: &std::path::Path, args: &[&str]) -> String {
    let output = git(dir, args);
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Clone URL of `owner/repo`, authenticated with `token` when given.
pub fn clone_url(server: &Server, owner: &str, repo: &str, token: Option<&str>) -> String {
    let base = server.url().strip_prefix("http://").unwrap();
    match token {
        Some(token) => format!("http://oauth2:{token}@{base}/{owner}/{repo}.git"),
        None => format!("http://{base}/{owner}/{repo}.git"),
    }
}

/// Commits `file` with `content` in the work tree `dir`.
pub fn commit_file(dir: &std::path::Path, file: &str, content: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    git_ok(dir, &["add", file]);
    git_ok(dir, &["commit", "-q", "-m", &format!("Add {file}")]);
}

// Test HTTP receiver that records requests sent to it
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Received {
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Receiver {
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
    stop: Arc<Mutex<bool>>,
}

impl Receiver {
    pub fn start(status: u16) -> Self {
        Self::spawn(status, None)
    }

    /// A receiver that answers every request with `302 Found` pointing at `location`.
    pub fn redirecting(location: &str) -> Self {
        Self::spawn(302, Some(location.to_owned()))
    }

    fn spawn(status: u16, location: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://127.0.0.1:{}", addr.port());
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        let stop = Arc::new(Mutex::new(false));
        let stop_clone = stop.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if *stop_clone.lock().unwrap() {
                    break;
                }
                if let Ok(mut stream) = stream {
                    let mut buffer = Vec::new();
                    let mut temp = [0; 8192];
                    loop {
                        match stream.read(&mut temp) {
                            Ok(0) => break,
                            Ok(n) => {
                                buffer.extend_from_slice(&temp[..n]);
                                if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let mut headers = HashMap::new();
                    let mut body = Vec::new();
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header_part = &buffer[..pos];
                        if let Ok(header_str) = std::str::from_utf8(header_part) {
                            for line in header_str.lines() {
                                if let Some(colon) = line.find(':') {
                                    let key = line[..colon].trim().to_ascii_lowercase();
                                    let value = line[colon + 1..].trim().to_string();
                                    headers.insert(key, value);
                                }
                            }
                        }
                        body.extend_from_slice(&buffer[pos + 4..]);
                    }
                    received_clone
                        .lock()
                        .unwrap()
                        .push(Received { headers, body });
                    let location = location
                        .as_deref()
                        .map(|location| format!("Location: {location}\r\n"))
                        .unwrap_or_default();
                    let response =
                        format!("HTTP/1.1 {status} OK\r\n{location}Content-Length: 0\r\n\r\n");
                    let _ = stream.write_all(response.as_bytes());
                } else {
                    break;
                }
            }
        });
        Self {
            url,
            received,
            stop,
        }
    }

    /// Waits for one request and returns it; panics when none arrives within five seconds.
    pub fn next(&self) -> Received {
        self.wait_for(1, Duration::from_secs(5))
            .into_iter()
            .next()
            .expect("timeout waiting for received request")
    }

    /// Waits until `count` requests arrived or `timeout` passed, then returns every request
    /// received, oldest first.
    pub fn wait_for(&self, count: usize, timeout: Duration) -> Vec<Received> {
        let start = std::time::Instant::now();
        loop {
            let received = self.received();
            if received.len() >= count || start.elapsed() >= timeout {
                return received;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Every request received so far, oldest first.
    pub fn received(&self) -> Vec<Received> {
        let mut received = self.received.lock().unwrap().clone();
        received.reverse();
        received
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
    }
}
