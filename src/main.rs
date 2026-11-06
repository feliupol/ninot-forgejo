//! Command-line entry point for the Forgejo fake.

use std::net::SocketAddr;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// In-memory fake of the Forgejo REST API and git hosting.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serves the fake until interrupted, printing its URL and admin token.
    Serve {
        /// Local port; a free one when 0.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Serves HTTPS with a certificate from a fresh local certificate authority.
        #[arg(long)]
        tls: bool,
        /// Refuse webhook targets on loopback, link-local and private addresses.
        #[arg(long)]
        deny_private_webhook_targets: bool,
    },
    /// Lists the operations of the pinned spec and which are implemented.
    Operations,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Operations => {
            print!("{}", operations_report());
            ExitCode::SUCCESS
        }
        Command::Serve {
            port,
            tls,
            deny_private_webhook_targets,
        } => serve(port, tls, deny_private_webhook_targets),
    }
}

fn operations_report() -> String {
    use ninot_forgejo::{IMPLEMENTED, OPERATIONS};

    let mut report = format!(
        "{} / {} operations implemented\n",
        IMPLEMENTED.len(),
        OPERATIONS.len()
    );
    for info in OPERATIONS {
        let mark = if IMPLEMENTED.contains(info) { "x" } else { " " };
        report.push_str(&format!(
            "[{mark}] {} {} {}\n",
            info.id,
            info.method.as_str(),
            info.path
        ));
    }
    report
}

fn serve(port: u16, tls: bool, deny_private_webhook_targets: bool) -> ExitCode {
    use ninot_forgejo::TargetPolicy;

    let options = ninot_forgejo::server::Options {
        address: SocketAddr::from(([127, 0, 0, 1], port)),
        tls,
        webhook_targets: if deny_private_webhook_targets {
            TargetPolicy::PublicOnly
        } else {
            TargetPolicy::Any
        },
    };
    let server = match ninot_forgejo::Server::start_with(options) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("url: {}", server.url());
    println!("token: {}", server.admin_token());
    if let Some(ca) = server.ca_cert_path() {
        println!("ca: {}", ca.display());
    }
    server.wait();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_documents_commands_and_rejects_invalid_arguments() {
        let help = Cli::try_parse_from(["ninot-forgejo", "--help"])
            .err()
            .expect("help exits");
        assert!(help.to_string().contains("operations"));
        assert!(help.to_string().contains("serve"));
        assert!(Cli::try_parse_from(["ninot-forgejo", "serve", "--port", "not-a-port"]).is_err());
        assert!(Cli::try_parse_from(["ninot-forgejo", "unknown"]).is_err());
    }
}
