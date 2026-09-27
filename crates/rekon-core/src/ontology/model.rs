//! Data model of the ontology graph as stored in `.rekon/ontology/`: nodes and edges,
//! each with the code (evidence) that proves it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::schema::Schema;

pub const GRAPH_VERSION: u32 = 1;
pub const FACTS_VERSION: u32 = 1;

/// How an evidence entry was obtained (`sourceType`). The model's evidence is checked
/// against the file before it is stored.
pub mod source {
    pub const LLM: &str = "LLM";
    /// Manifests: Cargo.toml, package.json, *.csproj, requirements.txt, go.mod.
    pub const CONFIG: &str = "Config";
    /// Repository structure seen by rekon itself: files, folders, symbol outline.
    pub const STATIC: &str = "Static";
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// Repository path with `/` separators.
    pub file: String,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default)]
    pub reason: String,
    pub source_type: String,
}

impl Evidence {
    pub fn same_place(&self, other: &Evidence) -> bool {
        self.file == other.file && self.start_line == other.start_line && self.end_line == other.end_line
    }

    pub fn location(&self) -> String {
        if self.start_line == self.end_line {
            format!("{}:{}", self.file, self.start_line)
        } else {
            format!("{}:{}-{}", self.file, self.start_line, self.end_line)
        }
    }
}

/// Appends entries that point at a new place; returns true when something was added.
pub fn add_evidence(list: &mut Vec<Evidence>, more: impl IntoIterator<Item = Evidence>) -> bool {
    let mut added = false;
    for e in more {
        if !list.iter().any(|x| x.same_place(&e)) {
            list.push(e);
            added = true;
        }
    }
    added
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// `<type>:<name>` in kebab case, e.g. `component:payment-service`.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub confidence: f64,
    /// Free-form properties (e.g. `path`, `package`, `language`).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

impl Node {
    pub fn new(kind: &str, name: &str) -> Self {
        Self {
            id: node_id(kind, name),
            kind: kind.to_string(),
            name: name.to_string(),
            description: None,
            confidence: 1.0,
            metadata: Map::new(),
            evidence: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    /// `edge:<source id>:<relation>:<target id>`.
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub relation: String,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

impl Edge {
    pub fn new(source: &str, relation: &str, target: &str) -> Self {
        Self {
            id: edge_id(source, relation, target),
            source: source.to_string(),
            target: target.to_string(),
            relation: relation.to_string(),
            confidence: 1.0,
            metadata: Map::new(),
            evidence: Vec::new(),
        }
    }
}

/// One indexing run ("ontology_scan").
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub at: String,
    /// Files in scope of the ontology.
    pub files: usize,
    /// Files sent to the model in this run.
    pub analyzed: usize,
    /// Files whose facts were still fresh.
    pub reused: usize,
    pub errors: usize,
    pub cost_usd: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub version: u32,
    /// Definitions the graph was built with (the UI reads type colors and relation
    /// names from here, so extended schemas display without code changes).
    pub schema: Schema,
    /// The System node of the analyzed repository.
    pub root: Option<String>,
    pub scan: Scan,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Node reference by type and name, as the model gives it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ref {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
}

/// Edge extracted from one file, before its ends are resolved to node ids.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RawEdge {
    pub source: Ref,
    #[serde(rename = "type")]
    pub relation: String,
    pub target: Ref,
    pub confidence: f64,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// What the model found in one file ("ontology_file_index" entry). Valid while the
/// content hash and the schema fingerprint match; evidence points only into this file.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileFacts {
    pub version: u32,
    pub hash: String,
    pub schema: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<RawEdge>,
}

/// Machine identifier of a name: `PaymentService` → `payment-service`,
/// `POST /orders` → `post-orders`, `init::run` → `init-run`.
pub fn slug(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    let mut sep = false;
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            sep = !out.is_empty();
            continue;
        }
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                sep = !out.is_empty();
            }
        }
        if sep {
            out.push('-');
            sep = false;
        }
        out.extend(c.to_lowercase());
    }
    if out.is_empty() { "unnamed".to_string() } else { out }
}

pub fn node_id(kind: &str, name: &str) -> String {
    format!("{}:{}", slug(kind), slug(name))
}

pub fn edge_id(source: &str, relation: &str, target: &str) -> String {
    format!("edge:{source}:{relation}:{target}")
}

/// Rounds a confidence to two decimals within 0–1.
pub fn confidence(value: Option<f64>) -> f64 {
    let v = value.filter(|v| v.is_finite()).unwrap_or(0.5).clamp(0.0, 1.0);
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_kebab_case() {
        assert_eq!(slug("PaymentService"), "payment-service");
        assert_eq!(slug("POST /orders"), "post-orders");
        assert_eq!(slug("rekon-core"), "rekon-core");
        assert_eq!(slug("rekon_core"), "rekon-core");
        assert_eq!(slug("HTTPServer"), "http-server");
        assert_eq!(slug("LlmRequest"), "llm-request");
        assert_eq!(slug("init::run"), "init-run");
        assert_eq!(slug("Store::put_level1"), "store-put-level1");
        assert_eq!(slug("Base64Encoder"), "base64-encoder");
        assert_eq!(slug("Zażółć gęślą"), "zażółć-gęślą");
        assert_eq!(slug("  "), "unnamed");
        assert_eq!(node_id("DataEntity", "OrderCreated"), "data-entity:order-created");
    }

    #[test]
    fn json_shape_follows_the_data_model() {
        let mut n = Node::new("Component", "PaymentService");
        n.evidence.push(Evidence {
            file: "services/payment/payment.service.ts".into(),
            start_line: 12,
            end_line: 184,
            symbol: Some("PaymentService".into()),
            reason: "Class implementing payment operations".into(),
            source_type: source::LLM.into(),
        });
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["id"], "component:payment-service");
        assert_eq!(v["type"], "Component");
        assert_eq!(v["evidence"][0]["startLine"], 12);
        assert_eq!(v["evidence"][0]["sourceType"], "LLM");
        let e = Edge::new("component:payment-service", "calls", "dependency:stripe");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["id"], "edge:component:payment-service:calls:dependency:stripe");
        assert_eq!(v["type"], "calls");
    }

    #[test]
    fn evidence_is_added_once_per_place() {
        let e = |a, b| Evidence {
            file: "a.rs".into(),
            start_line: a,
            end_line: b,
            symbol: None,
            reason: String::new(),
            source_type: source::LLM.into(),
        };
        let mut list = vec![e(1, 2)];
        assert!(!add_evidence(&mut list, [e(1, 2)]));
        assert!(add_evidence(&mut list, [e(1, 3)]));
        assert_eq!(list.len(), 2);
        assert_eq!(confidence(Some(0.956)), 0.96);
        assert_eq!(confidence(Some(7.0)), 1.0);
        assert_eq!(confidence(None), 0.5);
    }
}
