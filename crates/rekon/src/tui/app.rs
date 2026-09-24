//! TUI state and event handling. Background threads (scan, init, highlighting)
//! report through one channel; the UI thread owns all state.

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
use rekon_core::model::{DirNote, FileNote, Freshness, ProjectNote, freshness};
use rekon_core::scan::{Excluder, Tree};

use super::highlight::{self, Lines};
use super::rows::{self, Desc, Row, RowKind, RowRef};

const TICK: Duration = Duration::from_secs(2);
const RESCAN: Duration = Duration::from_secs(15);
/// Files larger than this are not shown in the code panel.
const MAX_SHOWN_BYTES: u64 = 20_000_000;

pub enum Bg {
    Analyzed(Tree),
    InitProgress(Progress),
    InitDone(Result<String, String>),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popup {
    Overview(u16),
    Help,
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

    /// Adjusts the offset so the selection is visible (when following).
    pub fn scroll_to_selection(&mut self, rows: usize) {
        let h = self.height().max(1);
        if self.follow {
            if self.sel < self.offset {
                self.offset = self.sel;
            } else if self.sel >= self.offset + h {
                self.offset = self.sel + 1 - h;
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

    pub fn clear(&mut self) {
        *self = Notes::default();
    }
}

pub struct App {
    pub ctx: Arc<Ctx>,
    pub tree: Tree,
    pub notes: Notes,
    pub expanded: HashSet<String>,
    pub focus: Focus,
    pub wide: bool,
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
    hl_cache: highlight::Cache,
    bg_tx: Sender<Bg>,
    bg_rx: Receiver<Bg>,
    excluder: Excluder,
    last_tick: Instant,
    last_rescan: Instant,
    rescanning: bool,
    pub quit: bool,
    pub dirty: bool,
}

impl App {
    /// Lists the repository (fast) and starts hashing and, without a map, init in the background.
    pub fn new(ctx: Arc<Ctx>) -> anyhow::Result<Self> {
        let (tree, _) = Tree::list(&ctx.root)?;
        let (bg_tx, bg_rx) = channel();
        let excluder = Excluder::new(&ctx.config.exclude)?;
        let mut app = Self {
            ctx,
            tree,
            notes: Notes::default(),
            expanded: HashSet::new(),
            focus: Focus::Tree,
            wide: false,
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
            hl_cache: highlight::Cache::default(),
            bg_tx,
            bg_rx,
            excluder,
            last_tick: Instant::now(),
            last_rescan: Instant::now(),
            rescanning: false,
            quit: false,
            dirty: true,
        };
        app.start_rescan();
        if init::needs_init(&app.ctx) {
            app.start_init();
        }
        Ok(app)
    }

    /// Test constructor: a given tree, nothing in the background.
    #[cfg(test)]
    pub fn for_tests(ctx: Arc<Ctx>, tree: Tree) -> Self {
        let (bg_tx, bg_rx) = channel();
        let excluder = Excluder::new(&ctx.config.exclude).unwrap();
        Self {
            ctx,
            tree,
            notes: Notes::default(),
            expanded: HashSet::new(),
            focus: Focus::Tree,
            wide: false,
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
            hl_cache: highlight::Cache::default(),
            bg_tx,
            bg_rx,
            excluder,
            last_tick: Instant::now(),
            last_rescan: Instant::now(),
            rescanning: false,
            quit: false,
            dirty: true,
        }
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

    pub fn error(&mut self, message: String) {
        self.errors += 1;
        self.last_error = Some(message);
        self.dirty = true;
    }

    /// Handles background messages and periodic refresh. Call often.
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
        if self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            self.tick();
        }
        if self.last_rescan.elapsed() >= RESCAN {
            self.last_rescan = Instant::now();
            self.start_rescan();
        }
    }

    /// Every 2 s: re-stat visible files and the open file, reload changed notes.
    fn tick(&mut self) {
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
        let init_running = self.init.is_some();
        if node.is_dir {
            let key = self.tree.dir_key(i);
            let summary = self.notes.dir(&self.ctx, &path).summary.clone();
            return to_desc(
                summary.as_ref().map(|s| s.text.clone()),
                freshness(summary.as_ref(), Some(&key)),
                init_running,
            );
        }
        let info = node.file.clone().unwrap_or_default();
        if let Some(label) = info.label() {
            return Desc::Static(label);
        }
        let summary = self.notes.file(&self.ctx, &path).summary.clone();
        to_desc(
            summary.as_ref().map(|s| s.text.clone()),
            freshness(summary.as_ref(), info.hash.as_deref()),
            init_running,
        )
    }

    pub fn project_summary(&mut self) -> Option<String> {
        self.notes.project(&self.ctx).summary.as_ref().map(|s| s.text.clone())
    }

    pub fn project_overview(&mut self) -> Option<String> {
        self.notes.project(&self.ctx).overview.clone()
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
        self.code_panel.sel = restore(&self.code_rows, &self.code_sel_key, self.code_panel.sel);
    }

    fn build_code_rows(&self, _width: usize) -> Vec<Row> {
        let Some(open) = &self.open else { return Vec::new() };
        let hl = open.highlighted.as_ref();
        open.lines
            .iter()
            .enumerate()
            .map(|(i, raw)| rows::code_row(i as u32 + 1, 0, Vec::new(), hl.and_then(|h| h.get(i)), raw))
            .collect()
    }

    pub fn selected_tree_path(&self) -> Option<&str> {
        match &self.tree_rows.get(self.tree_panel.sel)?.target {
            RowRef::Node(p) => Some(p),
            _ => None,
        }
    }

    // ----- input -----

    pub fn on_key(&mut self, key: KeyEvent) {
        self.dirty = true;
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
                _ => {}
            }
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

    fn move_sel(&mut self, delta: isize) {
        let (panel, rows, key) = match self.focus {
            Focus::Tree => (&mut self.tree_panel, &self.tree_rows, &mut self.tree_sel_key),
            Focus::Code => (&mut self.code_panel, &self.code_rows, &mut self.code_sel_key),
        };
        if rows.is_empty() {
            return;
        }
        let target = (panel.sel as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        panel.sel = target;
        panel.follow = true;
        *key = Some(rows[target].target.clone());
    }

    fn activate(&mut self, focus_code: bool) {
        if self.focus == Focus::Code {
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
            self.focus = Focus::Tree;
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
                let Some(row) = self.code_rows.get(index) else { return };
                self.code_panel.sel = index;
                self.code_sel_key = Some(row.target.clone());
            }
        }
    }

    // ----- open file -----

    pub fn open_file(&mut self, path: &str) {
        self.code_panel = Panel {
            follow: true,
            area: self.code_panel.area,
            ..Default::default()
        };
        self.code_sel_key = None;
        self.reload_open(path);
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
            open.problem = Some(rekon_core::text::too_large(size));
        } else {
            match std::fs::read(&abs) {
                Ok(bytes) if bytes[..bytes.len().min(8000)].contains(&0) => {
                    open.problem = Some(rekon_core::text::binary(size));
                }
                Ok(bytes) => {
                    let hash = rekon_core::hash::hash_bytes(&bytes);
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    open.lines = text.lines().map(str::to_string).collect();
                    open.highlighted = self.hl_cache.get(path, &hash);
                    if open.highlighted.is_none() {
                        let tx = self.bg_tx.clone();
                        let (p, h) = (path.to_string(), hash.clone());
                        std::thread::spawn(move || {
                            let lines = highlight::highlight(&p, &text);
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
        self.open = Some(open);
        self.dirty = true;
    }

    /// Full description of the selected item, for the footer.
    pub fn selected_description(&mut self) -> String {
        if self.focus == Focus::Code
            && let Some(open) = &self.open
        {
            let path = open.path.clone();
            let summary = self
                .notes
                .file(&self.ctx, &path)
                .summary
                .as_ref()
                .map(|s| s.text.clone());
            return format!(
                "{path} — {}",
                summary.unwrap_or_else(|| rekon_core::text::NO_DESCRIPTION.into())
            );
        }
        let Some(path) = self.selected_tree_path().map(str::to_string) else {
            return String::new();
        };
        let Some(i) = self.tree.get(&path) else {
            return String::new();
        };
        let text = match self.desc(i) {
            Desc::Fresh(t) | Desc::Static(t) => t,
            Desc::Stale(t) => format!("⚠ {t}"),
            Desc::Missing => rekon_core::text::NO_DESCRIPTION.into(),
            Desc::Pending => rekon_core::text::PENDING.into(),
        };
        let shown = if self.tree.node(i).is_dir {
            format!("{path}/")
        } else {
            path
        };
        format!("{shown} — {text}")
    }

    pub fn jobs_running(&self) -> usize {
        usize::from(self.init.is_some())
    }
}

fn to_desc(text: Option<String>, f: Freshness, init_running: bool) -> Desc {
    match (f, text) {
        (Freshness::Fresh, Some(t)) => Desc::Fresh(t),
        (Freshness::Stale, Some(t)) => Desc::Stale(t),
        _ if init_running => Desc::Pending,
        _ => Desc::Missing,
    }
}

/// Index of the row with `key`, else the old index clamped to the list.
fn restore(rows: &[Row], key: &Option<RowRef>, old: usize) -> usize {
    key.as_ref()
        .and_then(|k| rows.iter().position(|r| &r.target == k))
        .unwrap_or_else(|| old.min(rows.len().saturating_sub(1)))
}
