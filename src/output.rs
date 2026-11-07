//! Result envelopes: compact text for humans/AI, JSON for machines.
//!
//! Text mode is the default because it is the low-token mode — a few lines
//! of structured text instead of a screenshot or a dump of HTML. JSON mode
//! (`--json` / `VEIL_BROWSER_JSON=1`) wraps the same data in a stable
//! envelope so scripts can parse it reliably.

use serde::Serialize;
use serde_json::{json, Value};

/// What a command produced, before formatting.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub command: String,
    pub session: String,
    /// Short one-line summary, always present in text mode.
    pub summary: String,
    /// Structured payload (JSON mode, and the `data` the daemon returns).
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
    /// Optional multi-line body for text mode (snapshot, read, tabs…).
    #[serde(skip)]
    pub body: Option<String>,
}

impl Outcome {
    pub fn new(command: impl Into<String>, session: impl Into<String>, summary: impl Into<String>) -> Self {
        Outcome {
            command: command.into(),
            session: session.into(),
            summary: summary.into(),
            data: Value::Null,
            body: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Render for `--json`.
    pub fn to_json(&self) -> String {
        let mut v = json!({
            "ok": true,
            "command": self.command,
            "session": self.session,
            "summary": self.summary,
        });
        if !self.data.is_null() {
            v["data"] = self.data.clone();
        }
        if let Some(body) = &self.body {
            v["text"] = json!(body);
        }
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
    }

    /// Render for text mode: summary line, then the body verbatim.
    pub fn to_text(&self) -> String {
        match &self.body {
            Some(b) if !b.is_empty() => format!("{}\n{}", self.summary, b),
            _ => self.summary.clone(),
        }
    }
}

/// Error envelope, shared by text and JSON modes.
pub fn error_json(command: &str, session: &str, message: &str, hint: Option<&str>) -> String {
    let mut v = json!({
        "ok": false,
        "command": command,
        "session": session,
        "error": { "message": message },
    });
    if let Some(h) = hint {
        v["error"]["hint"] = json!(h);
    }
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
}

pub fn print_outcome(outcome: &Outcome, json_mode: bool) {
    if json_mode {
        println!("{}", outcome.to_json());
    } else {
        println!("{}", outcome.to_text());
    }
}

pub fn print_error(command: &str, session: &str, err: &anyhow::Error, json_mode: bool) {
    if json_mode {
        // Surface the first line as the message; the hint (if any) is the
        // remainder. Callers embed hints in the error chain with context.
        let msg = format!("{err:#}");
        eprintln!("{}", error_json(command, session, &msg, None));
    } else {
        eprintln!("error: {err:#}");
    }
}
