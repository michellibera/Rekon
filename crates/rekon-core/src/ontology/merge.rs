//! Deterministic merge of static facts and per-file model facts into one graph.
//! No model calls: the same facts always give the same graph.
//!
//! 1. Root System, packages and dependencies from manifests (`Config` evidence).
//! 2. Nodes of all files, united by id (evidence added up).
//! 3. Code elements with the same name under different types become one node.
//! 4. Edge ends resolved by id, then by name; an unknown end becomes a node whose
//!    evidence is the edge's.
//! 5. `implementedBy` links from code elements to their source files.
//! 6. Top-level components go into their package (`contains`), and every part of the
//!    graph that cannot be reached from the root is attached to its package, so the
//!    explorer can reach everything from the root.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde_json::Value;

use super::manifest::Manifests;
use super::model::{self, Edge, Evidence, FileFacts, Node, Ref, add_evidence, node_id, slug, source};
use super::schema::{COMPONENT, CONTAINS, DEPENDENCY, DEPENDS_ON, IMPLEMENTED_BY, SOURCE_CODE, SYSTEM, Schema};

pub struct Input<'a> {
    pub schema: &'a Schema,
    /// Repository name (the root System).
    pub repo: &'a str,
    /// One-sentence description of the project, if known.
    pub summary: Option<&'a str>,
    pub manifests: &'a Manifests,
    /// README path and line count: evidence of the root when there is no workspace manifest.
    pub readme: Option<(&'a str, u32)>,
    /// Fresh facts per file.
    pub facts: &'a [(String, FileFacts)],
    /// Line count of a repository file.
    pub lines: &'a dyn Fn(&str) -> Option<u32>,
}

pub struct Merged {
    pub root: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

struct Builder<'a> {
    schema: &'a Schema,
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<String, Edge>,
}

fn evidence(file: &str, lines: (u32, u32), symbol: Option<&str>, reason: &str, kind: &str) -> Evidence {
    Evidence {
        file: file.to_string(),
        start_line: lines.0,
        end_line: lines.1,
        symbol: symbol.map(str::to_string),
        reason: reason.to_string(),
        source_type: kind.to_string(),
    }
}

impl Builder<'_> {
    fn add_node(&mut self, node: Node) -> String {
        let id = node.id.clone();
        match self.nodes.get_mut(&id) {
            Some(n) => absorb(n, node),
            None => {
                self.nodes.insert(id.clone(), node);
            }
        }
        id
    }

    fn add_edge(&mut self, source: &str, relation: &str, target: &str, confidence: f64, ev: Vec<Evidence>) {
        if source == target {
            return;
        }
        let id = model::edge_id(source, relation, target);
        let e = self.edges.entry(id).or_insert_with(|| {
            let mut e = Edge::new(source, relation, target);
            e.confidence = 0.0;
            e
        });
        e.confidence = e.confidence.max(confidence);
        add_evidence(&mut e.evidence, ev);
    }

    fn has_incoming(&self, id: &str, relation: &str) -> bool {
        self.edges.values().any(|e| e.target == id && e.relation == relation)
    }
}

/// Adds what `other` knows about the same element to `n`.
fn absorb(n: &mut Node, other: Node) {
    add_evidence(&mut n.evidence, other.evidence);
    n.confidence = n.confidence.max(other.confidence);
    if n.description.is_none() {
        n.description = other.description;
    }
    for (k, v) in other.metadata {
        n.metadata.entry(k).or_insert(v);
    }
}

fn is_package(n: &Node) -> bool {
    n.metadata.get("package").and_then(Value::as_bool) == Some(true)
}

/// Folder of a package, from its metadata.
fn package_dir(n: &Node) -> Option<&str> {
    n.metadata.get("path").and_then(Value::as_str)
}

fn in_dir(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/"))
}

