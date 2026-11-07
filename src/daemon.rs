//! Persistent background daemon.
//!
//! One daemon per session, listening on a Unix socket
//! (`~/.cache/veil-browser/sessions/<name>/daemon.sock`). The CLI is a
//! thin client: it sends one NDJSON request per command and the daemon —
//! which already owns the Chrome process and all tab WebSockets — answers.
//! The browser therefore never restarts between commands, which is what
//! makes `veil-browser click @e1` fast after `veil-browser snapshot`.
//!
//! Wire protocol (one JSON object per line, both directions):
//!   request:  {"command": "open", "url": "https://example.com"}
//!   response: {"ok": true, "summary": "…", "data": {...}, "text": "…"}
//!             {"ok": false, "error": "…", "hint": "…"}

use crate::browser::{self, TargetInfo};
use crate::cdp::Cdp;
use crate::config::{Config, Session};
use crate::keys;
use crate::snapshot::{self, AxNode, ElementRef};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

const MAX_READ_CHARS: usize = 20_000;

struct Tab {
    id: String,
    cdp: Arc<Cdp>,
    refs: HashMap<String, ElementRef>,
}

pub struct Daemon {
    session: Session,
    config: Config,
    chrome: Option<std::process::Child>,
    chrome_version: String,
    started_at: Instant,
    tabs: Vec<Tab>,
    active: usize,
    /// Set when `close` removes the last tab — the daemon exits after
    /// answering that request.
    shutdown_after_response: bool,
    /// Current proxy credentials, refreshed by the CLI on every request
    /// (the runtime rotates the egress-proxy token). Used by the per-tab
    /// Fetch handler to answer proxy 407 challenges.
    proxy_auth: Arc<Mutex<Option<(String, String)>>>,
}

fn ok(summary: impl Into<String>) -> Value {
    json!({ "ok": true, "summary": summary.into() })
}
fn ok_data(summary: impl Into<String>, data: Value) -> Value {
    json!({ "ok": true, "summary": summary.into(), "data": data })
}
fn ok_text(summary: impl Into<String>, text: String, data: Value) -> Value {
    json!({ "ok": true, "summary": summary.into(), "text": text, "data": data })
}
fn err(message: impl Into<String>, hint: Option<&str>) -> Value {
    let mut v = json!({ "ok": false, "error": message.into() });
    if let Some(h) = hint {
        v["hint"] = json!(h);
    }
    v
}

