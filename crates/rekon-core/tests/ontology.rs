mod common;

use common::{ctx, repo, write};
use rekon_core::ontology::graph::{self, GraphIndex};
use rekon_core::ontology::index::{self, Options, Progress};

fn noop(_: &Progress) {}

fn load(root: &std::path::Path) -> GraphIndex {
    graph::load(&root.join(".rekon")).unwrap().expect("graph.json written")
}

#[test]
fn index_builds_a_grounded_graph_and_reuses_unchanged_files() {
    let dir = repo();
    let root = dir.path();
    write(
        root,
        "src/api/orders.rs",
        "use crate::store::Store;\n\npub struct Order {\n    id: u32,\n}\n\npub fn create(store: &Store) -> Order {\n    Order { id: 1 }\n}\n",
    );
    write(root, "src/store.rs", "pub struct Store {\n    path: String,\n}\n");
    write(root, "tests/it.rs", "fn test_it() {}\n");
    let (ctx, fake) = ctx(root);

    let report = index::run(&ctx, &Options::default(), &noop).unwrap();
    // src/main.rs, src/api/orders.rs, src/api/mod.rs, src/store.rs, Cargo.toml; tests left out.
    assert_eq!((report.files, report.analyzed, report.errors), (5, 5, 0), "{report:?}");
    let calls = fake.call_count();
    assert!(calls >= 1);
    let g = load(root);
    let root_node = g.root().unwrap();
    assert_eq!(root_node.kind, "System");
    // Static facts: the package from Cargo.toml.
    assert!(g.node("component:shop").is_some());
    let order = g.node("data-entity:order").expect("struct from the outline");
    assert_eq!(order.evidence[0].file, "src/api/orders.rs");
    assert_eq!((order.evidence[0].start_line, order.evidence[0].end_line), (3, 5));
    // Cross-file reference resolved to the element defined in store.rs.
    assert!(
        g.edge("edge:component:orders-module:calls:data-entity:store").is_some(),
        "{:#?}",
        g.graph.edges.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert!(g.node("source-code:src-store-rs").is_some());
    // Every node has evidence except possibly the root.
    for n in &g.graph.nodes {
        assert!(
            !n.evidence.is_empty() || n.id == root_node.id,
            "{} has no evidence",
            n.id
        );
    }
    // Every edge has evidence.
    for e in &g.graph.edges {
        assert!(!e.evidence.is_empty(), "{} has no evidence", e.id);
    }
    assert!(root.join(".rekon/ontology/facts/src/store.rs.json").exists());

    // Nothing changed: no model call, same graph.
    let again = index::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(fake.call_count(), calls);
    assert_eq!((again.analyzed, again.reused), (0, 5));
    assert_eq!(load(root).graph.nodes, g.graph.nodes);

    // One file changed: only it goes to the model.
    write(
        root,
        "src/store.rs",
        "pub struct Store {\n    path: String,\n}\n\npub fn open() {}\n",
    );
    let third = index::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!((third.analyzed, third.reused), (1, 4));
    assert_eq!(fake.call_count(), calls + 1);
    assert!(load(root).node("process:store-open").is_some());

    // A deleted file loses its facts and its nodes.
    std::fs::remove_file(root.join("src/store.rs")).unwrap();
    index::run(&ctx, &Options::default(), &noop).unwrap();
    assert!(!root.join(".rekon/ontology/facts/src/store.rs.json").exists());
    assert!(load(root).node("process:store-open").is_none());
}

#[test]
fn forced_paths_are_analyzed_again() {
    let dir = repo();
    let root = dir.path();
    let (ctx, fake) = ctx(root);
    index::run(&ctx, &Options::default(), &noop).unwrap();
    let calls = fake.call_count();
    let report = index::run(
        &ctx,
        &Options {
            force: true,
            paths: Some(vec!["src/main.rs".into()]),
            dry_run: false,
        },
        &noop,
    )
    .unwrap();
    assert_eq!(report.analyzed, 1);
    assert_eq!(fake.call_count(), calls + 1);
    // A dry run lists the requests and calls nothing.
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {}
",
    )
    .unwrap();
    let dry = index::run(
        &ctx,
        &Options {
            dry_run: true,
            ..Options::default()
        },
        &noop,
    )
    .unwrap();
    assert_eq!(dry.planned, [vec!["src/main.rs:1-1".to_string()]]);
    assert_eq!(fake.call_count(), calls + 1);
}