pub fn merge(input: &Input) -> Merged {
    let mut b = Builder {
        schema: input.schema,
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    let root = add_static(&mut b, input);
    for (_, facts) in input.facts {
        for n in &facts.nodes {
            b.add_node(n.clone());
        }
    }
    let alias = unify_code_names(&mut b);
    add_fact_edges(&mut b, input, &alias);
    add_source_files(&mut b, input);
    contain_components(&mut b, &root);
    attach_unreachable(&mut b, &root);
    Merged {
        root,
        nodes: b.nodes.into_values().collect(),
        edges: b.edges.into_values().collect(),
    }
}

/// Root, packages, dependencies; returns the root id.
fn add_static(b: &mut Builder, input: &Input) -> String {
    let m = input.manifests;
    let mut root = Node::new(SYSTEM, input.repo);
    root.description = input.summary.map(str::to_string);
    root.metadata.insert("root".into(), Value::Bool(true));
    let root_package = m.packages.iter().find(|p| p.dir.is_empty());
    if let Some((file, lines)) = &m.workspace {
        root.evidence
            .push(evidence(file, *lines, None, "workspace manifest", source::CONFIG));
    } else if let Some(p) = root_package {
        root.evidence.push(evidence(
            &p.manifest,
            p.lines,
            Some(&p.name),
            "root manifest",
            source::CONFIG,
        ));
    } else if let Some((readme, n)) = input.readme {
        root.evidence.push(evidence(
            readme,
            (1, n.clamp(1, 30)),
            None,
            "project readme",
            source::STATIC,
        ));
    }
    let root_id = b.add_node(root);

    for p in &m.packages {
        let mut n = Node::new(COMPONENT, &p.name);
        n.metadata.insert("package".into(), Value::Bool(true));
        n.metadata.insert("path".into(), Value::String(p.dir.clone()));
        n.metadata.insert("manifest".into(), Value::String(p.manifest.clone()));
        let ev = evidence(&p.manifest, p.lines, Some(&p.name), "package manifest", source::CONFIG);
        n.evidence.push(ev.clone());
        let id = b.add_node(n);
        b.add_edge(&root_id, CONTAINS, &id, 1.0, vec![ev]);
    }
    let dependency = |b: &mut Builder, name: &str, ev: Evidence| -> String {
        let mut n = Node::new(DEPENDENCY, name);
        n.evidence.push(ev);
        b.add_node(n)
    };
    // A dependency becomes a node when a package uses it, with its workspace declaration
    // as evidence when there is one (so dev-only dependencies never appear).
    let key = |name: &str| name.to_lowercase().replace('_', "-");
    let declared: HashMap<String, &super::manifest::Dep> = m.shared.iter().map(|d| (key(&d.name), d)).collect();
    let mut uses: Vec<(String, &super::manifest::Dep)> = Vec::new();
    for p in &m.packages {
        let owner = node_id(COMPONENT, &p.name);
        uses.extend(p.deps.iter().map(|d| (owner.clone(), d)));
    }
    for (dir, d) in &m.loose {
        let owner = m
            .packages
            .iter()
            .filter(|p| in_dir(dir, &p.dir))
            .max_by_key(|p| p.dir.len())
            .map_or(root_id.clone(), |p| node_id(COMPONENT, &p.name));
        uses.push((owner, d));
    }
    for (owner, d) in uses {
        let ev = evidence(
            &d.manifest,
            (d.line, d.line),
            Some(&d.name),
            "dependency declaration",
            source::CONFIG,
        );
        let target = if m.is_package(&d.name) {
            node_id(COMPONENT, &d.name)
        } else {
            let id = node_id(DEPENDENCY, &d.name);
            if !b.nodes.contains_key(&id) {
                let first = declared.get(&key(&d.name)).map_or(ev.clone(), |s| {
                    evidence(
                        &s.manifest,
                        (s.line, s.line),
                        Some(&s.name),
                        "declared dependency",
                        source::CONFIG,
                    )
                });
                dependency(b, &d.name, first);
            }
            id
        };
        b.add_edge(&owner, DEPENDS_ON, &target, 1.0, vec![ev]);
    }
    root_id
}

/// Code elements with the same name under different types are one element seen by
/// different model calls; the one with the most evidence wins, on a tie the more
/// specific type (later in the schema: Event over DataEntity, Process over
/// Component). Returns old id → new id.
fn unify_code_names(b: &mut Builder) -> HashMap<String, String> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in b.nodes.values().filter(|n| b.schema.is_code(&n.kind)) {
        groups.entry(slug(&n.name)).or_default().push(n.id.clone());
    }
    let type_rank = |kind: &str| b.schema.types.iter().position(|t| t.id == kind).unwrap_or(0);
    let mut alias = HashMap::new();
    for ids in groups.into_values().filter(|ids| ids.len() > 1) {
        let best = ids
            .iter()
            .max_by(|x, y| {
                let (nx, ny) = (&b.nodes[*x], &b.nodes[*y]);
                (is_package(nx), nx.evidence.len())
                    .cmp(&(is_package(ny), ny.evidence.len()))
                    .then(nx.confidence.total_cmp(&ny.confidence))
                    .then(type_rank(&nx.kind).cmp(&type_rank(&ny.kind)))
            })
            .cloned()
            .expect("group is not empty");
        for id in ids.iter().filter(|id| **id != best) {
            if let Some(other) = b.nodes.remove(id) {
                absorb(b.nodes.get_mut(&best).expect("best exists"), other);
                alias.insert(id.clone(), best.clone());
            }
        }
    }
    // A plain name (`Runner`) and the only qualified name ending the same way
    // (`init::Runner`) are one element when they share a file, or when the plain one
    // is only referenced.
    let qualified = |name: &str| name.contains("::") || name.contains('.');
    let mut by_last: HashMap<String, Vec<String>> = HashMap::new();
    for n in b
        .nodes
        .values()
        .filter(|n| b.schema.is_code(&n.kind) && qualified(&n.name))
    {
        by_last.entry(last_segment(&n.name)).or_default().push(n.id.clone());
    }
    let plain: Vec<String> = b
        .nodes
        .values()
        .filter(|n| b.schema.is_code(&n.kind) && !qualified(&n.name) && !is_package(n))
        .map(|n| n.id.clone())
        .collect();
    for id in plain {
        let Some(target) = by_last
            .get(&slug(&b.nodes[&id].name))
            .filter(|t| t.len() == 1)
            .map(|t| t[0].clone())
        else {
            continue;
        };
        let (n, t) = (&b.nodes[&id], &b.nodes[&target]);
        let shared_file = n.evidence.iter().any(|e| t.evidence.iter().any(|f| f.file == e.file));
        if !(shared_file || is_referenced(n)) {
            continue;
        }
        if let Some(other) = b.nodes.remove(&id) {
            absorb(b.nodes.get_mut(&target).expect("target exists"), other);
            for v in alias.values_mut().filter(|v| **v == id) {
                *v = target.clone();
            }
            alias.insert(id, target);
        }
    }
    alias
}

