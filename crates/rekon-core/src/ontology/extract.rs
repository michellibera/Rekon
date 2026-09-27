//! Model extraction: one request maps a batch of files onto the ontology. The answer
//! is checked against the files (grounding) before it becomes facts: evidence must
//! point into a file of the request and inside the lines shown, and a cited symbol
//! must appear in the cited lines (the range moves to where it is, otherwise the
//! symbol is dropped and the confidence lowered). Elements without evidence are dropped.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use super::model::{self, Evidence, Graph, Node, RawEdge, Ref, source};
use super::outline::Item;
use super::schema::{SOURCE_CODE, Schema};
use crate::backend::{LlmRequest, TaskKind};
use crate::config::Config;
use crate::prompts::{CODE_MARKER, FILE_MARKER, clean_text, numbered_line};

const TASK: &str = include_str!("../../prompts/ontology.md");

/// Section markers of the input; the fake backend parses them too.
pub const OUTLINE_MARKER: &str = "OUTLINE:";
pub const KNOWN_MARKER: &str = "KNOWN ELEMENTS";
pub const SYMBOLS_MARKER: &str = "SYMBOLS DEFINED IN OTHER FILES";

/// Entries of the known-elements lists offered in one request.
const MAX_KNOWN: usize = 120;

/// A file prepared for extraction.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: String,
    pub hash: String,
    pub lines: Vec<String>,
    /// Lines worth analyzing: all but a test module at the end.
    pub code_lines: u32,
    pub outline: Vec<Item>,
    /// One-sentence description from the rekon map, if fresh.
    pub summary: Option<String>,
}

impl Source {
    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }
}

/// Part of a request: a whole file or a range of a long one.
#[derive(Clone, Copy)]
pub struct Part<'a> {
    pub source: &'a Source,
    pub range: (u32, u32),
}

/// Element defined in another file, offered so the model reuses its name.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    /// Ontology type (from the previous graph) or symbol kind (from the outline).
    pub label: String,
    pub name: String,
    pub path: String,
    /// Identifier that has to appear in the code of a request to offer the entry.
    pub key: String,
    /// `label` is an ontology type.
    pub typed: bool,
}

/// Identifiers too common to tell which symbol the code refers to.
const COMMON: &[&str] = &[
    "new", "run", "main", "get", "set", "default", "from", "into", "build", "init", "load", "save", "open", "parse",
    "len", "test", "tests", "new_", "update", "read", "write", "name", "path", "text", "value", "item", "items",
];

/// Last identifier of a code name (`init::run` → `run`); `None` for phrases.
fn code_key(name: &str) -> Option<String> {
    if name.contains(' ') || name.is_empty() {
        return None;
    }
    let key = name.rsplit([':', '.', '/']).find(|s| !s.is_empty())?.to_string();
    (key.chars().count() >= 3 && !COMMON.contains(&key.as_str()) && key.chars().all(is_ident)).then_some(key)
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Known elements: nodes of the previous graph, then types and top-level functions of
/// the outlines.
pub fn known_elements(sources: &[Source], previous: Option<&Graph>) -> Vec<Known> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Some(g) = previous {
        for n in &g.nodes {
            if n.kind == SOURCE_CODE || Some(&n.id) == g.root.as_ref() {
                continue;
            }
            let (Some(key), Some(ev)) = (code_key(&n.name), n.evidence.first()) else {
                continue;
            };
            if seen.insert((ev.file.clone(), n.name.clone())) {
                out.push(Known {
                    label: n.kind.clone(),
                    name: n.name.clone(),
                    path: ev.file.clone(),
                    key,
                    typed: true,
                });
            }
        }
    }
    for s in sources {
        for it in &s.outline {
            if !(it.is_type() || (it.depth == 0 && matches!(it.kind, "fn" | "def"))) {
                continue;
            }
            let Some(key) = code_key(&it.name) else { continue };
            if seen.insert((s.path.clone(), it.qualified.clone())) {
                out.push(Known {
                    label: it.kind.to_string(),
                    name: it.qualified.clone(),
                    path: s.path.clone(),
                    key,
                    typed: false,
                });
            }
        }
    }
    out
}

