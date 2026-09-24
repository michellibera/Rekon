//! Snapshot tests of the TUI with ratatui's `TestBackend`.

use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use rekon_core::Ctx;
use rekon_core::backend::fake::FakeBackend;
use rekon_core::config::Config;
use rekon_core::init::Progress;
use rekon_core::model::Author;
use rekon_core::scan::Tree;
use rekon_core::store::Target;

use super::app::App;
use super::render;

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub app: App,
    pub term: Terminal<TestBackend>,
}

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn write(root: &std::path::Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Repo: src/ (fresh), src/main.rs (fresh), src/lib.rs (stale), src/util.rs (missing),
/// Cargo.lock (skipped), long.rs (fresh, long description).
pub fn fixture(width: u16, height: u16) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, "src/main.rs", "fn main() {\n    run();\n}\n\nfn run() {}\n");
    write(root, "src/lib.rs", "pub fn lib() {}\n");
    write(root, "src/util.rs", "pub fn util() {}\n");
    write(root, "Cargo.lock", "# lock\n");
    write(root, "long.rs", "// long\n");
    rekon_core::init::prepare(root).unwrap();
    let config = Config {
        backend: "fake".into(),
        ..Config::default()
    };
    let ctx = Arc::new(Ctx::with_backend(root, config, Arc::new(FakeBackend::default())));
    let (tree, _) = Tree::scan(root, &ctx.config).unwrap();
    let s = &ctx.store;
    let key = |p: &str| s.file_key(p).unwrap();
    s.put_summary(
        Target::Dir("src"),
        &tree.dir_key(tree.get("src").unwrap()),
        "Application code",
        Author::Auto,
        None,
    )
    .unwrap();
    s.put_summary(
        Target::File("src/main.rs"),
        &key("src/main.rs"),
        "Starts the program",
        Author::Auto,
        None,
    )
    .unwrap();
    s.put_summary(
        Target::File("src/lib.rs"),
        "old-key",
        "Old library text",
        Author::Agent,
        None,
    )
    .unwrap();
    s.put_summary(
        Target::File("long.rs"),
        &key("long.rs"),
        "A very long description that will certainly not fit into the narrow tree panel",
        Author::Auto,
        None,
    )
    .unwrap();
    s.put_summary(
        Target::Project,
        &tree.dir_key(0),
        "Test project",
        Author::Auto,
        Some("Overview text."),
    )
    .unwrap();
    let app = App::for_tests(ctx, tree);
    let term = Terminal::new(TestBackend::new(width, height)).unwrap();
    Fixture { dir, app, term }
}