fn is_referenced(n: &Node) -> bool {
    n.metadata.get("referenced").and_then(Value::as_bool) == Some(true)
}

/// Last segment of a code name (`Store::update` → `update`), as a slug.
fn last_segment(name: &str) -> String {
    slug(name.rsplit([':', '.', '/']).find(|s| !s.is_empty()).unwrap_or(name))
}

fn add_fact_edges(b: &mut Builder, input: &Input, alias: &HashMap<String, String>) {
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_last: HashMap<String, Vec<String>> = HashMap::new();
    for n in b.nodes.values() {
        by_name.entry(slug(&n.name)).or_default().push(n.id.clone());
        by_last.entry(last_segment(&n.name)).or_default().push(n.id.clone());
    }
    for (_, facts) in input.facts {
        for e in &facts.edges {
            let mut resolve = |r: &Ref, b: &mut Builder| -> String {
                let id = node_id(&r.kind, &r.name);
                let id = alias.get(&id).cloned().unwrap_or(id);
                if b.nodes.contains_key(&id) {
                    return id;
                }
                let code = b.schema.is_code(&r.kind);
                let pick = |ids: &Vec<String>| {
                    ids.iter()
                        .find(|i| b.schema.is_code(&b.nodes[*i].kind) == code)
                        .or(ids.first())
                        .cloned()
                };
                if let Some(found) = by_name.get(&slug(&r.name)).and_then(pick) {
                    return found;
                }
                if let Some(ids) = by_last.get(&last_segment(&r.name))
                    && ids.len() == 1
                {
                    return ids[0].clone();
                }
                // An element only referenced: its evidence is the reference.
                let mut n = Node::new(&r.kind, &r.name);
                n.confidence = e.confidence;
                n.metadata.insert("referenced".into(), Value::Bool(true));
                n.evidence = e
                    .evidence
                    .iter()
                    .map(|ev| Evidence {
                        reason: format!("referenced: {}", ev.reason),
                        ..ev.clone()
                    })
                    .collect();
                let id = b.add_node(n);
                by_name.entry(slug(&r.name)).or_default().push(id.clone());
                by_last.entry(last_segment(&r.name)).or_default().push(id.clone());
                id
            };
            let s = resolve(&e.source, b);
            let t = resolve(&e.target, b);
            b.add_edge(&s, &e.relation, &t, e.confidence, e.evidence.clone());
        }
    }
}

