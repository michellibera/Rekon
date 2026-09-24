//! Upfront initialization: project overview, then descriptions of all files and
//! folders. Every result is saved right away, so an interrupted run resumes.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::Ctx;
use crate::config::Config;
use crate::jobs::run_parallel;
use crate::model::{Freshness, freshness};
use crate::prompts::{self, Entry};
use crate::scan::{self, ROOT, Tree};
use crate::store::{self, DEFAULT_STYLE, Guard, Target};

const DIRS_PER_BATCH: usize = 20;
const EXCLUDE_LINE: &str = "/.rekon/";

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Narrows files, folders and cleanup to a subtree (`""` = whole repository).
    pub prefix: String,
    /// Regenerates descriptions in scope, including the ones written by an agent.
    pub force: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
    pub dirs_done: usize,
    pub dirs_total: usize,
    pub errors: usize,
    pub last_error: Option<String>,
    pub cost: f64,
}

impl Progress {
    pub fn line(&self) -> String {
        crate::text::progress_line(
            (self.files_done, self.files_total),
            (self.dirs_done, self.dirs_total),
            self.errors,
            self.cost,
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub overview: bool,
    pub files: usize,
    pub dirs: usize,
    pub skipped: usize,
    pub errors: usize,
    pub removed: usize,
    pub cost: f64,
}

/// Creates `.rekon/` with default config.json and style.md and hides it from git
/// through `.git/info/exclude`. Returns true when the folder was created.
pub fn prepare(root: &Path) -> Result<bool> {
    let dir = root.join(crate::MAP_DIR);
    let created = !dir.is_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let config = dir.join("config.json");
    if !config.exists() {
        store::write_atomic(&config, Config::default().to_pretty_json().as_bytes())?;
    }
    let style = dir.join("style.md");
    if !style.exists() {
        store::write_atomic(&style, DEFAULT_STYLE.as_bytes())?;
    }
    add_git_exclude(root)?;
    Ok(created)
}

fn add_git_exclude(root: &Path) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .context("cannot run git")?;
    if !out.status.success() {
        return Ok(());
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = root.join(rel);
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.lines().any(|l| l.trim() == EXCLUDE_LINE) {
        return Ok(());
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(EXCLUDE_LINE);
    text.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))
}

struct Runner<'a> {
    ctx: &'a Ctx,
    tree: &'a Tree,
    force: bool,
    system: String,
    overview: String,
    progress: Mutex<Progress>,
    on_progress: &'a (dyn Fn(&Progress) + Sync),
}

struct FileBatch {
    dir: usize,
    files: Vec<usize>,
}

impl<'a> Runner<'a> {
    fn new(ctx: &'a Ctx, tree: &'a Tree, force: bool, on_progress: &'a (dyn Fn(&Progress) + Sync)) -> Self {
        Self {
            ctx,
            tree,
            force,
            system: ctx.system_prompt(),
            overview: ctx.store.project_note().overview.unwrap_or_default(),
            progress: Mutex::new(Progress::default()),
            on_progress,
        }
    }

