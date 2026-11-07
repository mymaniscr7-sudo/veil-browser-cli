//! Command-line interface definition (clap).
//!
//! Every command maps 1:1 to a daemon request. The CLI itself owns no
//! browser state — that is the daemon's job — so `veil-browser` stays fast
//! and stateless between invocations.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "veil-browser", version, about = "VEIL Browser CLI — a fast command-line browser for AI agents")]
pub struct Cli {
    /// Browser session (separate sessions = separate profiles/tabs).
    /// Env: VEIL_BROWSER_SESSION
    #[arg(long, global = true, env = "VEIL_BROWSER_SESSION", default_value = "default")]
    pub session: String,

    /// Machine-readable JSON output. Env: VEIL_BROWSER_JSON=1
    #[arg(long, global = true, env = "VEIL_BROWSER_JSON")]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Open a URL in the active tab (starts the browser if needed).
    Open {
        /// URL, bare domain (example.com), or search query.
        url: String,
    },
    /// Go back in history.
    Back,
    /// Go forward in history.
    Forward,
    /// Reload the active tab.
    Reload,
    /// Print the active tab's URL.
    Url,
    /// Print the active tab's title.
    Title,
    /// Print the page's visible text (compact, whitespace-collapsed).
    Read {
        /// Maximum characters to return (default 20000).
        #[arg(long)]
        max_chars: Option<u64>,
    },
    /// Print the accessibility-tree snapshot with stable @eN references.
    Snapshot {
        /// Only interactive elements (cheapest view for choosing an action).
        #[arg(long)]
        interactive: bool,
    },
    /// Click an element from the last snapshot.
    Click {
        /// Element reference, e.g. @e1.
        #[arg(value_name = "REF")]
        reference: String,
    },
    /// Set an input's value instantly (native setter + input/change events).
    Fill {
        /// Element reference, e.g. @e2.
        #[arg(value_name = "REF")]
        reference: String,
        /// Text to put in the field.
        text: String,
    },
    /// Type into an element through the normal editing path.
    Type {
        /// Element reference, e.g. @e2.
        #[arg(value_name = "REF")]
        reference: String,
        /// Text to type.
        text: String,
    },
    /// Press a key: Enter, Tab, Escape, ArrowDown, a, F5, Ctrl+l …
    Press {
        /// Key name.
        key: String,
    },
    /// Scroll the page (down, up, top, bottom, left, right) or to an element (@eN).
    Scroll {
        /// Direction, or an element reference.
        #[arg(default_value = "down")]
        direction: String,
    },
    /// Wait. Default 1000ms; pass milliseconds, or --for-load to wait for the page.
    Wait {
        /// Milliseconds to wait (default 1000).
        ms: Option<u64>,
        /// Wait until the page reports load complete instead.
        #[arg(long)]
        for_load: bool,
    },
    /// Save a screenshot (optional fallback — snapshots are the default).
    Screenshot {
        /// Output file, e.g. output.png.
        #[arg(default_value = "screenshot.png")]
        path: String,
        /// Capture the full page, not just the viewport.
        #[arg(long)]
        full_page: bool,
    },
    /// Open a URL in a new tab and switch to it.
    #[command(name = "new-tab")]
    NewTab {
        /// URL to open (default about:blank).
        url: Option<String>,
    },
    /// List tabs. The active tab is marked with *.
    Tabs,
    /// Close the active tab (stops the browser if it was the last tab).
    Close,
    /// Show daemon / browser status for this session.
    Status,
    /// Stop the daemon (and Chrome) for this session.
    Stop,

    /// Internal: run the persistent daemon. Not for direct use.
    #[command(hide = true, name = "__daemon")]
    Daemon,
}

impl Command {
    /// The daemon request name for this command.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Open { .. } => "open",
            Command::Back => "back",
            Command::Forward => "forward",
            Command::Reload => "reload",
            Command::Url => "url",
            Command::Title => "title",
            Command::Read { .. } => "read",
            Command::Snapshot { .. } => "snapshot",
            Command::Click { .. } => "click",
            Command::Fill { .. } => "fill",
            Command::Type { .. } => "type",
            Command::Press { .. } => "press",
            Command::Scroll { .. } => "scroll",
            Command::Wait { .. } => "wait",
            Command::Screenshot { .. } => "screenshot",
            Command::NewTab { .. } => "new-tab",
            Command::Tabs => "tabs",
            Command::Close => "close",
            Command::Status => "status",
            Command::Stop => "stop",
            Command::Daemon => "__daemon",
        }
    }

    /// Build the daemon request JSON for this command.
    pub fn to_request(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Command::Open { url } => json!({ "command": "open", "url": url }),
            Command::Read { max_chars } => json!({ "command": "read", "max_chars": max_chars }),
            Command::Snapshot { interactive } => json!({ "command": "snapshot", "interactive": interactive }),
            Command::Click { reference } => json!({ "command": "click", "ref": reference }),
            Command::Fill { reference, text } => json!({ "command": "fill", "ref": reference, "text": text }),
            Command::Type { reference, text } => json!({ "command": "type", "ref": reference, "text": text }),
            Command::Press { key } => json!({ "command": "press", "key": key }),
            Command::Scroll { direction } => json!({ "command": "scroll", "direction": direction }),
            Command::Wait { ms, for_load } => json!({ "command": "wait", "ms": ms, "for_load": for_load }),
            Command::Screenshot { path, full_page } => {
                json!({ "command": "screenshot", "path": path, "full_page": full_page })
            }
            Command::NewTab { url } => json!({ "command": "new-tab", "url": url }),
            _ => json!({ "command": self.name() }),
        }
    }
}
