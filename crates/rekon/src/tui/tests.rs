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
    pub fake: Arc<FakeBackend>,
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
    let fake = Arc::new(FakeBackend::default());
    let ctx = Arc::new(Ctx::with_backend(root, config, fake.clone()));
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
    let app = App::with_tree(ctx, tree).unwrap();
    let term = Terminal::new(TestBackend::new(width, height)).unwrap();
    Fixture { dir, app, term, fake }
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

    /// Waits until the job pool is idle and its results are handled.
    pub fn wait_jobs(&mut self) {
        let started = std::time::Instant::now();
        while self.app.pool.pending_count() > 0 {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "jobs did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.app.poll();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.app.poll();
    }

    /// Rows of the code panel, trimmed.
    pub fn code_lines(&mut self) -> Vec<String> {
        let screen = self.draw();
        let a = self.app.code_panel.area;
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

    /// Opens src/main.rs from the tree and waits for its level-1 blocks.
    pub fn open_main(&mut self) {
        self.app.expanded.insert("src".into());
        self.draw();
        self.app.open_file("src/main.rs");
        self.wait_jobs();
        self.key(KeyCode::Tab);
    }
}

#[test]
fn opening_a_file_splits_it_once() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    assert_eq!(fx.fake.call_count(), 1);
    assert_eq!(
        fx.code_lines(),
        [
            "▸    1 fn main() {              Opis testowy: linie",
            "│    2     run();               1-4",
            "│    3 }",
            "│    4",
            "·    5 fn run() {}              Opis testowy: linie",
            "│                               5-5",
        ]
    );
    // Opening again reads the note.
    fx.app.open_file("src/main.rs");
    fx.wait_jobs();
    fx.draw();
    assert_eq!(fx.fake.call_count(), 1);
}

#[test]
fn expanding_splits_lazily_and_reuses_children() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    fx.draw();
    fx.key(KeyCode::Right);
    fx.draw();
    fx.wait_jobs();
    assert_eq!(fx.fake.call_count(), 2);
    assert_eq!(
        fx.code_lines(),
        [
            "▾ 1–4  Opis testowy: linie 1-4",
            "  ·    1 fn main() {            Opis testowy: linie",
            "  │    2     run();             1-2",
            "  ·    3 }                      Opis testowy: linie",
            "  │    4                        3-4",
            "·    5 fn run() {}              Opis testowy: linie",
            "│                               5-5",
        ]
    );
    fx.key(KeyCode::Left); // collapse
    fx.draw();
    fx.key(KeyCode::Right); // expand again from the note
    fx.draw();
    fx.wait_jobs();
    assert_eq!(fx.fake.call_count(), 2);
    assert!(fx.code_lines()[0].starts_with("▾ 1–4"));
}

#[test]
fn selection_jumps_between_headers_and_o_hides_code() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    fx.draw();
    assert_eq!(fx.app.code_panel.sel, 0);
    fx.key(KeyCode::Down);
    fx.draw();
    assert_eq!(fx.app.code_panel.sel, 4, "next block, over the code lines");
    fx.key(KeyCode::Up);
    fx.draw();
    assert_eq!(fx.app.code_panel.sel, 0);
    let footer = fx.draw().join(
        "
",
    );
    assert!(footer.contains("src/main.rs:1–4 — Opis testowy: linie 1-4"), "{footer}");
    fx.key(KeyCode::Char('o'));
    assert_eq!(
        fx.code_lines(),
        ["▸ 1–4  Opis testowy: linie 1-4", "· 5–5  Opis testowy: linie 5-5"]
    );
}

#[test]
fn click_on_block_header_toggles_it() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    fx.draw();
    let area = fx.app.code_panel.area;
    let click = |row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y + row,
        modifiers: KeyModifiers::NONE,
    };
    fx.app.on_mouse(click(0));
    fx.draw();
    fx.wait_jobs();
    assert!(fx.code_lines()[0].starts_with("▾ 1–4"));
    fx.app.on_mouse(click(0));
    assert!(
        fx.code_lines()[0].starts_with("▸    1 fn main() {"),
        "second click collapses"
    );
    // A click on a code line selects its block.
    fx.app.on_mouse(click(2));
    fx.draw();
    assert_eq!(fx.app.code_panel.sel, 0);
}