fn identifiers(parts: &[Part]) -> HashSet<String> {
    let mut out = HashSet::new();
    for p in parts {
        for nr in p.range.0..=p.range.1 {
            let Some(line) = p.source.lines.get(nr as usize - 1) else {
                continue;
            };
            out.extend(
                line.split(|c: char| !is_ident(c))
                    .filter(|w| w.len() >= 3)
                    .map(str::to_string),
            );
        }
    }
    out
}

/// JSON schema of the answer; types and relations are limited to the schema's ids.
pub fn answer_schema(schema: &Schema) -> Value {
    let types: Vec<&str> = schema
        .types
        .iter()
        .filter(|t| t.extract)
        .map(|t| t.id.as_str())
        .collect();
    let relations: Vec<&str> = schema
        .relations
        .iter()
        .filter(|r| r.extract)
        .map(|r| r.id.as_str())
        .collect();
    let evidence = json!({ "type": "array", "items": { "type": "object",
        "required": ["file", "start", "end", "reason"],
        "properties": { "file": { "type": "string" }, "start": { "type": "integer" },
                        "end": { "type": "integer" }, "symbol": { "type": "string" },
                        "reason": { "type": "string" } } } });
    let reference = json!({ "type": "object", "required": ["type", "name"],
        "properties": { "type": { "type": "string", "enum": types }, "name": { "type": "string" } } });
    json!({ "type": "object", "required": ["nodes", "edges"], "properties": {
        "nodes": { "type": "array", "items": { "type": "object",
            "required": ["type", "name", "description", "confidence", "evidence"],
            "properties": { "type": { "type": "string", "enum": types }, "name": { "type": "string" },
                            "description": { "type": "string" }, "confidence": { "type": "number" },
                            "evidence": evidence } } },
        "edges": { "type": "array", "items": { "type": "object",
            "required": ["source", "relation", "target", "confidence", "evidence"],
            "properties": { "source": reference, "relation": { "type": "string", "enum": relations },
                            "target": reference, "confidence": { "type": "number" },
                            "evidence": evidence } } } } })
}

pub fn request(
    config: &Config,
    system: &str,
    schema: &Schema,
    project: Option<&str>,
    known: &[Known],
    parts: &[Part],
) -> LlmRequest {
    let mut input = String::new();
    if let Some(p) = project {
        input.push_str(&format!("PROJECT: {p}\n\n"));
    }
    input.push_str("TYPES (id: meaning):\n");
    for t in schema.types.iter().filter(|t| t.extract) {
        input.push_str(&format!("- {}: {}\n", t.id, t.description));
    }
    input.push_str("\nRELATIONS (id: source -> target, meaning):\n");
    for r in schema.relations.iter().filter(|r| r.extract) {
        input.push_str(&format!("- {}: {}\n", r.id, r.description));
    }
    let words = identifiers(parts);
    let own: HashSet<&str> = parts.iter().map(|p| p.source.path.as_str()).collect();
    let offered: Vec<&Known> = known
        .iter()
        .filter(|k| !own.contains(k.path.as_str()) && words.contains(&k.key))
        .take(MAX_KNOWN)
        .collect();
    let typed_keys: HashSet<&str> = offered.iter().filter(|k| k.typed).map(|k| k.key.as_str()).collect();
    for (typed, title) in [
        (
            true,
            format!("{KNOWN_MARKER} (from other files; reuse the type and name when you mean them):"),
        ),
        (false, format!("{SYMBOLS_MARKER} (use these names):")),
    ] {
        let list: Vec<&&Known> = offered
            .iter()
            .filter(|k| k.typed == typed && (typed || !typed_keys.contains(k.key.as_str())))
            .collect();
        if list.is_empty() {
            continue;
        }
        input.push_str(&format!("\n{title}\n"));
        for k in list {
            input.push_str(&format!("- {} {} ({})\n", k.label, k.name, k.path));
        }
    }
    for part in parts {
        let src = part.source;
        input.push_str(&format!("\n{FILE_MARKER}{} ---\n", src.path));
        if let Some(s) = &src.summary {
            input.push_str(&format!("SUMMARY: {s}\n"));
        }
        if part.range != (1, src.line_count()) {
            input.push_str(&format!(
                "LINES: {}-{} of {}\n",
                part.range.0,
                part.range.1,
                src.line_count()
            ));
        }
        let items: Vec<&Item> = src
            .outline
            .iter()
            .filter(|it| it.start >= part.range.0 && it.start <= part.range.1)
            .collect();
        if !items.is_empty() {
            input.push_str(&format!("{OUTLINE_MARKER}\n"));
            for it in items {
                let what = if it.kind == "impl" {
                    it.qualified.clone()
                } else {
                    format!("{} {}", it.kind, it.qualified)
                };
                input.push_str(&format!("- {}-{} {what}\n", it.start, it.end));
            }
        }
        input.push_str(&format!("{CODE_MARKER}\n"));
        for nr in part.range.0..=part.range.1 {
            let text = src.lines.get(nr as usize - 1).map_or("", String::as_str);
            input.push_str(&numbered_line(nr, text));
            input.push('\n');
        }
    }
    LlmRequest {
        kind: TaskKind::Ontology,
        model: config.models.ontology.clone(),
        system: system.to_string(),
        task: TASK.trim().to_string(),
        input,
        schema: answer_schema(schema),
    }
}

