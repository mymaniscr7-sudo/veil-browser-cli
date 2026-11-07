#!/usr/bin/env bash
# End-to-end smoke test for VEIL Browser CLI (needs network + Chrome).
# Usage: ./scripts/e2e.sh [path-to-veil-browser-binary]
set -euo pipefail
BIN="${1:-./target/debug/veil-browser}"
SESSION="e2e-test"
export VEIL_BROWSER_SESSION="$SESSION"

fail() { echo "FAIL: $*" >&2; exit 1; }
check() { # check <needle> <output> <label>
  echo "$2" | grep -qF "$1" || fail "$3 (missing '$1' in: $2)"
}

"$BIN" stop >/dev/null 2>&1 || true
sleep 0.5

echo "== status (cold start, daemon auto-starts) =="
OUT=$("$BIN" status); echo "$OUT"
check "session 'e2e-test'" "$OUT" "status"

echo "== open example.com =="
OUT=$("$BIN" open https://example.com); echo "$OUT"
check "example.com" "$OUT" "open"

echo "== url / title =="
check "https://example.com" "$("$BIN" url)" "url"
check "Example Domain" "$("$BIN" title)" "title"

echo "== read =="
OUT=$("$BIN" read); echo "$OUT"
check "This domain is for use in documentation" "$OUT" "read"

echo "== snapshot =="
OUT=$("$BIN" snapshot); echo "$OUT"
check "[RootWebArea] \"Example Domain\"" "$OUT" "snapshot root"
check "@e1 [link] \"Learn more\"" "$OUT" "snapshot link ref"
check "Learn more" "$OUT" "snapshot link text"

echo "== snapshot --interactive =="
OUT=$("$BIN" snapshot --interactive); echo "$OUT"
check "[link]" "$OUT" "interactive snapshot"

echo "== click the link, then back =="
REF=$(echo "$OUT" | grep -o '@e[0-9]*' | head -1)
echo "clicking $REF"
"$BIN" click "$REF" >/dev/null
"$BIN" wait 1500 >/dev/null
OUT=$("$BIN" url); echo "$OUT"
check "iana.org" "$OUT" "click navigated to iana.org"
"$BIN" back >/dev/null
check "example.com" "$("$BIN" url)" "back"

echo "== scroll / press / wait =="
"$BIN" scroll down >/dev/null
"$BIN" press End >/dev/null
"$BIN" wait 500 >/dev/null

echo "== new-tab / tabs =="
"$BIN" new-tab https://example.com >/dev/null
OUT=$("$BIN" tabs); echo "$OUT"
check "2 tab" "$OUT" "tabs count"
"$BIN" close >/dev/null
OUT=$("$BIN" tabs); echo "$OUT"
check "1 tab" "$OUT" "tabs after close"

echo "== fill / type / press Enter on a local form =="
FORM_DIR=$(mktemp -d)
cat > "$FORM_DIR/form.html" <<'HTML'
<!doctype html><html><body>
<h1>Form test</h1>
<form onsubmit="document.getElementById('out').textContent='submitted:'+document.getElementById('q').value; return false;">
<input id="q" type="text" aria-label="Search box">
<button type="submit">Go</button>
</form>
<p id="out"></p>
</body></html>
HTML
(cd "$FORM_DIR" && python3 -m http.server 8901 >/dev/null 2>&1 & echo $! > "$FORM_DIR/server.pid")
sleep 1
"$BIN" open http://127.0.0.1:8901/form.html >/dev/null
OUT=$("$BIN" snapshot --interactive); echo "$OUT"
check "@e2 [textbox]" "$OUT" "form textbox ref"
"$BIN" fill @e2 "hello veil" >/dev/null
"$BIN" click @e3 >/dev/null
check "submitted:hello veil" "$("$BIN" read)" "fill + click submit"
"$BIN" fill @e2 "" >/dev/null
"$BIN" type @e2 "typed!" >/dev/null
"$BIN" press Enter >/dev/null
check "submitted:typed!" "$("$BIN" read)" "type + press Enter submits"
kill "$(cat "$FORM_DIR/server.pid")" 2>/dev/null || true
rm -rf "$FORM_DIR"

echo "== screenshot (fallback only) =="
"$BIN" screenshot /tmp/veil-e2e.png
test -s /tmp/veil-e2e.png || fail "screenshot empty"

echo "== json mode =="
OUT=$("$BIN" --json url); echo "$OUT"
check '"ok": true' "$OUT" "json mode"

echo "== stop =="
"$BIN" stop
echo "E2E OK"
