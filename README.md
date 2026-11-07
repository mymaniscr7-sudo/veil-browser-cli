# VEIL Browser CLI

**A fast command-line web browser designed specifically for AI agents.**

Not another visual browser. VEIL Browser CLI lets an AI understand and control websites using the smallest, fastest, most reliable output possible.

## Core principle

The browser returns **compact structured text and accessibility information** instead of sending large screenshots or full HTML to the AI. Screenshots are only an optional fallback.

```bash
$ veil-browser open https://example.com
opened https://example.com/

$ veil-browser snapshot
snapshot: 2 interactive elements (14 AX nodes) — https://example.com/
  [heading] "Example Domain"
  [link] "Learn more" (https://www.iana.org/domains/example)
```

That's the whole interaction model: `snapshot` to see, `@e1` to act. No pixels, no HTML soup, no guessing selectors.

## Quick start

```bash
# 1. Install Chrome for Testing (or set VEIL_BROWSER_CHROME to any Chrome/Chromium)
./scripts/install-chrome.sh

# 2. Build
cargo build --release
sudo install -m 755 target/release/veil-browser /usr/local/bin/veil-browser
# …or just use ./target/debug/veil-browser while developing

# 3. Browse
veil-browser open https://example.com
veil-browser snapshot
veil-browser click @e1
veil-browser back
```

The first command auto-starts a persistent background daemon, so the browser doesn't restart for every command. Subsequent commands answer in milliseconds.

## Commands

| Command | What it does |
|---|---|
| `veil-browser open <url>` | Open a URL in the active tab. Bare domains (`example.com`) and search queries also work. |
| `veil-browser back` / `forward` | History navigation. |
| `veil-browser reload` | Reload the active tab. |
| `veil-browser url` / `title` | Print the active tab's URL / title. |
| `veil-browser read` | Print the page's visible text, whitespace-collapsed (`--max-chars N`). |
| `veil-browser snapshot` | Accessibility-tree snapshot with stable `@eN` refs. |
| `veil-browser snapshot --interactive` | Only interactive elements — the cheapest view for choosing the next action. |
| `veil-browser click @e1` | Click an element (real mouse events at its centre; JS click fallback). |
| `veil-browser fill @e2 "text"` | Set an input's value instantly (native setter + `input`/`change` events, so React/Vue inputs pick it up). |
| `veil-browser type @e2 "text"` | Type into an element through the normal editing path. |
| `veil-browser press Enter` | Press a key: `Enter`, `Tab`, `Escape`, `ArrowDown`, `a`, `F5`, `Ctrl+l`, … |
| `veil-browser scroll down` | Scroll `down` / `up` / `top` / `bottom` / `left` / `right`, or to an element (`scroll @e3`). |
| `veil-browser wait [ms]` | Wait (default 1000 ms); `wait --for-load` waits for the page. |
| `veil-browser screenshot output.png` | Save a PNG — the optional fallback, not the default. `--full-page` for the whole page. |
| `veil-browser new-tab <url>` | Open a URL in a new tab and switch to it. |
| `veil-browser tabs` | List tabs; the active tab is marked `*`. |
| `veil-browser close` | Close the active tab (stops the browser if it was the last tab). |
| `veil-browser status` | Daemon / Chrome / session status. |
| `veil-browser stop` | Stop the daemon (and Chrome) for this session. |

## Output modes

**Text (default)** — compact, low-token, made for an AI to read:

```
snapshot: 2 interactive elements (14 AX nodes) — https://example.com/
  [heading] "Example Domain"
  [link] "Learn more" (https://www.iana.org/domains/example)
```

**JSON (`--json` or `VEIL_BROWSER_JSON=1`)** — a stable envelope for scripts:

```json
{
  "ok": true,
  "command": "url",
  "session": "default",
  "summary": "https://example.com/",
  "data": { "url": "https://example.com/" }
}
```

Errors use the same envelope with `"ok": false` and an `error.message` (exit code 1).

## Sessions

Separate AI tasks get separate browsers:

