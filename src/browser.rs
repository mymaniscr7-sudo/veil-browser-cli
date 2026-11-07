//! Chrome process management + the HTTP `/json` endpoints.
//!
//! The daemon owns one Chrome process per session. Chrome is launched
//! headless with remote debugging on a per-session port; the daemon then
//! talks to it over HTTP (`/json/list`, `/json/new`, …) for target
//! management and over WebSocket (CDP) for everything else.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// One entry from Chrome's `/json/list`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub url: String,
    pub web_socket_debugger_url: Option<String>,
}

/// Minimal raw HTTP over a plain TCP socket — no HTTP client crate needed
/// for a localhost debug endpoint.
///
/// Note: Chrome's debug server keeps connections alive, so this must read
/// exactly `Content-Length` bytes (or the chunked body) — never
/// `read_to_end`, which would wait forever for an EOF that never comes.
async fn http_request(port: u16, method: &str, path: &str) -> Result<Value> {
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connecting to Chrome debug port {port}"))?;
    let mut stream = tokio::io::BufReader::new(stream);
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nContent-Length: 0\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;

    // Status line + headers.
    let mut line = String::new();
    stream.read_line(&mut line).await?;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        line.clear();
        stream.read_line(&mut line).await?;
        let header = line.trim();
        if header.is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = v.trim().parse().ok(),
                "transfer-encoding" if v.to_ascii_lowercase().contains("chunked") => chunked = true,
                _ => {}
            }
        }
    }

    let body = if chunked {
        let mut out = Vec::new();
        loop {
            line.clear();
            stream.read_line(&mut line).await?;
            let size = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            stream.read_exact(&mut chunk).await?;
            out.extend_from_slice(&chunk);
            // Trailing CRLF after the chunk.
            let mut crlf = [0u8; 2];
            stream.read_exact(&mut crlf).await?;
        }
        out
    } else if let Some(len) = content_length {
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        buf
    } else {
        Vec::new()
    };
    let text = String::from_utf8_lossy(&body);
    serde_json::from_str(text.trim())
        .with_context(|| format!("parsing Chrome response for {path}: {text}"))
}

async fn http_get(port: u16, path: &str) -> Result<Value> {
    http_request(port, "GET", path).await
}

/// PUT helper for `/json/new?url=…` (Chrome accepts GET or PUT here).
async fn http_put(port: u16, path: &str) -> Result<Value> {
    http_request(port, "PUT", path).await
}

pub async fn list_targets(port: u16) -> Result<Vec<TargetInfo>> {
    let v = http_get(port, "/json/list").await?;
    Ok(serde_json::from_value(v)?)
}

pub async fn version(port: u16) -> Result<Value> {
    http_get(port, "/json/version").await
}

/// Create a new tab. Chrome's `/json/new` needs the URL URL-encoded.
pub async fn new_target(port: u16, url: &str) -> Result<TargetInfo> {
    let encoded: String = url
        .chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~:/?#[]@!$&'()*+,;=%".contains(c) {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect();
    let v = http_put(port, &format!("/json/new?{encoded}")).await?;
    Ok(serde_json::from_value(v)?)
}

pub async fn close_target(port: u16, target_id: &str) -> Result<()> {
    // `/json/close` answers with plain text ("Target is closing"), not
    // JSON — any response (or even a parse failure) means the request
    // reached Chrome, so never propagate the parse error.
    let _ = http_get(port, &format!("/json/close/{target_id}")).await;
    Ok(())
}