    fn update(&self, f: impl FnOnce(&mut Progress)) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut p);
        p.cost = self.ctx.cost.usd();
        (self.on_progress)(&p);
    }

    fn error(&self, count: usize, message: String) {
        self.ctx.store.log(&format!("error: {message}"));
        self.update(|p| {
            p.errors += count;
            p.last_error = Some(message);
        });
    }

    fn file_key(&self, i: usize) -> Option<&str> {
        self.tree.node(i).file.as_ref()?.hash.as_deref()
    }

    fn needs_file(&self, i: usize) -> bool {
        let node = self.tree.node(i);
        node.file.as_ref().is_some_and(|f| f.is_text())
            && (self.force
                || freshness(self.ctx.store.file_note(&node.path).summary.as_ref(), self.file_key(i))
                    != Freshness::Fresh)
    }

    fn needs_dir(&self, i: usize) -> bool {
        let key = self.tree.dir_key(i);
        self.force
            || freshness(
                self.ctx.store.dir_note(&self.tree.node(i).path).summary.as_ref(),
                Some(&key),
            ) != Freshness::Fresh
    }

    /// Children of a folder with their current descriptions (or static labels).
    fn entries(&self, dir: usize) -> Vec<Entry> {
        self.tree
            .node(dir)
            .children
            .iter()
            .map(|&c| {
                let n = self.tree.node(c);
                let description = if n.is_dir {
                    self.ctx.store.dir_note(&n.path).summary.map(|s| s.text)
                } else {
                    n.file
                        .as_ref()
                        .and_then(|f| f.label())
                        .or_else(|| self.ctx.store.file_note(&n.path).summary.map(|s| s.text))
                };
                Entry {
                    name: n.name.clone(),
                    is_dir: n.is_dir,
                    description,
                }
            })
            .collect()
    }

    fn overview_step(&self) -> bool {
        let key = self.tree.dir_key(ROOT);
        let note = self.ctx.store.project_note();
        if !self.force && freshness(note.summary.as_ref(), Some(&key)) == Freshness::Fresh {
            return false;
        }
        let req = prompts::overview_request(&self.ctx.config, &self.system, &self.ctx.root, self.tree);
        let result = self.ctx.ask(&req).and_then(|resp| {
            let summary = resp
                .json
                .get("summary")
                .and_then(|v| v.as_str())
                .and_then(prompts::clean_text);
            let overview = resp.json.get("overview").and_then(|v| v.as_str()).map(str::trim);
            let summary = summary.context("overview answer without summary")?;
            let guard = Guard {
                start_key: key.clone(),
                force: self.force,
            };
            let current = || Some(key.clone());
            self.ctx
                .store
                .put_summary_guarded(Target::Project, &guard, Some(&current), &summary, overview)
        });
        match result {
            Ok(written) => written,
            Err(e) => {
                self.error(1, format!("overview: {e:#}"));
                false
            }
        }
    }

    /// Splits files of one folder into batches by `batch_max_files` and `batch_max_chars`.
    fn batches(&self, dir: usize, files: Vec<usize>, max_files: usize) -> Vec<FileBatch> {
        let config = &self.ctx.config;
        let mut out = Vec::new();
        let mut current = Vec::new();
        let mut chars = 0usize;
        for i in files {
            let size = self
                .tree
                .node(i)
                .file
                .as_ref()
                .map_or(0, |f| f.size as usize)
                .min(config.batch_max_chars);
            if !current.is_empty() && (current.len() >= max_files || chars + size > config.batch_max_chars) {
                out.push(FileBatch {
                    dir,
                    files: std::mem::take(&mut current),
                });
                chars = 0;
            }
            chars += size;
            current.push(i);
        }
        if !current.is_empty() {
            out.push(FileBatch { dir, files: current });
        }
        out
    }

    /// Describes one batch; paths missing from the answer get one retry in a
    /// smaller batch. Returns the number of saved descriptions.
    fn run_file_batch(&self, batch: FileBatch, retry: bool) -> usize {
        let config = &self.ctx.config;
        let dir_path = &self.tree.node(batch.dir).path;
        let per_file = config.batch_max_chars / batch.files.len().max(1);
        let heads: Vec<(String, String)> = batch
            .files
            .iter()
            .map(|&i| {
                let p = self.tree.node(i).path.clone();
                let head = prompts::file_head(&self.ctx.root, &p, config.head_lines, per_file);
                (p, head)
            })
            .collect();
        let req = prompts::files_request(
            config,
            &self.system,
            &self.overview,
            dir_path,
            &self.entries(batch.dir),
            &heads,
        );
        let answers = match self.ctx.ask(&req) {
            Ok(resp) => prompts::parse_path_summaries(&resp.json, "files"),
            Err(e) => {
                self.error(batch.files.len(), format!("files in {}: {e:#}", display(dir_path)));
                self.update(|p| p.files_done += batch.files.len());
                return 0;
            }
        };
        let mut saved = 0;
        let mut missing = Vec::new();
        for &i in &batch.files {
            let node = self.tree.node(i);
            let Some((_, text)) = answers.iter().find(|(p, _)| *p == node.path) else {
                missing.push(i);
                continue;
            };
            let Some(key) = self.file_key(i) else { continue };
            let guard = Guard {
                start_key: key.to_string(),
                force: self.force,
            };
            match self
                .ctx
                .store
                .put_summary_guarded(Target::File(&node.path), &guard, None, text, None)
            {
                Ok(true) => saved += 1,
                Ok(false) => {}
                Err(e) => self.error(1, format!("{}: {e:#}", node.path)),
            }
            self.update(|p| p.files_done += 1);
        }
        if !missing.is_empty() {
            if retry {
                let smaller = missing.len().div_ceil(2).max(1);
                for b in self.batches(batch.dir, missing, smaller) {
                    saved += self.run_file_batch(b, false);
                }
            } else {
                let names: Vec<_> = missing.iter().map(|&i| self.tree.node(i).path.clone()).collect();
                self.update(|p| p.files_done += missing.len());
                self.error(
                    missing.len(),
                    format!("no description returned for {}", names.join(", ")),
                );
            }
        }
        saved
    }

    fn run_dir_batch(&self, dirs: Vec<usize>) -> usize {
        let items: Vec<(String, Vec<Entry>)> = dirs
            .iter()
            .map(|&d| (self.tree.node(d).path.clone(), self.entries(d)))
            .collect();
        let req = prompts::dirs_request(&self.ctx.config, &self.system, &self.overview, &items);
        let answers = match self.ctx.ask(&req) {
            Ok(resp) => prompts::parse_path_summaries(&resp.json, "dirs"),
            Err(e) => {
                self.error(dirs.len(), format!("folders: {e:#}"));
                self.update(|p| p.dirs_done += dirs.len());
                return 0;
            }
        };
        let mut saved = 0;
        let mut missing = Vec::new();
        for &d in &dirs {
            let path = &self.tree.node(d).path;
            self.update(|p| p.dirs_done += 1);
            let Some((_, text)) = answers.iter().find(|(p, _)| p == path) else {
                missing.push(path.clone());
                continue;
            };
            let key = self.tree.dir_key(d);
            let guard = Guard {
                start_key: key.clone(),
                force: self.force,
            };
            let current = || Some(key.clone());
            match self
                .ctx
                .store
                .put_summary_guarded(Target::Dir(path), &guard, Some(&current), text, None)
            {
                Ok(true) => saved += 1,
                Ok(false) => {}
                Err(e) => self.error(1, format!("{path}: {e:#}")),
            }
        }
        if !missing.is_empty() {
            self.error(
                missing.len(),
                format!("no description returned for {}", missing.join(", ")),
            );
        }
        saved
    }

    fn files_step(&self, files: Vec<usize>) -> usize {
        // Group by folder, keeping the tree order.
        let mut by_dir: Vec<(usize, Vec<usize>)> = Vec::new();
        for i in files {
            let dir = self.tree.node(i).parent.unwrap_or(ROOT);
            match by_dir.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, v)) => v.push(i),
                None => by_dir.push((dir, vec![i])),
            }
        }
        let batches: Vec<FileBatch> = by_dir
            .into_iter()
            .flat_map(|(d, f)| self.batches(d, f, self.ctx.config.batch_max_files))
            .collect();
        run_parallel(self.ctx.config.workers, batches, |b| self.run_file_batch(b, true))
            .into_iter()
            .sum()
    }

    fn dirs_step(&self, dirs: Vec<usize>) -> usize {
        let max_depth = dirs.iter().map(|&d| self.tree.depth(d)).max().unwrap_or(0);
        let mut saved = 0;
        for depth in (1..=max_depth).rev() {
            let level: Vec<usize> = dirs.iter().copied().filter(|&d| self.tree.depth(d) == depth).collect();
            let batches: Vec<Vec<usize>> = level.chunks(DIRS_PER_BATCH).map(<[usize]>::to_vec).collect();
            saved += run_parallel(self.ctx.config.workers, batches, |b| self.run_dir_batch(b))
                .into_iter()
                .sum::<usize>();
        }
        saved
    }
}