/// Shortest path suffixes (by folders) that are unique among `paths`.
fn short_names(paths: &BTreeSet<String>) -> HashMap<String, String> {
    let suffix = |p: &str, k: usize| {
        let parts: Vec<&str> = p.split('/').collect();
        parts[parts.len().saturating_sub(k)..].join("/")
    };
    let mut out = HashMap::new();
    for p in paths {
        let depth = p.split('/').count();
        let name = (1..=depth)
            .map(|k| suffix(p, k))
            .find(|s| paths.iter().filter(|q| suffix(q, s.split('/').count()) == *s).count() == 1)
            .unwrap_or_else(|| p.clone());
        out.insert(p.clone(), name);
    }
    out
}

fn add_source_files(b: &mut Builder, input: &Input) {
    let mut links: Vec<(String, String, f64, Vec<Evidence>)> = Vec::new();
    for n in b.nodes.values() {
        if !b.schema.is_code(&n.kind) || is_package(n) || is_referenced(n) {
            continue;
        }
        let mut by_file: BTreeMap<&str, Vec<Evidence>> = BTreeMap::new();
        for e in n.evidence.iter().filter(|e| e.source_type == source::LLM) {
            by_file.entry(e.file.as_str()).or_default().push(e.clone());
        }
        for (file, ev) in by_file {
            links.push((n.id.clone(), file.to_string(), n.confidence, ev));
        }
    }
    let files: BTreeSet<String> = links.iter().map(|l| l.1.clone()).collect();
    let names = short_names(&files);
    for file in &files {
        let mut n = Node::new(SOURCE_CODE, file);
        n.name = names.get(file).cloned().unwrap_or_else(|| file.clone());
        n.metadata.insert("path".into(), Value::String(file.clone()));
        let last = (input.lines)(file).unwrap_or(1).max(1);
        n.evidence
            .push(evidence(file, (1, last), None, "source file", source::STATIC));
        b.add_node(n);
    }
    for (id, file, confidence, ev) in links {
        b.add_edge(&id, IMPLEMENTED_BY, &node_id(SOURCE_CODE, &file), confidence, ev);
    }
}

/// Package whose folder holds `file` (the deepest one).
fn package_of(b: &Builder, file: &str) -> Option<String> {
    b.nodes
        .values()
        .filter(|n| is_package(n))
        .filter_map(|n| Some((n, package_dir(n)?)))
        .filter(|(_, dir)| in_dir(file, dir))
        .max_by_key(|(_, dir)| dir.len())
        .map(|(n, _)| n.id.clone())
}

