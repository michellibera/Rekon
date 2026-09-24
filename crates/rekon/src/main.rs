//! `rekon` binary: CLI subcommands; without a subcommand it opens the TUI.

mod tui;

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rekon_core::init::{self, Options, Progress};
use rekon_core::model::Author;
use rekon_core::scan::{Tree, normalize_prefix};
use rekon_core::{Ctx, apply, check, context, segment, setup, view};

#[derive(Parser)]
#[command(
    name = "rekon",
    version,
    about = "Map of a repository: one-line descriptions of files, folders and code blocks"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create .rekon/ and describe the project, all files and folders (only what is missing or stale)
    Init {
        /// Limit files and folders to this subtree
        prefix: Option<String>,
        /// Regenerate descriptions in scope, including ones written by an agent
        #[arg(long)]
        force: bool,
        /// Only create .rekon/ with default config.json and style.md
        #[arg(long)]
        no_generate: bool,
    },
    /// Print the overview and the tree with descriptions
    Tree {
        prefix: Option<String>,
        /// Number of levels to show
        #[arg(long)]
        depth: Option<usize>,
        /// Only missing and stale descriptions
        #[arg(long)]
        stale: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the note of a file or folder, with blocks
    Show {
        path: String,
        #[arg(long)]
        json: bool,
    },
    /// Split a file into level-1 blocks, or the block with lines A-B into sub-blocks
    Segment {
        file: String,
        /// Range of an existing block, e.g. 15-40
        #[arg(long, value_name = "A-B")]
        lines: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Save descriptions from JSON on stdin: {"summaries": {"path": "text", "dir/": "text", ".": "text"}, "overview": "..."}
    Apply {
        /// Author recorded in the notes
        #[arg(long, value_enum, default_value = "agent")]
        by: By,
    },
    /// List changed files with outdated descriptions; with --hook acts as the Claude Code Stop hook
    Check {
        #[arg(long)]
        hook: bool,
    },
    /// Print the overview and tree to depth 2; with --hook acts as the Claude Code SessionStart hook
    Context {
        #[arg(long)]
        hook: bool,
    },
    /// Install the rekon-init skill and the hooks in ~/.claude (once per machine)
    Setup {
        /// Only show the changes
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum By {
    Agent,
    Auto,
}

/// Exit code for usage errors (bad arguments, missing map).
const USAGE: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("rekon: {e:#}");
            ExitCode::from(if e.downcast_ref::<Usage>().is_some() { USAGE } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<u8> {
    let cwd = std::env::current_dir()?;
    let Some(command) = cli.command else {
        tui::run(std::sync::Arc::new(Ctx::open(&cwd)?))?;
        return Ok(0);
    };
    match command {
        Cmd::Init {
            prefix,
            force,
            no_generate,
        } => {
            let root = rekon_core::scan::find_root(&cwd)?;
            init::prepare(&root)?;
            if no_generate {
                eprintln!("Created {}", root.join(rekon_core::MAP_DIR).display());
                return Ok(0);
            }
            let ctx = Ctx::at_root(&root)?;
            cmd_init(&ctx, &normalize_prefix(prefix.as_deref()), force)
        }
        Cmd::Tree {
            prefix,
            depth,
            stale,
            json,
        } => {
            let ctx = open_with_map(&cwd)?;
            let (tree, _) = Tree::scan(&ctx.root, &ctx.config)?;
            let prefix = rel_prefix(&ctx, &cwd, prefix.as_deref());
            if tree.get(&prefix).is_none() {
                return Err(Usage(format!("{prefix} is not a file or folder of this repository")).into());
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view::tree_json(&ctx, &tree, &prefix, depth, stale))?
                );
            } else {
                print!("{}", view::tree_text(&ctx, &tree, &prefix, depth, stale));
            }
            Ok(0)
        }
        Cmd::Show { path, json } => {
            let ctx = open_with_map(&cwd)?;
            let (tree, _) = Tree::scan(&ctx.root, &ctx.config)?;
            let rel = rel_prefix(&ctx, &cwd, Some(&path));
            let Some(i) = tree.get(&rel).filter(|_| !rel.is_empty()) else {
                eprintln!("rekon: {path} is not a file or folder of this repository");
                return Ok(USAGE);
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&view::show_json(&ctx, &tree, i))?);
            } else {
                print!("{}", view::show_text(&ctx, &tree, i));
            }
            Ok(0)
        }
        Cmd::Segment { file, lines, force } => {
            let ctx = open_with_map(&cwd)?;
            let rel = rel_prefix(&ctx, &cwd, Some(&file));
            let blocks = match lines {
                None => segment::level1(&ctx, &rel, force)?,
                Some(r) => {
                    let range = parse_range(&r).with_context(|| format!("invalid range {r}, expected A-B"))?;
                    segment::split_block(&ctx, &rel, range, force)?
                }
            };
            if blocks.is_empty() {
                println!("(leaf: no smaller blocks)");
            }
            for b in blocks {
                println!("{}–{}  {}", b.lines.0, b.lines.1, b.summary);
            }
            Ok(0)
        }
        Cmd::Apply { by } => {
            let ctx = open_with_map(&cwd)?;
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let input = apply::parse(&input).map_err(|e| Usage(format!("{e:#}")))?;
            let by = match by {
                By::Agent => Author::Agent,
                By::Auto => Author::Auto,
            };
            let report = apply::apply(&ctx, &input, by)?;
            eprintln!("Saved: {}", report.written.len());
            for w in &report.warnings {
                eprintln!("warning: {w}");
            }
            for (path, e) in &report.errors {
                eprintln!("error: {path}: {e}");
            }
            Ok(if report.errors.is_empty() { 0 } else { 1 })
        }
        Cmd::Check { hook: true } => {
            // A hook must never break the session: errors are logged, not shown.
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            match check::stop_hook(&input, &setup::exe_path()?) {
                Ok(Some(out)) => println!("{out}"),
                Ok(None) => {}
                Err(e) => log_hook_error(&cwd, "check", &e),
            }
            Ok(0)
        }
        Cmd::Check { hook: false } => {
            let ctx = open_with_map(&cwd)?;
            let stale = check::stale_changes(&ctx.root)?;
            if stale.is_empty() {
                eprintln!("All changed files have fresh descriptions.");
            }
            for p in stale {
                println!("{p}");
            }
            Ok(0)
        }
        Cmd::Context { hook: true } => {
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            match context::session_start_hook(&input, &setup::exe_path()?) {
                Ok(Some(out)) => print!("{out}"),
                Ok(None) => {}
                Err(e) => log_hook_error(&cwd, "context", &e),
            }
            Ok(0)
        }
        Cmd::Context { hook: false } => {
            let ctx = open_with_map(&cwd)?;
            print!("{}", context::render(&ctx, &setup::exe_path()?)?);
            Ok(0)
        }
        Cmd::Setup { dry_run } => {
            let dir = setup::claude_dir()?;
            let changes = setup::setup(&dir, &setup::exe_path()?, dry_run)?;
            if changes.is_empty() {
                eprintln!("Nothing to change in {}", dir.display());
            }
            for c in changes {
                println!("{}{c}", if dry_run { "would " } else { "" });
            }
            Ok(0)
        }
    }
}

