//! Minimal async CDP client over a WebSocket.
//!
//! Deliberately small: one connection per tab, a pending-request map keyed
//! by message id, and a `Notify` that fires on `Page.loadEventFired`.
//! No browser-automation framework — the daemon talks straight to Chrome,
//! which keeps startup fast and the dependency tree tiny.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A live CDP session for one tab (its own WebSocket).
pub struct Cdp {
    write: Mutex<futures_util::stream::SplitSink<Ws, Message>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// Fired whenever the page finishes loading.
    pub load_notify: Arc<Notify>,
    /// Fired on any `Page.frameNavigated` / load event — used by waiters.
    pub navigated: Arc<Notify>,
    /// Channel carrying CDP *events* (method notifications) to the
    /// per-tab handler task — e.g. `Fetch.requestPaused` /
    /// `Fetch.authRequired`. The reader task forwards them here instead
    /// of handling them inline, so a handler can make CDP calls of its
    /// own without deadlocking the reader.
    event_tx: mpsc::UnboundedSender<Value>,
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
}

impl Cdp {
    /// Connect to a target's `webSocketDebuggerUrl`.
    pub async fn connect(ws_url: &str) -> Result<Arc<Self>> {
        let (ws, _) = connect_async(ws_url)
            .await
            .with_context(|| format!("connecting to CDP at {ws_url}"))?;
        let (write, mut read) = ws.split();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cdp = Arc::new(Cdp {
            write: Mutex::new(write),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            load_notify: Arc::new(Notify::new()),
            navigated: Arc::new(Notify::new()),
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
        });

        // Reader task: route responses to their waiter, surface load events.
        let weak = Arc::downgrade(&cdp);
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                let Some(cdp) = weak.upgrade() else { break };
                let Ok(Message::Text(text)) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                    let tx = cdp.pending.lock().await.remove(&id);
                    if let Some(tx) = tx {
                        if let Some(err) = v.get("error") {
                            let _ = tx.send(Err(err.to_string()));
                        } else {
                            let _ = tx.send(Ok(v.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                } else if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                    match method {
                        "Page.loadEventFired" => {
                            cdp.load_notify.notify_waiters();
                            cdp.navigated.notify_waiters();
                        }
                        "Page.frameNavigated" => cdp.navigated.notify_waiters(),
                        _ => {}
                    }
                    // Forward every event to the per-tab handler task
                    // (Fetch interception, …). Best-effort: if no handler
                    // is listening, the event is simply dropped.
                    let _ = cdp.event_tx.send(v);
                }
            }
        });
        Ok(cdp)
    }

    /// Take the event receiver (once — the per-tab handler task owns it).
    pub async fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<Value>> {
        self.event_rx.lock().await.take()
    }

    /// Send one CDP command and wait for its result (default 30 s).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_timeout(method, params, Duration::from_secs(30)).await
    }

    pub async fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "id": id, "method": method, "params": params });
        {
            let mut w = self.write.lock().await;
            w.send(Message::Text(msg.to_string().into()))
                .await
                .context("sending CDP message")?;
        }
        let res = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| anyhow!("CDP call {method} timed out after {timeout:?}"))?
            .map_err(|_| anyhow!("CDP connection closed during {method}"))?;
        res.map_err(|e| anyhow!("CDP {method} failed: {e}"))
    }

    /// `Runtime.evaluate` — returns the JS value (unwrapped from the
    /// RemoteObject), or an error with the JS exception text.
    pub async fn eval(&self, expression: &str) -> Result<Value> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(exc) = res.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("JavaScript exception");
            anyhow::bail!("{text}");
        }
        Ok(res
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Evaluate and return a string, for `url` / `title` / `read`.
    pub async fn eval_string(&self, expression: &str) -> Result<String> {
        match self.eval(expression).await? {
            Value::String(s) => Ok(s),
            other => Ok(other.to_string()),
        }
    }

    /// Wait until the page reports a load event or `readyState === 'complete'`,
    /// polling as a fallback (some navigations never fire a clean load event
    /// in headless mode).
    pub async fn wait_for_load(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Value::String(state)) = self.eval("document.readyState").await {
                if state == "complete" {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(()); // best effort — don't fail a navigation on this
            }
            let remaining = deadline - tokio::time::Instant::now();
            let wait = tokio::time::timeout(remaining.min(Duration::from_millis(300)), self.load_notify.notified());
            let _ = wait.await;
        }
    }
}