fn display(path: &str) -> &str {
    if path.is_empty() { "." } else { path }
}

/// Full pipeline: scan, overview, files, folders, cleanup.
pub fn run(ctx: &Ctx, opts: &Options, on_progress: &(dyn Fn(&Progress) + Sync)) -> Result<Report> {
    let (tree, warnings) = Tree::scan(&ctx.root, &ctx.config)?;
    for w in warnings {
        ctx.store.log(&format!("warning: {w}"));
    }
    run_on_tree(ctx, &tree, opts, on_progress)
}

pub fn run_on_tree(ctx: &Ctx, tree: &Tree, opts: &Options, on_progress: &(dyn Fn(&Progress) + Sync)) -> Result<Report> {
    let prefix = &opts.prefix;
    if !prefix.is_empty() && tree.get(prefix).is_none() {
        anyhow::bail!("{prefix} is not a file or folder of this repository");
    }
    let mut runner = Runner::new(ctx, tree, opts.force, on_progress);
    let scope = tree.subtree(prefix);
    let files: Vec<usize> = scope.iter().copied().filter(|&i| runner.needs_file(i)).collect();
    let dirs: Vec<usize> = scope
        .iter()
        .copied()
        .filter(|&i| i != ROOT && tree.node(i).is_dir && runner.needs_dir(i))
        .collect();
    let overview_needed = {
        let key = tree.dir_key(ROOT);
        (opts.force && prefix.is_empty())
            || freshness(ctx.store.project_note().summary.as_ref(), Some(&key)) != Freshness::Fresh
    };
    if overview_needed || !files.is_empty() || !dirs.is_empty() {
        // Report a missing CLI once, before any work.
        ctx.backend()?;
    }
    runner.update(|p| {
        p.files_total = files.len();
        p.dirs_total = dirs.len();
    });

    let mut report = Report::default();
    if overview_needed {
        // The project overview is refreshed with --force only for the whole repository.
        runner.force = opts.force && prefix.is_empty();
        report.overview = runner.overview_step();
        runner.force = opts.force;
        runner.overview = ctx.store.project_note().overview.unwrap_or_default();
    }
    report.files = runner.files_step(files);
    report.dirs = runner.dirs_step(dirs);
    report.removed = cleanup(ctx, tree, prefix)?;
    report.skipped = scope
        .iter()
        .filter(|&&i| tree.node(i).file.as_ref().is_some_and(|f| f.label().is_some()))
        .count();
    let p = runner.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
    report.errors = p.errors;
    report.cost = ctx.cost.usd();
    Ok(report)
}