/// Find a custom egress CA (e.g. a MITM proxy's CA): `VEIL_BROWSER_CA_CERT`,
/// or the usual locations.
fn find_egress_ca() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VEIL_BROWSER_CA_CERT") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    for p in ["/run/hatch/egress-tls/hatch-egress-ca.pem", "/usr/local/share/ca-certificates/hatch-egress-ca.crt"] {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Import a custom egress CA into an NSS database directory, so TLS through
/// that proxy validates instead of showing a privacy error. Best-effort:
/// if `certutil` is missing or the import fails, Chrome simply keeps its
/// default trust store (fail-closed — MITM'd sites will show a privacy
/// error rather than silently trusting an unknown CA).
fn import_ca_into_db(db_dir: &PathBuf, ca: &PathBuf) {
    let db = format!("sql:{}", db_dir.display());
    let already = std::process::Command::new("certutil")
        .args(["-L", "-d", &db])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("veil-egress-ca"))
        .unwrap_or(false);
    if already {
        return;
    }
    match std::process::Command::new("certutil")
        .args(["-A", "-d", &db, "-t", "CT,C,c", "-n", "veil-egress-ca", "-i"])
        .arg(ca)
        .output()
    {
        Ok(o) if o.status.success() => {
            eprintln!("[veil-browser] imported egress CA {} into {}", ca.display(), db_dir.display());
        }
        Ok(o) => {
            eprintln!(
                "[veil-browser] warning: could not import egress CA ({}): {}",
                ca.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => {
            eprintln!("[veil-browser] warning: certutil unavailable ({e}); TLS via MITM proxy may fail");
        }
    }
}

/// Find Chrome processes whose command line contains
/// `--remote-debugging-port=<port>` (the main process, not renderers —
/// they carry `--type=renderer`) and terminate them. Only used to reclaim
/// a session port from a crashed daemon's orphaned browser.
fn kill_orphan_chrome(debug_port: u16) {
    let needle = format!("--remote-debugging-port={debug_port}");
    let Ok(entries) = std::fs::read_dir("/proc") else { return };
    for entry in entries.flatten() {
        let pid: u32 = match entry.file_name().to_str().and_then(|n| n.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else { continue };
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if cmdline.contains(&needle) && cmdline.contains("chrome") && !cmdline.contains("--type=") {
            // SIGTERM, then SIGKILL if it lingers.
            let _ = std::process::Command::new("kill").arg(pid.to_string()).output();
            for _ in 0..10 {
                if std::fs::read(format!("/proc/{pid}/cmdline")).is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if std::fs::read(format!("/proc/{pid}/cmdline")).is_ok() {
                let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).output();
            }
        }
    }
}

/// Launch Chrome headless with remote debugging, waiting until the
/// `/json/version` endpoint answers (or timing out).
pub async fn launch_chrome(
    chrome_path: &PathBuf,
    profile_dir: &PathBuf,
    debug_port: u16,
    headless: bool,
    window_size: (u32, u32),
    log_file: &PathBuf,
    // Explicit proxy server for Chrome, e.g. the local auth shim
    // (`http://127.0.0.1:<port>`). When set, the env-proxy logic below is skipped.
    proxy_server: Option<String>,
    // Isolated HOME for Chrome. Chrome reads its NSS trust store from
    // `$HOME/.pki/nssdb`; the real HOME's copy can be read-only (as on
    // this host, where the runtime mounts it with a client-auth-only
    // trust entry). Seeding a writable copy here lets Chrome trust the
    // egress CA. `None` = use the process HOME unchanged.
    chrome_home: Option<PathBuf>,
) -> Result<std::process::Child> {
    std::fs::create_dir_all(profile_dir)?;
    // Trust store: import the egress CA into the profile DB (harmless) and
    // into the isolated HOME's NSS DB, which is the one Chrome actually
    // reads for server-cert trust.
    if let Some(ca) = find_egress_ca() {
        import_ca_into_db(profile_dir, &ca);
        if let Some(home) = &chrome_home {
            let nssdb = home.join(".pki").join("nssdb");
            if std::fs::create_dir_all(&nssdb).is_ok() {
                import_ca_into_db(&nssdb, &ca);
            }
        }
    }
    // If a Chrome is already listening on this port, it is an orphan from
    // a crashed daemon (a live daemon would own the session socket, and
    // we would not be launching). Reclaim the port: find the Chrome main
    // process by its command line and terminate it, so the new daemon
    // gets a fresh browser instead of failing forever on a stale port.
    if version(debug_port).await.is_ok() {
        eprintln!("[veil-browser] reclaiming debug port {debug_port} from an orphaned Chrome");
        kill_orphan_chrome(debug_port);
        // Give it a moment to release the port/profile lock.
        for _ in 0..20 {
            if version(debug_port).await.is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if version(debug_port).await.is_ok() {
            anyhow::bail!("could not reclaim debug port {debug_port} from orphaned Chrome");
        }
    }
    // Append, don't truncate: the daemon already wrote startup lines
    // (shim port, CA import) to this file before launching Chrome.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("opening {}", log_file.display()))?;
    let log_err = log.try_clone()?;
    let mut cmd = std::process::Command::new(chrome_path);
    if let Some(home) = &chrome_home {
        cmd.env("HOME", home);
    }
    // Proxy: this environment (and many corporate ones) reaches the
    // internet through an egress proxy in the env vars. Chrome on Linux
    // does not reliably pick those up, so pass them explicitly. The value
    // is never logged. Without a proxy env var, Chrome connects directly.
    if let Some(server) = proxy_server {
        cmd.arg(format!("--proxy-server={server}"));
        // No --proxy-bypass-list: Chrome's default already bypasses
        // loopback, so local test servers are reached directly. (Note:
        // "<-loopback>" would do the opposite — force loopback *through*
        // the proxy.)
    } else if let Ok(proxy) = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy")) {
        if !proxy.is_empty() {
            // Pass only scheme://host:port — Chrome ignores credentials in
            // --proxy-server, and putting them on the command line would
            // expose them in `ps`. Authentication is handled per-request
            // via CDP Fetch auth (see daemon.rs), with fresh credentials
            // supplied by the CLI on every invocation.
            let host_port = proxy
                .split("://")
                .nth(1)
                .unwrap_or(&proxy)
                .rsplit('@')
                .next()
                .unwrap_or(&proxy);
            let scheme = proxy.split("://").next().unwrap_or("http");
            cmd.arg(format!("--proxy-server={scheme}://{host_port}"));
            if let Ok(no_proxy) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
                if !no_proxy.is_empty() {
                    cmd.arg(format!("--proxy-bypass-list={}", no_proxy.replace(',', ";")));
                }
            }
        }
    }
    cmd.arg(format!("--remote-debugging-port={debug_port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--no-sandbox")
        .arg(format!("--window-size={},{}", window_size.0, window_size.1))
        .arg("--hide-scrollbars")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if headless {
        cmd.arg("--headless=new");
    }
    let child = cmd.spawn().with_context(|| format!("launching Chrome at {}", chrome_path.display()))?;

    // Wait for the debug endpoint.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if version(debug_port).await.is_ok() {
            return Ok(child);
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("Chrome did not start its debug endpoint on port {debug_port} within 20s (see {})", log_file.display());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
