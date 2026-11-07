//! Configuration: session directories, Chrome discovery, env overrides.
//!
//! Everything is local-only by design. A session is a directory under
//! `~/.cache/veil-browser/sessions/<name>/` holding the daemon socket,
//! pid/log files and an isolated Chrome profile — so separate AI tasks
//! never share cookies, history or tabs.

use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// A named browser session. The default session is `"default"`.
#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    /// `~/.cache/veil-browser/sessions/<name>/`
    pub dir: PathBuf,
    /// Unix socket the daemon listens on.
    pub socket: PathBuf,
    /// Daemon pid file.
    pub pid_file: PathBuf,
    /// Daemon log file.
    pub log_file: PathBuf,
    /// Isolated Chrome user-data dir for this session.
    pub profile_dir: PathBuf,
    /// Remote-debugging port for this session's Chrome (deterministic,
    /// derived from the session name so two sessions never collide).
    pub debug_port: u16,
}

pub fn base_dir() -> PathBuf {
    std::env::var_os("VEIL_BROWSER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(|h| PathBuf::from(h).join("veil-browser")))
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache").join("veil-browser"))
                .unwrap_or_else(|| PathBuf::from(".veil-browser"))
        })
}

impl Session {
    pub fn new(name: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let debug_port = 9222 + (hasher.finish() % 200) as u16;
        let dir = base_dir().join("sessions").join(sanitize(name));
        Session {
            name: name.to_string(),
            socket: dir.join("daemon.sock"),
            pid_file: dir.join("daemon.pid"),
            log_file: dir.join("daemon.log"),
            profile_dir: dir.join("chrome-profile"),
            dir,
            debug_port,
        }
    }

    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating session dir {}", self.dir.display()))
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Runtime configuration, from env vars with sane defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to a Chrome / Chrome-for-Testing binary.
    pub chrome_path: PathBuf,
    /// Headless mode (default true; `VEIL_BROWSER_HEADLESS=0` shows a window).
    pub headless: bool,
    /// Initial window size.
    pub window_size: (u32, u32),
    /// Default navigation / CDP timeout, seconds.
    pub timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            chrome_path: discover_chrome()?,
            headless: std::env::var("VEIL_BROWSER_HEADLESS")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true),
            window_size: (1280, 900),
            timeout_secs: std::env::var("VEIL_BROWSER_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        })
    }
}

/// Find a Chrome binary: explicit env var first, then the usual locations
/// (including the Chrome-for-Testing cache populated by
/// `scripts/install-chrome.sh`).
pub fn discover_chrome() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VEIL_BROWSER_CHROME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let candidates = [
        home.join(".cache/veil-browser/chrome-linux64/chrome"),
        home.join(".cache/puppeteer/chrome/linux-*/chrome-linux64/chrome"),
        PathBuf::from("/usr/bin/google-chrome"),
        PathBuf::from("/usr/bin/google-chrome-stable"),
        PathBuf::from("/usr/bin/chromium"),
        PathBuf::from("/usr/bin/chromium-browser"),
        PathBuf::from("/opt/google/chrome/chrome"),
    ];
    for c in candidates {
        if c.exists() {
            return Ok(c);
        }
    }
    // Last resort: whatever is on PATH.
    for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome"] {
        if let Ok(out) = std::process::Command::new("which").arg(name).output() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    anyhow::bail!(
        "no Chrome/Chromium binary found. Set VEIL_BROWSER_CHROME or run scripts/install-chrome.sh \
         (downloads Chrome for Testing into ~/.cache/veil-browser/)"
    )
}
