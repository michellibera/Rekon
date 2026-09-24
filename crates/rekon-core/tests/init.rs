mod common;

use common::{ctx, repo, write};
use rekon_core::backend::fake::description;
use rekon_core::init::{self, Options, Progress};
use rekon_core::model::Author;
use rekon_core::scan::Tree;
use rekon_core::{segment, view};

fn noop(_: &Progress) {}

#[test]
fn prepare_creates_map_and_hides_it_from_git() {
    let dir = repo();
    let root = dir.path();
    assert!(init::prepare(root).unwrap());
    assert!(!init::prepare(root).unwrap());
    assert!(root.join(".rekon/config.json").exists());
    assert!(root.join(".rekon/style.md").exists());
    let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches("/.rekon/").count(), 1);
    let (paths, _) = rekon_core::scan::git_files(root).unwrap();
    assert!(!paths.iter().any(|p| p.starts_with(".rekon")));
    assert!(!paths.iter().any(|p| p.starts_with("target")));
}

#[test]
fn init_describes_everything_then_is_idempotent() {
    let dir = repo();
    let root = dir.path();
    let (ctx, fake) = ctx(root);
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(report.errors, 0);
    assert!(report.overview);
    // README.md, Cargo.toml, .gitignore, src/main.rs, src/api/orders.rs, src/api/mod.rs
    assert_eq!(report.files, 6);
    assert_eq!(report.dirs, 2);
    assert_eq!(report.skipped, 2, "Cargo.lock and logo.png");

    let note = ctx.store.file_note("src/api/orders.rs");
    let summary = note.summary.unwrap();
    assert_eq!(summary.text, description("src/api/orders.rs"));
    assert_eq!(summary.by, Author::Auto);
    assert_eq!(
        ctx.store.dir_note("src/api").summary.unwrap().text,
        description("src/api")
    );
    assert!(ctx.store.file_note("Cargo.lock").summary.is_none());
    assert!(ctx.store.project_note().overview.is_some());

    let calls = fake.call_count();
    let again = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(fake.call_count(), calls, "second run must not call the model");
    assert_eq!((again.files, again.dirs, again.overview), (0, 0, false));
}

#[test]
fn interrupted_init_resumes_only_missing_parts() {
    let dir = repo();
    let root = dir.path();
    let (ctx, fake) = ctx(root);
    // A run limited to src/api stands in for an interrupted run.
    init::run(
        &ctx,
        &Options {
            prefix: "src/api".into(),
            force: false,
        },
        &noop,
    )
    .unwrap();
    assert!(ctx.store.file_note("src/api/orders.rs").summary.is_some());
    assert!(ctx.store.file_note("src/main.rs").summary.is_none());
    let calls = fake.call_count();
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(report.files, 4, "README.md, Cargo.toml, .gitignore, src/main.rs");
    assert_eq!(report.dirs, 1, "src");
    // files: root batch + src batch; dirs: one batch for depth 1.
    assert_eq!(fake.call_count() - calls, 3);
}

#[test]
fn changed_file_becomes_stale_and_is_refreshed() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    init::run(&ctx, &Options::default(), &noop).unwrap();
    write(root, "src/main.rs", "fn main() {}\n");
    let (tree, _) = Tree::scan(root, &ctx.config).unwrap();
    let stale = view::tree_text(&ctx, &tree, "", None, true);
    assert!(stale.contains("⚠ src/main.rs"), "{stale}");
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(report.files, 1);
    let (tree, _) = Tree::scan(root, &ctx.config).unwrap();
    assert!(!view::tree_text(&ctx, &tree, "", None, true).contains("main.rs"));
}

#[test]
fn agent_summary_survives_init_but_not_force() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    let key = ctx.store.file_key("src/main.rs").unwrap();
    ctx.store
        .put_summary(
            rekon_core::store::Target::File("src/main.rs"),
            &key,
            "Agent text",
            Author::Agent,
            None,
        )
        .unwrap();
    init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(ctx.store.file_note("src/main.rs").summary.unwrap().text, "Agent text");
    init::run(
        &ctx,
        &Options {
            prefix: "src/main.rs".into(),
            force: true,
        },
        &noop,
    )
    .unwrap();
    assert_eq!(ctx.store.file_note("src/main.rs").summary.unwrap().by, Author::Auto);
}