/// Facts of one request, split by file (each element keeps its evidence in that file).
#[derive(Debug, Default)]
pub struct Extracted {
    pub files: BTreeMap<String, (Vec<Node>, Vec<RawEdge>)>,
    /// Nodes and edges dropped as invalid or without evidence.
    pub dropped: usize,
}

pub fn parse(json: &Value, schema: &Schema, parts: &[Part]) -> Extracted {
    let mut out = Extracted::default();
    for p in parts {
        out.files.entry(p.source.path.clone()).or_default();
    }
    let list = |key: &str| json.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    for v in list("nodes") {
        let Some(node) = parse_node(&v, schema, parts) else {
            out.dropped += 1;
            continue;
        };
        for (file, evidence) in by_file(&node.evidence) {
            let entry = out.files.entry(file).or_default();
            entry.0.push(Node {
                evidence,
                ..node.clone()
            });
        }
    }
    for v in list("edges") {
        let Some(edge) = parse_edge(&v, schema, parts) else {
            out.dropped += 1;
            continue;
        };
        for (file, evidence) in by_file(&edge.evidence) {
            let entry = out.files.entry(file).or_default();
            entry.1.push(RawEdge {
                evidence,
                ..edge.clone()
            });
        }
    }
    out
}

fn by_file(evidence: &[Evidence]) -> BTreeMap<String, Vec<Evidence>> {
    let mut out: BTreeMap<String, Vec<Evidence>> = BTreeMap::new();
    for e in evidence {
        out.entry(e.file.clone()).or_default().push(e.clone());
    }
    out
}

fn clean_name(s: &str) -> Option<String> {
    let t = clean_text(s)?;
    let t = t.trim_matches(|c| c == '`' || c == '"' || c == '\'').trim();
    (!t.is_empty()).then(|| t.chars().take(100).collect())
}

fn parse_ref(v: &Value, schema: &Schema) -> Option<Ref> {
    let kind = schema.canonical_type(v.get("type")?.as_str()?)?.to_string();
    let name = clean_name(v.get("name")?.as_str()?)?;
    Some(Ref { kind, name })
}

fn parse_node(v: &Value, schema: &Schema, parts: &[Part]) -> Option<Node> {
    let kind = schema.canonical_type(v.get("type")?.as_str()?)?;
    if !schema.type_def(kind)?.extract {
        return None;
    }
    let name = clean_name(v.get("name")?.as_str()?)?;
    let (evidence, exact) = ground_all(v.get("evidence"), parts);
    if evidence.is_empty() {
        return None;
    }
    let mut node = Node::new(kind, &name);
    node.description = v.get("description").and_then(Value::as_str).and_then(clean_text);
    node.confidence = penalized(v, exact);
    node.evidence = evidence;
    Some(node)
}