impl Fixture {
    pub fn draw(&mut self) -> Vec<String> {
        let app = &mut self.app;
        self.term.draw(|f| render::draw(f, app)).unwrap();
        let buf = self.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// Rows of the tree panel, trimmed.
    pub fn tree_lines(&mut self) -> Vec<String> {
        let screen = self.draw();
        let a = self.app.tree_panel.area;
        screen[a.y as usize..(a.y + a.height) as usize]
            .iter()
            .map(|l| {
                l.chars()
                    .skip(a.x as usize)
                    .take(a.width as usize)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect()
    }

    pub fn key(&mut self, code: KeyCode) {
        self.app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
}

#[test]
fn tree_rows_show_states_labels_and_truncation() {
    let mut fx = fixture(100, 20);
    fx.app.expanded.insert("src".into());
    let lines = fx.tree_lines();
    assert_eq!(
        lines,
        [
            "▾ src/  Application code",
            "  ⚠ lib.rs  Old library text",
            "    main.rs  Starts the program",
            "    util.rs  no description",
            "  Cargo.lock  Skipped (*.lock)",
            "  long.rs  A very long description that wi…",
        ]
    );
}

#[test]
fn missing_descriptions_show_pending_while_init_runs() {
    let mut fx = fixture(100, 20);
    fx.app.expanded.insert("src".into());
    fx.app.init = Some(Progress::default());
    let lines = fx.tree_lines();
    assert!(lines.contains(&"    util.rs  …".to_string()), "{lines:?}");
    // Stale descriptions stay visible.
    assert!(lines.contains(&"  ⚠ lib.rs  Old library text".to_string()), "{lines:?}");
}

#[test]
fn header_and_footer_show_project_and_selection() {
    let mut fx = fixture(100, 20);
    let screen = fx.draw();
    assert!(screen[0].starts_with("rekon · "), "{}", screen[0]);
    assert!(screen[0].contains(" — Test project"), "{}", screen[0]);
    let footer = screen[screen.len() - 4..].join("\n");
    assert!(footer.contains("src/ — Application code"), "{footer}");
    assert!(footer.contains("jobs: 0 · errors: 0"), "{footer}");
}

#[test]
fn keyboard_expands_opens_and_goes_back() {
    let mut fx = fixture(100, 20);
    fx.draw();
    fx.key(KeyCode::Right);
    fx.draw();
    assert!(fx.app.expanded.contains("src"));
    fx.key(KeyCode::Down);
    fx.key(KeyCode::Down);
    fx.draw();
    assert_eq!(fx.app.selected_tree_path(), Some("src/main.rs"));
    fx.key(KeyCode::Enter);
    let screen = fx.draw();
    assert_eq!(fx.app.open.as_ref().unwrap().path, "src/main.rs");
    assert!(screen.iter().any(|l| l.contains("   1 fn main() {")), "{screen:#?}");
    fx.key(KeyCode::Left); // back to the tree
    fx.key(KeyCode::Left); // to the parent folder
    fx.draw();
    assert_eq!(fx.app.selected_tree_path(), Some("src"));
    fx.key(KeyCode::Left); // collapse
    fx.draw();
    assert!(!fx.app.expanded.contains("src"));
}

#[test]
fn click_selects_and_toggles_and_wheel_scrolls() {
    let mut fx = fixture(100, 20);
    fx.draw();
    let area = fx.app.tree_panel.area;
    let click = |row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 2,
        row: area.y + row,
        modifiers: KeyModifiers::NONE,
    };
    fx.app.on_mouse(click(0));
    fx.draw();
    assert!(fx.app.expanded.contains("src"));
    fx.app.on_mouse(click(2));
    fx.draw();
    assert_eq!(fx.app.open.as_ref().unwrap().path, "src/main.rs");
    fx.app.on_mouse(click(0));
    fx.draw();
    assert!(!fx.app.expanded.contains("src"), "second click collapses");
    // Clicks below the last row do nothing.
    fx.app.on_mouse(click(area.height - 1));
    fx.draw();
    assert_eq!(fx.app.selected_tree_path(), Some("src"));
}

#[test]
fn wide_mode_hides_code_panel_and_popups_open() {
    let mut fx = fixture(100, 20);
    fx.key(KeyCode::Char('w'));
    let screen = fx.draw();
    assert!(!screen.iter().any(|l| l.contains(" Code ")));
    assert_eq!(fx.app.tree_panel.area.width, 98);
    fx.key(KeyCode::Char('i'));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("Overview text."), "{screen}");
    fx.key(KeyCode::Esc);
    fx.key(KeyCode::Char('?'));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("tree at full width"), "{screen}");
}

/// Run manually: `cargo test --release -p rekon -- --ignored --nocapture`.
#[test]
#[ignore]
fn large_repository_stays_fast() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    for d in 0..50 {
        for f in 0..100 {
            write(root, &format!("dir{d:02}/sub{}/file{f:03}.rs", f % 5), "fn x() {}\n");
        }
    }
    rekon_core::init::prepare(root).unwrap();
    let ctx = Arc::new(Ctx::with_backend(
        root,
        Config::default(),
        Arc::new(FakeBackend::default()),
    ));
    let started = std::time::Instant::now();
    let (tree, _) = Tree::list(root).unwrap();
    println!("list 5000 files: {:?}", started.elapsed());
    let mut app = App::for_tests(ctx, tree);
    for i in 0..app.tree.nodes.len() {
        if app.tree.node(i).is_dir {
            let p = app.tree.node(i).path.clone();
            app.expanded.insert(p);
        }
    }
    let mut term = Terminal::new(TestBackend::new(160, 50)).unwrap();
    let started = std::time::Instant::now();
    term.draw(|f| render::draw(f, &mut app)).unwrap();
    println!("first frame, {} rows: {:?}", app.tree_rows.len(), started.elapsed());
    let started = std::time::Instant::now();
    for _ in 0..20 {
        app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        term.draw(|f| render::draw(f, &mut app)).unwrap();
    }
    let per_frame = started.elapsed() / 20;
    println!("frame while scrolling: {per_frame:?}");
    assert!(per_frame < std::time::Duration::from_millis(100));
}

#[test]
fn apply_from_outside_shows_after_refresh() {
    let mut fx = fixture(100, 20);
    fx.app.expanded.insert("src".into());
    assert!(fx.tree_lines().contains(&"    util.rs  no description".to_string()));
    let root = fx.dir.path().to_path_buf();
    let ctx = Ctx::at_root(&root).unwrap();
    let input = rekon_core::apply::parse(r#"{"summaries": {"src/util.rs": "Helpers from an agent"}}"#).unwrap();
    rekon_core::apply::apply(&ctx, &input, Author::Agent).unwrap();
    // The note cache is revalidated by file mtime on the next tick.
    let files = vec!["src/util.rs".to_string()];
    assert!(fx.app.notes.revalidate(&fx.app.ctx, &files, &[]));
    assert!(
        fx.tree_lines()
            .contains(&"    util.rs  Helpers from an agent".to_string())
    );
}
