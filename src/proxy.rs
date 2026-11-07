//! Authenticating-proxy shim.
//!
//! The problem: many environments (including this dev host) reach the
//! internet through an egress proxy that requires HTTP Basic auth, with a
//! token that rotates. Chrome ignores credentials in `--proxy-server` and
//! cannot prompt for them in headless mode, so a direct `--proxy-server`
//! flag fails with `ERR_TUNNEL_CONNECTION_FAILED`.
//!
//! The shim fixes that without putting credentials on any command line:
//! Chrome talks to a local, unauthenticated proxy on 127.0.0.1; the shim
//! forwards each request to the real egress proxy, adding a
//! `Proxy-Authorization: Basic …` header built from the *current*
//! credentials — which the CLI refreshes on every invocation (see
//! `client.rs`), because the daemon itself was started under an older,
//! possibly rotated token.
//!
//! Only used when `HTTPS_PROXY`/`https_proxy` is set. Without a proxy env
//! var, Chrome connects directly and no shim is started.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub type AuthStore = Arc<Mutex<Option<(String, String)>>>;

/// Parse `https://user:pass@host:port` → (host, port, fallback credentials).
pub fn parse_proxy_url(url: &str) -> Option<(String, u16, Option<(String, String)>)> {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let (userinfo, hostport) = match after_scheme.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, after_scheme),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (hostport.to_string(), 3128),
    };
    let creds = userinfo.and_then(|u| u.split_once(':')).map(|(a, b)| (a.to_string(), b.to_string()));
    Some((host, port, creds))
}

pub struct ProxyShim {
    /// Local port Chrome should use: `--proxy-server=http://127.0.0.1:<port>`.
    pub local_port: u16,
}

/// Start the shim. `auth` is the shared credential store the CLI refreshes.
pub async fn start(egress_url: &str, auth: AuthStore) -> Result<ProxyShim> {
    let (host, port, fallback) = parse_proxy_url(egress_url)
        .ok_or_else(|| anyhow::anyhow!("cannot parse proxy URL"))?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("binding proxy shim")?;
    let local_port = listener.local_addr()?.port();
    let fallback = fallback.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let auth = auth.clone();
            let fallback = fallback.clone();
            let host = host.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(stream, &host, port, auth, fallback).await {
                    eprintln!("[veil-browser] proxy shim: {e:#}");
                }
            });
        }
    });
    Ok(ProxyShim { local_port })
}

async fn read_headers(reader: &mut BufReader<TcpStream>) -> Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("connection closed while reading headers");
        }
        buf.extend_from_slice(line.as_bytes());
        if line == "\r\n" || line == "\n" {
            break;
        }
        if buf.len() > 65536 {
            anyhow::bail!("headers too large");
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    Ok((text, buf))
}

fn basic_auth(auth: &Option<(String, String)>) -> Option<String> {
    auth.as_ref().map(|(u, p)| {
        use base64::Engine;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
        )
    })
}

async fn handle_conn(
    stream: TcpStream,
    egress_host: &str,
    egress_port: u16,
    auth: AuthStore,
    fallback: Option<(String, String)>,
) -> Result<()> {
    let mut client = BufReader::new(stream);
    let (head, raw_head) = read_headers(&mut client).await?;
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("").to_string();

    let creds = auth.lock().await.clone().or(fallback);
    let auth_header = basic_auth(&creds);

    let mut upstream = TcpStream::connect((egress_host, egress_port))
        .await
        .with_context(|| format!("connecting to egress proxy {egress_host}:{egress_port}"))?;

    if method.eq_ignore_ascii_case("CONNECT") {
        // HTTPS tunnel: CONNECT target, then splice raw bytes.
        let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(a) = &auth_header {
            req.push_str(&format!("Proxy-Authorization: {a}\r\n"));
        }
        req.push_str("Proxy-Connection: Keep-Alive\r\n\r\n");
        upstream.write_all(req.as_bytes()).await?;
        upstream.flush().await?;
        let mut up_reader = BufReader::new(upstream);
        let (resp_head, _) = read_headers(&mut up_reader).await?;
        let status = resp_head.lines().next().unwrap_or("");
        let mut client_stream = client.into_inner();
        client_stream.write_all(resp_head.as_bytes()).await?;
        if !status.contains(" 200") {
            anyhow::bail!("proxy CONNECT {target} failed: {}", status.trim());
        }
        tokio::io::copy_bidirectional(&mut client_stream, &mut up_reader).await?;
    } else {
        // Plain HTTP: forward the request, injecting the auth header.
        let mut req = raw_head;
        if let Some(a) = &auth_header {
            // Insert before the final blank line ("\r\n\r\n").
            let insert = format!("Proxy-Authorization: {a}\r\n");
            let pos = req.windows(4).position(|w| w == b"\r\n\r\n");
            if let Some(at) = pos {
                // Insert after the CRLF ending the last header line, i.e.
                // before the final blank line's CRLF.
                let at = at + 2;
                let mut out = Vec::with_capacity(req.len() + insert.len());
                out.extend_from_slice(&req[..at]);
                out.extend_from_slice(insert.as_bytes());
                out.extend_from_slice(&req[at..]);
                req = out;
            }
        }
        upstream.write_all(&req).await?;
        upstream.flush().await?;
        let mut client_stream = client.into_inner();
        tokio::io::copy_bidirectional(&mut client_stream, &mut upstream).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxy_url_forms() {
        let (host, port, creds) = parse_proxy_url("http://user:pass@proxy.example:8080").unwrap();
        assert_eq!(host, "proxy.example");
        assert_eq!(port, 8080);
        assert_eq!(creds, Some(("user".into(), "pass".into())));
        let (host, port, creds) = parse_proxy_url("http://proxy.example:3128").unwrap();
        assert_eq!(host, "proxy.example");
        assert_eq!(port, 3128);
        assert_eq!(creds, None);
    }
}