impl Daemon {
    pub async fn start(session: Session, config: Config) -> Result<()> {
        session.ensure_dir()?;
        // Remove a stale socket left by a crashed daemon.
        let _ = std::fs::remove_file(&session.socket);
        let log_path = session.log_file.clone();
        // Credentials store, created before Chrome launches so the proxy
        // shim (below) can share it. The CLI refreshes it on every request.
        let proxy_auth: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        // If an egress proxy is configured, start the local auth shim and
        // point Chrome at it (see proxy.rs). Without a proxy, Chrome
        // connects directly.
        let proxy_server = match std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy")) {
            Ok(url) if !url.is_empty() => {
                let shim = crate::proxy::start(&url, proxy_auth.clone()).await?;
                eprintln!("[veil-browser] proxy shim on 127.0.0.1:{}", shim.local_port);
                Some(format!("http://127.0.0.1:{}", shim.local_port))
            }
            _ => None,
        };
        let chrome = browser::launch_chrome(
            &config.chrome_path,
            &session.profile_dir,
            session.debug_port,
            config.headless,
            config.window_size,
            &log_path,
            proxy_server,
            // Isolated HOME so Chrome's NSS trust store is a writable,
            // per-session copy (see browser.rs).
            Some(session.dir.join("chrome-home")),
        )
        .await?;
        let chrome_pid = chrome.id();
        let version = browser::version(session.debug_port)
            .await
            .ok()
            .and_then(|v| v.get("Browser").and_then(|b| b.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();

        eprintln!("[veil-browser] chrome up (pid {chrome_pid}), attaching tabs…");
        let mut daemon = Daemon {
            session: session.clone(),
            config,
            chrome: Some(chrome),
            chrome_version: version,
            started_at: Instant::now(),
            tabs: Vec::new(),
            active: 0,
            shutdown_after_response: false,
            proxy_auth,
        };
        daemon.attach_existing_tabs().await?;
        eprintln!("[veil-browser] attached {} tab(s)", daemon.tabs.len());

        let state = Arc::new(Mutex::new(daemon));
        let listener = UnixListener::bind(&session.socket)
            .with_context(|| format!("binding daemon socket {}", session.socket.display()))?;
        std::fs::write(&session.pid_file, std::process::id().to_string())?;
        eprintln!(
            "[veil-browser] daemon up: session={} pid={} chrome_pid={} port={} socket={}",
            session.name,
            std::process::id(),
            chrome_pid,
            session.debug_port,
            session.socket.display()
        );

        loop {
            let (stream, _) = listener.accept().await?;
            let conn_state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, conn_state.clone()).await {
                    eprintln!("[veil-browser] connection error: {e:#}");
                }
                // If a request asked for shutdown, kill Chrome and exit
                // once its connection is done. (A bare process::exit here
                // used to orphan Chrome, leaving it holding the session's
                // debug port and profile for the next daemon.)
                if conn_state.lock().await.shutdown_after_response {
                    let mut guard = conn_state.lock().await;
                    if let Some(mut chrome) = guard.chrome.take() {
                        let _ = chrome.kill();
                        let _ = chrome.wait();
                    }
                    let _ = std::fs::remove_file(&guard.session.socket);
                    let _ = std::fs::remove_file(&guard.session.pid_file);
                    std::process::exit(0);
                }
            });
            if state.lock().await.shutdown_after_response {
                break;
            }
        }
        // Graceful shutdown: kill Chrome.
        let mut guard = state.lock().await;
        if let Some(mut chrome) = guard.chrome.take() {
            let _ = chrome.kill();
            let _ = chrome.wait();
        }
        let _ = std::fs::remove_file(&session.socket);
        let _ = std::fs::remove_file(&session.pid_file);
        Ok(())
    }

    /// Attach to every page target Chrome already has (normally just the
    /// initial `about:blank` tab).
    async fn attach_existing_tabs(&mut self) -> Result<()> {
        let targets = browser::list_targets(self.session.debug_port).await?;
        for t in targets.into_iter().filter(|t| t.kind == "page") {
            if let Ok(tab) = self.attach_target(&t).await {
                self.tabs.push(tab);
            }
        }
        if self.tabs.is_empty() {
            let t = browser::new_target(self.session.debug_port, "about:blank").await?;
            self.tabs.push(self.attach_target(&t).await?);
        }
        self.active = 0;
        Ok(())
    }

    async fn attach_target(&self, target: &TargetInfo) -> Result<Tab> {
        let ws = target
            .web_socket_debugger_url
            .clone()
            .ok_or_else(|| anyhow!("target {} has no debugger URL", target.id))?;
        let cdp = Cdp::connect(&ws).await?;
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Runtime.enable", json!({})).await?;
        let _ = cdp.call("Accessibility.enable", json!({})).await;
        // Intercept requests so proxy 407 challenges can be answered with
        // fresh credentials (Chrome ignores credentials in --proxy-server
        // and cannot prompt in headless mode). Every paused request is
        // continued immediately; only auth challenges get credentials.
        let fetch = cdp
            .call(
                "Fetch.enable",
                json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }], "handleAuthRequests": true }),
            )
            .await;
        if let Err(e) = &fetch {
            eprintln!("[veil-browser] Fetch.enable failed for tab {}: {e:#}", target.id);
        }
        if fetch.is_ok() {
            if let Some(mut rx) = cdp.take_event_receiver().await {
                let cdp_clone = cdp.clone();
                let auth = self.proxy_auth.clone();
                tokio::spawn(async move {
                    while let Some(ev) = rx.recv().await {
                        let method = ev.get("method").and_then(|m| m.as_str()).unwrap_or("");
                        if method == "Fetch.authRequired" {
                            eprintln!("[veil-browser] proxy/server auth challenge");
                        }
                        let request_id = ev
                            .get("params")
                            .and_then(|p| p.get("requestId"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        match method {
                            "Fetch.requestPaused" => {
                                let _ = cdp_clone
                                    .call("Fetch.continueRequest", json!({ "requestId": request_id }))
                                    .await;
                            }
                            "Fetch.authRequired" => {
                                let creds = auth.lock().await.clone();
                                let response = match creds {
                                    Some((username, password)) => json!({
                                        "response": "ProvideCredentials",
                                        "username": username,
                                        "password": password,
                                    }),
                                    None => json!({ "response": "Default" }),
                                };
                                let _ = cdp_clone
                                    .call(
                                        "Fetch.continueWithAuth",
                                        json!({ "requestId": request_id, "authChallengeResponse": response }),
                                    )
                                    .await;
                            }
                            _ => {}
                        }
                    }
                });
            }
        }
        Ok(Tab { id: target.id.clone(), cdp, refs: HashMap::new() })
    }

    fn active_tab(&mut self) -> Result<&mut Tab> {
        self.tabs.get_mut(self.active).ok_or_else(|| anyhow!("no active tab"))
    }

    async fn current_url_title(tab: &Tab) -> (String, String) {
        let url = tab.cdp.eval_string("location.href").await.unwrap_or_default();
        let title = tab.cdp.eval_string("document.title").await.unwrap_or_default();
        (url, title)
    }

    async fn dispatch(&mut self, req: &Value) -> Value {
        // Refresh proxy credentials on every request (the CLI sends the
        // current ones; the daemon's own env token may have rotated).
        if let (Some(u), Some(p)) = (
            req.get("proxy_auth").and_then(|a| a.get("username")).and_then(|v| v.as_str()),
            req.get("proxy_auth").and_then(|a| a.get("password")).and_then(|v| v.as_str()),
        ) {
            *self.proxy_auth.lock().await = Some((u.to_string(), p.to_string()));
        }
        let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let result: Result<Value> = async {
            match command {
                "ping" => Ok(ok("pong")),
                "open" => self.cmd_open(req).await,
                "back" => self.cmd_history(req, "history.back()").await,
                "forward" => self.cmd_history(req, "history.forward()").await,
                "reload" => self.cmd_reload().await,
                "url" => self.cmd_url().await,
                "title" => self.cmd_title().await,
                "read" => self.cmd_read(req).await,
                "snapshot" => self.cmd_snapshot(req).await,
                "click" => self.cmd_click(req).await,
                "fill" => self.cmd_fill(req, true).await,
                "type" => self.cmd_fill(req, false).await,
                "press" => self.cmd_press(req).await,
                "scroll" => self.cmd_scroll(req).await,
                "wait" => self.cmd_wait(req).await,
                "screenshot" => self.cmd_screenshot(req).await,
                "new-tab" => self.cmd_new_tab(req).await,
                "tabs" => self.cmd_tabs().await,
                "close" => self.cmd_close().await,
                "status" => self.cmd_status().await,
                "stop" => {
                    self.shutdown_after_response = true;
                    Ok(ok("daemon stopping"))
                }
                "" => Err(anyhow!("empty request")),
                other => Err(anyhow!("unknown command: {other}")),
            }
        }
        .await;
        match result {
            Ok(v) => v,
            Err(e) => err(format!("{e:#}"), None),
        }
    }

    // ---- navigation -------------------------------------------------------

    async fn cmd_open(&mut self, req: &Value) -> Result<Value> {
        let url = req.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            anyhow::bail!("open needs a URL: veil-browser open https://example.com");
        }
        let url = normalize_url(&url);
        let tab = self.active_tab()?;
        tab.cdp.call("Page.navigate", json!({ "url": url })).await?;
        tab.cdp.wait_for_load(Duration::from_secs(20)).await?;
        let (cur, title) = Self::current_url_title(tab).await;
        Ok(ok_data(
            format!("opened {cur}"),
            json!({ "url": cur, "title": title }),
        ))
    }

    async fn cmd_history(&mut self, req: &Value, js: &str) -> Result<Value> {
        let tab = self.active_tab()?;
        tab.cdp.eval(js).await?;
        tab.cdp.wait_for_load(Duration::from_secs(15)).await?;
        let (cur, title) = Self::current_url_title(tab).await;
        let verb = if req.get("command").and_then(|c| c.as_str()) == Some("back") { "back to" } else { "forward to" };
        Ok(ok_data(format!("{verb} {cur}"), json!({ "url": cur, "title": title })))
    }

    async fn cmd_reload(&mut self) -> Result<Value> {
        let tab = self.active_tab()?;
        tab.cdp.call("Page.reload", json!({ "ignoreCache": false })).await?;
        tab.cdp.wait_for_load(Duration::from_secs(20)).await?;
        let (cur, title) = Self::current_url_title(tab).await;
        Ok(ok_data(format!("reloaded {cur}"), json!({ "url": cur, "title": title })))
    }

    async fn cmd_url(&mut self) -> Result<Value> {
        let tab = self.active_tab()?;
        let url = tab.cdp.eval_string("location.href").await?;
        Ok(ok_data(url.clone(), json!({ "url": url })))
    }

    async fn cmd_title(&mut self) -> Result<Value> {
        let tab = self.active_tab()?;
        let title = tab.cdp.eval_string("document.title").await?;
        Ok(ok_data(title.clone(), json!({ "title": title })))
    }

    async fn cmd_read(&mut self, req: &Value) -> Result<Value> {
        let max = req.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(MAX_READ_CHARS as u64) as usize;
        let tab = self.active_tab()?;
        let text = tab
            .cdp
            .eval_string("document.body ? document.body.innerText : document.documentElement.innerText")
            .await?;
        let text = collapse_whitespace(&text);
        let (url, title) = Self::current_url_title(tab).await;
        let truncated = text.len() > max;
        let body: String = text.chars().take(max).collect();
        Ok(ok_text(
            format!("read {} chars from {url}{}", body.chars().count(), if truncated { " (truncated)" } else { "" }),
            body,
            json!({ "url": url, "title": title, "chars": text.chars().count(), "truncated": truncated }),
        ))
    }

    // ---- snapshot / interaction ------------------------------------------

    async fn cmd_snapshot(&mut self, req: &Value) -> Result<Value> {
        let interactive = req.get("interactive").and_then(|v| v.as_bool()).unwrap_or(false);
        let tab = self.active_tab()?;
        let res = tab
            .cdp
            .call("Accessibility.getFullAXTree", json!({ "pierce": true }))
            .await?;
        let nodes: Vec<AxNode> = serde_json::from_value(res.get("nodes").cloned().unwrap_or(json!([])))
            .context("parsing accessibility tree")?;
        let snap = snapshot::render(&nodes, interactive);
        let (url, title) = Self::current_url_title(tab).await;
        let count = snap.refs.len();
        tab.refs = snap.refs.clone();
        Ok(ok_text(
            format!(
                "snapshot: {} interactive element{} ({} AX nodes) — {url}",
                count,
                if count == 1 { "" } else { "s" },
                snap.total_nodes
            ),
            snap.text,
            json!({ "url": url, "title": title, "elements": count, "ax_nodes": snap.total_nodes, "interactive_only": interactive }),
        ))
    }

    /// Resolve a ref to a JS objectId for the element.
    async fn resolve_object_id(&mut self, reference: &str) -> Result<String> {
        let tab = self.active_tab()?;
        let backend = tab
            .refs
            .get(reference)
            .and_then(|e| e.backend_dom_node_id)
            .ok_or_else(|| anyhow!("element {reference} has no DOM node — re-run snapshot"))?;
        let res = tab
            .cdp
            .call("DOM.resolveNode", json!({ "backendNodeId": backend }))
            .await?;
        res.get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|o| o.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("could not resolve {reference} to a DOM object"))
    }

    async fn cmd_click(&mut self, req: &Value) -> Result<Value> {
        let reference = snapshot::parse_ref(req.get("ref").and_then(|r| r.as_str()).unwrap_or(""))
            .ok_or_else(|| anyhow!("click needs an element reference, e.g. veil-browser click @e1"))?;
        // Resolve + scroll into view via JS, then click at the centre of the
        // element's box with real mouse events (more faithful than JS click).
        let object_id = self.resolve_object_id(&reference).await?;
        let tab = self.active_tab()?;
        let _ = tab
            .cdp
            .call(
                "Runtime.callFunctionOn",
                json!({ "objectId": object_id, "functionDeclaration": "function(){ this.scrollIntoView({block:'center', inline:'center'}); return this.outerHTML ? this.outerHTML.slice(0,200) : this.tagName; }", "returnByValue": true }),
            )
            .await;
        // Box model, via backend node id.
        let backend = tab.refs.get(&reference).and_then(|e| e.backend_dom_node_id);
        let mut clicked = false;
        if let Some(backend) = backend {
            if let Ok(box_model) = tab.cdp.call("DOM.getBoxModel", json!({ "backendNodeId": backend })).await {
                if let Some(content) = box_model.get("model").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                    if content.len() >= 8 {
                        let xs: Vec<f64> = (0..4).map(|i| content[i * 2].as_f64().unwrap_or(0.0)).collect();
                        let ys: Vec<f64> = (0..4).map(|i| content[i * 2 + 1].as_f64().unwrap_or(0.0)).collect();
                        let x = xs.iter().sum::<f64>() / 4.0;
                        let y = ys.iter().sum::<f64>() / 4.0;
                        tab.cdp.call("Input.dispatchMouseEvent", json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 })).await?;
                        tab.cdp.call("Input.dispatchMouseEvent", json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 })).await?;
                        clicked = true;
                    }
                }
            }
        }
        if !clicked {
            // Fallback: plain JS click.
            tab.cdp
                .call(
                    "Runtime.callFunctionOn",
                    json!({ "objectId": object_id, "functionDeclaration": "function(){ this.click(); return true; }", "returnByValue": true }),
                )
                .await?;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (url, _) = Self::current_url_title(tab).await;
        Ok(ok_data(format!("clicked {reference}"), json!({ "ref": reference, "url": url })))
    }

    async fn cmd_fill(&mut self, req: &Value, instant: bool) -> Result<Value> {
        let reference = snapshot::parse_ref(req.get("ref").and_then(|r| r.as_str()).unwrap_or(""))
            .ok_or_else(|| anyhow!("fill/type needs an element reference, e.g. veil-browser fill @e2 \"text\""))?;
        let text = req.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let object_id = self.resolve_object_id(&reference).await?;
        let tab = self.active_tab()?;
        if instant {
            // `fill`: set the value in one go (native setter + input/change
            // events, so React/Vue-controlled inputs pick it up).
            tab.cdp
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": object_id,
                        "functionDeclaration": "function(text){ this.focus(); var proto = this.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : (this.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype); var setter = Object.getOwnPropertyDescriptor(proto, 'value'); if (setter && setter.set) { setter.set.call(this, text); } else { this.value = text; } this.dispatchEvent(new Event('input', {bubbles:true})); this.dispatchEvent(new Event('change', {bubbles:true})); return this.value; }",
                        "arguments": [{ "value": text }],
                        "returnByValue": true
                    }),
                )
                .await?;
        } else {
            // `type`: focus, then insert text through CDP (goes through the
            // normal editing path, triggering input events per insertion).
            let _ = tab
                .cdp
                .call(
                    "Runtime.callFunctionOn",
                    json!({ "objectId": object_id, "functionDeclaration": "function(){ this.focus(); if (this.setSelectionRange && this.value) { this.setSelectionRange(this.value.length, this.value.length); } return true; }", "returnByValue": true }),
                )
                .await;
            tab.cdp.call("Input.insertText", json!({ "text": text })).await?;
        }
        Ok(ok_data(
            format!("{} {} with {} chars", if instant { "filled" } else { "typed into" }, reference, text.chars().count()),
            json!({ "ref": reference, "chars": text.chars().count() }),
        ))
    }

    async fn cmd_press(&mut self, req: &Value) -> Result<Value> {
        let key_spec = req.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
        let ev = keys::parse_key(&key_spec)
            .ok_or_else(|| anyhow!("unknown key '{key_spec}' — try Enter, Tab, Escape, ArrowDown, a, F5, Ctrl+l"))?;
        let modifiers = keys::modifiers_for(&key_spec);
        let tab = self.active_tab()?;
        let base = json!({ "key": ev.key, "code": ev.code, "windowsVirtualKeyCode": ev.windows_virtual_key_code, "modifiers": modifiers });
        let mut down = base.clone();
        // Full key sequence, as a real keyboard would produce it:
        // rawKeyDown (+text) → char → keyUp. The text on rawKeyDown is
        // load-bearing: without "\r" on Enter, Chrome never triggers a
        // form's implicit submission from a text field.
        down["type"] = json!("rawKeyDown");
        let down_text = ev.text.clone().or_else(|| {
            if ev.key == "Enter" {
                Some("\r".to_string())
            } else {
                None
            }
        });
        if let Some(t) = &down_text {
            down["text"] = json!(t);
        }
        tab.cdp.call("Input.dispatchKeyEvent", down).await?;
        if let Some(t) = &down_text {
            let mut ch = base.clone();
            ch["type"] = json!("char");
            ch["text"] = json!(t);
            let _ = tab.cdp.call("Input.dispatchKeyEvent", ch).await;
        }
        let mut up = base.clone();
        up["type"] = json!("keyUp");
        tab.cdp.call("Input.dispatchKeyEvent", up).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(ok_data(format!("pressed {key_spec}"), json!({ "key": key_spec })))
    }

    async fn cmd_scroll(&mut self, req: &Value) -> Result<Value> {
        let direction = req.get("direction").and_then(|d| d.as_str()).unwrap_or("down").to_string();
        if let Some(reference) = snapshot::parse_ref(&direction) {
            let object_id = self.resolve_object_id(&reference).await?;
            let tab = self.active_tab()?;
            tab.cdp
                .call(
                    "Runtime.callFunctionOn",
                    json!({ "objectId": object_id, "functionDeclaration": "function(){ this.scrollIntoView({block:'center'}); return true; }", "returnByValue": true }),
                )
                .await?;
            return Ok(ok_data(format!("scrolled to {reference}"), json!({ "ref": reference })));
        }
        let tab = self.active_tab()?;
        let js = match direction.to_ascii_lowercase().as_str() {
            "down" => "window.scrollBy({top: window.innerHeight * 0.8, behavior: 'instant'}); window.scrollY",
            "up" => "window.scrollBy({top: -window.innerHeight * 0.8, behavior: 'instant'}); window.scrollY",
            "top" => "window.scrollTo({top: 0, behavior: 'instant'}); window.scrollY",
            "bottom" => "window.scrollTo({top: document.body.scrollHeight, behavior: 'instant'}); window.scrollY",
            "left" => "window.scrollBy({left: -window.innerWidth * 0.8, behavior: 'instant'}); window.scrollX",
            "right" => "window.scrollBy({left: window.innerWidth * 0.8, behavior: 'instant'}); window.scrollX",
            other => anyhow::bail!("unknown scroll direction '{other}' — use down, up, top, bottom, left, right, or @eN"),
        };
        let y = tab.cdp.eval(js).await?;
        Ok(ok_data(format!("scrolled {direction}"), json!({ "direction": direction, "position": y })))
    }

    async fn cmd_wait(&mut self, req: &Value) -> Result<Value> {
        let ms = req.get("ms").and_then(|v| v.as_u64()).unwrap_or(1000);
        let for_load = req.get("for_load").and_then(|v| v.as_bool()).unwrap_or(false);
        if for_load {
            let tab = self.active_tab()?;
            tab.cdp.wait_for_load(Duration::from_secs(20)).await?;
            Ok(ok("page loaded"))
        } else {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(ok_data(format!("waited {ms}ms"), json!({ "ms": ms })))
        }
    }

    async fn cmd_screenshot(&mut self, req: &Value) -> Result<Value> {
        let path = req.get("path").and_then(|p| p.as_str()).unwrap_or("screenshot.png").to_string();
        let full_page = req.get("full_page").and_then(|v| v.as_bool()).unwrap_or(false);
        let tab = self.active_tab()?;
        let res = tab
            .cdp
            .call("Page.captureScreenshot", json!({ "format": "png", "captureBeyondViewport": full_page }))
            .await?;
        let data = res.get("data").and_then(|d| d.as_str()).unwrap_or("");
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .context("decoding screenshot")?;
        tokio::fs::write(&path, &bytes).await.with_context(|| format!("writing {path}"))?;
        Ok(ok_data(
            format!("screenshot saved to {path} ({} bytes)", bytes.len()),
            json!({ "path": path, "bytes": bytes.len(), "full_page": full_page }),
        ))
    }

    // ---- tabs -------------------------------------------------------------

    async fn cmd_new_tab(&mut self, req: &Value) -> Result<Value> {
        let url = req.get("url").and_then(|u| u.as_str()).unwrap_or("about:blank").to_string();
        let url = normalize_url(&url);
        let target = browser::new_target(self.session.debug_port, &url).await?;
        let tab = self.attach_target(&target).await?;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        let tab = self.active_tab()?;
        tab.cdp.wait_for_load(Duration::from_secs(15)).await?;
        let (cur, title) = Self::current_url_title(tab).await;
        Ok(ok_data(
            format!("new tab: {cur}"),
            json!({ "tab_id": target.id, "url": cur, "title": title, "tabs": self.tabs.len() }),
        ))
    }

    async fn cmd_tabs(&mut self) -> Result<Value> {
        let targets = browser::list_targets(self.session.debug_port).await?;
        let mut lines: Vec<String> = Vec::new();
        let mut data: Vec<Value> = Vec::new();
        for (i, t) in targets.iter().filter(|t| t.kind == "page").enumerate() {
            let active = self.tabs.get(self.active).map(|a| a.id == t.id).unwrap_or(false);
            let marker = if active { "*" } else { " " };
            lines.push(format!("{marker} [{i}] {} — {}", t.title, t.url));
            data.push(json!({ "index": i, "id": t.id, "title": t.title, "url": t.url, "active": active }));
        }
        Ok(ok_text(
            format!("{} tab{}", data.len(), if data.len() == 1 { "" } else { "s" }),
            lines.join("\n"),
            json!({ "tabs": data }),
        ))
    }

    async fn cmd_close(&mut self) -> Result<Value> {
        let tab = self.active_tab()?;
        let id = tab.id.clone();
        let (url, _) = Self::current_url_title(tab).await;
        browser::close_target(self.session.debug_port, &id).await?;
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.shutdown_after_response = true;
            return Ok(ok_data(
                format!("closed last tab ({url}); browser stopped"),
                json!({ "url": url, "tabs": 0 }),
            ));
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        Ok(ok_data(format!("closed tab ({url})"), json!({ "url": url, "tabs": self.tabs.len() })))
    }

    async fn cmd_status(&mut self) -> Result<Value> {
        let (url, title) = match self.tabs.get(self.active) {
            Some(tab) => Self::current_url_title(tab).await,
            None => (String::new(), String::new()),
        };
        Ok(ok_data(
            format!(
                "session '{}' — {} tab{}, active: {}",
                self.session.name,
                self.tabs.len(),
                if self.tabs.len() == 1 { "" } else { "s" },
                if url.is_empty() { "(none)".to_string() } else { url.clone() }
            ),
            json!({
                "session": self.session.name,
                "daemon_pid": std::process::id(),
                "chrome_pid": self.chrome.as_ref().map(|c| c.id()),
                "chrome": self.chrome_version,
                "debug_port": self.session.debug_port,
                "uptime_secs": self.started_at.elapsed().as_secs(),
                "tabs": self.tabs.len(),
                "active_url": url,
                "active_title": title,
                "headless": self.config.headless,
            }),
        ))
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<Mutex<Daemon>>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = err(format!("invalid request JSON: {e}"), None);
                writer.write_all(format!("{resp}\n").as_bytes()).await?;
                continue;
            }
        };
        let resp = state.lock().await.dispatch(&req).await;
        writer.write_all(format!("{resp}\n").as_bytes()).await?;
        writer.flush().await?;
        if state.lock().await.shutdown_after_response {
            break;
        }
    }
    Ok(())
}

fn normalize_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("about:") || raw.starts_with("file://") || raw.starts_with("data:") {
        raw.to_string()
    } else if raw.contains('.') && !raw.contains(' ') {
        format!("https://{raw}")
    } else {
        // Treat it as a search query.
        format!("https://duckduckgo.com/?q={}", url_encode(raw))
    }
}

fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~".contains(c) {
                vec![c]
            } else if c == ' ' {
                vec!['+']
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !prev_space {
                out.push('\n');
                prev_space = true;
            }
            continue;
        }
        if !out.is_empty() && !prev_space {
            out.push('\n');
        }
        out.push_str(line);
        prev_space = false;
    }
    out.trim().to_string()
}