/// Removes notes of files and folders that no longer exist (within `prefix`).
fn cleanup(ctx: &Ctx, tree: &Tree, prefix: &str) -> Result<usize> {
    let in_scope = |p: &str| prefix.is_empty() || p == prefix || p.starts_with(&format!("{prefix}/"));
    let mut removed = 0;
    for p in ctx.store.file_note_paths() {
        if in_scope(&p) && tree.get(&p).is_none_or(|i| tree.node(i).is_dir) {
            ctx.store.remove_file_note(&p)?;
            removed += 1;
        }
    }
    for p in ctx.store.dir_note_paths() {
        if in_scope(&p) && tree.get(&p).is_none_or(|i| !tree.node(i).is_dir) {
            ctx.store.remove_dir_note(&p)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Describes the given files (one batch per folder); used by the TUI (`r`, after `e`).
pub fn describe_files(ctx: &Ctx, tree: &Tree, paths: &[String], force: bool) -> Result<usize> {
    let noop = |_: &Progress| {};
    let runner = Runner::new(ctx, tree, force, &noop);
    let files: Vec<usize> = paths
        .iter()
        .filter_map(|p| tree.get(p))
        .filter(|&i| tree.node(i).file.as_ref().is_some_and(|f| f.is_text()))
        .collect();
    let saved = runner.files_step(files);
    runner_result(&runner, saved)
}

/// Describes the given folders; used by the TUI (`r`).
pub fn describe_dirs(ctx: &Ctx, tree: &Tree, paths: &[String], force: bool) -> Result<usize> {
    let noop = |_: &Progress| {};
    let runner = Runner::new(ctx, tree, force, &noop);
    let dirs: Vec<usize> = paths
        .iter()
        .filter_map(|p| tree.get(p))
        .filter(|&i| i != ROOT && tree.node(i).is_dir)
        .collect();
    let saved = runner.dirs_step(dirs);
    runner_result(&runner, saved)
}

/// Regenerates the project overview and summary; used by the TUI (`r` on the root).
pub fn describe_project(ctx: &Ctx, tree: &Tree, force: bool) -> Result<usize> {
    let noop = |_: &Progress| {};
    let runner = Runner::new(ctx, tree, force, &noop);
    let saved = usize::from(runner.overview_step());
    runner_result(&runner, saved)
}

fn runner_result(runner: &Runner, saved: usize) -> Result<usize> {
    let p = runner.progress.lock().unwrap_or_else(|e| e.into_inner());
    match &p.last_error {
        Some(e) => anyhow::bail!("{e}"),
        None => Ok(saved),
    }
}

/// True when the repository has no project note yet (the TUI then starts init in the background).
pub fn needs_init(ctx: &Ctx) -> bool {
    !ctx.store.project_path().exists()
}

pub use scan::normalize_prefix;
