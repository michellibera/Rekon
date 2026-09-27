//! Tests of the ontology view with a small graph (the order/payment example).

use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Color;
use rekon_core::ontology::model::{Edge, Evidence, GRAPH_VERSION, Graph, Node, Scan};
use rekon_core::ontology::schema::Schema;

use super::app::View;
use super::graph::{Filter, Sel};
use super::tests::{Fixture, fixture};

const ORDER_SERVICE: &str = "component:order-service";
const PAYMENT_SERVICE: &str = "component:payment-service";
const ORDER_CREATED: &str = "event:order-created";
const EMITS: &str = "edge:component:order-service:emits:event:order-created";
const CONSUMES: &str = "edge:component:payment-service:consumes:event:order-created";

fn ev(file: &str, a: u32, b: u32, reason: &str) -> Evidence {
    Evidence {
        file: file.into(),
        start_line: a,
        end_line: b,
        symbol: None,
        reason: reason.into(),
        source_type: "LLM".into(),
    }
}

fn graph(extra: bool) -> Graph {
    let node = |kind: &str, name: &str, e: Evidence| {
        let mut n = Node::new(kind, name);
        n.evidence.push(e);
        n
    };
    let edge = |s: &str, r: &str, t: &str, e: Vec<Evidence>| {
        let mut x = Edge::new(s, r, t);
        x.evidence = e;
        x
    };
    let mut nodes = vec![
        node("System", "Shop", ev("src/main.rs", 1, 3, "entry point")),
        node("Component", "OrderService", ev("src/main.rs", 1, 3, "orders")),
        node("Component", "PaymentService", ev("src/lib.rs", 1, 1, "payments")),
        node("DataStore", "PostgreSQL", ev("src/util.rs", 1, 1, "database")),
        node("Process", "CreateOrder", ev("src/main.rs", 5, 5, "function")),
        node("DataEntity", "Order", ev("src/main.rs", 2, 2, "record")),
        node("Event", "OrderCreated", ev("src/main.rs", 2, 2, "event")),
    ];
    let mut edges = vec![
        edge(
            "system:shop",
            "contains",
            ORDER_SERVICE,
            vec![ev("src/main.rs", 1, 1, "part")],
        ),
        edge(
            "system:shop",
            "contains",
            PAYMENT_SERVICE,
            vec![ev("src/lib.rs", 1, 1, "part")],
        ),
        edge(
            "system:shop",
            "contains",
            "data-store:postgre-sql",
            vec![ev("src/util.rs", 1, 1, "part")],
        ),
        edge(
            ORDER_SERVICE,
            "executes",
            "process:create-order",
            vec![ev("src/main.rs", 2, 2, "call")],
        ),
        edge(
            ORDER_SERVICE,
            "writes",
            "data-entity:order",
            vec![ev("src/main.rs", 2, 2, "save")],
        ),
        edge(
            ORDER_SERVICE,
            "emits",
            ORDER_CREATED,
            vec![ev("src/main.rs", 2, 2, "publish"), ev("src/lib.rs", 1, 1, "event type")],
        ),
        edge(
            PAYMENT_SERVICE,
            "consumes",
            ORDER_CREATED,
            vec![ev("src/lib.rs", 1, 1, "handler")],
        ),
        edge(
            "data-entity:order",
            "storedIn",
            "data-store:postgre-sql",
            vec![ev("src/util.rs", 1, 1, "table")],
        ),
    ];
    if extra {
        nodes.push(node("DataEntity", "Invoice", ev("src/util.rs", 1, 1, "invoice")));
        edges.push(edge(
            ORDER_SERVICE,
            "produces",
            "data-entity:invoice",
            vec![ev("src/util.rs", 1, 1, "x")],
        ));
    }
    Graph {
        version: GRAPH_VERSION,
        schema: Schema::builtin(),
        root: Some("system:shop".into()),
        scan: Scan::default(),
        nodes,
        edges,
    }
}

