# VEIL Browser CLI — Architecture

## Why this exists

Today's AI agents browse like humans: screenshots, mouse coordinates, HTML dumps. That is slow, token-hungry and brittle — the exact problem VEIL OS exists to solve. The browser should speak the agent's language: **structured, semantic, compact**.

VEIL Browser CLI is the browser component of VEIL OS (see `../veil-os/`): the tool hierarchy from the VEIL research is *semantic adapter first, accessibility tree second, screenshot last*. This CLI implements levels 1–2 directly and keeps level 4 as an explicit fallback.

## Components

```
┌─────────────────────────────┐
│ veil-browser (CLI, clap)    │  stateless, one process per command
│  parses args → NDJSON req   │
└──────────────┬──────────────┘
               │ Unix socket ~/.cache/veil-browser/sessions/<name>/daemon.sock
               │ (auto-starts daemon via setsid if absent)
┌──────────────▼──────────────┐
│ veil-browser __daemon       │  one per session, owns everything below
│  NDJSON RPC server          │
│  tab registry + @eN maps    │
└──────┬──────────────┬───────┘
       │ HTTP /json   │ WebSocket (CDP, one per tab)
┌──────▼──────┐ ┌──────▼──────────────┐
│ Chrome for  │ │ CDP client (cdp.rs) │
│ Testing     │ │  pending map by id  │
│ headless    │ │  load-event Notify  │
└─────────────┘ └─────────────────────┘
```

### Crate layout (`src/`)

| File | Responsibility |
|---|---|
| `main.rs` | CLI entry point; `__daemon` mode; exit codes; text/JSON printing. |
| `cli.rs` | clap definitions; every command → daemon request JSON. |
| `client.rs` | Unix-socket client; auto-starts the daemon (detached via `setsid`) and retries once. |
| `daemon.rs` | NDJSON RPC server; tab registry; **all command implementations** (navigate, snapshot, click, fill, …). |
| `cdp.rs` | Minimal async CDP client: one WebSocket per tab, pending-request map, `Page.loadEventFired` notification, `Runtime.evaluate` helpers, `wait_for_load`. |
| `browser.rs` | Chrome process launch (headless, per-session profile/port) + raw HTTP `/json/list`, `/json/new`, `/json/close`, `/json/version` (no HTTP client crate — localhost debug endpoint only). |
| `snapshot.rs` | `Accessibility.getFullAXTree` → compact text + stable `@eN` refs; pure, unit-tested. |
| `proxy.rs` | Local authenticating-proxy shim (see §7); pure URL parsing, unit-tested. |
| `keys.rs` | `press` key-name → CDP key event mapping. Pure, unit-tested. |
| `config.rs` | Session directories, Chrome discovery, env overrides. |
| `output.rs` | Text / JSON result envelopes. |

No browser-automation framework (no Playwright/Puppeteer/chromiumoxide). The daemon talks straight to Chrome over CDP, which keeps the binary small, startup fast, and the behaviour explicit.

## Key design decisions

### 1. Persistent daemon, stateless CLI

Restarting Chrome per command would cost seconds and lose tabs, cookies and `@eN` refs. The daemon owns the Chrome process and all tab WebSockets; the CLI is a thin client that exits after each command. First use auto-starts the daemon; later commands answer in milliseconds.

### 2. Accessibility tree, not HTML or screenshots

`Accessibility.getFullAXTree` (with `pierce: true`, so shadow DOM is included) is the page's semantic structure — the same tree a screen reader uses. The renderer (`snapshot.rs`):

- collapses `generic`/`none` containers and `ignored` nodes,
- keeps headings, links, buttons, inputs, static text,
- annotates state (`disabled`, `checked`, `focused`, `expanded`) and link URLs,
- truncates long names/values so one line stays one line.

`snapshot --interactive` drops static text entirely — the cheapest view for an agent deciding what to do next.

### 3. Stable element references `@e1`, `@e2`, …

Refs are assigned in document order on each snapshot and stored in the daemon per tab, mapped to Chrome's `backendDOMNodeId`. `click`/`fill`/`type`/`scroll @eN` resolve through `DOM.resolveNode` → JS `objectId`, so the agent never sees a CSS selector, an XPath or a pixel coordinate. Refs reset on each snapshot — the daemon says so in the error if an agent uses a stale one.

Clicking uses real mouse events at the centre of the element's box (`DOM.getBoxModel` + `Input.dispatchMouseEvent`), with a JS `click()` fallback. Filling uses the native value setter plus `input`/`change` events, so framework-controlled inputs (React/Vue) update correctly. Typing focuses the element and uses `Input.insertText`.

### 4. Separate sessions, separate profiles

A session is a directory: `~/.cache/veil-browser/sessions/<name>/` with its own daemon socket, pid/log, Chrome profile and deterministic debug port (hash of the name, 9222–9421). Separate AI tasks never share cookies, history or tabs. This mirrors VEIL's rule from the research: *browser: separate profile per task, no host cookies/passwords/SSH*.

