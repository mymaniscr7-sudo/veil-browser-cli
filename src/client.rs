//! CLI-side daemon client.
//!
//! Connects to the session daemon over its Unix socket, auto-starting it
//! (detached, via `setsid`) when it isn't running yet, then sends one
//! NDJSON request and returns the daemon's response object.

use crate::config::Session;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Proxy credentials for this invocation, parsed from the environment's
/// egress-proxy URL (`https://user:pass@host:port`). The runtime rotates
/// this token, so the CLI passes the *current* one with every request —
/// the daemon (long-lived, started under an older token) must not rely on
/// its own environment. Sent only over the local Unix socket, never logged.
fn proxy_auth_from_env() -> Option<Value> {
    let proxy = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy")).ok()?;
    let after_scheme = proxy.split("://").nth(1).unwrap_or(&proxy);
    let userinfo = after_scheme.split('@').next()?;
    if !after_scheme.contains('@') || userinfo.is_empty() {
        return None;
    }
    let (user, pass) = userinfo.split_once(':')?;
    Some(serde_json::json!({ "username": user, "password": pass }))
}

/// Send one request to the session daemon, starting it first if needed.
pub fn send(session: &Session, request: &Value) -> Result<Value> {
    let mut request = request.clone();
    if let Some(auth) = proxy_auth_from_env() {
        request["proxy_auth"] = auth;
    }
    let request = &request;
    match try_send(session, request) {
        Ok(v) => Ok(v),
        Err(first_err) => {
            // Maybe the daemon simply isn't running — start it and retry once.
            if !daemon_alive(session) {
                start_daemon(session)?;
                wait_for_socket(session, Duration::from_secs(25))?;
                try_send(session, request).context("daemon started but the request still failed")
            } else {
                Err(first_err)
            }
        }
    }
}

fn try_send(session: &Session, request: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(&session.socket)
        .with_context(|| format!("connecting to daemon socket {}", session.socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp: Value = serde_json::from_str(&line).context("parsing daemon response")?;
    Ok(resp)
}

fn daemon_alive(session: &Session) -> bool {
    UnixStream::connect(&session.socket).is_ok()
}

fn wait_for_socket(session: &Session, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if daemon_alive(session) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    anyhow::bail!(
        "daemon did not come up within {timeout:?} — see {}",
        session.log_file.display()
    )
}

fn start_daemon(session: &Session) -> Result<()> {
    session.ensure_dir()?;
    let exe = std::env::current_exe().context("locating veil-browser binary")?;
    let log = std::fs::File::create(&session.log_file)?;
    let log_err = log.try_clone()?;
    // `setsid` detaches the daemon into its own session so it survives
    // the CLI exiting. Spawn (don't wait): setsid execs the daemon, so
    // waiting for it would block until the daemon itself exits.
    let _child = std::process::Command::new("setsid")
        .arg(&exe)
        .arg("__daemon")
        .arg("--session")
        .arg(&session.name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .context("spawning daemon via setsid")?;
    Ok(())
}