/// Error that maps to exit code 2.
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

fn log_hook_error(cwd: &std::path::Path, hook: &str, e: &anyhow::Error) {
    if let Ok(root) = rekon_core::scan::find_root(cwd) {
        rekon_core::store::Store::new(&root).log(&format!("{hook} hook error: {e:#}"));
    }
}

fn open_with_map(cwd: &std::path::Path) -> Result<Ctx> {
    let ctx = Ctx::open(cwd)?;
    if !ctx.store.exists() {
        return Err(Usage(rekon_core::text::NO_MAP.into()).into());
    }
    Ok(ctx)
}

/// Converts a path given relative to the current folder into a repository path.
fn rel_prefix(ctx: &Ctx, cwd: &std::path::Path, path: Option<&str>) -> String {
    let given = normalize_prefix(path);
    let root = std::fs::canonicalize(&ctx.root).unwrap_or_else(|_| ctx.root.clone());
    let here = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let base = here
        .strip_prefix(&root)
        .ok()
        .and_then(|p| p.to_str())
        .map(|p| p.replace('\\', "/"))
        .unwrap_or_default();
    let joined = match (base.is_empty(), given.is_empty()) {
        (true, _) => given,
        (false, true) => base,
        (false, false) => format!("{base}/{given}"),
    };
    normalize_prefix(Some(&joined))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(['-', '–'])?;
    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a >= 1 && a <= b).then_some((a, b))
}

fn cmd_init(ctx: &Ctx, prefix: &str, force: bool) -> Result<u8> {
    let tty = std::io::stderr().is_terminal();
    let print = |p: &Progress| {
        let mut err = std::io::stderr().lock();
        if tty {
            let _ = write!(err, "\r\x1b[2K{}", p.line());
        } else {
            let _ = writeln!(err, "{}", p.line());
        }
        let _ = err.flush();
    };
    let report = init::run(
        ctx,
        &Options {
            prefix: prefix.to_string(),
            force,
        },
        &print,
    )?;
    if tty {
        eprintln!();
    }
    eprintln!(
        "Described: {} files, {} folders{} · skipped: {} · errors: {} · removed notes: {} · cost ~${:.2}",
        report.files,
        report.dirs,
        if report.overview { ", project overview" } else { "" },
        report.skipped,
        report.errors,
        report.removed,
        report.cost
    );
    if report.errors > 0 {
        eprintln!("Details in {}", ctx.store.dir().join("rekon.log").display());
    }
    Ok(if report.errors > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("15-40"), Some((15, 40)));
        assert_eq!(parse_range("15–40"), Some((15, 40)));
        assert_eq!(parse_range("40-15"), None);
        assert_eq!(parse_range("0-3"), None);
        assert_eq!(parse_range("x"), None);
    }
}