### 5. Two output modes, one envelope

Text is the default (low-token). `--json` wraps the same data in `{"ok", "command", "session", "summary", "data", "text"}`. Errors are `{"ok": false, "error": {"message"}}` with exit code 1, in both modes.

### 6. Local-only by construction

The daemon listens on a Unix socket in the user's cache dir — never TCP. Chrome runs headless with an isolated profile. Nothing in this CLI bypasses the VEIL Permission Broker: it is a *capability* the broker hands to an agent, and network/navigation policy (allowlisted domains, confirm-before-publish/purchase, …) belongs in the broker, not here.

### 7. Proxy and TLS in restricted environments (`proxy.rs`, `browser.rs`)

Two environment problems, both found the hard way while testing on the dev host:

- **Chrome's debug HTTP endpoints keep connections alive**, so the first implementation's `read_to_end` hung forever. The raw HTTP client now reads exactly `Content-Length` bytes (or the chunked body).
- **Authenticating egress proxy:** Chrome ignores credentials in `--proxy-server` and can't prompt headless, and the runtime rotates the egress token per invocation. So the daemon runs a local shim on 127.0.0.1 that adds `Proxy-Authorization` from credentials the CLI refreshes on every request (kept in memory only, never logged or put on a command line). A `Fetch.enable` interceptor also answers proxy/server 407 challenges via CDP as a second path.
- **MITM TLS:** the egress proxy terminates TLS with its own CA. Chrome reads server-cert trust from `$HOME/.pki/nssdb`, which on this host is a read-only tmpfs mount holding the CA with client-auth-only trust (`C,,`). Importing the CA into the Chrome profile alone does nothing. The fix: launch Chrome with an isolated `HOME` (`<session>/chrome-home`) whose `.pki/nssdb` is a writable copy seeded with the CA at `CT,C,c` trust via `certutil`. No `--ignore-certificate-errors` anywhere — validation still happens, against the right CA.
- **Orphan recovery:** `stop` kills Chrome on shutdown (an earlier `process::exit` in the connection task used to orphan it, leaving the port and profile locked). If a daemon did crash, the next start reclaims its debug port by terminating the orphaned Chrome main process for that port.

### 8. CDP details that bit us

- `Accessibility.getFullAXTree` returns `backendDOMNodeId` (capital DOM) — serde's `camelCase` would parse it as `backendDomNodeId` and silently yield `None`, breaking every click/fill. The field has an explicit `#[serde(rename)]`, plus a regression test that parses real Chrome JSON.
- Only AX nodes *with* a backend DOM id get a `@eN` ref; nodes on Chrome's internal error pages have none, and a ref that can never resolve is worse than no ref.
- `InlineTextBox` nodes duplicate their parent `StaticText` line by line — the renderer skips them.
- `press Enter` must dispatch `rawKeyDown` *with* `text: "\r"` plus a `char` event; a bare `keyDown` never triggers a form's implicit submission from a text field.
- Chrome's `/json/close` answers with plain text (`Target is closing`), not JSON — the client must not try to parse it.

## Daemon wire protocol

One JSON object per line over the Unix socket, both directions.

Request: `{"command": "open", "url": "https://example.com"}`

Response (success): `{"ok": true, "summary": "opened https://example.com/", "data": {"url": …, "title": …}}`
Response (with body): `{"ok": true, "summary": "snapshot: …", "text": "…lines…", "data": {"elements": 2, …}}`
Response (error): `{"ok": false, "error": "unknown element @e9 — run … first"}`

Commands: `ping`, `open`, `back`, `forward`, `reload`, `url`, `title`, `read`, `snapshot`, `click`, `fill`, `type`, `press`, `scroll`, `wait`, `screenshot`, `new-tab`, `tabs`, `close`, `status`, `stop`.

## Navigation waiting

After `open`/`reload`/history navigation the daemon waits for `document.readyState === 'complete'` (polling + `Page.loadEventFired` notification, 15–20 s timeout, best-effort — a slow page never fails the command outright). `wait --for-load` exposes the same wait explicitly.

## What v1 deliberately does not do

- No downloads, file uploads, or login automation (those are broker-gated actions in VEIL, not browser primitives).
- No JavaScript evaluation command for agents (keeps the surface auditable; add it behind a broker capability if ever needed).
- No multi-window / headed mode by default (`VEIL_BROWSER_HEADLESS=0` exists for debugging).
- No screenshot-by-default — `screenshot` is an explicit opt-in fallback.

## Testing

- `cargo test` — unit tests for the snapshot renderer, ref parsing and key mapping, plus integration tests in `tests/` (no Chrome needed).
- `scripts/e2e.sh` — live test against example.com: open, url/title, read, snapshot, click → iana.org, back, scroll/press/wait, new-tab/tabs/close, screenshot, JSON mode, stop.
