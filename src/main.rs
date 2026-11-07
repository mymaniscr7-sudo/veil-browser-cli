//! `veil-browser` — the CLI front-end.
//!
//! Two modes:
//! * **CLI** (default): parse args, send one request to the session daemon
//!   (starting it first if needed), print the result as compact text or
//!   JSON, exit. No browser state lives in this process.
//! * **`__daemon`**: run the persistent daemon for a session (started
//!   automatically by the CLI; not for direct use).

use anyhow::{Context, Result};
use clap::Parser;
use veil_browser::cli::{Cli, Command};
use veil_browser::config::{Config, Session};
use veil_browser::daemon::Daemon;
use veil_browser::output::{print_error, print_outcome, Outcome};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let session = Session::new(&cli.session);

    if matches!(cli.command, Command::Daemon) {
        // Daemon mode: log to the session log file via the client's
        // redirection (stdout/stderr already point there).
        let config = match Config::from_env() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[veil-browser] daemon config error: {e:#}");
                std::process::exit(1);
            }
        };
        if let Err(e) = Daemon::start(session, config).await {
            eprintln!("[veil-browser] daemon failed: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(e) = run(cli, session).await {
        // `run` already printed the error; just set the exit code.
        std::process::exit(e.downcast::<ExitCode>().map(|c| c.0).unwrap_or(1));
    }
}

#[derive(Debug)]
struct ExitCode(i32);

impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command failed (exit {})", self.0)
    }
}

impl std::error::Error for ExitCode {}

async fn run(cli: Cli, session: Session) -> Result<()> {
    let command = cli.command.clone();
    let name = command.name().to_string();
    let json_mode = cli.json;

    // Screenshot paths must be absolute — the daemon runs with a different
    // working directory, so resolve them here in the CLI.
    let mut request = command.to_request();
    if let Command::Screenshot { path, .. } = &command {
        let abs = std::path::Path::new(path);
        let abs = if abs.is_absolute() {
            abs.to_path_buf()
        } else {
            std::env::current_dir()?.join(abs)
        };
        request["path"] = serde_json::Value::String(abs.to_string_lossy().to_string());
    }

    let resp = veil_browser::client::send(&session, &request)
        .with_context(|| format!("talking to the browser daemon for session '{}'", session.name))?;

    if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let mut outcome = Outcome::new(
            &name,
            &session.name,
            resp.get("summary").and_then(|s| s.as_str()).unwrap_or("ok"),
        );
        if let Some(data) = resp.get("data") {
            outcome = outcome.with_data(data.clone());
        }
        if let Some(text) = resp.get("text").and_then(|t| t.as_str()) {
            outcome = outcome.with_body(text);
        }
        print_outcome(&outcome, json_mode);
        Ok(())
    } else {
        let message = resp
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        let hint = resp.get("hint").and_then(|h| h.as_str()).map(|s| s.to_string());
        let err = anyhow::Error::msg(if let Some(h) = hint { format!("{message}\n hint: {h}") } else { message });
        print_error(&name, &session.name, &err, json_mode);
        Err(anyhow::Error::new(ExitCode(1)))
    }
}