fn write_graph(fx: &Fixture, extra: bool) {
    let path = fx.dir.path().join(".rekon/ontology/graph.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(&graph(extra)).unwrap()).unwrap();
}

/// Fixture with the graph written and the ontology view open and drawn.
fn open() -> Fixture {
    let mut fx = fixture(160, 40);
    write_graph(&fx, false);
    fx.draw();
    fx.key(KeyCode::Char('2'));
    fx.draw();
    fx
}

fn positions(fx: &Fixture) -> Vec<(String, i32, i32)> {
    let mut v: Vec<(String, i32, i32)> = fx
        .app
        .explorer
        .nodes
        .iter()
        .map(|(id, p)| (id.clone(), p.x, p.y))
        .collect();
    v.sort();
    v
}

fn visible(fx: &Fixture) -> Vec<String> {
    let mut v: Vec<String> = fx.app.explorer.nodes.keys().cloned().collect();
    v.sort();
    v
}

fn sel(fx: &Fixture) -> Option<Sel> {
    fx.app.explorer.sel.clone()
}

fn mouse(fx: &mut Fixture, kind: MouseEventKind, x: i32, y: i32) {
    fx.app.on_mouse(MouseEvent {
        kind,
        column: x as u16,
        row: y as u16,
        modifiers: KeyModifiers::NONE,
    });
    fx.draw();
}

#[test]
fn ontology_tab_without_a_graph_says_how_to_build_it() {
    let mut fx = fixture(120, 30);
    fx.draw();
    fx.key(KeyCode::Char('2'));
    let screen = fx.draw().join("\n");
    assert_eq!(fx.app.view, View::Ontology);
    assert!(screen.contains("1 Tree │ 2 Ontology"), "{screen}");
    assert!(screen.contains("No ontology yet"), "{screen}");
    fx.key(KeyCode::Char('1'));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("src/"), "tree again: {screen}");
}

#[test]
fn graph_starts_at_the_root_with_its_neighbors_below() {
    let mut fx = open();
    assert_eq!(sel(&fx), Some(Sel::Node("system:shop".into())));
    assert_eq!(
        visible(&fx),
        [
            "component:order-service",
            "component:payment-service",
            "data-store:postgre-sql",
            "system:shop"
        ]
    );
    let screen = fx.draw().join("\n");
    for name in ["Shop", "OrderService", "PaymentService", "PostgreSQL", "contains"] {
        assert!(screen.contains(name), "{name} in {screen}");
    }
    let root = fx.app.explorer.frame.rect(&Sel::Node("system:shop".into())).unwrap();
    for id in [ORDER_SERVICE, PAYMENT_SERVICE] {
        let r = fx.app.explorer.frame.rect(&Sel::Node(id.into())).unwrap();
        assert!(r.y > root.bottom(), "{id} below the root");
        assert!(
            fx.app.explorer.area.contains((r.x as u16, r.y as u16).into()),
            "{id} visible"
        );
    }
    // The footer describes the selection.
    assert!(screen.contains("System · Shop"), "{screen}");
}

#[test]
fn many_neighbors_of_one_relation_wait_behind_a_group_marker() {
    let mut fx = fixture(160, 40);
    let mut g = graph(false);
    for i in 0..6 {
        let name = format!("Table{i}");
        g.nodes.push(Node::new("DataEntity", &name));
        g.edges
            .push(Edge::new(ORDER_SERVICE, "reads", &format!("data-entity:table{i}")));
    }
    let path = fx.dir.path().join(".rekon/ontology/graph.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(&g).unwrap()).unwrap();
    fx.draw();
    fx.key(KeyCode::Char('2'));
    fx.draw();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.key(KeyCode::Char(' '));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("6 ▸"), "count on the marker: {screen}");
    let line = fx
        .app
        .explorer
        .frame
        .edges
        .iter()
        .find(|e| e.id == format!("group:{ORDER_SERVICE}:reads"));
    assert_eq!(
        line.and_then(|e| e.label.as_ref()).map(|l| l.text.as_str()),
        Some("reads"),
        "relation on the branch"
    );
    assert!(!visible(&fx).contains(&"data-entity:table0".to_string()));
    assert!(
        visible(&fx).contains(&"process:create-order".to_string()),
        "loose neighbors shown"
    );
    let group = Sel::Group(ORDER_SERVICE.into(), "reads".into());
    fx.app.explorer.select(group.clone());
    fx.key(KeyCode::Char(' '));
    fx.draw();
    assert!(fx.app.explorer.frame.rect(&group).is_none(), "marker gone");
    assert!((0..6).all(|i| visible(&fx).contains(&format!("data-entity:table{i}"))));
}

