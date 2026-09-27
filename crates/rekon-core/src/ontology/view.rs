//! Text and JSON views of the stored graph for `rekon ontology show`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::graph::GraphIndex;
use super::model::Node;

fn head(n: &Node) -> String {
    format!("{} · {} ({})", n.kind, n.name, n.id)
}

/// Root, counts per type and the root's relations.
pub fn summary_text(g: &GraphIndex) -> String {
    let s = &g.graph.scan;
    let mut out = format!(
        "{} nodes, {} edges · scan {} · {} files ({} analyzed, {} unchanged, {} errors)\n",
        g.graph.nodes.len(),
        g.graph.edges.len(),
        s.at,
        s.files,
        s.analyzed,
        s.reused,
        s.errors
    );
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &g.graph.nodes {
        *counts.entry(n.kind.as_str()).or_default() += 1;
    }
    let order = |k: &str| g.schema().types.iter().position(|t| t.id == k).unwrap_or(usize::MAX);
    let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
    counts.sort_by_key(|(k, _)| order(k));
    let list: Vec<String> = counts.iter().map(|(k, c)| format!("{k} {c}")).collect();
    out.push_str(&format!("types: {}\n\n", list.join(" · ")));
    if let Some(root) = g.root() {
        out.push_str(&node_text(g, root));
    }
    out
}

/// A node, its evidence and its relations read from it.
pub fn node_text(g: &GraphIndex, n: &Node) -> String {
    let mut out = format!("{}\n", head(n));
    if let Some(d) = &n.description {
        out.push_str(&format!("{d}\n"));
    }
    out.push_str(&format!("confidence {}\n", n.confidence));
    if !n.evidence.is_empty() {
        out.push_str("evidence:\n");
        for e in &n.evidence {
            out.push_str(&format!("  {} [{}] {}\n", e.location(), e.source_type, e.reason));
        }
    }
    let neighbors = g.neighbors(&n.id);
    if !neighbors.is_empty() {
        out.push_str("relations:\n");
        let width = neighbors
            .iter()
            .map(|nb| g.relation_label(nb.edge, nb.outgoing).chars().count())
            .max()
            .unwrap_or(0);
        for nb in neighbors {
            let label = g.relation_label(nb.edge, nb.outgoing);
            let places: Vec<String> = nb.edge.evidence.iter().map(|e| e.location()).collect();
            out.push_str(&format!(
                "  {label:<width$} → {}  [{}]\n",
                head(nb.node),
                places.join(", ")
            ));
        }
    }
    out
}

pub fn node_json(g: &GraphIndex, n: &Node) -> Value {
    let relations: Vec<Value> = g
        .neighbors(&n.id)
        .iter()
        .map(|nb| {
            json!({
                "relation": g.relation_label(nb.edge, nb.outgoing),
                "direction": if nb.outgoing { "out" } else { "in" },
                "edge": nb.edge,
                "node": { "id": nb.node.id, "type": nb.node.kind, "name": nb.node.name },
            })
        })
        .collect();
    json!({ "node": n, "relations": relations })
}
