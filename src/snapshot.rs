//! Accessibility-tree snapshots.
//!
//! The whole point of VEIL Browser CLI: instead of shipping a screenshot
//! or a megabyte of HTML to the AI, ship the accessibility tree — the same
//! structure a screen reader uses. It is compact, semantic, and stable.
//!
//! Every element an agent can act on gets a stable reference for the life
//! of the snapshot: `@e1`, `@e2`, … in document order. The daemon keeps the
//! mapping `ref → backendDOMNodeId` so `click @e1` / `fill @e2 …` resolve
//! without the agent ever seeing a selector, an XPath, or a pixel.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// One node of Chrome's full accessibility tree (CDP
/// `Accessibility.getFullAXTree`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
    pub node_id: String,
    #[serde(default)]
    pub ignored: bool,
    #[serde(default)]
    pub role: Option<Value>,
    #[serde(default)]
    pub name: Option<Value>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub description: Option<Value>,
    // NOTE: serde's camelCase would produce "backendDomNodeId", but Chrome
    // sends "backendDOMNodeId" — the explicit rename is load-bearing.
    // Without it every ref resolves to "no DOM node" and click/fill break.
    #[serde(default, rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<i64>,
    #[serde(default)]
    pub child_ids: Vec<String>,
    #[serde(default)]
    pub properties: Vec<AxProperty>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxProperty {
    pub name: String,
    pub value: Value,
}

fn str_value(v: &Option<Value>) -> Option<String> {
    let v = v.as_ref()?;
    // AX string values arrive as {"type": "string", "value": "…"}.
    if let Some(s) = v.get("value").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    v.as_str().map(|s| s.to_string())
}