```bash
veil-browser --session research open https://example.com
veil-browser --session shopping open https://example.com
# or: VEIL_BROWSER_SESSION=research veil-browser open …
```

Each session has its own isolated Chrome profile (cookies, history, logins), its own daemon, its own debug port and its own Unix socket under `~/.cache/veil-browser/sessions/<name>/`. Tasks never leak tabs or state into each other.

## A typical agent loop

```bash
veil-browser open https://news.ycombinator.com
veil-browser snapshot --interactive     # what can I do here?
veil-browser click @e5                  # open a story
veil-browser read                       # read it
veil-browser snapshot --interactive     # what's next?
veil-browser fill @e2 "search term"     # fill the search box
veil-browser press Enter
veil-browser wait --for-load
veil-browser snapshot
```

Only reach for `screenshot` when the accessibility tree genuinely can't answer the question (a canvas, a CAPTCHA, a visual layout check).

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `VEIL_BROWSER_CHROME` | auto-discovered | Path to a Chrome / Chrome-for-Testing binary. |
| `VEIL_BROWSER_SESSION` | `default` | Session name (same as `--session`). |
| `VEIL_BROWSER_JSON` | off | `1` = JSON output (same as `--json`). |
| `VEIL_BROWSER_HEADLESS` | `1` | `0` = show a real browser window. |
| `VEIL_BROWSER_TIMEOUT` | `30` | CDP/navigation timeout, seconds. |
| `VEIL_BROWSER_HOME` | `~/.cache/veil-browser` | Where sessions, profiles and Chrome live. |
| `VEIL_BROWSER_CA_CERT` | auto-discovered | Custom egress CA to trust (see below). |

## Behind an authenticating proxy / MITM egress

If your network reaches the internet through a proxy that requires auth (or terminates TLS with its own CA), the daemon handles it automatically:

- **Proxy:** Chrome is pointed at a local shim (`proxy.rs`) on 127.0.0.1, which forwards to the egress proxy from `HTTPS_PROXY`/`https_proxy`, adding `Proxy-Authorization`. Chrome ignores credentials in `--proxy-server` and can't prompt in headless mode, so the shim is what makes auth work — and the CLI refreshes the credentials on *every* invocation, because the runtime rotates the egress token and a long-lived daemon must not rely on the token it started with. Credentials never appear on a command line or in a log.
- **TLS:** if a custom CA exists (`VEIL_BROWSER_CA_CERT`, `/run/hatch/egress-tls/hatch-egress-ca.pem`, or `/usr/local/share/ca-certificates/hatch-egress-ca.crt`), it is imported with `certutil` into a writable, per-session NSS database at `<session>/chrome-home/.pki/nssdb`, and Chrome is launched with `HOME` pointed there. (Chrome reads server-cert trust from `$HOME/.pki/nssdb`, which on some hosts is a read-only mount — importing into the profile alone does not work.)
- **Orphan recovery:** if a previous daemon crashed, the next start reclaims its debug port by terminating the orphaned Chrome main process for that port, instead of failing forever.

Without a proxy env var, none of this activates and Chrome connects directly.

## Architecture

Rust CLI + persistent daemon, Chrome/Chromium for Testing as the engine, Chrome DevTools Protocol over WebSocket, accessibility-tree snapshots for page understanding. See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design, the daemon wire protocol, and the security model.

## Development

```bash
cargo test                 # unit + snapshot tests (no Chrome needed)
./scripts/e2e.sh           # live end-to-end test against example.com (needs network + Chrome)
cargo build --release
```

## Security

The daemon listens only on a Unix socket in the user's own cache directory — never on TCP. Each session's Chrome profile is isolated and contains no host cookies, passwords or SSH keys. The browser is a capability VEIL hands to an agent; it is not a way around the VEIL Permission Broker — network and navigation policy belong in the broker, and this CLI is designed to sit behind it (see `veil-os/`).

## License

MIT. Part of [VEIL OS](../veil-os/README.md) — an AI-native operating environment, with humans in control.
