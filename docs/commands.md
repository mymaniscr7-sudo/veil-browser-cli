# Command reference

All commands accept the global flags `--session <name>` and `--json`
(or `VEIL_BROWSER_SESSION` / `VEIL_BROWSER_JSON=1`).

## Navigation

```bash
veil-browser open https://example.com   # also: open example.com, open "search query"
veil-browser back
veil-browser forward
veil-browser reload
veil-browser url                              # https://example.com/
veil-browser title                            # Example Domain
```

`open` waits for the page to load (up to 20 s, best-effort) before returning.

## Reading a page

```bash
veil-browser read                     # visible text, whitespace-collapsed
veil-browser read --max-chars 5000    # truncate to 5000 chars
veil-browser snapshot                 # accessibility tree + @eN refs
veil-browser snapshot --interactive   # only actionable elements
```

Example snapshot:

```
snapshot: 2 interactive elements (14 AX nodes) — https://example.com/
  [heading] "Example Domain"
  [link] "Learn more" (https://www.iana.org/domains/example)
```

Refs are assigned in document order and are valid until the next `snapshot` on that tab.

## Acting on a page

```bash
veil-browser click @e1
veil-browser fill @e2 "hello@example.com"   # set value instantly
veil-browser type @e2 "hello"              # type through the editing path
veil-browser press Enter
veil-browser press Tab
veil-browser press ArrowDown
veil-browser press Ctrl+l
veil-browser scroll down                   # down | up | top | bottom | left | right
veil-browser scroll @e3                    # scroll an element into view
veil-browser wait                          # wait 1000 ms
veil-browser wait 2500                     # wait 2500 ms
veil-browser wait --for-load               # wait for the page to finish loading
```

`fill` vs `type`: `fill` sets the value in one go (good for forms); `type` goes through the browser's editing path (good for autocomplete / JS that reacts per keystroke).

## Tabs and screenshots

```bash
veil-browser new-tab https://example.com
veil-browser tabs
* [0] Example Domain — https://example.com/
  [1] Example Domain — https://example.com/
veil-browser close                  # closes the active tab
veil-browser screenshot output.png  # PNG fallback; --full-page for the whole page
```

## Status and lifecycle

```bash
veil-browser status   # session, daemon/chrome pids, debug port, uptime, tabs, active URL
veil-browser stop     # stop the daemon and Chrome for this session
```

The daemon starts automatically on first use and keeps running between commands. `close` on the last tab also stops the browser.

## JSON output

```bash
veil-browser --json url
```

```json
{
  "ok": true,
  "command": "url",
  "session": "default",
  "summary": "https://example.com/",
  "data": { "url": "https://example.com/" }
}
```

On error, `"ok"` is `false`, `error.message` explains what happened, and the process exits with code 1.