fn drawn(fx: &Fixture) -> Vec<String> {
    let mut v: Vec<String> = fx
        .app
        .explorer
        .frame
        .nodes
        .iter()
        .filter_map(|n| match &n.sel {
            Sel::Node(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

#[test]
fn filters_show_only_matching_nodes_and_reveal_hidden_ones() {
    let mut fx = open();
    fx.key(KeyCode::Char('f'));
    let screen = fx.draw().join(
        "
",
    );
    assert!(
        screen.contains("[ ] emits  (1)"),
        "nothing picked at the start: {screen}"
    );
    fx.key(KeyCode::Esc);
    // `emits` joins OrderService and OrderCreated; OrderCreated was hidden.
    assert!(fx.app.explorer.toggle_filter(Filter::Relation("emits".into())));
    fx.draw();
    assert_eq!(drawn(&fx), [ORDER_SERVICE, ORDER_CREATED]);
    let emits = fx.app.explorer.frame.edges.iter().find(|e| e.id == EMITS);
    assert!(emits.is_some(), "the picked relation is drawn");
    assert_eq!(
        fx.app.explorer.frame.edges.iter().filter(|e| !e.more).count(),
        1,
        "only it"
    );
    // A node type instead: only its nodes.
    fx.app.explorer.toggle_filter(Filter::Relation("emits".into()));
    fx.app.explorer.toggle_filter(Filter::Kind("DataEntity".into()));
    fx.draw();
    assert_eq!(drawn(&fx), ["data-entity:order"], "revealed through OrderService");
    // Nothing picked: the revealed nodes stay, the others come back.
    let place = fx.app.explorer.nodes["data-entity:order"].clone();
    fx.app.explorer.toggle_filter(Filter::Kind("DataEntity".into()));
    fx.draw();
    assert!(drawn(&fx).contains(&"system:shop".to_string()));
    assert_eq!(fx.app.explorer.nodes["data-entity:order"], place);
}

#[test]
fn space_expands_in_place_then_walks_through_the_relation() {
    let mut fx = open();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    let before = positions(&fx);
    let zoom = fx.app.explorer.zoom;
    fx.key(KeyCode::Char(' '));
    fx.draw();
    // Expanded: its neighbors appeared, the selection stayed, nothing visible moved.
    assert_eq!(sel(&fx), Some(Sel::Node(ORDER_SERVICE.into())));
    let after = positions(&fx);
    for p in &before {
        assert!(after.contains(p), "{p:?} moved");
    }
    assert_eq!(after.len(), before.len() + 3);
    assert_eq!(fx.app.explorer.zoom, zoom);
    let parent = fx.app.explorer.nodes[ORDER_SERVICE].clone();
    for id in ["process:create-order", "data-entity:order", ORDER_CREATED] {
        let p = &fx.app.explorer.nodes[id];
        assert!(p.y > parent.y, "{id} below OrderService");
        assert_eq!(p.parent.as_deref(), Some(ORDER_SERVICE));
    }
    // Then ↓: the relation right below, then the node it leads to.
    fx.key(KeyCode::Down);
    fx.draw();
    let Some(Sel::Edge(edge)) = sel(&fx) else {
        panic!("expected a relation, got {:?}", sel(&fx));
    };
    assert!(edge.starts_with("edge:component:order-service:"), "{edge}");
    fx.key(KeyCode::Down);
    fx.draw();
    let Some(Sel::Node(child)) = sel(&fx) else {
        panic!("expected a node, got {:?}", sel(&fx));
    };
    assert!(edge.ends_with(&child), "{edge} leads to {child}");
    // ← and → move between neighbors on the same row.
    let row_y = fx.app.explorer.nodes[&child].y;
    fx.key(KeyCode::Right);
    fx.key(KeyCode::Left);
    fx.draw();
    if let Some(Sel::Node(n)) = sel(&fx) {
        assert_eq!(fx.app.explorer.nodes[&n].y, row_y);
    }
    // Esc goes one level up.
    fx.app.explorer.select(Sel::Node(child.clone()));
    fx.key(KeyCode::Esc);
    assert_eq!(sel(&fx), Some(Sel::Node(ORDER_SERVICE.into())));
}

#[test]
fn expanding_a_node_draws_its_edge_to_an_already_visible_node() {
    let mut fx = open();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    fx.key(KeyCode::Char(' '));
    fx.draw();
    let payment = fx.app.explorer.nodes[PAYMENT_SERVICE].clone();
    assert!(fx.app.explorer.frame.edges.iter().all(|e| e.id != CONSUMES));
    fx.app.explorer.select(Sel::Node(ORDER_CREATED.into()));
    fx.draw();
    fx.key(KeyCode::Char(' '));
    fx.draw();
    assert_eq!(sel(&fx), Some(Sel::Node(ORDER_CREATED.into())), "expanded, not moved");
    let consumes = fx
        .app
        .explorer
        .frame
        .edges
        .iter()
        .find(|e| e.id == CONSUMES)
        .expect("edge drawn");
    assert_eq!(consumes.from, ORDER_CREATED);
    assert_eq!(consumes.label.as_ref().map(|l| l.text.as_str()), Some("consumedBy"));
    assert_eq!(
        fx.app.explorer.nodes[PAYMENT_SERVICE], payment,
        "PaymentService did not move"
    );
}

#[test]
fn enter_shows_the_evidence_of_a_relation_and_brackets_switch_it() {
    let mut fx = open();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    fx.key(KeyCode::Down);
    fx.draw();
    fx.app.explorer.select(Sel::Edge(EMITS.into()));
    fx.draw();
    fx.key(KeyCode::Enter);
    let screen = fx.draw().join("\n");
    assert_eq!(fx.app.open.as_ref().unwrap().path, "src/main.rs");
    assert!(
        screen.contains("OrderService —emits→ OrderCreated · evidence 1/2: src/main.rs:2"),
        "{screen}"
    );
    // Line 2 is highlighted, line 1 is not; the graph keeps the focus.
    let area = fx.app.code_panel.area;
    let bg = |row: u16| fx.term.backend().buffer()[(area.x + 8, area.y + row)].bg;
    assert_eq!(bg(1), Color::Rgb(66, 56, 18));
    assert_ne!(bg(0), Color::Rgb(66, 56, 18));
    assert_eq!(fx.app.focus, super::app::Focus::Tree);
    fx.key(KeyCode::Char(']'));
    let screen = fx.draw().join("\n");
    assert_eq!(fx.app.open.as_ref().unwrap().path, "src/lib.rs");
    assert!(screen.contains("evidence 2/2: src/lib.rs:1"), "{screen}");
    // Enter on the same relation cycles through its evidence.
    fx.key(KeyCode::Enter);
    fx.draw();
    assert_eq!(fx.app.open.as_ref().unwrap().path, "src/main.rs");
    // A node shows its own evidence.
    fx.app.explorer.select(Sel::Node("process:create-order".into()));
    fx.key(KeyCode::Enter);
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("Process · CreateOrder · evidence 1/1: src/main.rs:5"),
        "{screen}"
    );
    // Opening a file from the tree leaves the evidence view.
    fx.key(KeyCode::Char('1'));
    fx.app.open_file("src/lib.rs");
    assert!(fx.app.evidence.is_none());
}

#[test]
fn space_collapses_what_a_node_revealed() {
    let mut fx = open();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    fx.key(KeyCode::Char(' '));
    fx.draw();
    assert_eq!(visible(&fx).len(), 7);
    fx.key(KeyCode::Char(' '));
    fx.draw();
    assert_eq!(
        visible(&fx),
        [
            "component:order-service",
            "component:payment-service",
            "data-store:postgre-sql",
            "system:shop"
        ]
    );
    // Expanded again, the nodes come back to their places.
    fx.key(KeyCode::Char(' '));
    fx.draw();
    let first = positions(&fx);
    fx.key(KeyCode::Char(' '));
    fx.key(KeyCode::Char(' '));
    fx.draw();
    assert_eq!(positions(&fx), first);
}

#[test]
fn mouse_selects_expands_pans_and_zooms() {
    let mut fx = open();
    let r = fx.app.explorer.frame.rect(&Sel::Node(ORDER_SERVICE.into())).unwrap();
    let (x, y) = r.center();
    let down = MouseEventKind::Down(MouseButton::Left);
    mouse(&mut fx, down, x, y);
    assert_eq!(sel(&fx), Some(Sel::Node(ORDER_SERVICE.into())));
    assert!(!fx.app.explorer.nodes[ORDER_SERVICE].expanded);
    mouse(&mut fx, down, x, y);
    assert!(fx.app.explorer.nodes[ORDER_SERVICE].expanded, "double click expands");
    mouse(&mut fx, MouseEventKind::Up(MouseButton::Left), x, y);
    // Drag on empty canvas pans.
    let area = fx.app.explorer.area;
    let empty = (area.y as i32..(area.y + area.height) as i32)
        .flat_map(|y| (area.x as i32..(area.x + area.width) as i32).map(move |x| (x, y)))
        .find(|&(x, y)| fx.app.explorer.frame.hit(x, y).is_empty())
        .unwrap();
    let cam = fx.app.explorer.cam;
    mouse(&mut fx, down, empty.0, empty.1);
    mouse(
        &mut fx,
        MouseEventKind::Drag(MouseButton::Left),
        empty.0 + 6,
        empty.1 + 2,
    );
    mouse(&mut fx, MouseEventKind::Up(MouseButton::Left), empty.0 + 6, empty.1 + 2);
    assert_eq!(fx.app.explorer.cam, (cam.0 - 6.0, cam.1 - 2.0));
    // The wheel zooms around the pointer.
    mouse(&mut fx, MouseEventKind::ScrollUp, empty.0, empty.1);
    assert!(fx.app.explorer.zoom > 1.0);
    // A click on a relation label selects the relation.
    let label = fx
        .app
        .explorer
        .frame
        .edges
        .iter()
        .find_map(|e| e.label.as_ref().map(|l| (e.id.clone(), l.rect().center())))
        .unwrap();
    mouse(&mut fx, down, label.1.0, label.1.1);
    assert_eq!(sel(&fx), Some(Sel::Edge(label.0)));
}

#[test]
fn a_new_analysis_keeps_the_view() {
    let mut fx = open();
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    fx.key(KeyCode::Char(' '));
    fx.draw();
    let before = positions(&fx);
    // graph.json replaced on disk (e.g. by `rekon ontology index`).
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_graph(&fx, true);
    fx.app.tick();
    fx.draw();
    assert_eq!(positions(&fx), before);
    // The new neighbor waits behind "+1 more" under the expanded node.
    assert_eq!(fx.app.explorer.more.get(ORDER_SERVICE).map(|m| m.count), Some(1));
}

/// Prints the screen: `cargo test -p rekon print_graph_screen -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_graph_screen() {
    let mut fx = open();
    println!("{}", fx.draw().join("\n"));
    fx.app.explorer.select(Sel::Node(ORDER_SERVICE.into()));
    fx.draw();
    fx.key(KeyCode::Down);
    fx.draw();
    fx.app.explorer.select(Sel::Node(ORDER_CREATED.into()));
    fx.draw();
    fx.key(KeyCode::Down);
    fx.draw();
    fx.app.explorer.select(Sel::Edge(EMITS.into()));
    fx.key(KeyCode::Enter);
    println!("{}", fx.draw().join("\n"));
    fx.key(KeyCode::Char('-'));
    fx.key(KeyCode::Char('-'));
    println!("{}", fx.draw().join("\n"));
}

/// Prints the ontology view of a real repository (its `graph.json`), without the model:
/// `REKON_REPO=path cargo test -p rekon print_repository_graph -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_repository_graph() {
    let Ok(root) = std::env::var("REKON_REPO") else { return };
    let root = std::path::PathBuf::from(root);
    let ctx = std::sync::Arc::new(rekon_core::Ctx::with_backend(
        &root,
        rekon_core::config::Config::default(),
        std::sync::Arc::new(rekon_core::backend::fake::FakeBackend::default()),
    ));
    let (tree, _) = rekon_core::scan::Tree::list(&root).unwrap();
    let app = super::app::App::with_tree(ctx, tree).unwrap();
    let term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 50)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut fx = Fixture {
        dir,
        app,
        term,
        fake: std::sync::Arc::new(rekon_core::backend::fake::FakeBackend::default()),
    };
    fx.draw();
    fx.key(KeyCode::Char('2'));
    println!("{}", fx.draw().join("\n"));
    let steps: Vec<String> = std::env::var("REKON_STEPS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_string)
        .collect();
    for step in steps.iter().filter(|s| !s.is_empty()) {
        match step.as_str() {
            "down" => fx.key(KeyCode::Down),
            "up" => fx.key(KeyCode::Up),
            "left" => fx.key(KeyCode::Left),
            "right" => fx.key(KeyCode::Right),
            "enter" => fx.key(KeyCode::Enter),
            "space" => fx.key(KeyCode::Char(' ')),
            "zoomout" => fx.key(KeyCode::Char('-')),
            id => fx.app.explorer.select(Sel::Node(id.to_string())),
        }
        fx.draw();
    }
    println!("{}", fx.draw().join("\n"));
}