fn parse_edge(v: &Value, schema: &Schema, parts: &[Part]) -> Option<RawEdge> {
    let (relation, swap) = schema.canonical_relation(v.get("relation")?.as_str()?)?;
    let mut source = parse_ref(v.get("source")?, schema)?;
    let mut target = parse_ref(v.get("target")?, schema)?;
    if swap {
        std::mem::swap(&mut source, &mut target);
    }
    if model::slug(&source.name) == model::slug(&target.name) {
        return None;
    }
    let (evidence, exact) = ground_all(v.get("evidence"), parts);
    if evidence.is_empty() {
        return None;
    }
    Some(RawEdge {
        source,
        relation: relation.to_string(),
        target,
        confidence: penalized(v, exact),
        evidence,
    })
}

/// Confidence of the answer, lowered when some evidence could not be verified.
fn penalized(v: &Value, exact: bool) -> f64 {
    let c = v.get("confidence").and_then(Value::as_f64);
    model::confidence(c.map(|c| if exact { c } else { c * 0.8 }))
}

fn ground_all(v: Option<&Value>, parts: &[Part]) -> (Vec<Evidence>, bool) {
    let mut out = Vec::new();
    let mut exact = true;
    for e in v.and_then(Value::as_array).into_iter().flatten() {
        if let Some((ev, ok)) = ground(e, parts) {
            exact &= ok;
            model::add_evidence(&mut out, [ev]);
        }
    }
    (out, exact)
}

fn find_part<'a>(file: &str, parts: &[Part<'a>]) -> Option<Part<'a>> {
    let file = file.trim().replace('\\', "/");
    let file = file.trim_start_matches("./");
    parts
        .iter()
        .find(|p| p.source.path == file)
        .or_else(|| {
            (!file.is_empty())
                .then(|| parts.iter().find(|p| p.source.path.ends_with(&format!("/{file}"))))
                .flatten()
        })
        .or_else(|| (parts.len() == 1).then(|| &parts[0]))
        .copied()
}

/// Candidates to look for: the whole symbol, its last identifier, its longest one.
fn needles(symbol: &str) -> Vec<String> {
    let words: Vec<&str> = symbol
        .split(|c: char| !is_ident(c))
        .filter(|w| w.chars().count() >= 2 && !matches!(*w, "self" | "this" | "let" | "mut" | "pub" | "fn" | "await"))
        .collect();
    let mut out = vec![symbol.trim().to_string()];
    if let Some(last) = words.last() {
        out.push(last.to_string());
    }
    if let Some(longest) = words.iter().max_by_key(|w| w.len()) {
        out.push(longest.to_string());
    }
    out.dedup();
    out.retain(|n| !n.is_empty());
    out
}

/// Does `line` contain `needle` (as a whole word when it is an identifier)?
fn line_has(line: &str, needle: &str) -> bool {
    if !needle.chars().all(is_ident) {
        return line.contains(needle);
    }
    line.match_indices(needle).any(|(i, _)| {
        let before = line[..i].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = line[i + needle.len()..].chars().next().is_none_or(|c| !is_ident(c));
        before && after
    })
}