impl AxNode {
    pub fn role(&self) -> &str {
        self.role.as_ref().and_then(|r| r.get("value")).and_then(|v| v.as_str()).unwrap_or("unknown")
    }
    pub fn name(&self) -> Option<String> {
        str_value(&self.name)
    }
    pub fn value(&self) -> Option<String> {
        str_value(&self.value)
    }
    pub fn property(&self, name: &str) -> Option<&Value> {
        self.properties.iter().find(|p| p.name == name).map(|p| &p.value)
    }
    pub fn bool_property(&self, name: &str) -> bool {
        self.property(name)
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// What the daemon remembers for one `@eN` reference.
#[derive(Debug, Clone, Serialize)]
pub struct ElementRef {
    /// `@e1`
    pub reference: String,
    pub role: String,
    pub name: Option<String>,
    /// Chrome backend DOM node id — the stable handle for CDP DOM calls.
    pub backend_dom_node_id: Option<i64>,
}

/// Roles that are actionable (or at least worth a reference) even without
/// a visible name.
const INTERACTIVE_ROLES: &[&str] = &[
    "button", "link", "textbox", "searchbox", "combobox", "checkbox", "radio", "slider",
    "spinbutton", "menuitem", "menuitemcheckbox", "menuitemradio", "tab", "switch",
    "listbox", "option", "treeitem", "image", "heading",
];

/// Roles that carry no information on their own — collapse them, keep the
/// children.
const CONTAINER_ROLES: &[&str] = &["generic", "none", "presentation", "paragraph"];

/// Result of rendering a snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Text-mode rendering (one line per node).
    pub text: String,
    /// `@eN` → element, in document order.
    pub refs: HashMap<String, ElementRef>,
    /// Total AX nodes seen (before filtering), for the summary line.
    pub total_nodes: usize,
}

fn is_interactive(node: &AxNode) -> bool {
    let role = node.role();
    if role == "StaticText" || role == "RootWebArea" || role == "WebArea" {
        return false;
    }
    if INTERACTIVE_ROLES.contains(&role) {
        return true;
    }
    // A named node with a backend id is usually worth addressing.
    node.name().map(|n| !n.trim().is_empty()).unwrap_or(false) && node.backend_dom_node_id.is_some()
}

/// Render the full AX tree into compact text.
///
/// * `interactive_only` — `snapshot --interactive`: only nodes that got a
///   `@eN` reference, one per line. This is the cheapest possible view for
///   an agent deciding what to click/type next.
pub fn render(nodes: &[AxNode], interactive_only: bool) -> Snapshot {
    let by_id: HashMap<&str, &AxNode> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let total_nodes = nodes.len();
    let mut refs: HashMap<String, ElementRef> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    let mut counter = 0usize;

    // The tree root is the node no other node lists as a child.
    let child_ids: std::collections::HashSet<&str> =
        nodes.iter().flat_map(|n| n.child_ids.iter().map(|s| s.as_str())).collect();
    let roots: Vec<&AxNode> = nodes.iter().filter(|n| !child_ids.contains(n.node_id.as_str())).collect();

    fn walk(
        node: &AxNode,
        by_id: &HashMap<&str, &AxNode>,
        depth: usize,
        counter: &mut usize,
        refs: &mut HashMap<String, ElementRef>,
        lines: &mut Vec<String>,
        interactive_only: bool,
    ) {
        if node.ignored {
            // Ignored nodes (e.g. presentational wrappers) still have
            // children worth showing — walk through them.
            for cid in &node.child_ids {
                if let Some(child) = by_id.get(cid.as_str()) {
                    walk(child, by_id, depth, counter, refs, lines, interactive_only);
                }
            }
            return;
        }
        let role = node.role();
        // InlineTextBox nodes just repeat their parent StaticText's words
        // line by line — skip them, the parent already carries the text.
        if role == "InlineTextBox" {
            return;
        }
        let name = node.name().unwrap_or_default();
        let name = name.trim();
        let value = node.value().unwrap_or_default();
        let value = value.trim();

        let interactive = is_interactive(node);
        let container = CONTAINER_ROLES.contains(&role);

        // Assign a reference to every interactive node that has a DOM node
        // behind it (and to headings, which agents often want to scroll
        // to / quote). Nodes without a backend DOM id — e.g. on Chrome's
        // internal error pages — get no ref, because no action on them
        // could ever resolve.
        let reference = if interactive && node.backend_dom_node_id.is_some() {
            *counter += 1;
            let r = format!("@e{counter}");
            refs.insert(
                r.clone(),
                ElementRef {
                    reference: r.clone(),
                    role: role.to_string(),
                    name: if name.is_empty() { None } else { Some(name.to_string()) },
                    backend_dom_node_id: node.backend_dom_node_id,
                },
            );
            Some(r)
        } else {
            None
        };

        let show = if interactive_only {
            interactive
        } else {
            // Skip empty containers and nameless generics; keep everything
            // that carries text or meaning.
            interactive
                || !name.is_empty()
                || !value.is_empty()
                || role == "StaticText"
                || role == "heading"
        };

        if show && !(container && name.is_empty() && value.is_empty() && reference.is_none()) {
            let indent = "  ".repeat(depth.min(12));
            let mut line = String::new();
            line.push_str(&indent);
            if let Some(r) = &reference {
                line.push_str(r);
                line.push(' ');
            }
            line.push('[');
            line.push_str(role);
            line.push(']');
            if !name.is_empty() {
                line.push(' ');
                line.push_str(&truncate(&format!("\"{name}\""), 120));
            }
            if !value.is_empty() && value != name {
                line.push_str(" = ");
                line.push_str(&truncate(&format!("\"{value}\""), 120));
            }
            // State flags that change what an agent should do.
            let mut flags: Vec<&str> = Vec::new();
            if node.bool_property("disabled") {
                flags.push("disabled");
            }
            if node.bool_property("checked") {
                flags.push("checked");
            }
            if node.bool_property("focused") {
                flags.push("focused");
            }
            if node.bool_property("expanded") {
                flags.push("expanded");
            }
            if let Some(url) = node.property("url").and_then(|v| v.get("value")).and_then(|v| v.as_str()) {
                if !url.is_empty() && url != name {
                    line.push_str(&format!(" ({})", truncate(url, 100)));
                }
            }
            if !flags.is_empty() {
                line.push_str(&format!(" <{}>", flags.join(",")));
            }
            lines.push(line);
        }

        // Children: containers don't add a level of indentation.
        let child_depth = if container && reference.is_none() { depth } else { depth + 1 };
        for cid in &node.child_ids {
            if let Some(child) = by_id.get(cid.as_str()) {
                walk(child, by_id, child_depth, counter, refs, lines, interactive_only);
            }
        }
    }

    for root in roots {
        walk(root, &by_id, 0, &mut counter, &mut refs, &mut lines, interactive_only);
    }

    Snapshot { text: lines.join("\n"), refs, total_nodes }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Parse a `@eN` reference, returning the number. Accepts `@e1`, `e1`.
pub fn parse_ref(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('@');
    if s.len() > 1 && s.starts_with('e') && s[1..].chars().all(|c| c.is_ascii_digit()) {
        Some(format!("@{s}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, role: &str, name: &str, children: Vec<String>) -> AxNode {
        AxNode {
            node_id: id.into(),
            ignored: false,
            role: Some(json!({"type": "string", "value": role})),
            name: if name.is_empty() { None } else { Some(json!({"type": "string", "value": name})) },
            value: None,
            description: None,
            backend_dom_node_id: Some(id.trim_start_matches('n').parse().unwrap_or(0)),
            child_ids: children,
            properties: vec![],
        }
    }

    #[test]
    fn refs_are_stable_and_in_order() {
        let nodes = vec![
            node("n1", "RootWebArea", "", vec!["n2".into(), "n3".into()]),
            node("n2", "button", "Sign in", vec![]),
            node("n3", "link", "Learn more", vec![]),
        ];
        let snap = render(&nodes, false);
        assert!(snap.text.contains("@e1 [button] \"Sign in\""));
        assert!(snap.text.contains("@e2 [link] \"Learn more\""));
        assert_eq!(snap.refs.len(), 2);
        assert_eq!(snap.refs["@e1"].role, "button");
    }

    #[test]
    fn interactive_only_drops_static_text() {
        let nodes = vec![
            node("n1", "RootWebArea", "", vec!["n2".into(), "n3".into()]),
            node("n2", "StaticText", "Hello world", vec![]),
            node("n3", "button", "Go", vec![]),
        ];
        let snap = render(&nodes, true);
        assert!(!snap.text.contains("Hello world"));
        assert!(snap.text.contains("@e1 [button] \"Go\""));
    }

    #[test]
    fn parse_ref_accepts_forms() {
        assert_eq!(parse_ref("@e12").as_deref(), Some("@e12"));
        assert_eq!(parse_ref("e3").as_deref(), Some("@e3"));
        assert!(parse_ref("hello").is_none());
    }
}
