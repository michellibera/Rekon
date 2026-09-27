//! TUI state and event handling. Background threads (scan, init, highlighting) and
//! the job pool (model calls) report through channels; the UI thread owns all state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::Line;
use rekon_core::Ctx;
use rekon_core::init::{self, Options, Progress};
use rekon_core::jobs::{JobDone, JobKey, JobPool};
use rekon_core::model::{Blocks, DirNote, FileNote, Freshness, ProjectNote, freshness};
use rekon_core::ontology::graph::GraphIndex;
use rekon_core::ontology::index::{self as ontology, Progress as OntologyProgress};
use rekon_core::ontology::model::{Edge, Evidence};
use rekon_core::scan::{Excluder, Tree};
use rekon_core::{segment, text};

use super::editor::EditRequest;
use super::graph::{Click, Dir, Explorer, Sel};
use super::highlight::{self, Lines};
use super::rows::{self, BlockView, Desc, Row, RowKind, RowRef};

const TICK: Duration = Duration::from_secs(2);
const RESCAN: Duration = Duration::from_secs(15);
/// Files larger than this are not shown in the code panel.
const MAX_SHOWN_BYTES: u64 = 20_000_000;

pub enum Bg {
    Analyzed(Tree),
    InitProgress(Progress),
    InitDone(Result<String, String>),
    OntologyProgress(OntologyProgress),
    OntologyDone(Result<String, String>),
    Highlighted {
        path: String,
        hash: String,
        lines: Option<Vec<Line<'static>>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Code,
}

/// Representation of the project in the left panel (its tabs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Tree,
    Ontology,
}

/// Evidence of the element selected in the graph, shown in the code panel.
pub struct EvidenceNav {
    pub sel: Sel,
    /// What the evidence proves, e.g. `Store —writes→ notes`.
    pub title: String,
    pub items: Vec<Evidence>,
    pub index: usize,
}

impl EvidenceNav {
    pub fn current(&self) -> &Evidence {
        &self.items[self.index]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popup {
    Overview(u16),
    Help,
    /// Filters of the graph, with the cursor on this entry.
    Filter(usize),
}

pub struct OpenFile {
    pub path: String,
    pub hash: Option<String>,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub lines: Vec<String>,
    /// Why the code cannot be shown (binary, too large).
    pub problem: Option<String>,
    pub highlighted: Option<Lines>,
}

/// Selection and scroll state of one panel.
#[derive(Default)]
pub struct Panel {
    pub sel: usize,
    pub offset: usize,
    /// Keep the selection in view (off after scrolling with the wheel).
    pub follow: bool,
    /// Inner area (without borders) of the last frame.
    pub area: Rect,
}

impl Panel {
    pub fn height(&self) -> usize {
        self.area.height as usize
    }

    /// Adjusts the offset so the selection, rows `sel..end`, is visible (when following);
    /// a selection taller than the panel shows its top.
    pub fn scroll_to_selection(&mut self, rows: usize, end: usize) {
        let h = self.height().max(1);
        if self.follow {
            let end = end.max(self.sel + 1);
            if self.sel < self.offset {
                self.offset = self.sel;
            } else if end > self.offset + h {
                self.offset = self.sel.min(end - h);
            }
        }
        self.offset = self.offset.min(rows.saturating_sub(h));
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        let a = self.area;
        x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height
    }
}

struct Cached<T> {
    mtime: Option<SystemTime>,
    note: T,
}

/// Notes read from `.rekon/`, revalidated by the modification time of their files.
#[derive(Default)]
pub struct Notes {
    files: HashMap<String, Cached<FileNote>>,
    dirs: HashMap<String, Cached<DirNote>>,
    project: Option<Cached<ProjectNote>>,
}

fn mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Notes {
    pub fn file(&mut self, ctx: &Ctx, path: &str) -> &FileNote {
        &self
            .files
            .entry(path.to_string())
            .or_insert_with(|| Cached {
                mtime: mtime(&ctx.store.file_note_path(path)),
                note: ctx.store.file_note(path),
            })
            .note
    }

    pub fn dir(&mut self, ctx: &Ctx, path: &str) -> &DirNote {
        &self
            .dirs
            .entry(path.to_string())
            .or_insert_with(|| Cached {
                mtime: mtime(&ctx.store.dir_note_path(path)),
                note: ctx.store.dir_note(path),
            })
            .note
    }

    pub fn project(&mut self, ctx: &Ctx) -> &ProjectNote {
        &self
            .project
            .get_or_insert_with(|| Cached {
                mtime: mtime(&ctx.store.project_path()),
                note: ctx.store.project_note(),
            })
            .note
    }

    /// Drops cached notes whose files changed on disk; returns true if any did.
    pub fn revalidate(&mut self, ctx: &Ctx, files: &[String], dirs: &[String]) -> bool {
        let mut changed = false;
        for p in files {
            if self
                .files
                .get(p)
                .is_some_and(|c| c.mtime != mtime(&ctx.store.file_note_path(p)))
            {
                self.files.remove(p);
                changed = true;
            }
        }
        for p in dirs {
            if self
                .dirs
                .get(p)
                .is_some_and(|c| c.mtime != mtime(&ctx.store.dir_note_path(p)))
            {
                self.dirs.remove(p);
                changed = true;
            }
        }
        if self
            .project
            .as_ref()
            .is_some_and(|c| c.mtime != mtime(&ctx.store.project_path()))
        {
            self.project = None;
            changed = true;
        }
        changed
    }

    pub fn forget_file(&mut self, path: &str) {
        self.files.remove(path);
    }

    pub fn forget_dir(&mut self, path: &str) {
        self.dirs.remove(path);
    }

    pub fn clear(&mut self) {
        *self = Notes::default();
    }
}

pub struct App {
    pub ctx: Arc<Ctx>,
    pub tree: Tree,
    pub notes: Notes,
    pub expanded: HashSet<String>,
    /// Expanded blocks as (file path, line range).
    pub expanded_blocks: HashSet<(String, (u32, u32))>,
    pub focus: Focus,
    pub wide: bool,
    /// What the left panel shows: the tree or the ontology graph.
    pub view: View,
    pub explorer: Explorer,
    pub evidence: Option<EvidenceNav>,
    /// Ontology analysis running in the background.
    pub ontology: Option<OntologyProgress>,
    /// Tabs of the left panel in the last frame, for clicks.
    pub tabs: Vec<(Rect, View)>,
    /// One-time message for the status line, cleared by the next key.
    pub notice: Option<String>,
    /// Descriptions only in the code panel (`o`).
    pub desc_only: bool,
    pub popup: Option<Popup>,
    pub open: Option<OpenFile>,
    pub tree_rows: Vec<Row>,
    pub code_rows: Vec<Row>,
    pub tree_panel: Panel,
    pub code_panel: Panel,
    tree_sel_key: Option<RowRef>,
    code_sel_key: Option<RowRef>,
    pub init: Option<Progress>,
    pub errors: usize,
    pub last_error: Option<String>,
    pub pool: JobPool,
    done_rx: Receiver<JobDone>,
    hl_cache: highlight::Cache,
    bg_tx: Sender<Bg>,
    bg_rx: Receiver<Bg>,
    excluder: Excluder,
    last_tick: Instant,
    last_rescan: Instant,
    rescanning: bool,
    /// Set by `e`; the event loop runs the editor and calls [`App::after_edit`].
    pub editor_request: Option<EditRequest>,
    pub quit: bool,
    pub dirty: bool,
}

impl App {
    /// Lists the repository (fast) and starts hashing and, without a map, init in the background.
    pub fn new(ctx: Arc<Ctx>) -> anyhow::Result<Self> {
        let (tree, _) = Tree::list(&ctx.root)?;
        let mut app = Self::with_tree(ctx, tree)?;
        app.start_rescan();
        if init::needs_init(&app.ctx) {
            app.start_init();
        }
        Ok(app)
    }

    /// State for a given tree, nothing started in the background (also for tests).
    pub fn with_tree(ctx: Arc<Ctx>, tree: Tree) -> anyhow::Result<Self> {
        let (bg_tx, bg_rx) = channel();
        let (done_tx, done_rx) = channel();
        let excluder = Excluder::new(&ctx.config.exclude)?;
        let pool = JobPool::new(ctx.config.workers, done_tx);
        Ok(Self {
            ctx,
            tree,
            notes: Notes::default(),
            expanded: HashSet::new(),
            expanded_blocks: HashSet::new(),
            focus: Focus::Tree,
            wide: false,
            view: View::Tree,
            explorer: Explorer::default(),
            evidence: None,
            ontology: None,
            tabs: Vec::new(),
            notice: None,
            desc_only: false,
            popup: None,
            open: None,
            tree_rows: Vec::new(),
            code_rows: Vec::new(),
            tree_panel: Panel {
                follow: true,
                ..Default::default()
            },
            code_panel: Panel {
                follow: true,
                ..Default::default()
            },
            tree_sel_key: None,
            code_sel_key: None,
            init: None,
            errors: 0,
            last_error: None,
            pool,
            done_rx,
            hl_cache: highlight::Cache::default(),
            bg_tx,
            bg_rx,
            excluder,
            last_tick: Instant::now(),
            last_rescan: Instant::now(),
            rescanning: false,
            editor_request: None,
            quit: false,
            dirty: true,
        })
    }

    pub fn repo_name(&self) -> String {
        self.ctx
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string()
    }

    fn start_rescan(&mut self) {
        if self.rescanning {
            return;
        }
        self.rescanning = true;
        let old = self.tree.clone();
        let ctx = Arc::clone(&self.ctx);
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            if let Ok((mut tree, _)) = Tree::list(&ctx.root) {
                tree.reuse_from(&old);
                if tree.analyze_all(&ctx.root, &ctx.config, None).is_ok() {
                    let _ = tx.send(Bg::Analyzed(tree));
                    return;
                }
            }
            let _ = tx.send(Bg::Analyzed(old));
        });
    }

