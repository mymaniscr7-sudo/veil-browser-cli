//! VEIL Browser CLI — library root.
//!
//! The binary is a thin CLI front-end. All of the real work lives here so
//! the snapshot renderer, key mapping and output formatting can be unit
//! tested without launching Chrome:
//!
//! * [`config`] — session paths, Chrome discovery, env overrides.
//! * [`cdp`] — minimal async Chrome DevTools Protocol client over WebSocket.
//! * [`browser`] — Chrome process management + the HTTP `/json` endpoints.
//! * [`snapshot`] — Accessibility-tree → compact text + stable `@eN` refs.
//! * [`daemon`] — persistent background daemon (Unix socket, NDJSON RPC).
//! * [`client`] — CLI-side daemon client (auto-starts the daemon).
//! * [`output`] — text / JSON result envelopes.
//! * [`keys`] — `press` key-name mapping.

pub mod browser;
pub mod cdp;
pub mod client;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod keys;
pub mod output;
pub mod proxy;
pub mod snapshot;
