//! Integration tests for the snapshot renderer (pure, no Chrome needed).
//! Live end-to-end coverage lives in `scripts/e2e.sh`.

use serde_json::json;
use veil_browser::snapshot::{parse_ref, render, AxNode};

fn node(id: &str, role: &str, name: &str, children: Vec<String>) -> AxNode {
    AxNode {
        node_id: id.into(),
        ignored: false,
        role: Some(json!({"type": "string", "value": role})),
        name: if name.is_empty() { None } else { Some(json!({"type": "string", "value": name})) },
        value: None,
        description: None,
        backend_dom_node_id: Some(100 + id.trim_start_matches('n').parse::<i64>().unwrap_or(0)),
        child_ids: children,
        properties: vec![],
    }
}

#[test]
fn full_snapshot_keeps_text_and_assigns_refs_in_order() {
    let nodes = vec![
        node("n1", "RootWebArea", "", vec!["n2".into(), "n3".into(), "n4".into()]),
        node("n2", "heading", "Example Domain", vec![]),
        node("n3", "StaticText", "This domain is for use in documentation.", vec![]),
        node("n4", "link", "Learn more", vec![]),
    ];
    let snap = render(&nodes, false);
    assert!(snap.text.contains("[heading] \"Example Domain\""));
    assert!(snap.text.contains("This domain is for use in documentation"));
    // heading + link are interactive → @e1, @e2 in document order.
    assert!(snap.text.contains("@e1 [heading]"));
    assert!(snap.text.contains("@e2 [link] \"Learn more\""));
    assert_eq!(snap.refs.len(), 2);
    assert_eq!(snap.refs["@e2"].backend_dom_node_id, Some(104));
}

#[test]
fn ignored_nodes_are_transparent() {
    let mut nodes = vec![
        node("n1", "RootWebArea", "", vec!["n2".into()]),
        node("n2", "generic", "", vec!["n3".into()]),
        node("n3", "button", "Go", vec![]),
    ];
    nodes[1].ignored = true;
    let snap = render(&nodes, false);
    assert!(snap.text.contains("@e1 [button] \"Go\""));
}

#[test]
fn backend_dom_node_id_deserializes_from_chrome_json() {
    // Regression test: Chrome sends "backendDOMNodeId" (capital DOM),
    // which serde's camelCase does NOT produce. If this field ever stops
    // parsing, every click/fill fails with "no DOM node".
    let raw = r#"{"nodeId":"n1","role":{"type":"string","value":"link"},"name":{"type":"string","value":"Learn more"},"backendDOMNodeId":42,"childIds":[]}"#;
    let node: AxNode = serde_json::from_str(raw).unwrap();
    assert_eq!(node.backend_dom_node_id, Some(42));
    let snap = render(&[node], false);
    assert_eq!(snap.refs["@e1"].backend_dom_node_id, Some(42));
}

#[test]
fn parse_ref_forms() {
    assert_eq!(parse_ref("@e1").as_deref(), Some("@e1"));
    assert_eq!(parse_ref("e42").as_deref(), Some("@e42"));
    assert!(parse_ref("@x1").is_none());
    assert!(parse_ref("").is_none());
}