    fn start_init(&mut self) {
        if self.init.is_some() {
            return;
        }
        if let Err(e) = init::prepare(&self.ctx.root) {
            self.error(format!("init: {e:#}"));
            return;
        }
        self.init = Some(Progress::default());
        let ctx = Arc::clone(&self.ctx);
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let progress_tx = std::sync::Mutex::new(tx.clone());
            let on_progress = |p: &Progress| {
                let _ = progress_tx.lock().map(|t| t.send(Bg::InitProgress(p.clone())));
            };
            let result = init::run(&ctx, &Options::default(), &on_progress)
                .map(|r| format!("init: {} files, {} folders, {} errors", r.files, r.dirs, r.errors))
                .map_err(|e| format!("init: {e:#}"));
            let _ = tx.send(Bg::InitDone(result));
        });
    }

    /// Runs the ontology analysis in the background: files changed since the last
    /// run, or `paths` again with `force`.
    fn start_ontology(&mut self, paths: Option<Vec<String>>, force: bool) {
        if self.ontology.is_some() {
            return;
        }
        if let Err(e) = init::prepare(&self.ctx.root) {
            self.error(format!("ontology: {e:#}"));
            return;
        }
        self.ontology = Some(OntologyProgress::default());
        let ctx = Arc::clone(&self.ctx);
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let progress_tx = std::sync::Mutex::new(tx.clone());
            let on_progress = |p: &OntologyProgress| {
                let _ = progress_tx.lock().map(|t| t.send(Bg::OntologyProgress(p.clone())));
            };
            let opts = ontology::Options {
                force,
                paths,
                dry_run: false,
            };
            let result = ontology::run(&ctx, &opts, &on_progress)
                .map(|r| text::ontology_done(r.analyzed, r.nodes, r.edges, r.errors))
                .map_err(|e| format!("ontology: {e:#}"));
            let _ = tx.send(Bg::OntologyDone(result));
        });
    }

    /// Switches the left panel between the tree and the ontology graph.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
        if view == View::Ontology {
            if self.explorer.area.width == 0 {
                self.explorer.area = self.tree_panel.area;
            }
            self.explorer.load(self.ctx.store.dir());
        }
    }

    pub fn error(&mut self, message: String) {
        self.errors += 1;
        self.last_error = Some(message);
        self.dirty = true;
    }

    /// Handles background messages, finished jobs and periodic refresh. Call often.
    pub fn poll(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            self.dirty = true;
            match msg {
                Bg::Analyzed(tree) => {
                    self.tree = tree;
                    self.rescanning = false;
                    self.last_rescan = Instant::now();
                }
                Bg::InitProgress(p) => {
                    if p.files_done + p.dirs_done != self.init.as_ref().map_or(0, |o| o.files_done + o.dirs_done) {
                        self.notes.clear();
                    }
                    self.init = Some(p);
                }
                Bg::InitDone(result) => {
                    self.init = None;
                    self.notes.clear();
                    if let Err(e) = result {
                        self.error(e);
                    }
                }
                Bg::OntologyProgress(p) => self.ontology = Some(p),
                Bg::OntologyDone(result) => {
                    self.ontology = None;
                    self.explorer.load(self.ctx.store.dir());
                    match result {
                        Ok(m) => self.notice = Some(m),
                        Err(e) => self.error(e),
                    }
                }
                Bg::Highlighted { path, hash, lines } => {
                    let lines = lines.map(Arc::new);
                    if let Some(l) = &lines {
                        self.hl_cache.put(path.clone(), hash.clone(), l.clone());
                    }
                    if let Some(open) = self.open.as_mut()
                        && open.path == path
                        && open.hash.as_deref() == Some(hash.as_str())
                    {
                        open.highlighted = lines;
                    }
                }
            }
        }
        while let Ok(done) = self.done_rx.try_recv() {
            self.dirty = true;
            match &done.key {
                JobKey::FileSummary(p) | JobKey::Level1(p) | JobKey::Split(p, _) => self.notes.forget_file(p),
                JobKey::DirSummary(p) => self.notes.forget_dir(p),
                JobKey::Init => self.notes.clear(),
            }
            if let Err(e) = done.result {
                self.error(e);
            }
        }
        if self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            self.tick();
        }
        if self.last_rescan.elapsed() >= RESCAN {
            self.last_rescan = Instant::now();
            self.start_rescan();
        }
    }

    /// Every 2 s: re-stat visible files and the open file, reload changed notes (and
    /// the ontology graph, when it changed on disk).
    pub fn tick(&mut self) {
        if self.view == View::Ontology && self.explorer.load(self.ctx.store.dir()) {
            self.dirty = true;
        }
        let (start, end) = (
            self.tree_panel.offset,
            self.tree_panel.offset + self.tree_panel.height(),
        );
        let visible: Vec<String> = self.tree_rows[start.min(self.tree_rows.len())..end.min(self.tree_rows.len())]
            .iter()
            .filter_map(|r| match &r.target {
                RowRef::Node(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for p in &visible {
            let Some(i) = self.tree.get(p) else { continue };
            if self.tree.node(i).is_dir {
                dirs.push(p.clone());
            } else {
                files.push(p.clone());
                if self
                    .tree
                    .refresh_file(i, &self.ctx.root, &self.ctx.config, &self.excluder)
                {
                    self.dirty = true;
                }
            }
        }
        if let Some(open) = &self.open {
            files.push(open.path.clone());
            let meta = std::fs::metadata(self.ctx.root.join(&open.path)).ok();
            let (size, mtime) = (
                meta.as_ref().map_or(0, |m| m.len()),
                meta.and_then(|m| m.modified().ok()),
            );
            if size != open.size || mtime != open.mtime {
                let path = open.path.clone();
                self.reload_open(&path);
                if let Some(i) = self.tree.get(&path) {
                    self.tree
                        .refresh_file(i, &self.ctx.root, &self.ctx.config, &self.excluder);
                }
            }
        }
        if self.notes.revalidate(&self.ctx, &files, &dirs) {
            self.dirty = true;
        }
    }

    // ----- descriptions -----

    pub fn desc(&mut self, i: usize) -> Desc {
        let node = self.tree.node(i);
        let path = node.path.clone();
        if node.is_dir {
            let key = self.tree.dir_key(i);
            let pending = self.init.is_some() || self.pool.is_pending(&JobKey::DirSummary(path.clone()));
            let summary = self.notes.dir(&self.ctx, &path).summary.clone();
            return to_desc(
                summary.as_ref().map(|s| s.text.clone()),
                freshness(summary.as_ref(), Some(&key)),
                pending,
            );
        }
        let info = node.file.clone().unwrap_or_default();
        if let Some(label) = info.label() {
            return Desc::Static(label);
        }
        let pending = self.init.is_some() || self.pool.is_pending(&JobKey::FileSummary(path.clone()));
        let summary = self.notes.file(&self.ctx, &path).summary.clone();
        to_desc(
            summary.as_ref().map(|s| s.text.clone()),
            freshness(summary.as_ref(), info.hash.as_deref()),
            pending,
        )
    }

    pub fn project_summary(&mut self) -> Option<String> {
        self.notes.project(&self.ctx).summary.as_ref().map(|s| s.text.clone())
    }

    pub fn project_overview(&mut self) -> Option<String> {
        self.notes.project(&self.ctx).overview.clone()
    }

    /// Fresh blocks of the open file.
    pub fn open_blocks(&mut self) -> Option<Blocks> {
        let open = self.open.as_ref()?;
        let (path, hash) = (open.path.clone(), open.hash.clone()?);
        self.notes
            .file(&self.ctx, &path)
            .blocks
            .clone()
            .filter(|b| b.hash == hash)
    }

    /// Why the open file has no blocks (for the panel title), if it has none.
    pub fn blocks_status(&mut self) -> Option<String> {
        let open = self.open.as_ref()?;
        if open.problem.is_some() {
            return None;
        }
        let path = open.path.clone();
        let lines = open.lines.len() as u32;
        if self.pool.is_pending(&JobKey::Level1(path.clone())) {
            return Some(text::SPLITTING.to_string());
        }
        if lines > self.ctx.config.max_segment_lines {
            return Some(text::too_long_for_blocks(lines, self.ctx.config.max_segment_lines));
        }
        if self.open_blocks().is_none() {
            let stale = self.notes.file(&self.ctx, &path).blocks.is_some();
            return Some(if stale { text::BLOCKS_OUTDATED } else { text::NO_BLOCKS }.to_string());
        }
        None
    }

    // ----- rows -----

    /// Rebuilds the rows of both panels for the given inner widths.
    pub fn rebuild(&mut self, tree_width: usize, code_width: usize) {
        let visible = rows::visible_nodes(&self.tree, &self.expanded);
        let mut out = Vec::with_capacity(visible.len());
        for (i, depth) in visible {
            let desc = self.desc(i);
            let expanded = self.expanded.contains(&self.tree.node(i).path);
            out.push(rows::tree_row(&self.tree, i, depth, expanded, &desc, tree_width));
        }
        self.tree_rows = out;
        self.tree_panel.sel = restore(&self.tree_rows, &self.tree_sel_key, self.tree_panel.sel);
        self.code_rows = self.build_code_rows(code_width);
        let mut sel = restore(&self.code_rows, &self.code_sel_key, self.code_panel.sel);
        if self.has_headers() && self.code_rows.get(sel).is_some_and(|r| r.kind != RowKind::BlockHeader) {
            // With blocks the selection stays on headers: the one above, else the first.
            sel = (0..=sel)
                .rev()
                .find(|&i| self.code_rows[i].kind == RowKind::BlockHeader)
                .or_else(|| self.code_rows.iter().position(|r| r.kind == RowKind::BlockHeader))
                .unwrap_or(0);
        }
        self.code_panel.sel = sel;
    }

    fn build_code_rows(&mut self, width: usize) -> Vec<Row> {
        // Evidence is shown in plain lines, so its range reads as in the file.
        let blocks = if self.evidence_lines().is_some() {
            None
        } else {
            self.open_blocks()
        };
        let Some(open) = &self.open else { return Vec::new() };
        let hl = open.highlighted.as_ref().map(|h| h.as_slice());
        let Some(blocks) = blocks.filter(|b| !b.items.is_empty()) else {
            return open
                .lines
                .iter()
                .enumerate()
                .map(|(i, raw)| rows::code_row(i as u32 + 1, 0, Vec::new(), hl.and_then(|h| h.get(i)), raw))
                .collect();
        };
        let path = open.path.clone();
        let expanded = |r: (u32, u32)| self.expanded_blocks.contains(&(path.clone(), r));
        let pending = |r: (u32, u32)| self.pool.is_pending(&JobKey::Split(path.clone(), r));
        let view = BlockView {
            blocks: &blocks.items,
            expanded: &expanded,
            pending: &pending,
            desc_only: self.desc_only,
            lines: &open.lines,
            highlighted: hl,
            width,
        };
        rows::block_rows(&view)
    }

    pub fn selected_tree_path(&self) -> Option<&str> {
        match &self.tree_rows.get(self.tree_panel.sel)?.target {
            RowRef::Node(p) => Some(p),
            _ => None,
        }
    }

    fn selected_block(&self) -> Option<(u32, u32)> {
        match self.code_rows.get(self.code_panel.sel)?.target {
            RowRef::Block(r) => Some(r),
            _ => None,
        }
    }

    fn has_headers(&self) -> bool {
        self.code_rows.iter().any(|r| r.kind == RowKind::BlockHeader)
    }

    // ----- input -----

    pub fn on_key(&mut self, key: KeyEvent) {
        self.dirty = true;
        self.notice = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(popup) = self.popup {
            match (popup, key.code) {
                (_, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Char('i')) => self.popup = None,
                (Popup::Overview(s), KeyCode::Down | KeyCode::Char('j')) => self.popup = Some(Popup::Overview(s + 1)),
                (Popup::Overview(s), KeyCode::Up | KeyCode::Char('k')) => {
                    self.popup = Some(Popup::Overview(s.saturating_sub(1)))
                }
                (Popup::Filter(_), KeyCode::Char('f')) => self.popup = None,
                (Popup::Filter(i), KeyCode::Down | KeyCode::Char('j')) => {
                    let last = self.explorer.filter_entries().len().saturating_sub(1);
                    self.popup = Some(Popup::Filter((i + 1).min(last)))
                }
                (Popup::Filter(i), KeyCode::Up | KeyCode::Char('k')) => {
                    self.popup = Some(Popup::Filter(i.saturating_sub(1)))
                }
                (Popup::Filter(i), KeyCode::Char(' ') | KeyCode::Enter) => {
                    if let Some((f, _)) = self.explorer.filter_entries().into_iter().nth(i)
                        && !self.explorer.toggle_filter(f)
                    {
                        self.notice = Some(text::reveal_stopped(super::graph::REVEAL_MAX));
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('1') => return self.set_view(View::Tree),
            KeyCode::Char('2') => return self.set_view(View::Ontology),
            KeyCode::Char('[') => return self.evidence_step(-1),
            KeyCode::Char(']') => return self.evidence_step(1),
            _ => {}
        }
        if self.view == View::Ontology && self.focus == Focus::Tree && self.graph_key(key) {
            return;
        }
        let page = match self.focus {
            Focus::Tree => self.tree_panel.height(),
            Focus::Code => self.code_panel.height(),
        }
        .max(1) as isize;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('i') => self.popup = Some(Popup::Overview(0)),
            KeyCode::Char('w') => {
                self.wide = !self.wide;
                if self.wide {
                    self.focus = Focus::Tree;
                }
            }
            KeyCode::Char('o') => self.desc_only = !self.desc_only,
            KeyCode::Char('r') => self.regenerate(),
            KeyCode::Char('R') => self.start_init(),
            KeyCode::Char('e') => self.request_editor(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Tree if !self.wide => Focus::Code,
                    _ => Focus::Tree,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(page),
            KeyCode::PageUp => self.move_sel(-page),
            KeyCode::Home => self.move_sel(isize::MIN / 2),
            KeyCode::End => self.move_sel(isize::MAX / 2),
            KeyCode::Right | KeyCode::Char('l') => self.activate(false),
            KeyCode::Enter => self.activate(true),
            KeyCode::Left | KeyCode::Char('h') => self.back(),
            _ => {}
        }
    }

    /// Keys of the graph panel; false for keys left to the common handling.
    fn graph_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let half = i32::from(self.explorer.area.height / 2).max(1);
        match key.code {
            KeyCode::Up if shift => self.explorer.pan(0, -3),
            KeyCode::Down if shift => self.explorer.pan(0, 3),
            KeyCode::Left if shift => self.explorer.pan(-8, 0),
            KeyCode::Right if shift => self.explorer.pan(8, 0),
            KeyCode::Up | KeyCode::Char('k') => self.explorer.go(Dir::Up),
            KeyCode::Down | KeyCode::Char('j') => self.explorer.go(Dir::Down),
            KeyCode::Left | KeyCode::Char('h') => self.explorer.go(Dir::Left),
            KeyCode::Right | KeyCode::Char('l') => self.explorer.go(Dir::Right),
            KeyCode::Char(' ') => self.explorer.toggle(),
            KeyCode::Backspace => self.explorer.collapse_selected(),
            KeyCode::Esc => self.explorer.up_level(),
            KeyCode::Enter => self.show_evidence(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.explorer.zoom_step(1, None),
            KeyCode::Char('-') => self.explorer.zoom_step(-1, None),
            KeyCode::Char('0') => self.explorer.zoom_to(1.0, None),
            KeyCode::Char('c') => self.explorer.center = Some(0.33),
            KeyCode::Home => self.explorer.select_root(),
            KeyCode::PageUp => self.explorer.pan(0, -half),
            KeyCode::PageDown => self.explorer.pan(0, half),
            KeyCode::Char('L') => {
                if self.explorer.lineage() == 0 {
                    self.notice = Some(text::NO_LINEAGE.into());
                }
            }
            KeyCode::Char('f') => self.popup = Some(Popup::Filter(0)),
            KeyCode::Char('R') => self.start_ontology(None, false),
            KeyCode::Char('r') => {
                let files = self.selection_files();
                if !files.is_empty() {
                    self.start_ontology(Some(files), true);
                }
            }
            _ => return false,
        }
        true
    }

    /// Files of the evidence of the element selected in the graph.
    fn selection_files(&self) -> Vec<String> {
        let (Some(g), Some(sel)) = (&self.explorer.graph, &self.explorer.sel) else {
            return Vec::new();
        };
        let evidence = match sel {
            Sel::Node(id) => g.node(id).map(|n| &n.evidence),
            Sel::Edge(id) => g.edge(id).map(|e| &e.evidence),
            Sel::More(_) | Sel::Group(..) => None,
        };
        let mut files: Vec<String> = evidence.into_iter().flatten().map(|e| e.file.clone()).collect();
        files.sort();
        files.dedup();
        files
    }

    /// Enter in the graph: the evidence of the selected element in the code panel;
    /// again on the same element, its next evidence.
    fn show_evidence(&mut self) {
        let Some(sel) = self.explorer.sel.clone() else { return };
        if let Some(ev) = self.evidence.as_mut().filter(|ev| ev.sel == sel) {
            ev.index = (ev.index + 1) % ev.items.len();
            self.open_evidence();
            return;
        }
        let Some(g) = self.explorer.graph.clone() else { return };
        let (title, items) = match &sel {
            Sel::Node(id) => match g.node(id) {
                Some(n) => (format!("{} · {}", n.kind, n.name), n.evidence.clone()),
                None => return,
            },
            Sel::Edge(id) => match g.edge(id) {
                Some(e) => (edge_title(&g, e), e.evidence.clone()),
                None => return,
            },
            Sel::More(id) => return self.explorer.show_more(&id.clone()),
            Sel::Group(id, label) => return self.explorer.open_group(&id.clone(), &label.clone()),
        };
        if items.is_empty() {
            self.notice = Some(text::NO_EVIDENCE.into());
            return;
        }
        self.evidence = Some(EvidenceNav {
            sel,
            title,
            items,
            index: 0,
        });
        self.open_evidence();
    }

    /// `[` / `]`: previous or next evidence of the element shown.
    fn evidence_step(&mut self, delta: isize) {
        let Some(ev) = self.evidence.as_mut() else { return };
        let n = ev.items.len() as isize;
        ev.index = (ev.index as isize + delta).rem_euclid(n) as usize;
        self.open_evidence();
    }

    /// Opens the file of the current evidence (without splitting it into blocks) and
    /// scrolls to its lines.
    fn open_evidence(&mut self) {
        let Some(item) = self.evidence.as_ref().map(|ev| ev.current().clone()) else {
            return;
        };
        if self.open.as_ref().is_none_or(|o| o.path != item.file) {
            self.code_panel = Panel {
                follow: true,
                area: self.code_panel.area,
                ..Default::default()
            };
            self.reload_open(&item.file);
        }
        let start = item.start_line.max(1);
        self.code_panel.sel = start as usize - 1;
        self.code_panel.offset = (start as usize - 1).saturating_sub(3);
        self.code_panel.follow = true;
        self.code_sel_key = Some(RowRef::Line(start));
        self.dirty = true;
    }

    /// Lines of the evidence shown in the open file.
    pub fn evidence_lines(&self) -> Option<(u32, u32)> {
        let ev = self.evidence.as_ref()?.current();
        (self.open.as_ref()?.path == ev.file).then_some((ev.start_line, ev.end_line))
    }

    /// Moves the selection; in the code panel with blocks it only stops on headers.
    fn move_sel(&mut self, delta: isize) {
        let headers_only = self.focus == Focus::Code && self.has_headers();
        let (panel, rows, key) = match self.focus {
            Focus::Tree => (&mut self.tree_panel, &self.tree_rows, &mut self.tree_sel_key),
            Focus::Code => (&mut self.code_panel, &self.code_rows, &mut self.code_sel_key),
        };
        if rows.is_empty() {
            return;
        }
        let mut target = (panel.sel as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        if headers_only {
            let is_header = |i: usize| rows[i].kind == RowKind::BlockHeader;
            let forward = (target..rows.len()).find(|&i| is_header(i));
            let backward = (0..=target).rev().find(|&i| is_header(i));
            let pick = if delta > 0 {
                forward.or(backward)
            } else {
                backward.or(forward)
            };
            // A single step must leave the current header.
            target = match pick {
                Some(i) if i == panel.sel && delta == 1 => (i + 1..rows.len()).find(|&j| is_header(j)).unwrap_or(i),
                Some(i) => i,
                None => panel.sel,
            };
        }
        panel.sel = target;
        panel.follow = true;
        *key = Some(rows[target].target.clone());
    }

    fn activate(&mut self, focus_code: bool) {
        if self.focus == Focus::Code {
            if let Some(range) = self.selected_block() {
                self.expand_block(range, true);
            }
            return;
        }
        let Some(row) = self.tree_rows.get(self.tree_panel.sel).cloned() else {
            return;
        };
        let RowRef::Node(path) = &row.target else { return };
        match row.kind {
            RowKind::Dir => {
                if !self.expanded.insert(path.clone()) {
                    self.move_sel(1);
                }
            }
            RowKind::File => {
                self.open_file(path);
                if focus_code && !self.wide {
                    self.focus = Focus::Code;
                }
            }
            _ => {}
        }
    }

    fn back(&mut self) {
        if self.focus == Focus::Code {
            self.back_in_code();
            return;
        }
        let Some(row) = self.tree_rows.get(self.tree_panel.sel).cloned() else {
            return;
        };
        let RowRef::Node(path) = &row.target else { return };
        if row.kind == RowKind::Dir && self.expanded.remove(path) {
            return;
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.select_tree_path(parent);
        }
    }

    /// Collapses the selected block, else selects its parent header, else goes to the tree.
    fn back_in_code(&mut self) {
        let Some(open) = &self.open else {
            self.focus = Focus::Tree;
            return;
        };
        let path = open.path.clone();
        let sel = self.code_panel.sel;
        let Some(range) = self.selected_block() else {
            self.focus = Focus::Tree;
            return;
        };
        if self.expanded_blocks.remove(&(path, range)) {
            return;
        }
        let depth = self.code_rows[sel].depth;
        let parent = (0..sel)
            .rev()
            .find(|&i| self.code_rows[i].kind == RowKind::BlockHeader && self.code_rows[i].depth < depth);
        match parent {
            Some(i) => {
                self.code_panel.sel = i;
                self.code_panel.follow = true;
                self.code_sel_key = Some(self.code_rows[i].target.clone());
            }
            None => self.focus = Focus::Tree,
        }
    }

    /// Expands a block: children from the note, or a split job when there are none.
    /// On an expanded block a key moves to its first child, a click collapses it.
    fn expand_block(&mut self, range: (u32, u32), from_key: bool) {
        let Some(path) = self.open.as_ref().map(|o| o.path.clone()) else {
            return;
        };
        let Some(blocks) = self.open_blocks() else { return };
        let Some(block) = blocks.find(range) else { return };
        if block.is_leaf() {
            return;
        }
        let key = (path.clone(), range);
        if self.expanded_blocks.contains(&key) {
            if from_key {
                self.move_sel(1);
            } else {
                self.expanded_blocks.remove(&key);
            }
            return;
        }
        self.expanded_blocks.insert(key);
        if block.children.is_none() {
            self.submit_split(&path, range, false);
        }
    }

    fn select_tree_path(&mut self, path: &str) {
        if let Some(i) = self
            .tree_rows
            .iter()
            .position(|r| r.target == RowRef::Node(path.to_string()))
        {
            self.tree_panel.sel = i;
            self.tree_panel.follow = true;
            self.tree_sel_key = Some(self.tree_rows[i].target.clone());
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        if m.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(&(_, view)) = self.tabs.iter().find(|(r, _)| r.contains((x, y).into()))
        {
            self.set_view(view);
            self.dirty = true;
            return;
        }
        let in_graph = self.explorer.area.contains((x, y).into());
        if self.view == View::Ontology && (self.explorer.dragging() || in_graph) {
            self.graph_mouse(m);
            return;
        }
        let focus = if self.tree_panel.contains(x, y) {
            Focus::Tree
        } else if !self.wide && self.code_panel.contains(x, y) {
            Focus::Code
        } else {
            return;
        };
        let (panel, rows) = match focus {
            Focus::Tree => (&mut self.tree_panel, self.tree_rows.len()),
            Focus::Code => (&mut self.code_panel, self.code_rows.len()),
        };
        match m.kind {
            MouseEventKind::ScrollDown => {
                panel.follow = false;
                panel.offset = (panel.offset + 3).min(rows.saturating_sub(panel.height()));
                self.dirty = true;
            }
            MouseEventKind::ScrollUp => {
                panel.follow = false;
                panel.offset = panel.offset.saturating_sub(3);
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let index = panel.offset + (y - panel.area.y) as usize;
                if index >= rows {
                    return;
                }
                self.focus = focus;
                self.click(index);
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Mouse in the graph: click selects, double click expands (a node) or shows the
    /// evidence (an edge), dragging moves a node or the view, the wheel zooms.
    fn graph_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = Focus::Tree;
                match self.explorer.press(x, y) {
                    Some(Click::Double(Sel::Node(_))) => self.explorer.toggle(),
                    Some(Click::Double(Sel::More(id))) => self.explorer.show_more(&id),
                    Some(Click::Double(Sel::Edge(_))) => self.show_evidence(),
                    _ => {}
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.explorer.drag_to(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.explorer.release(),
            MouseEventKind::ScrollUp => self.explorer.zoom_step(1, Some((x, y))),
            MouseEventKind::ScrollDown => self.explorer.zoom_step(-1, Some((x, y))),
            _ => return,
        }
        self.dirty = true;
    }

    /// Click on row `index` of the focused panel: select, then expand/collapse or open.
    pub fn click(&mut self, index: usize) {
        match self.focus {
            Focus::Tree => {
                let Some(row) = self.tree_rows.get(index).cloned() else {
                    return;
                };
                self.tree_panel.sel = index;
                self.tree_sel_key = Some(row.target.clone());
                let RowRef::Node(path) = &row.target else { return };
                match row.kind {
                    RowKind::Dir => {
                        if !self.expanded.remove(path) {
                            self.expanded.insert(path.clone());
                        }
                    }
                    RowKind::File => self.open_file(path),
                    _ => {}
                }
            }
            Focus::Code => {
                let Some(row) = self.code_rows.get(index).cloned() else {
                    return;
                };
                if let RowRef::Block(range) = row.target {
                    self.code_panel.sel = index;
                    self.code_sel_key = Some(row.target.clone());
                    self.expand_block(range, false);
                } else if !self.has_headers() {
                    self.code_panel.sel = index;
                    self.code_sel_key = Some(row.target.clone());
                } else if let Some(i) = (0..index)
                    .rev()
                    .find(|&i| self.code_rows[i].kind == RowKind::BlockHeader)
                {
                    // A code line selects its block: the nearest header above it.
                    self.code_panel.sel = i;
                    self.code_sel_key = Some(self.code_rows[i].target.clone());
                }
            }
        }
    }

    // ----- jobs -----

    /// Queues level-1 blocks of a file (ignored while the same job is pending).
    fn submit_level1(&mut self, path: &str, force: bool) {
        let ctx = Arc::clone(&self.ctx);
        let p = path.to_string();
        self.pool.submit(JobKey::Level1(p.clone()), move || {
            segment::level1(&ctx, &p, force).map(|_| ())
        });
    }

    fn submit_split(&mut self, path: &str, range: (u32, u32), force: bool) {
        let ctx = Arc::clone(&self.ctx);
        let p = path.to_string();
        self.pool.submit(JobKey::Split(p.clone(), range), move || {
            segment::split_block(&ctx, &p, range, force).map(|_| ())
        });
    }

    /// Queues a description of a file or folder with the current tree.
    fn submit_summary(&mut self, path: &str, is_dir: bool, force: bool) {
        let ctx = Arc::clone(&self.ctx);
        let tree = self.tree.clone();
        let paths = vec![path.to_string()];
        if is_dir {
            self.pool.submit(JobKey::DirSummary(path.to_string()), move || {
                init::describe_dirs(&ctx, &tree, &paths, force).map(|_| ())
            });
        } else {
            self.pool.submit(JobKey::FileSummary(path.to_string()), move || {
                init::describe_files(&ctx, &tree, &paths, force).map(|_| ())
            });
        }
    }

    /// `r`: in the tree regenerates the selected description; in the code panel the
    /// selected block's split (or the file's blocks).
    fn regenerate(&mut self) {
        if self.focus == Focus::Tree {
            let Some(path) = self.selected_tree_path().map(str::to_string) else {
                return;
            };
            let Some(i) = self.tree.get(&path) else { return };
            let node = self.tree.node(i);
            if node.is_dir {
                self.submit_summary(&path, true, true);
            } else if node.file.as_ref().is_some_and(|f| f.is_text()) {
                self.submit_summary(&path, false, true);
            }
            return;
        }
        let Some(open) = &self.open else { return };
        if open.problem.is_some() {
            return;
        }
        let path = open.path.clone();
        match self.selected_block() {
            Some(range)
                if self
                    .open_blocks()
                    .and_then(|b| b.find(range).map(|b| b.line_count()))
                    .unwrap_or(0)
                    > 1 =>
            {
                self.expanded_blocks.insert((path.clone(), range));
                self.submit_split(&path, range, true);
            }
            Some(_) => {}
            None => self.submit_level1(&path, true),
        }
    }

    // ----- editor -----

    /// `e`: asks the event loop to suspend the TUI and run the editor on the open file
    /// (at the selected block) or on the file selected in the tree.
    fn request_editor(&mut self) {
        let selected_line = match self.code_rows.get(self.code_panel.sel).map(|r| &r.target) {
            Some(RowRef::Line(nr)) => Some(*nr),
            _ => None,
        };
        let (path, line) = match (self.focus, &self.open) {
            (Focus::Tree, _) if self.view == View::Ontology => match &self.evidence {
                Some(ev) => (ev.current().file.clone(), ev.current().start_line),
                None => return,
            },
            (Focus::Code, Some(open)) => (
                open.path.clone(),
                self.selected_block().map(|r| r.0).or(selected_line).unwrap_or(1),
            ),
            _ => match self.selected_tree_path().and_then(|p| self.tree.get(p)) {
                Some(i) if !self.tree.node(i).is_dir => (self.tree.node(i).path.clone(), 1),
                _ => return,
            },
        };
        let abs = self.ctx.root.join(&path);
        let hash_before = rekon_core::hash::hash_file(&abs).ok();
        self.editor_request = Some(EditRequest {
            path,
            abs,
            line,
            hash_before,
        });
    }

    /// After the editor returns: reload, and when the file changed describe it and
    /// split it again right away.
    pub fn after_edit(&mut self, req: EditRequest, result: anyhow::Result<()>) {
        self.dirty = true;
        if let Err(e) = result {
            self.error(format!("editor: {e:#}"));
        }
        if let Some(i) = self.tree.get(&req.path) {
            self.tree
                .refresh_file(i, &self.ctx.root, &self.ctx.config, &self.excluder);
        }
        if self.open.as_ref().is_some_and(|o| o.path == req.path) {
            self.reload_open(&req.path);
        }
        let hash_after = rekon_core::hash::hash_file(&req.abs).ok();
        if hash_after.is_some() && hash_after != req.hash_before {
            self.submit_summary(&req.path, false, false);
            let fits = self.open.as_ref().is_some_and(|o| {
                o.path == req.path && o.problem.is_none() && o.lines.len() as u32 <= self.ctx.config.max_segment_lines
            });
            if fits {
                self.submit_level1(&req.path, false);
            }
        }
    }

    // ----- open file -----

    pub fn open_file(&mut self, path: &str) {
        self.evidence = None;
        self.code_panel = Panel {
            follow: true,
            area: self.code_panel.area,
            ..Default::default()
        };
        self.code_sel_key = None;
        self.reload_open(path);
        // Blocks are split when missing or outdated, right on opening.
        let fits = self.open.as_ref().is_some_and(|o| {
            o.problem.is_none() && !o.lines.is_empty() && o.lines.len() as u32 <= self.ctx.config.max_segment_lines
        });
        if fits && self.open_blocks().is_none() && self.excluder.matched(path).is_none() {
            self.submit_level1(path, false);
        }
    }

    /// Reads the file into the code panel (again), keeping the scroll position.
    fn reload_open(&mut self, path: &str) {
        let abs: PathBuf = self.ctx.root.join(path);
        let meta = std::fs::metadata(&abs).ok();
        let size = meta.as_ref().map_or(0, |m| m.len());
        let mtime = meta.and_then(|m| m.modified().ok());
        let mut open = OpenFile {
            path: path.to_string(),
            hash: None,
            size,
            mtime,
            lines: Vec::new(),
            problem: None,
            highlighted: None,
        };
        if size > MAX_SHOWN_BYTES {
            open.problem = Some(text::too_large(size));
        } else {
            match std::fs::read(&abs) {
                Ok(bytes) if bytes[..bytes.len().min(8000)].contains(&0) => {
                    open.problem = Some(text::binary(size));
                }
                Ok(bytes) => {
                    let hash = rekon_core::hash::hash_bytes(&bytes);
                    let content = String::from_utf8_lossy(&bytes).into_owned();
                    open.lines = content.lines().map(str::to_string).collect();
                    open.highlighted = self.hl_cache.get(path, &hash);
                    if open.highlighted.is_none() {
                        let tx = self.bg_tx.clone();
                        let (p, h) = (path.to_string(), hash.clone());
                        std::thread::spawn(move || {
                            let lines = highlight::highlight(&p, &content);
                            let _ = tx.send(Bg::Highlighted {
                                path: p,
                                hash: h,
                                lines,
                            });
                        });
                    }
                    open.hash = Some(hash);
                }
                Err(e) => open.problem = Some(e.to_string()),
            }
        }
        self.notes.forget_file(path);
        self.open = Some(open);
        self.dirty = true;
    }

    /// Full description of the selected item, for the footer.
    pub fn selected_description(&mut self) -> String {
        if self.view == View::Ontology && self.focus == Focus::Tree {
            return self.graph_description();
        }
        if self.focus == Focus::Code
            && let Some(open) = &self.open
        {
            let path = open.path.clone();
            if let Some(range) = self.selected_block()
                && let Some(b) = self.open_blocks().and_then(|b| b.find(range).cloned())
            {
                return format!("{path}:{}–{} — {}", range.0, range.1, b.summary);
            }
            let summary = self
                .notes
                .file(&self.ctx, &path)
                .summary
                .as_ref()
                .map(|s| s.text.clone());
            return format!("{path} — {}", summary.unwrap_or_else(|| text::NO_DESCRIPTION.into()));
        }
        let Some(path) = self.selected_tree_path().map(str::to_string) else {
            return String::new();
        };
        let Some(i) = self.tree.get(&path) else {
            return String::new();
        };
        let desc = match self.desc(i) {
            Desc::Fresh(t) | Desc::Static(t) => t,
            Desc::Stale(t) => format!("⚠ {t}"),
            Desc::Missing => text::NO_DESCRIPTION.into(),
            Desc::Pending => text::PENDING.into(),
        };
        let shown = if self.tree.node(i).is_dir {
            format!("{path}/")
        } else {
            path
        };
        format!("{shown} — {desc}")
    }

    fn graph_description(&self) -> String {
        let (Some(g), Some(sel)) = (&self.explorer.graph, &self.explorer.sel) else {
            return String::new();
        };
        match sel {
            Sel::Node(id) => g.node(id).map_or_else(String::new, |n| {
                let description = n.description.as_deref().unwrap_or(text::NO_DESCRIPTION);
                text::node_line(&n.kind, &n.name, description, n.confidence, n.evidence.len(), &n.id)
            }),
            Sel::Edge(id) => g.edge(id).map_or_else(String::new, |e| {
                text::edge_line(&edge_title(g, e), e.confidence, e.evidence.len())
            }),
            Sel::More(id) => {
                let count = self.explorer.more.get(id).map_or(0, |m| m.count);
                text::more_line(count, g.node(id).map_or("", |n| n.name.as_str()))
            }
            Sel::Group(id, label) => {
                let count = self
                    .explorer
                    .groups
                    .get(&(id.clone(), label.clone()))
                    .map_or(0, |m| m.count);
                text::group_line(label, count, g.node(id).map_or("", |n| n.name.as_str()))
            }
        }
    }

    pub fn jobs_running(&self) -> usize {
        self.pool.pending_count() + usize::from(self.init.is_some()) + usize::from(self.ontology.is_some())
    }
}

/// `source —relation→ target` with node names.
fn edge_title(g: &GraphIndex, e: &Edge) -> String {
    let name = |id: &str| g.node(id).map_or_else(|| id.to_string(), |n| n.name.clone());
    format!("{} —{}→ {}", name(&e.source), e.relation, name(&e.target))
}

fn to_desc(text: Option<String>, f: Freshness, pending: bool) -> Desc {
    match (f, text) {
        (Freshness::Fresh, Some(t)) => Desc::Fresh(t),
        (Freshness::Stale, Some(t)) => Desc::Stale(t),
        _ if pending => Desc::Pending,
        _ => Desc::Missing,
    }
}

/// Index of the row with `key`, else the old index clamped to the list.
fn restore(rows: &[Row], key: &Option<RowRef>, old: usize) -> usize {
    key.as_ref()
        .and_then(|k| rows.iter().position(|r| &r.target == k))
        .unwrap_or_else(|| old.min(rows.len().saturating_sub(1)))
}