#[test]
fn missing_paths_get_one_retry() {
    let dir = repo();
    let root = dir.path();
    let (ctx, fake) = ctx(root);
    fake.omit.lock().unwrap().push("src/api/mod.rs".into());
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(report.errors, 1);
    assert!(ctx.store.file_note("src/api/mod.rs").summary.is_none());
    fake.omit.lock().unwrap().clear();
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!((report.errors, report.files), (0, 1));
}

#[test]
fn deleted_files_lose_their_notes() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    init::run(&ctx, &Options::default(), &noop).unwrap();
    std::fs::remove_dir_all(root.join("src/api")).unwrap();
    let report = init::run(&ctx, &Options::default(), &noop).unwrap();
    assert_eq!(report.removed, 3, "two file notes and one folder note");
    assert!(!ctx.store.file_note_path("src/api/orders.rs").exists());
}

#[test]
fn segment_covers_file_and_drills_down_to_leaves() {
    let dir = repo();
    let root = dir.path();
    let (ctx, fake) = ctx(root);
    let rel = "src/api/orders.rs";
    let n = common::orders().lines().count() as u32;
    let level1 = segment::level1(&ctx, rel, false).unwrap();
    assert!(level1.len() >= 2);
    assert_eq!(level1.first().unwrap().lines.0, 1);
    assert_eq!(level1.last().unwrap().lines.1, n);
    for w in level1.windows(2) {
        assert_eq!(w[0].lines.1 + 1, w[1].lines.0);
    }
    let calls = fake.call_count();
    assert_eq!(segment::level1(&ctx, rel, false).unwrap(), level1);
    assert_eq!(fake.call_count(), calls, "stored blocks are reused");

    // Drill down until every block is a leaf.
    let mut queue: Vec<(u32, u32)> = level1
        .iter()
        .filter(|b| b.children.is_none())
        .map(|b| b.lines)
        .collect();
    let mut steps = 0;
    while let Some(range) = queue.pop() {
        steps += 1;
        assert!(steps < 100);
        let children = segment::split_block(&ctx, rel, range, false).unwrap();
        queue.extend(children.iter().filter(|b| b.children.is_none()).map(|b| b.lines));
    }
    let blocks = ctx.store.file_note(rel).blocks.unwrap();
    fn all_resolved(b: &[rekon_core::model::Block]) -> bool {
        b.iter().all(|b| b.children.as_ref().is_some_and(|c| all_resolved(c)))
    }
    assert!(all_resolved(&blocks.items));

    // Changing the file makes blocks stale; splitting a block then fails until level 1 is redone.
    write(root, rel, "fn x() {}\n\nfn y() {}\n");
    assert!(segment::split_block(&ctx, rel, level1[0].lines, false).is_err());
    let fresh = segment::level1(&ctx, rel, false).unwrap();
    assert_eq!(fresh.last().unwrap().lines.1, 3);
}

#[test]
fn tree_text_shows_overview_and_labels() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    init::run(&ctx, &Options::default(), &noop).unwrap();
    let (tree, _) = Tree::scan(root, &ctx.config).unwrap();
    let text = view::tree_text(&ctx, &tree, "", Some(2), false);
    assert!(text.contains("Opis testowy: przegląd projektu"), "{text}");
    assert!(text.contains("\nsrc/ — Opis testowy: src\n"), "{text}");
    assert!(text.contains("\n  api/ — Opis testowy: src/api\n"), "{text}");
    assert!(!text.contains("orders.rs"), "depth 2 hides level 3: {text}");
    assert!(text.contains("Cargo.lock — Skipped (*.lock)"), "{text}");
    assert!(text.contains("logo.png — Binary file (7 B)"), "{text}");
}
