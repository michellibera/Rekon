mod common;

use common::{ctx, git, repo, write};
use rekon_core::init::{self, Options};
use rekon_core::model::Author;
use rekon_core::{apply, check, context};
use serde_json::{Value, json};

fn hook_input(root: &std::path::Path, active: bool) -> String {
    json!({ "cwd": root, "hook_event_name": "Stop", "stop_hook_active": active }).to_string()
}

fn committed_repo() -> tempfile::TempDir {
    let dir = repo();
    let root = dir.path();
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"],
    );
    dir
}

#[test]
fn apply_saves_as_agent_and_bad_entries_fail_alone() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    let input = apply::parse(
        r#"{"summaries": {
            "src/main.rs": "Starts the program",
            "src/api/": "REST endpoints",
            ".": "Test shop",
            "missing.rs": "Nothing",
            "src/main.rs/": "Not a folder",
            "README.md": "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone"
        }, "overview": "Shop overview."}"#,
    )
    .unwrap();
    let report = apply::apply(&ctx, &input, Author::Agent).unwrap();
    assert_eq!(report.written.len(), 4, "{report:?}");
    assert_eq!(report.errors.len(), 2, "{report:?}");
    assert_eq!(report.warnings.len(), 1);

    let main = ctx.store.file_note("src/main.rs").summary.unwrap();
    assert_eq!((main.text.as_str(), main.by), ("Starts the program", Author::Agent));
    assert_eq!(main.hash, ctx.store.file_key("src/main.rs").unwrap());
    let project = ctx.store.project_note();
    assert_eq!(project.summary.unwrap().text, "Test shop");
    assert_eq!(project.overview.unwrap(), "Shop overview.");

    // Descriptions from apply are fresh, so init has nothing to redo for them.
    let (tree, _) = rekon_core::scan::Tree::scan(root, &ctx.config).unwrap();
    let text = rekon_core::view::tree_text(&ctx, &tree, "", None, true);
    assert!(!text.contains("src/main.rs"), "{text}");
    assert!(!text.contains("src/api/ "), "{text}");
}

#[test]
fn apply_rejects_invalid_json() {
    assert!(apply::parse("{\"summary\": {}}").is_err());
    assert!(apply::parse("not json").is_err());
}

#[test]
fn stop_hook_blocks_on_stale_changed_file_and_is_silent_otherwise() {
    let dir = committed_repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    init::run(&ctx, &Options::default(), &|_| {}).unwrap();
    assert_eq!(
        check::stop_hook(&hook_input(root, false), "rekon").unwrap(),
        None,
        "all fresh"
    );

    write(root, "src/main.rs", "fn main() { changed(); }\n");
    write(root, "src/new/util.rs", "pub fn util() {}\n");
    write(root, "Cargo.lock", "# changed but excluded\n");
    let out = check::stop_hook(&hook_input(root, false), "rekon")
        .unwrap()
        .expect("blocks");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("src/main.rs, src/new/util.rs, src/new/"), "{reason}");
    assert!(!reason.contains("Cargo.lock"), "{reason}");
    assert!(reason.contains("rekon apply <<'EOF'"), "{reason}");

    assert_eq!(
        check::stop_hook(&hook_input(root, true), "rekon").unwrap(),
        None,
        "stop_hook_active"
    );

    let input = apply::parse(
        r#"{"summaries": {"src/main.rs": "Calls changed", "src/new/util.rs": "Helper", "src/new/": "Helpers"}}"#,
    )
    .unwrap();
    apply::apply(&ctx, &input, Author::Agent).unwrap();
    assert_eq!(
        check::stop_hook(&hook_input(root, false), "rekon").unwrap(),
        None,
        "fresh after apply"
    );
}

#[test]
fn hooks_are_silent_outside_a_mapped_repository() {
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(
        check::stop_hook(&hook_input(plain.path(), false), "rekon").unwrap(),
        None
    );
    assert_eq!(
        context::session_start_hook(&hook_input(plain.path(), false), "rekon").unwrap(),
        None
    );
    let dir = repo();
    assert_eq!(
        check::stop_hook(&hook_input(dir.path(), false), "rekon").unwrap(),
        None,
        "repo without .rekon"
    );
    assert_eq!(
        context::session_start_hook(&hook_input(dir.path(), false), "rekon").unwrap(),
        None
    );
}

#[test]
fn session_start_prints_overview_and_two_levels() {
    let dir = repo();
    let root = dir.path();
    let (ctx, _fake) = ctx(root);
    init::run(&ctx, &Options::default(), &|_| {}).unwrap();
    let out = context::session_start_hook(&hook_input(root, false), "rekon")
        .unwrap()
        .unwrap();
    assert!(out.contains("Opis testowy: przegląd projektu"), "{out}");
    assert!(out.contains("  api/ — Opis testowy: src/api"), "{out}");
    assert!(!out.contains("orders.rs"), "{out}");
    assert!(out.contains("Cargo.lock — Skipped (*.lock)"), "{out}");
    assert!(out.lines().count() <= 153);
}
