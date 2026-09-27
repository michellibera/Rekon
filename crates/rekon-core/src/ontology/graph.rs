//! Browsing the stored graph ("Ontology API" of the UI and the CLI): loads
//! `graph.json` and answers neighborhood queries. Never calls the model.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use super::graph_path;
use super::model::{Edge, Graph, Node};
use super::schema::{CONTAINS, IMPLEMENTED_BY, Schema};

/// Stored graph, or `None` when missing or unreadable.
pub fn read(map_dir: &Path) -> Option<Graph> {
    load(map_dir).ok().flatten().map(|g| g.graph)
}

/// Loads `graph.json`: `Ok(None)` when there is no graph yet.
pub fn load(map_dir: &Path) -> Result<Option<GraphIndex>> {
    let path = graph_path(map_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let graph: Graph = serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    Ok(Some(GraphIndex::new(graph)))
}

/// An edge seen from one of its nodes.
#[derive(Clone, Copy, Debug)]
pub struct Neighbor<'a> {
    pub edge: &'a Edge,
    /// The other end.
    pub node: &'a Node,
    /// The node the question was about is the edge's source.
    pub outgoing: bool,
}

pub struct GraphIndex {
    pub graph: Graph,
    nodes: HashMap<String, usize>,
    edges: HashMap<String, usize>,
    /// Node index → indices of its edges (both directions).
    adjacent: Vec<Vec<usize>>,
}

impl GraphIndex {
    pub fn new(graph: Graph) -> Self {
        let nodes: HashMap<String, usize> = graph.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        let edges = graph.edges.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
        let mut adjacent = vec![Vec::new(); graph.nodes.len()];
        for (i, e) in graph.edges.iter().enumerate() {
            for end in [&e.source, &e.target] {
                if let Some(&n) = nodes.get(end) {
                    adjacent[n].push(i);
                }
            }
        }
        Self {
            graph,
            nodes,
            edges,
            adjacent,
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.graph.schema
    }

    pub fn root(&self) -> Option<&Node> {
        self.node(self.graph.root.as_deref()?)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id).map(|&i| &self.graph.nodes[i])
    }

    pub fn edge(&self, id: &str) -> Option<&Edge> {
        self.edges.get(id).map(|&i| &self.graph.edges[i])
    }

    pub fn degree(&self, id: &str) -> usize {
        self.nodes.get(id).map_or(0, |&i| self.adjacent[i].len())
    }

    /// Edges of a node with their other ends: contained elements first, then
    /// elements of a source file, other relations, the container, and links to
    /// source files last; within a group the best connected first.
    pub fn neighbors(&self, id: &str) -> Vec<Neighbor<'_>> {
        let Some(&i) = self.nodes.get(id) else {
            return Vec::new();
        };
        let mut out: Vec<Neighbor> = self.adjacent[i]
            .iter()
            .filter_map(|&e| {
                let edge = &self.graph.edges[e];
                let outgoing = edge.source == id;
                let other = if outgoing { &edge.target } else { &edge.source };
                Some(Neighbor {
                    edge,
                    node: self.node(other)?,
                    outgoing,
                })
            })
            .collect();
        let rank = |n: &Neighbor| match (n.edge.relation.as_str(), n.outgoing) {
            (CONTAINS, true) => 0,
            (IMPLEMENTED_BY, false) => 1,
            (CONTAINS, false) => 3,
            (IMPLEMENTED_BY, true) => 4,
            _ => 2,
        };
        out.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(self.degree(&b.node.id).cmp(&self.degree(&a.node.id)))
                .then(a.node.name.cmp(&b.node.name))
                .then(a.edge.id.cmp(&b.edge.id))
        });
        out
    }

    /// Nodes matching `query`: the id, else the name or the end of the id (any case).
    pub fn find(&self, query: &str) -> Vec<&Node> {
        if let Some(n) = self.node(query) {
            return vec![n];
        }
        let q = query.to_lowercase();
        let slug = super::model::slug(query);
        self.graph
            .nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == q || n.id.ends_with(&format!(":{slug}")))
            .collect()
    }

    /// Name of the relation of `edge` read from `from` (its source or its target):
    /// the relation itself, or its inverse name when read from the target.
    pub fn relation_label(&self, edge: &Edge, from_source: bool) -> String {
        if from_source {
            return edge.relation.clone();
        }
        self.schema()
            .inverse(&edge.relation)
            .map_or_else(|| format!("{} (reverse)", edge.relation), str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::model::{GRAPH_VERSION, Scan};

    fn sample() -> Graph {
        let n = |kind: &str, name: &str| Node::new(kind, name);
        let e = |s: &str, r: &str, t: &str| Edge::new(s, r, t);
        Graph {
            version: GRAPH_VERSION,
            schema: Schema::builtin(),
            root: Some("system:shop".into()),
            scan: Scan::default(),
            nodes: vec![
                n("System", "Shop"),
                n("Component", "OrderService"),
                n("Event", "OrderCreated"),
                n("Component", "PaymentService"),
                n("SourceCode", "order.rs"),
            ],
            edges: vec![
                e("system:shop", "contains", "component:order-service"),
                e("system:shop", "contains", "component:payment-service"),
                e("component:order-service", "emits", "event:order-created"),
                e("component:payment-service", "consumes", "event:order-created"),
                e("component:order-service", "implementedBy", "source-code:order-rs"),
            ],
        }
    }

    #[test]
    fn neighbors_in_display_order_with_relation_names() {
        let g = GraphIndex::new(sample());
        assert_eq!(g.root().unwrap().name, "Shop");
        let names: Vec<(String, bool)> = g
            .neighbors("component:order-service")
            .iter()
            .map(|n| (n.node.name.clone(), n.outgoing))
            .collect();
        assert_eq!(
            names,
            [
                ("OrderCreated".to_string(), true),
                ("Shop".to_string(), false),
                ("order.rs".to_string(), true)
            ]
        );
        let consumes = g
            .edge("edge:component:payment-service:consumes:event:order-created")
            .unwrap();
        assert_eq!(g.relation_label(consumes, true), "consumes");
        assert_eq!(g.relation_label(consumes, false), "consumedBy");
        assert_eq!(g.find("orderservice").len(), 1, "any case");
        assert!(g.find("nothing").is_empty());
        assert_eq!(g.find("OrderService")[0].id, "component:order-service");
        assert_eq!(g.find("order-created")[0].id, "event:order-created");
        assert_eq!(g.degree("event:order-created"), 2);
    }

    #[test]
    fn missing_graph_is_none_and_invalid_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
        std::fs::create_dir_all(dir.path().join("ontology")).unwrap();
        std::fs::write(dir.path().join("ontology/graph.json"), "{").unwrap();
        assert!(load(dir.path()).is_err());
    }
}