/// Checks one evidence entry against the files. Returns the evidence and whether it
/// was confirmed (`false` when the cited symbol was not found anywhere).
fn ground(v: &Value, parts: &[Part]) -> Option<(Evidence, bool)> {
    let part = find_part(v.get("file").and_then(Value::as_str).unwrap_or(""), parts)?;
    let src = part.source;
    let (a, b) = part.range;
    if src.lines.is_empty() || a > b {
        return None;
    }
    let num = |k: &str, alt: &str| v.get(k).or_else(|| v.get(alt)).and_then(Value::as_i64);
    let start = num("start", "startLine")?;
    let end = num("end", "endLine").unwrap_or(start);
    let (lo, hi) = (i64::from(a), i64::from(b));
    if start.max(end) < lo || start.min(end) > hi {
        return None;
    }
    let mut start = start.clamp(lo, hi) as u32;
    let mut end = end.clamp(lo, hi) as u32;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let reason = v
        .get("reason")
        .and_then(Value::as_str)
        .and_then(clean_text)
        .unwrap_or_default();
    let mut symbol = v.get("symbol").and_then(Value::as_str).and_then(clean_name);
    let mut exact = true;
    if let Some(sym) = symbol.clone() {
        let has = |lo: u32, hi: u32, n: &str| (lo..=hi).any(|nr| line_has(&src.lines[nr as usize - 1], n));
        let needles = needles(&sym);
        if !needles.iter().any(|n| has(start, end, n)) {
            let nearest = needles.iter().find_map(|n| {
                (a..=b)
                    .filter(|&nr| line_has(&src.lines[nr as usize - 1], n))
                    .min_by_key(|&nr| nr.abs_diff(start))
            });
            match nearest {
                Some(line) => {
                    let definition = src
                        .outline
                        .iter()
                        .find(|it| it.start == line && needles.iter().any(|n| *n == it.name || *n == it.qualified));
                    match definition {
                        Some(it) => (start, end) = (it.start.max(a), it.end.min(b)),
                        None => (start, end) = (line, (line + (end - start)).min(b)),
                    }
                }
                None => {
                    symbol = None;
                    exact = false;
                }
            }
        }
    }
    Some((
        Evidence {
            file: src.path.clone(),
            start_line: start,
            end_line: end,
            symbol,
            reason,
            source_type: source::LLM.to_string(),
        },
        exact,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::outline;

    fn source(path: &str, text: &str) -> Source {
        Source {
            path: path.into(),
            hash: "h".into(),
            lines: text.lines().map(str::to_string).collect(),
            code_lines: text.lines().count() as u32,
            outline: outline::outline(path, text),
            summary: Some("Manages payments".into()),
        }
    }

    const PAYMENT: &str = "use stripe::Client;\n\npub struct PaymentService {\n    client: Client,\n}\n\nimpl PaymentService {\n    pub fn pay(&self) {\n        self.client.charge(1);\n    }\n}\n";

    #[test]
    fn request_lists_ontology_known_elements_and_numbered_code() {
        let s = source("src/payment.rs", PAYMENT);
        let other = source("src/client.rs", "pub struct Client;\npub struct Unused;\n");
        let known = known_elements(&[s.clone(), other], None);
        let parts = [Part {
            source: &s,
            range: (1, s.line_count()),
        }];
        let req = request(
            &Config::default(),
            "sys",
            &Schema::builtin(),
            Some("Shop"),
            &known,
            &parts,
        );
        assert_eq!(req.kind, TaskKind::Ontology);
        assert_eq!(req.model, "sonnet");
        let i = &req.input;
        assert!(i.starts_with("PROJECT: Shop\n"), "{i}");
        assert!(i.contains("- DataEntity: "), "{i}");
        assert!(i.contains("- consumes: "), "{i}");
        assert!(!i.contains("- SourceCode:"), "rekon adds source files itself");
        assert!(i.contains("SYMBOLS DEFINED IN OTHER FILES (use these names):\n- struct Client (src/client.rs)\n"));
        assert!(!i.contains("Unused"), "only symbols mentioned in the batch");
        assert!(i.contains(
            "--- file: src/payment.rs ---\nSUMMARY: Manages payments\nOUTLINE:\n- 3-5 struct PaymentService\n"
        ));
        assert!(i.contains("- 8-10 fn PaymentService::pay\n"), "{i}");
        assert!(i.contains("--- code ---\n    1 | use stripe::Client;\n"));
        let enums = &req.schema["properties"]["edges"]["items"]["properties"]["relation"]["enum"];
        assert!(enums.as_array().unwrap().iter().any(|r| r == "derivedFrom"));
    }

    #[test]
    fn answers_are_grounded_in_the_code() {
        let s = source("src/payment.rs", PAYMENT);
        let parts = [Part {
            source: &s,
            range: (1, s.line_count()),
        }];
        let answer = json!({
            "nodes": [
                {"type": "Component", "name": "PaymentService", "description": "Pays", "confidence": 0.9,
                 "evidence": [{"file": "payment.rs", "start": 3, "end": 5, "symbol": "PaymentService", "reason": "struct"}]},
                // Wrong lines: moved to the definition of the symbol.
                {"type": "Process", "name": "PaymentService::pay", "description": "x", "confidence": 1.0,
                 "evidence": [{"file": "src/payment.rs", "start": 1, "end": 1, "symbol": "pay", "reason": "fn"}]},
                // Symbol found nowhere: kept without it, lower confidence.
                {"type": "dependency", "name": "Stripe", "description": "x", "confidence": 1.0,
                 "evidence": [{"file": "src/payment.rs", "start": 1, "end": 1, "symbol": "StripeApi", "reason": "use"}]},
                // Outside the file or without evidence: dropped.
                {"type": "Component", "name": "Ghost", "description": "x", "confidence": 1.0,
                 "evidence": [{"file": "src/payment.rs", "start": 90, "end": 95, "reason": "none"}]},
                {"type": "Component", "name": "Nothing", "description": "x", "confidence": 1.0, "evidence": []},
                {"type": "Widget", "name": "Unknown type", "description": "x", "confidence": 1.0,
                 "evidence": [{"file": "src/payment.rs", "start": 1, "end": 1, "reason": "x"}]}
            ],
            "edges": [
                {"source": {"type": "Component", "name": "PaymentService"}, "relation": "calls",
                 "target": {"type": "Dependency", "name": "Stripe"}, "confidence": 0.95,
                 "evidence": [{"file": "src/payment.rs", "start": 9, "end": 9, "symbol": "charge", "reason": "charge"}]},
                // Inverse relation name: ends swapped.
                {"source": {"type": "Dependency", "name": "Stripe"}, "relation": "calledBy",
                 "target": {"type": "Process", "name": "PaymentService::pay"}, "confidence": 0.9,
                 "evidence": [{"file": "src/payment.rs", "start": 9, "end": 9, "reason": "call"}]},
                {"source": {"type": "Component", "name": "A"}, "relation": "likes",
                 "target": {"type": "Component", "name": "B"}, "confidence": 0.9,
                 "evidence": [{"file": "src/payment.rs", "start": 9, "end": 9, "reason": "x"}]}
            ]
        });
        let out = parse(&answer, &Schema::builtin(), &parts);
        assert_eq!(out.dropped, 4);
        let (nodes, edges) = &out.files["src/payment.rs"];
        let found: Vec<String> = nodes
            .iter()
            .map(|n| {
                let e = &n.evidence[0];
                format!(
                    "{} {}-{} {:?} {}",
                    n.id, e.start_line, e.end_line, e.symbol, n.confidence
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                "component:payment-service 3-5 Some(\"PaymentService\") 0.9",
                "process:payment-service-pay 8-10 Some(\"pay\") 1",
                "dependency:stripe 1-1 None 0.8",
            ]
        );
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[1].source.name, "PaymentService::pay");
        assert_eq!(edges[1].relation, "calls");
        assert_eq!(edges[1].target.name, "Stripe");
        assert!(edges.iter().all(|e| e.evidence[0].source_type == "LLM"));
    }

    #[test]
    fn evidence_of_a_long_file_stays_in_the_part_shown() {
        let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let s = source("big.txt", &text);
        let parts = [Part {
            source: &s,
            range: (11, 20),
        }];
        let e = json!({"file": "big.txt", "start": 5, "end": 25, "reason": "x"});
        let (ev, _) = ground(&e, &parts).unwrap();
        assert_eq!((ev.start_line, ev.end_line), (11, 20));
        let outside = json!({"file": "big.txt", "start": 21, "end": 25, "reason": "x"});
        assert!(ground(&outside, &parts).is_none());
        assert!(line_has("fn run() {", "run") && !line_has("running", "run"));
    }
}