#[test]
fn full_drill_down_reaches_leaves_with_fake_backend() {
    let mut fx = fixture(100, 40);
    let long: String = (1..=40)
        .map(|i| {
            if i % 6 == 0 {
                "
"
                .to_string()
            } else {
                format!(
                    "let v{i} = {i};
"
                )
            }
        })
        .collect();
    write(fx.dir.path(), "src/main.rs", &long);
    fx.open_main();
    for _ in 0..50 {
        fx.draw();
        let next = fx.app.code_rows.iter().position(|r| {
            let t: String = r.line.spans.iter().map(|s| s.content.as_ref()).collect();
            t.trim_start().starts_with('▸')
        });
        let Some(i) = next else { break };
        fx.app.click(i);
        fx.wait_jobs();
    }
    fx.draw();
    let headers: Vec<String> = fx
        .app
        .code_rows
        .iter()
        .filter(|r| r.kind == super::rows::RowKind::BlockHeader)
        .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect();
    assert!(headers.iter().all(|h| !h.trim_start().starts_with('▸')), "{headers:#?}");
    assert!(headers.iter().any(|h| h.trim_start().starts_with('·')));
    assert_eq!(fx.app.errors, 0, "{:?}", fx.app.last_error);
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
    let mut app = App::with_tree(ctx, tree).unwrap();
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

fn wait_init(fx: &mut Fixture) {
    let started = std::time::Instant::now();
    while fx.app.init.is_some() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "init did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        fx.app.poll();
    }
}

#[test]
fn edit_that_changes_the_file_redescribes_and_resplits_it() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    let abs = fx.dir.path().join("src/main.rs");
    let req = super::editor::EditRequest {
        path: "src/main.rs".into(),
        abs: abs.clone(),
        line: 1,
        hash_before: rekon_core::hash::hash_file(&abs).ok(),
    };
    std::fs::write(
        &abs,
        "fn main() {}

fn edited() {}
",
    )
    .unwrap();
    fx.app.after_edit(req, Ok(()));
    fx.wait_jobs();
    let note = fx.app.ctx.store.file_note("src/main.rs");
    assert_eq!(note.summary.unwrap().text, "Opis testowy: src/main.rs");
    assert_eq!(note.blocks.unwrap().items.last().unwrap().lines.1, 3);
    assert!(fx.code_lines().iter().any(|l| l.contains("fn edited() {}")));
}

#[test]
fn edit_without_changes_calls_nothing() {
    let mut fx = fixture(100, 24);
    fx.open_main();
    let calls = fx.fake.call_count();
    let abs = fx.dir.path().join("src/main.rs");
    let req = super::editor::EditRequest {
        path: "src/main.rs".into(),
        abs: abs.clone(),
        line: 1,
        hash_before: rekon_core::hash::hash_file(&abs).ok(),
    };
    fx.app.after_edit(req, Ok(()));
    fx.wait_jobs();
    assert_eq!(fx.fake.call_count(), calls);
}

#[test]
fn file_changed_outside_gets_warning_and_capital_r_refreshes_it() {
    let mut fx = fixture(100, 24);
    fx.app.expanded.insert("src".into());
    assert!(fx.tree_lines().contains(&"    main.rs  Starts the program".to_string()));
    write(
        fx.dir.path(),
        "src/main.rs",
        "fn main() { changed(); }
",
    );
    fx.app.tick();
    assert!(
        fx.tree_lines().contains(&"  ⚠ main.rs  Starts the program".to_string()),
        "{:?}",
        fx.tree_lines()
    );
    fx.key(KeyCode::Char('R'));
    assert!(fx.app.init.is_some());
    wait_init(&mut fx);
    let lines = fx.tree_lines();
    assert!(
        lines.contains(&"    main.rs  Opis testowy: src/main.rs".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"    util.rs  Opis testowy: src/util.rs".to_string()),
        "missing ones too: {lines:?}"
    );
}

#[test]
fn r_in_tree_regenerates_the_selected_description() {
    let mut fx = fixture(100, 24);
    fx.app.expanded.insert("src".into());
    fx.draw();
    fx.key(KeyCode::Down); // src/lib.rs (stale, by agent)
    fx.draw();
    assert_eq!(fx.app.selected_tree_path(), Some("src/lib.rs"));
    fx.key(KeyCode::Char('r'));
    fx.wait_jobs();
    assert!(
        fx.tree_lines()
            .contains(&"    lib.rs  Opis testowy: src/lib.rs".to_string())
    );
    fx.key(KeyCode::Up); // src/ folder
    fx.draw();
    fx.key(KeyCode::Char('r'));
    fx.wait_jobs();
    assert!(fx.tree_lines().contains(&"▾ src/  Opis testowy: src".to_string()));
    assert_eq!(fx.app.errors, 0, "{:?}", fx.app.last_error);
}