fn first_code_evidence(n: &Node) -> Option<&Evidence> {
    n.evidence
        .iter()
        .find(|e| e.source_type == source::LLM)
        .or(n.evidence.first())
}

fn containment(n: &Node, reason: &str) -> Vec<Evidence> {
    first_code_evidence(n)
        .map(|e| Evidence {
            reason: reason.to_string(),
            source_type: source::STATIC.to_string(),
            ..e.clone()
        })
        .into_iter()
        .collect()
}

/// Components that nothing contains go into the package of their file (or the root).
fn contain_components(b: &mut Builder, root: &str) {
    let top: Vec<String> = b
        .nodes
        .values()
        .filter(|n| n.kind == COMPONENT && !is_package(n) && n.id != root)
        .filter(|n| !b.has_incoming(&n.id, CONTAINS))
        .map(|n| n.id.clone())
        .collect();
    for id in top {
        let n = &b.nodes[&id];
        let owner = first_code_evidence(n)
            .and_then(|e| package_of(b, &e.file))
            .unwrap_or_else(|| root.to_string());
        let ev = containment(n, "defined in this package");
        let confidence = n.confidence;
        b.add_edge(&owner, CONTAINS, &id, confidence, ev);
    }
}

/// Attaches every part of the graph not reachable from the root: its most central
/// node goes into the package of its file (or the root).
fn attach_unreachable(b: &mut Builder, root: &str) {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in b.edges.values() {
        adj.entry(&e.source).or_default().push(&e.target);
        adj.entry(&e.target).or_default().push(&e.source);
    }
    let component = |start: &str, seen: &mut BTreeSet<String>| {
        let mut part = vec![start.to_string()];
        let mut queue = VecDeque::from([start.to_string()]);
        seen.insert(start.to_string());
        while let Some(id) = queue.pop_front() {
            for next in adj.get(id.as_str()).into_iter().flatten() {
                if seen.insert(next.to_string()) {
                    part.push(next.to_string());
                    queue.push_back(next.to_string());
                }
            }
        }
        part
    };
    let mut seen = BTreeSet::new();
    component(root, &mut seen);
    let ids: Vec<String> = b.nodes.keys().cloned().collect();
    let mut attach = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            continue;
        }
        let part = component(&id, &mut seen);
        let degree = |i: &String| adj.get(i.as_str()).map_or(0, Vec::len);
        let best = part
            .iter()
            .filter(|i| b.nodes[*i].kind != SOURCE_CODE)
            .max_by(|x, y| {
                (b.nodes[*x].kind == COMPONENT, degree(x))
                    .cmp(&(b.nodes[*y].kind == COMPONENT, degree(y)))
                    .then(y.cmp(x))
            })
            .unwrap_or(&part[0])
            .clone();
        attach.push(best);
    }
    for id in attach {
        let n = &b.nodes[&id];
        let owner = first_code_evidence(n)
            .and_then(|e| package_of(b, &e.file))
            .filter(|o| *o != id)
            .unwrap_or_else(|| root.to_string());
        // External things are needed, not contained.
        let (relation, reason) = match n.kind.as_str() {
            DEPENDENCY | SYSTEM => (DEPENDS_ON, "used by this package"),
            _ => (CONTAINS, "defined in this package"),
        };
        let ev = containment(n, reason);
        let confidence = n.confidence;
        b.add_edge(&owner, relation, &id, confidence, ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::manifest::{Dep, Package};
    use crate::ontology::model::RawEdge;

    fn ev(file: &str, a: u32, b: u32) -> Evidence {
        evidence(file, (a, b), None, "r", source::LLM)
    }

    fn node(kind: &str, name: &str, file: &str, a: u32, b: u32) -> Node {
        let mut n = Node::new(kind, name);
        n.confidence = 0.9;
        n.evidence.push(ev(file, a, b));
        n
    }

    fn edge(s: (&str, &str), rel: &str, t: (&str, &str), file: &str, line: u32) -> RawEdge {
        RawEdge {
            source: Ref {
                kind: s.0.into(),
                name: s.1.into(),
            },
            relation: rel.into(),
            target: Ref {
                kind: t.0.into(),
                name: t.1.into(),
            },
            confidence: 0.8,
            evidence: vec![ev(file, line, line)],
        }
    }

    fn facts(nodes: Vec<Node>, edges: Vec<RawEdge>) -> FileFacts {
        FileFacts {
            version: 1,
            hash: "h".into(),
            schema: "s".into(),
            nodes,
            edges,
        }
    }

    fn run(facts: &[(String, FileFacts)]) -> Merged {
        let manifests = Manifests {
            packages: vec![Package {
                name: "shop-core".into(),
                dir: "core".into(),
                manifest: "core/Cargo.toml".into(),
                lines: (1, 3),
                deps: vec![Dep {
                    name: "serde".into(),
                    manifest: "core/Cargo.toml".into(),
                    line: 6,
                }],
            }],
            workspace: Some(("Cargo.toml".into(), (1, 4))),
            ..Default::default()
        };
        let schema = Schema::builtin();
        let lines = |_: &str| Some(50);
        merge(&Input {
            schema: &schema,
            repo: "Shop",
            summary: Some("Test shop"),
            manifests: &manifests,
            readme: None,
            facts,
            lines: &lines,
        })
    }

    fn ids(m: &Merged) -> Vec<String> {
        m.edges
            .iter()
            .map(|e| e.id.trim_start_matches("edge:").to_string())
            .collect()
    }

    #[test]
    fn static_part_has_root_packages_and_dependencies() {
        let m = run(&[]);
        assert_eq!(m.root, "system:shop");
        assert_eq!(
            ids(&m),
            [
                "component:shop-core:dependsOn:dependency:serde",
                "system:shop:contains:component:shop-core",
            ]
        );
        let root = m.nodes.iter().find(|n| n.id == m.root).unwrap();
        assert_eq!(root.evidence[0].location(), "Cargo.toml:1-4");
        assert_eq!(root.evidence[0].source_type, "Config");
        assert_eq!(root.description.as_deref(), Some("Test shop"));
    }

    #[test]
    fn facts_are_united_resolved_linked_to_files_and_contained() {
        let f1 = facts(
            vec![
                node("Component", "OrderService", "core/order.rs", 3, 40),
                node("Process", "OrderService::create", "core/order.rs", 10, 20),
                node("Event", "OrderCreated", "core/order.rs", 1, 2),
            ],
            vec![
                edge(
                    ("Component", "OrderService"),
                    "executes",
                    ("Process", "OrderService::create"),
                    "core/order.rs",
                    10,
                ),
                edge(
                    ("Process", "OrderService::create"),
                    "emits",
                    ("Event", "OrderCreated"),
                    "core/order.rs",
                    18,
                ),
                // Defined elsewhere under another type: resolved by name.
                edge(
                    ("Component", "OrderService"),
                    "writes",
                    ("DataStore", "Repo"),
                    "core/order.rs",
                    15,
                ),
                edge(
                    ("Process", "create"),
                    "reads",
                    ("DataEntity", "Product"),
                    "core/order.rs",
                    12,
                ),
            ],
        );
        let f2 = facts(
            vec![
                // Same element typed differently in another file: united.
                node("DataEntity", "OrderCreated", "core/events.rs", 1, 9),
                node("DataStore", "Repo", "core/repo.rs", 1, 30),
                node("Component", "PaymentService", "pay/pay.rs", 1, 50),
            ],
            vec![edge(
                ("Component", "PaymentService"),
                "consumes",
                ("DataEntity", "OrderCreated"),
                "pay/pay.rs",
                7,
            )],
        );
        let m = run(&[("core/order.rs".into(), f1), ("core/events.rs".into(), f2)]);
        let n = |id: &str| m.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id}"));
        let created = n("event:order-created");
        assert_eq!(created.evidence.len(), 2, "evidence from both files");
        assert!(m.nodes.iter().all(|n| n.id != "data-entity:order-created"));
        // `create` resolved by its last segment; Product only referenced.
        let product = n("data-entity:product");
        assert_eq!(product.evidence[0].reason, "referenced: r");
        assert_eq!(n("source-code:core-order-rs").name, "order.rs");
        let edges = ids(&m);
        for expected in [
            "component:order-service:executes:process:order-service-create",
            "process:order-service-create:emits:event:order-created",
            "component:order-service:writes:data-store:repo",
            "process:order-service-create:reads:data-entity:product",
            "component:payment-service:consumes:event:order-created",
            "component:order-service:implementedBy:source-code:core-order-rs",
            "event:order-created:implementedBy:source-code:core-events-rs",
            "component:shop-core:contains:component:order-service",
            // Not in a package folder: the root holds it.
            "system:shop:contains:component:payment-service",
        ] {
            assert!(edges.contains(&expected.to_string()), "{expected} in {edges:#?}");
        }
        // Everything is reachable from the root.
        let mut reach = BTreeSet::from([m.root.clone()]);
        loop {
            let before = reach.len();
            for e in &m.edges {
                if reach.contains(&e.source) || reach.contains(&e.target) {
                    reach.insert(e.source.clone());
                    reach.insert(e.target.clone());
                }
            }
            if reach.len() == before {
                break;
            }
        }
        assert_eq!(reach.len(), m.nodes.len());
        let contains = m
            .edges
            .iter()
            .find(|e| e.id.ends_with(":contains:component:order-service"))
            .unwrap();
        assert_eq!(contains.evidence[0].source_type, "Static");
    }

    #[test]
    fn isolated_parts_get_one_attachment() {
        let f = facts(
            vec![
                node("DataEntity", "Invoice", "core/invoice.rs", 1, 5),
                node("DataEntity", "Line", "core/invoice.rs", 6, 9),
            ],
            vec![edge(
                ("DataEntity", "Invoice"),
                "derivedFrom",
                ("DataEntity", "Line"),
                "core/invoice.rs",
                2,
            )],
        );
        let m = run(&[("core/invoice.rs".into(), f)]);
        let attached: Vec<_> = m
            .edges
            .iter()
            .filter(|e| e.relation == "contains" && e.source == "component:shop-core")
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(attached.len(), 1, "{attached:?}");
    }

    #[test]
    fn plain_names_join_their_qualified_twin_and_externals_are_needed() {
        let f = facts(
            vec![
                node("Component", "Runner", "core/init.rs", 100, 200),
                node("Component", "init::Runner", "core/init.rs", 90, 99),
                node("DataEntity", "Options", "core/index.rs", 1, 5),
                node("DataEntity", "init::Options", "core/init.rs", 1, 5),
                node("System", "Claude CLI", "core/init.rs", 7, 7),
            ],
            vec![],
        );
        let m = run(&[("core/init.rs".into(), f)]);
        let ids: Vec<&str> = m.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"component:runner"), "{ids:?}");
        assert_eq!(
            m.nodes
                .iter()
                .find(|n| n.id == "component:init-runner")
                .unwrap()
                .evidence
                .len(),
            2
        );
        assert!(ids.contains(&"data-entity:options"), "another file, another element");
        assert!(
            m.edges
                .iter()
                .any(|e| e.id == "edge:component:shop-core:dependsOn:system:claude-cli"),
            "{:#?}",
            m.edges.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_file_names_are_unique() {
        let paths: BTreeSet<String> = ["a/src/mod.rs", "b/src/mod.rs", "a/src/x.rs", "c/other/mod.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let n = short_names(&paths);
        assert_eq!(n["a/src/x.rs"], "x.rs");
        assert_eq!(n["a/src/mod.rs"], "a/src/mod.rs");
        assert_eq!(n["c/other/mod.rs"], "other/mod.rs");
    }
}
