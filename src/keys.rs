//! `press` key names → CDP `Input.dispatchKeyEvent` parameters.
//!
//! Accepts the names an agent would naturally type (`Enter`, `Tab`,
//! `Escape`, `ArrowDown`, `a`, `F5`, `Ctrl+l`, …) and normalises them to
//! the `key` / `code` / `windowsVirtualKeyCode` triple Chrome expects.

/// A resolved key event: (key, code, windows virtual-key code, text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: String,
    pub code: String,
    pub windows_virtual_key_code: u32,
    /// Text produced by the key, if any (printable keys).
    pub text: Option<String>,
}

fn named(key: &str, code: &str, vk: u32) -> KeyEvent {
    KeyEvent { key: key.into(), code: code.into(), windows_virtual_key_code: vk, text: None }
}

fn printable(ch: char) -> KeyEvent {
    let upper = ch.to_ascii_uppercase();
    KeyEvent {
        key: ch.to_string(),
        code: format!("Key{upper}"),
        windows_virtual_key_code: upper as u32,
        text: Some(ch.to_string()),
    }
}

/// Parse a key spec like `Enter`, `ctrl+l`, `Shift+Tab`, `ArrowDown`, `a`.
/// Returns `None` for an empty / unrecognised spec.
pub fn parse_key(spec: &str) -> Option<KeyEvent> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    // Modifier + key, e.g. Ctrl+l, Shift+Tab, Alt+F4, Meta+a.
    if let Some((mods, base)) = spec.split_once('+') {
        let mods = mods.to_ascii_lowercase();
        if ["ctrl", "control", "shift", "alt", "meta", "cmd", "command"].contains(&mods.as_str()) {
            let mut ev = parse_key(base)?;
            // Record the modifier in the key name so the caller can set
            // `modifiers` on the CDP event; the base key itself is unchanged.
            ev.key = format!("{mods}+{}", ev.key);
            return Some(ev);
        }
    }
    let lower = spec.to_ascii_lowercase();
    let ev = match lower.as_str() {
        "enter" | "return" => named("Enter", "Enter", 13),
        "tab" => named("Tab", "Tab", 9),
        "escape" | "esc" => named("Escape", "Escape", 27),
        "space" | "spacebar" => named(" ", "Space", 32),
        "backspace" => named("Backspace", "Backspace", 8),
        "delete" | "del" => named("Delete", "Delete", 46),
        "arrowup" | "up" => named("ArrowUp", "ArrowUp", 38),
        "arrowdown" | "down" => named("ArrowDown", "ArrowDown", 40),
        "arrowleft" | "left" => named("ArrowLeft", "ArrowLeft", 37),
        "arrowright" | "right" => named("ArrowRight", "ArrowRight", 39),
        "home" => named("Home", "Home", 36),
        "end" => named("End", "End", 35),
        "pageup" | "pgup" => named("PageUp", "PageUp", 33),
        "pagedown" | "pgdn" => named("PageDown", "PageDown", 34),
        "f1" => named("F1", "F1", 112),
        "f2" => named("F2", "F2", 113),
        "f3" => named("F3", "F3", 114),
        "f4" => named("F4", "F4", 115),
        "f5" => named("F5", "F5", 116),
        "f6" => named("F6", "F6", 117),
        "f7" => named("F7", "F7", 118),
        "f8" => named("F8", "F8", 119),
        "f9" => named("F9", "F9", 120),
        "f10" => named("F10", "F10", 121),
        "f11" => named("F11", "F11", 122),
        "f12" => named("F12", "F12", 123),
        _ => {
            let mut chars = spec.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char, not a known name
            }
            if ch.is_ascii_alphabetic() {
                printable(ch)
            } else if ch.is_ascii_digit() {
                KeyEvent {
                    key: ch.to_string(),
                    code: format!("Digit{ch}"),
                    windows_virtual_key_code: ch as u32,
                    text: Some(ch.to_string()),
                }
            } else {
                KeyEvent { key: ch.to_string(), code: format!("Key{ch}"), windows_virtual_key_code: ch as u32, text: Some(ch.to_string()) }
            }
        }
    };
    Some(ev)
}

/// CDP `modifiers` bitmask for a parsed key spec (`Ctrl`=2, `Alt`=1,
/// `Shift`=8, `Meta`=4). Only the leading `mod+` prefix is considered.
pub fn modifiers_for(spec: &str) -> u32 {
    let lower = spec.trim().to_ascii_lowercase();
    let Some((mods, _)) = lower.split_once('+') else { return 0 };
    match mods {
        "alt" => 1,
        "ctrl" | "control" => 2,
        "meta" | "cmd" | "command" => 4,
        "shift" => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_arrows() {
        assert_eq!(parse_key("Enter").unwrap().windows_virtual_key_code, 13);
        assert_eq!(parse_key("ArrowDown").unwrap().code, "ArrowDown");
        assert_eq!(parse_key("Escape").unwrap().key, "Escape");
    }

    #[test]
    fn printable_and_modifier() {
        let a = parse_key("a").unwrap();
        assert_eq!(a.text.as_deref(), Some("a"));
        assert_eq!(modifiers_for("Ctrl+l"), 2);
        assert_eq!(modifiers_for("Shift+Tab"), 8);
        assert!(parse_key("").is_none());
        assert!(parse_key("NotAKeyName").is_none());
    }
}
