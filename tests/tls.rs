//! HTTPS for the REST API and git, trusting the certificate authority the fake publishes.

mod common;

use std::process::Command;

use common::TempDir;
use ninot_forgejo::Server;
use ninot_forgejo::server::Options;
use serde_json::json;

/// Runs git with the caller's TLS bypasses and certificate bundle removed.
fn git_tls(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env_remove("GIT_SSL_NO_VERIFY")
        .env_remove("GIT_SSL_CAINFO")
        .output()
        .unwrap()
}

#[test]
fn tls_publishes_a_ca_and_git_trusts_only_that_ca() {
    let server = Server::start_with(Options {
        tls: true,
        ..Default::default()
    })
    .unwrap();
    assert!(server.url().starts_with("https://"), "{}", server.url());

    let ca = server
        .ca_cert_path()
        .expect("a tls server publishes its certificate authority");
    let pem = std::fs::read_to_string(&ca).unwrap();
    assert!(
        pem.contains("BEGIN CERTIFICATE"),
        "the certificate authority file is a PEM certificate"
    );

    let certificate = reqwest::Certificate::from_pem(pem.as_bytes()).unwrap();
    let client = reqwest::blocking::Client::builder()
        .add_root_certificate(certificate)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let created = client
        .post(format!("{}/api/v1/user/repos", server.url()))
        .header("authorization", format!("token {}", server.admin_token()))
        .json(&json!({ "name": "tool", "auto_init": true }))
        .send()
        .unwrap();
    let status = created.status().as_u16();
    let body = created.text().unwrap();
    assert_eq!(status, 201, "{body}");

    let host = server.url().strip_prefix("https://").unwrap();
    let remote = format!(
        "https://oauth2:{}@{host}/admin/tool.git",
        server.admin_token()
    );
    let work = TempDir::new();
    let ca_info = format!("http.sslCAInfo={}", ca.display());
    let listed = git_tls(&work.0, &["-c", &ca_info, "ls-remote", &remote]);
    assert!(
        listed.status.success(),
        "git ls-remote failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let refused = git_tls(&work.0, &["ls-remote", &remote]);
    assert!(
        !refused.status.success(),
        "git trusted https without the published certificate authority: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
