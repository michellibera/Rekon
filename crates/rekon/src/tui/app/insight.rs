//! Insight layer of the TUI: extra lenses of the left panel (decisions, cards, criteria,
//! questions), the inspector of the right panel (relations, why, contract, example run,
//! compare), history and pins, and the actions that go with them. State lives in
//! [`Insight`]; everything works on the same "subject": the element the person is looking at.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rekon_core::cards::{self, Card};
use rekon_core::contracts;
use rekon_core::criteria::{self, Level};
use rekon_core::decisions::{self, Decision};
use rekon_core::model::{Evidence, Origin};
use rekon_core::questions::{self, Question};
use rekon_core::{element, relations, trace};

use super::super::editor::{EditRequest, Followup};
use super::super::rows::{DIM, Row, RowKind, RowRef, WARN, fit};
use super::{App, EvidenceNav, Focus, Popup, View};
use rekon_core::jobs::JobKey;

/// Age after which the relations index is rebuilt when it is needed.
const INDEX_TTL: Duration = Duration::from_secs(20);
const MAX_PINS: usize = 3;
const MAX_HISTORY: usize = 100;

/// Tabs of the right panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detail {
    #[default]
    Code,
    Relations,
    Why,
    Contract,
    Trace,
    Compare,
}

impl Detail {
    pub const ALL: [Detail; 6] = [
        Detail::Code,
        Detail::Relations,
        Detail::Why,
        Detail::Contract,
        Detail::Trace,
        Detail::Compare,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Detail::Code => "Code",
            Detail::Relations => "Relations",
            Detail::Why => "Why",
            Detail::Contract => "Contract",
            Detail::Trace => "Example",
            Detail::Compare => "Compare",
        }
    }

    fn step(self, delta: isize) -> Detail {
        let i = Detail::ALL.iter().position(|d| *d == self).unwrap_or(0) as isize;
        Detail::ALL[(i + delta).rem_euclid(Detail::ALL.len() as isize) as usize]
    }
}

/// The element (or file) the person is looking at.
#[derive(Clone, Debug, PartialEq)]
pub struct Subject {
    /// Element id, or the path for a whole file.
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start: u32,
    pub end: u32,
    pub is_file: bool,
}

impl Subject {
    fn of_file(path: &str, lines: u32) -> Self {
        Subject {
            id: path.to_string(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            kind: "file".into(),
            start: 1,
            end: lines.max(1),
            is_file: true,
        }
    }

    fn of_element(e: &element::Element) -> Self {
        Subject {
            id: e.id.clone(),
            path: e.path.clone(),
            name: e.name.clone(),
            kind: e.kind.clone(),
            start: e.start,
            end: e.end,
            is_file: e.kind == "file",
        }
    }
}

/// Where Enter takes the person from a line of the inspector.
#[derive(Clone, Debug, PartialEq)]
pub enum Nav {
    Element(String),
    Lines(String, u32, u32),
    Decision(String),
    Card(String),
    Task(String),
}

#[derive(Clone, Debug)]
pub struct InspItem {
    pub line: Line<'static>,
    pub nav: Option<Nav>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Question,
    Expected(String),
    TraceInput(String),
    NewTask,
    Criterion(String),
    TestPattern(String, String),
}

impl InputKind {
    pub fn title(&self) -> &'static str {
        match self {
            InputKind::Question => "Pin a question",
            InputKind::Expected(_) => "Result you expect for this example",
            InputKind::TraceInput(_) => "Example input to walk through",
            InputKind::NewTask => "New task",
            InputKind::Criterion(_) => "New criterion",
            InputKind::TestPattern(..) => "Part of the name of the test that checks it",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
}

#[derive(Default)]
pub struct Insight {
    pub detail: Detail,
    /// Selected selectable line of the inspector, and the first visible line.
    pub cursor: usize,
    pub scroll: usize,
    pub items: Vec<InspItem>,
    pub subject: Option<Subject>,
    subject_key: String,
    pub history: Vec<String>,
    pub hist_pos: usize,
    navigating: bool,
    pub pins: Vec<String>,
    pub decisions: Vec<Decision>,
    pub cards: Vec<Card>,
    pub tasks: Vec<criteria::Task>,
    pub questions: Vec<Question>,
    rel: Option<(Instant, Arc<relations::Index>)>,
    pub input: Option<Input>,
    pub text_title: String,
    pub text_body: String,
    /// Tabs of the right panel in the last frame, for clicks.
    pub tabs: Vec<(Rect, Detail)>,
    /// Inner area of the inspector in the last frame.
    pub area: Rect,
}

fn badge(o: Origin) -> Span<'static> {
    let color = match o {
        Origin::Human => Color::Green,
        Origin::Ai => Color::Magenta,
        Origin::Static => Color::Gray,
        Origin::Recorded => Color::Blue,
        Origin::TestResult => Color::Cyan,
        Origin::Predicted => Color::Yellow,
    };
    Span::styled(format!("[{}]", o.label()), Style::new().fg(color))
}

fn bold(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().add_modifier(Modifier::BOLD))
}

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), DIM)
}

fn plain(text: impl Into<String>) -> Span<'static> {
    Span::raw(text.into())
}

fn line(spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans)
}

fn text_item(spans: Vec<Span<'static>>) -> InspItem {
    InspItem {
        line: line(spans),
        nav: None,
    }
}

fn blank() -> InspItem {
    text_item(vec![])
}

fn level_span(tag: &str, l: Level) -> Span<'static> {
    let (glyph, color) = match l {
        Level::None => ("-", Color::DarkGray),
        Level::Current => ("ok", Color::Green),
        Level::Stale => ("stale", Color::Yellow),
        Level::Failing => ("FAIL", Color::Red),
        Level::Open => ("open", Color::Cyan),
    };
    Span::styled(format!("{tag}:{glyph}"), Style::new().fg(color))
}

/// Wraps `text` to `width` columns (words), for the inspector.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `label: text` wrapped, continuation lines indented.
fn field(items: &mut Vec<InspItem>, indent: usize, label: &str, text: &str, width: usize, style: Style) {
    if text.trim().is_empty() {
        return;
    }
    let pad = " ".repeat(indent);
    let head = format!("{label}: ");
    let avail = width.saturating_sub(indent + head.chars().count());
    for (i, l) in wrap(text, avail).into_iter().enumerate() {
        let lead = if i == 0 {
            head.clone()
        } else {
            " ".repeat(head.chars().count())
        };
        items.push(text_item(vec![plain(pad.clone()), dim(lead), Span::styled(l, style)]));
    }
}

impl App {
    // ----- data -----

    /// Reloads decisions, cards, criteria and questions from `.rekon/`.
    pub fn refresh_insight(&mut self) {
        let ctx = Arc::clone(&self.ctx);
        self.ins.decisions = decisions::list(&ctx);
        self.ins.cards = cards::list(&ctx);
        self.ins.tasks = criteria::list(&ctx);
        self.ins.questions = questions::list(&ctx);
        self.dirty = true;
    }

    fn is_lens(&self) -> bool {
        matches!(self.view, View::Decisions | View::Cards | View::Criteria | View::Ask)
    }

    /// Relations index, rebuilt when it is older than [`INDEX_TTL`].
    pub fn relations(&mut self) -> Option<Arc<relations::Index>> {
        if self.ins.rel.as_ref().is_none_or(|(t, _)| t.elapsed() > INDEX_TTL) {
            match relations::build(&self.ctx) {
                Ok(idx) => self.ins.rel = Some((Instant::now(), Arc::new(idx))),
                Err(e) => {
                    self.error(format!("relations: {e:#}"));
                    return None;
                }
            }
        }
        self.ins.rel.as_ref().map(|(_, i)| Arc::clone(i))
    }

    // ----- subject, history, pins -----

    /// Element under the cursor: the evidence shown, else the code line selected in the code
    /// panel, else the file selected in the tree.
    fn compute_subject(&mut self) -> Option<Subject> {
        let root = self.ctx.root.clone();
        if let Some(ev) = &self.evidence
            && let Some(open) = &self.open
        {
            let e = ev.current();
            if open.path == e.file
                && let Some(el) = element::at_line(&root, &e.file, e.start_line)
            {
                return Some(Subject::of_element(&el));
            }
        }
        if self.focus == Focus::Code
            && let Some(open) = &self.open
        {
            let line = match self.code_rows.get(self.code_panel.sel).map(|r| &r.target) {
                Some(RowRef::Line(n)) => Some(*n),
                Some(RowRef::Block((a, _))) => Some(*a),
                _ => None,
            };
            if let Some(n) = line
                && let Some(el) = element::at_line(&root, &open.path, n)
            {
                return Some(Subject::of_element(&el));
            }
            return Some(Subject::of_file(&open.path, open.lines.len() as u32));
        }
        if self.view == View::Tree {
            let path = self.selected_tree_path()?.to_string();
            let i = self.tree.get(&path)?;
            if self.tree.node(i).is_dir {
                return None;
            }
            let lines = self.tree.node(i).file.as_ref().and_then(|f| f.lines).unwrap_or(0);
            return Some(Subject::of_file(&path, lines));
        }
        None
    }

    /// Recomputes the subject when what it depends on changed, and records the visit.
    pub fn sync_subject(&mut self) {
        let key = format!(
            "{:?}|{:?}|{:?}|{:?}|{}|{}",
            self.view,
            self.tree_sel_key,
            self.evidence.as_ref().map(|e| (e.sel.clone(), e.index)),
            self.focus,
            self.code_panel.sel,
            self.open.as_ref().map_or("", |o| o.path.as_str()),
        );
        if key == self.ins.subject_key {
            return;
        }
        self.ins.subject_key = key;
        let subject = self.compute_subject();
        if let Some(s) = &subject
            && !self.ins.navigating
            && self.ins.history.get(self.ins.hist_pos) != Some(&s.id)
        {
            self.ins.history.truncate(self.ins.hist_pos + 1);
            self.ins.history.push(s.id.clone());
            if self.ins.history.len() > MAX_HISTORY {
                self.ins.history.remove(0);
            }
            self.ins.hist_pos = self.ins.history.len() - 1;
        }
        if subject != self.ins.subject {
            self.ins.cursor = 0;
            self.ins.scroll = 0;
        }
        self.ins.subject = subject;
    }

    /// `<` / `>`: back and forward through the visited elements.
    fn history_step(&mut self, delta: isize) {
        let target = self.ins.hist_pos as isize + delta;
        if target < 0 || target as usize >= self.ins.history.len() {
            return;
        }
        let id = self.ins.history[target as usize].clone();
        self.ins.hist_pos = target as usize;
        self.ins.navigating = true;
        self.goto_element(&id);
        self.ins.navigating = false;
    }

    /// `P`: pins the subject for comparison (up to three, the oldest drops off).
    fn toggle_pin(&mut self) {
        let Some(s) = self.ins.subject.clone() else { return };
        if let Some(i) = self.ins.pins.iter().position(|p| *p == s.id) {
            self.ins.pins.remove(i);
            self.notice = Some(format!("unpinned {}", s.name));
        } else {
            if self.ins.pins.len() == MAX_PINS {
                self.ins.pins.remove(0);
            }
            self.ins.pins.push(s.id);
            self.notice = Some(format!(
                "pinned {} ({}/{MAX_PINS}): view Compare with v",
                s.name,
                self.ins.pins.len()
            ));
        }
    }

    /// Shows an element (or file) in the code panel, in the tree view.
    pub fn goto_element(&mut self, id: &str) {
        let path = id.split_once("::").map_or(id, |(p, _)| p).to_string();
        if !self.ctx.root.join(&path).is_file() {
            self.notice = Some(format!("{id} no longer exists"));
            return;
        }
        let el = element::load(&self.ctx.root, id);
        if self.view != View::Tree {
            self.set_view(View::Tree);
        }
        // Open the folders above the file so it can be selected.
        let mut prefix = String::new();
        for part in path.split('/').take(path.matches('/').count()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            self.expanded.insert(prefix.clone());
        }
        let width = self.tree_panel.area.width as usize;
        self.rebuild(width, self.code_panel.area.width as usize);
        self.select_tree_path(&path);
        match el.filter(|e| e.kind != "file") {
            Some(e) => self.show_lines(&path, e.start, e.end, &e.name),
            None => self.open_file(&path),
        }
        self.dirty = true;
    }

    /// Puts the lines in the code panel, highlighted, without splitting the file into blocks.
    pub fn show_lines(&mut self, path: &str, start: u32, end: u32, title: &str) {
        self.evidence = Some(EvidenceNav {
            sel: format!("{path}:{start}"),
            title: title.to_string(),
            items: vec![Evidence {
                file: path.to_string(),
                start_line: start,
                end_line: end,
                symbol: None,
                reason: String::new(),
            }],
            index: 0,
        });
        self.open_evidence();
    }

    // ----- lenses (left panel) -----

    pub fn lens_rows(&self, width: usize) -> Vec<Row> {
        let row = |key: String, kind: RowKind, spans: Vec<Span<'static>>| Row {
            depth: 0,
            kind,
            target: RowRef::Node(key),
            line: Line::from(spans),
        };
        let mut out = Vec::new();
        match self.view {
            View::Decisions => {
                for d in &self.ins.decisions {
                    let (glyph, style) = match d.status {
                        decisions::Status::Accepted => ("●", Style::new().fg(Color::Green)),
                        decisions::Status::Proposed => ("?", WARN),
                        decisions::Status::Superseded => ("○", DIM),
                    };
                    let title = fit(&d.title, width.saturating_sub(24));
                    out.push(row(
                        d.id.clone(),
                        RowKind::File,
                        vec![
                            Span::styled(glyph, style),
                            plain(" "),
                            plain(title),
                            plain("  "),
                            badge(d.origin),
                            dim(format!(" {}", d.status.label())),
                        ],
                    ));
                }
            }
            View::Cards => {
                for c in &self.ins.cards {
                    let (glyph, style) = match c.status {
                        cards::Status::Proposed => ("◇", WARN),
                        cards::Status::Implemented => ("◆", Style::new().fg(Color::Cyan)),
                        cards::Status::Verified => ("✓", Style::new().fg(Color::Green)),
                    };
                    let title = fit(&c.title, width.saturating_sub(24));
                    out.push(row(
                        c.id.clone(),
                        RowKind::File,
                        vec![
                            Span::styled(glyph, style),
                            plain(" "),
                            plain(title),
                            plain("  "),
                            badge(c.origin),
                            dim(format!(" {}", c.status.label())),
                        ],
                    ));
                }
            }
            View::Criteria => {
                for t in &self.ins.tasks {
                    let done = t
                        .items
                        .iter()
                        .filter(|c| {
                            let s = criteria::status(&self.ctx, c);
                            s.implementation == Level::Current
                                && s.verification == Level::Current
                                && s.review == Level::Current
                        })
                        .count();
                    out.push(row(
                        t.id.clone(),
                        RowKind::Dir,
                        vec![
                            bold(fit(&t.title, width.saturating_sub(12))),
                            dim(format!("  {done}/{} met", t.items.len())),
                        ],
                    ));
                    for c in &t.items {
                        let s = criteria::status(&self.ctx, c);
                        out.push(row(
                            format!("{}/{}", t.id, c.id),
                            RowKind::File,
                            vec![
                                plain("  "),
                                level_span("I", s.implementation),
                                plain(" "),
                                level_span("T", s.verification),
                                plain(" "),
                                level_span("R", s.review),
                                plain("  "),
                                plain(fit(&c.text, width.saturating_sub(30))),
                            ],
                        ));
                    }
                }
            }
            View::Ask => {
                for q in &self.ins.questions {
                    let meta = q.answer.as_ref().map_or("not answered".to_string(), |a| {
                        format!("looked at {}/{} files", a.examined, a.total)
                    });
                    out.push(row(
                        q.id.clone(),
                        RowKind::Dir,
                        vec![
                            bold("? "),
                            bold(fit(&q.text, width.saturating_sub(30))),
                            dim(format!("  {meta}")),
                        ],
                    ));
                    let Some(a) = &q.answer else { continue };
                    for (n, i) in a.items.iter().enumerate() {
                        out.push(row(
                            format!("{}#{n}", q.id),
                            RowKind::File,
                            vec![
                                plain("  "),
                                plain(fit(&i.name, width / 3)),
                                plain("  "),
                                dim(fit(&i.role, width.saturating_sub(width / 3 + 6))),
                            ],
                        ));
                    }
                    for (n, o) in a.omitted.iter().enumerate() {
                        let style = DIM.add_modifier(Modifier::ITALIC);
                        let why = if o.deliberate {
                            o.reason.clone()
                        } else {
                            format!("{} (not looked at)", o.reason)
                        };
                        out.push(row(
                            format!("{}!{n}", q.id),
                            RowKind::Spacer,
                            vec![
                                plain("  "),
                                Span::styled(
                                    fit(&format!("left out: {} — {why}", o.path), width.saturating_sub(4)),
                                    style,
                                ),
                            ],
                        ));
                    }
                }
            }
            _ => {}
        }
        if out.is_empty() {
            let msg = match self.view {
                View::Decisions => {
                    "No decisions yet. n: write one; the Why tab (v) of any element can also reconstruct one with the model."
                }
                View::Cards => "No cards yet. N: describe the changes of the working tree; n: write a specification.",
                View::Criteria => "No tasks yet. n: new task; c in the Cards lens makes a task from a card.",
                _ => "No questions yet. n: pin a question; the answer lists the elements and what was left out.",
            };
            for l in wrap(msg, width.saturating_sub(2)) {
                out.push(row(String::new(), RowKind::Spacer, vec![dim(l)]));
            }
        }
        out
    }

    /// Key of the selected row of a lens.
    fn lens_key_selected(&self) -> Option<String> {
        match &self.tree_rows.get(self.tree_panel.sel)?.target {
            RowRef::Node(k) if !k.is_empty() => Some(k.clone()),
            _ => None,
        }
    }

    /// Element and lines that the selected row of a lens is about.
    fn lens_target(&self) -> Option<(String, u32, u32, String)> {
        let key = self.lens_key_selected()?;
        let root = &self.ctx.root;
        let of_id = |id: &str| -> Option<(String, u32, u32, String)> {
            let path = id.split_once("::").map_or(id, |(p, _)| p);
            match element::load(root, id) {
                Some(e) if e.kind != "file" => Some((e.path, e.start, e.end, e.name)),
                _ => {
                    let n = std::fs::read_to_string(root.join(path)).ok()?.lines().count() as u32;
                    Some((path.to_string(), 1, n.max(1), path.to_string()))
                }
            }
        };
        match self.view {
            View::Decisions => self
                .ins
                .decisions
                .iter()
                .find(|d| d.id == key)?
                .elements
                .iter()
                .find_map(|e| of_id(e)),
            View::Cards => {
                let c = self.ins.cards.iter().find(|c| c.id == key)?;
                c.basis
                    .first()
                    .map(|b| (b.file.clone(), b.start_line, b.end_line, c.title.clone()))
                    .or_else(|| c.elements.iter().find_map(|e| of_id(e)))
            }
            View::Criteria => {
                let (task, crit) = key.split_once('/')?;
                let c = self
                    .ins
                    .tasks
                    .iter()
                    .find(|t| t.id == task)?
                    .items
                    .iter()
                    .find(|c| c.id == crit)?;
                c.implementation.keys().find_map(|e| of_id(e))
            }
            View::Ask => {
                let (q, n) = key.split_once('#')?;
                let a = self.ins.questions.iter().find(|x| x.id == q)?.answer.as_ref()?;
                let i = a.items.get(n.parse::<usize>().ok()?)?;
                Some((i.path.clone(), i.start, i.end, i.name.clone()))
            }
            _ => None,
        }
    }

    /// Shows what the selected lens row is about in the code panel.
    pub fn show_lens_selection(&mut self) {
        match self.lens_target() {
            Some((path, a, b, name)) => self.show_lines(&path, a, b, &name),
            None => self.evidence = None,
        }
    }

    /// Keys of the lens views (left panel focused). Returns whether the key was used.
    pub fn lens_key(&mut self, key: KeyEvent) -> bool {
        if !self.is_lens() {
            return false;
        }
        let selected = self.lens_key_selected();
        let KeyCode::Char(c) = key.code else { return false };
        match (self.view, c) {
            (View::Decisions, 'n') => self.edit_decision(None),
            (View::Decisions, 's') => {
                if let Some(id) = selected {
                    self.edit_decision(Some(id));
                }
            }
            (View::Decisions, 'a') => {
                if let Some(id) = selected {
                    match decisions::accept(&self.ctx, &id) {
                        Ok(d) => self.notice = Some(format!("accepted {}", d.title)),
                        Err(e) => self.error(format!("{e:#}")),
                    }
                    self.refresh_insight();
                }
            }
            (View::Decisions, 'C') => self.spawn("check decisions against the working tree", |ctx| {
                let diff = decisions::git_diff(ctx, None)?;
                let findings = decisions::check(ctx, &diff)?;
                let mut out = String::new();
                if findings.is_empty() {
                    out.push_str("No decision is affected by the changes of the working tree.");
                }
                for f in findings {
                    out.push_str(&format!(
                        "{} — {}\n  {}\n  Revisit if: {}\n\n",
                        f.decision.id, f.decision.title, f.why, f.decision.revisit_if
                    ));
                }
                Ok(Some(("Decisions at risk".to_string(), out)))
            }),
            (View::Cards, 'n') => self.edit_card(),
            (View::Cards, 'N') => self.spawn("describe the working tree as cards", |ctx| {
                cards::from_diff(ctx, None)?;
                Ok(None)
            }),
            (View::Cards, 's') => {
                if let Some(id) = selected
                    && let Some(card) = self.ins.cards.iter().find(|c| c.id == id)
                {
                    let next = match card.status {
                        cards::Status::Proposed => cards::Status::Implemented,
                        cards::Status::Implemented => cards::Status::Verified,
                        cards::Status::Verified => cards::Status::Proposed,
                    };
                    if let Err(e) = cards::set_status(&self.ctx, &id, next) {
                        self.error(format!("{e:#}"));
                    }
                    self.refresh_insight();
                }
            }
            (View::Cards, 'b') => {
                if let Some(id) = selected
                    && let Some(card) = self.ins.cards.iter().find(|c| c.id == id)
                {
                    self.ins.text_title = format!("Brief for an agent: {}", card.title);
                    self.ins.text_body = cards::brief(card);
                    self.popup = Some(Popup::Text(0));
                }
            }
            (View::Cards, 'c') => {
                if let Some(id) = selected
                    && let Some(card) = self.ins.cards.iter().find(|c| c.id == id).cloned()
                {
                    match criteria::from_card(&self.ctx, &card) {
                        Ok(t) => self.notice = Some(format!("task {} with {} criteria", t.id, t.items.len())),
                        Err(e) => self.error(format!("{e:#}")),
                    }
                    self.refresh_insight();
                }
            }
            (View::Criteria, 'n') => self.open_input(InputKind::NewTask),
            (View::Criteria, 'a') => {
                if let Some(task) = selected
                    .as_deref()
                    .map(|k| k.split('/').next().unwrap_or(k).to_string())
                {
                    self.open_input(InputKind::Criterion(task));
                }
            }
            (View::Criteria, 't') => {
                if let Some((task, crit)) = selected.as_deref().and_then(|k| k.split_once('/')) {
                    self.open_input(InputKind::TestPattern(task.to_string(), crit.to_string()));
                }
            }
            (View::Criteria, 'l') => self.criteria_action(selected, |ctx, t, c, subject| {
                let id = subject
                    .ok_or_else(|| anyhow::anyhow!("pin (P) or look at the element that implements it first"))?;
                criteria::link_implementation(ctx, t, c, &id).map(|_| ())
            }),
            (View::Criteria, 'T') => {
                self.criteria_action(selected, |ctx, t, c, _| criteria::touch(ctx, t, c).map(|_| ()))
            }
            (View::Criteria, 'm') => {
                self.criteria_action(selected, |ctx, t, c, _| criteria::mark_reviewed(ctx, t, c).map(|_| ()))
            }
            (View::Criteria, 'o') => {
                let open = selected
                    .as_deref()
                    .and_then(|k| k.split_once('/'))
                    .and_then(|(t, c)| {
                        self.ins
                            .tasks
                            .iter()
                            .find(|x| x.id == t)?
                            .items
                            .iter()
                            .find(|i| i.id == c)
                    })
                    .is_some_and(|c| c.open);
                self.criteria_action(selected, move |ctx, t, c, _| {
                    criteria::set_open(ctx, t, c, !open).map(|_| ())
                });
            }
            (View::Criteria, 'V') => self.spawn("run the tests", |ctx| {
                let (tests, updated) = criteria::verify(ctx)?;
                Ok(Some((
                    "Tests".to_string(),
                    format!("{tests} test result(s) read, {updated} criterion(s) updated"),
                )))
            }),
            (View::Ask, 'n') => self.open_input(InputKind::Question),
            (View::Ask, 'a') => {
                if let Some(id) = selected.map(|k| k.split(['#', '!']).next().unwrap_or("").to_string()) {
                    self.spawn("answer the question", move |ctx| {
                        questions::answer(ctx, &id)?;
                        Ok(None)
                    });
                }
            }
            (View::Ask, 'x') => {
                if let Some(id) = selected.map(|k| k.split(['#', '!']).next().unwrap_or("").to_string()) {
                    let _ = questions::remove(&self.ctx, &id);
                    self.refresh_insight();
                }
            }
            _ => return false,
        }
        true
    }

    /// Runs `f(task, criterion, subject id)` on the selected criterion.
    fn criteria_action(
        &mut self,
        selected: Option<String>,
        f: impl FnOnce(&rekon_core::Ctx, &str, &str, Option<String>) -> anyhow::Result<()>,
    ) {
        let Some((task, crit)) = selected.as_deref().and_then(|k| k.split_once('/')) else {
            self.notice = Some("select a criterion".into());
            return;
        };
        let subject = self
            .ins
            .pins
            .last()
            .cloned()
            .or_else(|| self.ins.subject.as_ref().map(|s| s.id.clone()));
        if let Err(e) = f(&self.ctx, task, crit, subject) {
            self.error(format!("{e:#}"));
        }
        self.refresh_insight();
    }

    /// Footer text of the selected row of a lens.
    pub fn lens_description(&self) -> String {
        let Some(key) = self.lens_key_selected() else {
            return String::new();
        };
        match self.view {
            View::Decisions => self.ins.decisions.iter().find(|d| d.id == key).map_or(String::new(), |d| {
                format!(
                    "[{}] {}: {} — costs: {} — reconsider if: {}",
                    d.origin.label(),
                    d.title,
                    d.decision,
                    if d.cost.is_empty() { "-" } else { &d.cost },
                    if d.revisit_if.is_empty() { "-" } else { &d.revisit_if }
                )
            }),
            View::Cards => self.ins.cards.iter().find(|c| c.id == key).map_or(String::new(), |c| {
                format!("[{}] before: {} → after: {}", c.origin.label(), c.before, c.after)
            }),
            View::Criteria => match key.split_once('/') {
                Some((t, c)) => self
                    .ins
                    .tasks
                    .iter()
                    .find(|x| x.id == t)
                    .and_then(|t| t.items.iter().find(|i| i.id == c))
                    .map_or(String::new(), |c| {
                        format!(
                            "{} — implemented by {} element(s), {} test pattern(s). l link the element in view, t add a test, m mark reviewed, T re-vouch, V run tests",
                            c.text,
                            c.implementation.len(),
                            c.tests.len()
                        )
                    }),
                None => self.ins.tasks.iter().find(|t| t.id == key).map_or(String::new(), |t| t.title.clone()),
            },
            View::Ask => {
                let (q, rest) = key.split_once(['#', '!']).map_or((key.as_str(), ""), |(q, r)| (q, r));
                let Some(question) = self.ins.questions.iter().find(|x| x.id == q) else { return String::new() };
                let Some(a) = &question.answer else { return format!("{} — a: answer it", question.text) };
                if key.contains('#')
                    && let Some(i) = rest.parse::<usize>().ok().and_then(|n| a.items.get(n))
                {
                    return format!("[{}] {} — {} ({}:{}-{})", a.origin.label(), i.name, i.reason, i.path, i.start, i.end);
                }
                if key.contains('!')
                    && let Some(o) = rest.parse::<usize>().ok().and_then(|n| a.omitted.get(n))
                {
                    return format!("left out: {} — {}", o.path, o.reason);
                }
                format!("{} — looked at {} of {} files; what was left out is listed below the answer", question.text, a.examined, a.total)
            }
            _ => String::new(),
        }
    }

    // ----- jobs -----

    /// Runs `work` in the job pool; a returned `(title, text)` is shown in a window.
    pub fn spawn(
        &mut self,
        label: &str,
        work: impl FnOnce(&rekon_core::Ctx) -> anyhow::Result<Option<(String, String)>> + Send + 'static,
    ) {
        let ctx = Arc::clone(&self.ctx);
        let tx = self.insight_tx.clone();
        let key = JobKey::Insight(label.to_string());
        let label = label.to_string();
        let ok = self.pool.submit(key, move || {
            let shown = work(&ctx)?;
            let _ = tx.send((label, shown));
            Ok(())
        });
        self.notice = Some(if ok {
            "asking the model...".into()
        } else {
            "already running".into()
        });
    }

    /// Called when an insight job finished: reloads data and shows its result window.
    pub fn insight_done(&mut self, label: String, shown: Option<(String, String)>) {
        self.refresh_insight();
        self.ins.rel = None;
        match shown {
            Some((title, body)) => {
                self.ins.text_title = title;
                self.ins.text_body = body;
                self.popup = Some(Popup::Text(0));
            }
            None => self.notice = Some(format!("done: {label}")),
        }
    }

    // ----- input popup -----

    pub fn open_input(&mut self, kind: InputKind) {
        self.ins.input = Some(Input {
            kind,
            text: String::new(),
        });
        self.popup = Some(Popup::Input);
    }

    /// Keys while the input line is open.
    pub fn input_key(&mut self, key: KeyEvent) {
        let Some(input) = self.ins.input.as_mut() else {
            self.popup = None;
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.ins.input = None;
                self.popup = None;
            }
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Char(c) => input.text.push(c),
            KeyCode::Enter => {
                let Input { kind, text } = self.ins.input.take().expect("input open");
                self.popup = None;
                if !text.trim().is_empty() {
                    self.submit_input(kind, text.trim().to_string());
                }
            }
            _ => {}
        }
    }

    fn submit_input(&mut self, kind: InputKind, text: String) {
        match kind {
            InputKind::Question => match questions::pin(&self.ctx, &text) {
                Ok(q) => {
                    self.refresh_insight();
                    let id = q.id.clone();
                    self.spawn("answer the question", move |ctx| {
                        questions::answer(ctx, &id)?;
                        Ok(None)
                    });
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            InputKind::Expected(id) => match trace::set_expected(&self.ctx, &id, &text) {
                Ok(_) => self.notice = Some("expectation saved: c makes a card from it".into()),
                Err(e) => self.error(format!("{e:#}")),
            },
            InputKind::TraceInput(id) => self.spawn("predict a run", move |ctx| {
                trace::predict(ctx, &id, Some(&text))?;
                Ok(None)
            }),
            InputKind::NewTask => match criteria::new_task(&self.ctx, &text, &[]) {
                Ok(t) => {
                    self.refresh_insight();
                    self.open_input(InputKind::Criterion(t.id));
                    self.notice = Some("add its criteria one by one; Esc when done".into());
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            InputKind::Criterion(task) => match criteria::add(&self.ctx, &task, &text) {
                Ok(_) => {
                    self.refresh_insight();
                    self.open_input(InputKind::Criterion(task));
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            InputKind::TestPattern(task, crit) => {
                if let Err(e) = criteria::link_test(&self.ctx, &task, &crit, &text) {
                    self.error(format!("{e:#}"));
                }
                self.refresh_insight();
            }
        }
        self.dirty = true;
    }

    // ----- editor templates -----

    fn edit_template(&mut self, text: &str, followup: Followup, label: &str) {
        match super::super::editor::temp_file(text, "md") {
            Ok(abs) => {
                self.editor_request = Some(EditRequest {
                    path: label.to_string(),
                    abs,
                    line: 1,
                    hash_before: None,
                    followup: Some(followup),
                });
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn edit_decision(&mut self, supersede: Option<String>) {
        let mut base = match &supersede {
            Some(id) => match self.ins.decisions.iter().find(|d| &d.id == id) {
                Some(d) => d.clone(),
                None => return,
            },
            None => Decision::new("", ""),
        };
        if supersede.is_none() {
            base.elements = self.ins.subject.iter().map(|s| s.id.clone()).collect();
        } else {
            base.status = decisions::Status::Accepted;
        }
        self.edit_template(
            &decisions::template(&base),
            Followup::Decision { supersede },
            "decision",
        );
    }

    fn edit_card(&mut self) {
        let mut base = Card::spec("", "", "");
        base.elements = self.ins.subject.iter().map(|s| s.id.clone()).collect();
        self.edit_template(
            &cards::template(&base),
            Followup::Card {
                elements: base.elements.clone(),
            },
            "card",
        );
    }

    fn edit_contract(&mut self, s: &Subject) {
        let rec = contracts::get(&self.ctx, &s.id);
        self.edit_template(
            &contracts::template(&rec),
            Followup::Contract { element: s.id.clone() },
            "contract",
        );
    }

    /// After the editor closed on a template: parses and saves it.
    pub fn finish_template(&mut self, req: &EditRequest, followup: Followup) {
        let text = std::fs::read_to_string(&req.abs).unwrap_or_default();
        let _ = std::fs::remove_file(&req.abs);
        let ctx = Arc::clone(&self.ctx);
        let result: anyhow::Result<String> = (|| match followup {
            Followup::Decision { supersede } => {
                let mut d = decisions::parse_template(&text)?;
                d.origin = Origin::Human;
                let saved = match supersede {
                    Some(old) => decisions::supersede(&ctx, &old, d)?,
                    None => decisions::add(&ctx, d)?,
                };
                Ok(format!("decision recorded: {}", saved.id))
            }
            Followup::Card { elements } => {
                let mut c = cards::parse_template(&text)?;
                c.elements = elements;
                Ok(format!("card added: {}", cards::add(&ctx, c)?.id))
            }
            Followup::Contract { element } => {
                let c = contracts::parse_template(&text)?;
                contracts::set_desired(&ctx, &element, c)?;
                Ok("desired contract saved: a proposal, the code is unchanged".to_string())
            }
        })();
        match result {
            Ok(m) => self.notice = Some(m),
            Err(e) => self.error(format!("not saved: {e:#}")),
        }
        self.refresh_insight();
    }

    // ----- inspector -----

    /// Keys shared by every view: lenses, tabs of the right panel, history, pins.
    pub fn insight_global_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('3') => self.set_view(View::Decisions),
            KeyCode::Char('4') => self.set_view(View::Cards),
            KeyCode::Char('5') => self.set_view(View::Criteria),
            KeyCode::Char('6') => self.set_view(View::Ask),
            KeyCode::Char('v') => self.set_detail(self.ins.detail.step(1)),
            KeyCode::Char('V') => self.set_detail(self.ins.detail.step(-1)),
            KeyCode::Char('P') => self.toggle_pin(),
            KeyCode::Char('<') => self.history_step(-1),
            KeyCode::Char('>') => self.history_step(1),
            _ => return false,
        }
        true
    }

    pub fn set_detail(&mut self, d: Detail) {
        self.ins.detail = d;
        self.ins.cursor = 0;
        self.ins.scroll = 0;
        self.dirty = true;
    }

    /// Selectable lines of the inspector.
    fn selectable(&self) -> Vec<usize> {
        self.ins
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.nav.is_some())
            .map(|(n, _)| n)
            .collect()
    }

    fn selected_nav(&self) -> Option<Nav> {
        self.ins.items.get(self.ins.cursor)?.nav.clone()
    }

    /// Keys of the inspector (right panel focused, tab other than Code).
    pub fn inspector_key(&mut self, key: KeyEvent) -> bool {
        let subject = self.ins.subject.clone();
        let sel = self.selectable();
        let page = self.ins.area.height.max(1) as isize;
        let move_cursor = |app: &mut App, delta: isize| {
            if sel.is_empty() {
                app.ins.scroll = (app.ins.scroll as isize + delta).max(0) as usize;
                return;
            }
            let pos = sel.iter().position(|&i| i >= app.ins.cursor).unwrap_or(sel.len() - 1) as isize;
            let cur_is = sel.get(pos as usize) == Some(&app.ins.cursor);
            let target = if cur_is {
                pos + delta
            } else if delta > 0 {
                pos + delta - 1
            } else {
                pos + delta
            };
            app.ins.cursor = sel[target.clamp(0, sel.len() as isize - 1) as usize];
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => move_cursor(self, 1),
            KeyCode::Up | KeyCode::Char('k') => move_cursor(self, -1),
            KeyCode::PageDown => self.ins.scroll = self.ins.scroll.saturating_add(page as usize),
            KeyCode::PageUp => self.ins.scroll = self.ins.scroll.saturating_sub(page as usize),
            KeyCode::Home => {
                self.ins.scroll = 0;
                self.ins.cursor = sel.first().copied().unwrap_or(0);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(nav) = self.selected_nav() {
                    self.follow(nav);
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.focus = Focus::Tree,
            KeyCode::Char(c) => return self.detail_action(c, subject),
            _ => return false,
        }
        true
    }

    fn follow(&mut self, nav: Nav) {
        match nav {
            Nav::Element(id) => self.goto_element(&id),
            Nav::Lines(path, a, b) => self.show_lines(&path, a, b, "step"),
            Nav::Decision(id) => self.lens_select(View::Decisions, id),
            Nav::Card(id) => self.lens_select(View::Cards, id),
            Nav::Task(key) => self.lens_select(View::Criteria, key),
        }
    }

    fn lens_select(&mut self, view: View, key: String) {
        self.set_view(view);
        self.tree_sel_key = Some(RowRef::Node(key));
        self.focus = Focus::Tree;
    }

    /// Actions of the tabs (letters), given the subject.
    fn detail_action(&mut self, c: char, subject: Option<Subject>) -> bool {
        let Some(s) = subject else { return false };
        let nav = self.selected_nav();
        match (self.ins.detail, c) {
            (Detail::Why, 'n') => {
                self.edit_decision(None);
            }
            (Detail::Why, 'g') if !s.is_file => {
                let id = s.id.clone();
                self.spawn("reconstruct the reason", move |ctx| {
                    decisions::reconstruct(ctx, &id)?;
                    Ok(None)
                });
            }
            (Detail::Why, 'a') => {
                if let Some(Nav::Decision(id)) = nav {
                    match decisions::accept(&self.ctx, &id) {
                        Ok(d) => self.notice = Some(format!("accepted: {}", d.title)),
                        Err(e) => self.error(format!("{e:#}")),
                    }
                    self.refresh_insight();
                }
            }
            (Detail::Why, 's') => {
                if let Some(Nav::Decision(id)) = nav {
                    self.edit_decision(Some(id));
                }
            }
            (Detail::Contract, 'd') if !s.is_file => {
                let id = s.id.clone();
                self.spawn("describe the contract", move |ctx| {
                    contracts::derive(ctx, &id, true)?;
                    Ok(None)
                });
            }
            (Detail::Contract, 'e') if !s.is_file => self.edit_contract(&s),
            (Detail::Contract, 'i') if !s.is_file => {
                let id = s.id.clone();
                self.spawn("work out the impact", move |ctx| {
                    let card = contracts::impact(ctx, &id)?;
                    Ok(Some(("Proposed card".to_string(), cards::brief(&card))))
                });
            }
            (Detail::Contract, 'x') if !s.is_file => {
                let _ = contracts::clear_desired(&self.ctx, &s.id);
                self.notice = Some("desired contract dropped".into());
            }
            (Detail::Trace, 'p') if !s.is_file => {
                let id = s.id.clone();
                self.spawn("predict a run", move |ctx| {
                    trace::predict(ctx, &id, None)?;
                    Ok(None)
                });
            }
            (Detail::Trace, 'e') if !s.is_file => self.open_input(InputKind::TraceInput(s.id.clone())),
            (Detail::Trace, 'x') if !s.is_file => self.open_input(InputKind::Expected(s.id.clone())),
            (Detail::Trace, 'c') if !s.is_file => match trace::expectation_card(&self.ctx, &s.id) {
                Ok(card) => {
                    self.refresh_insight();
                    self.ins.text_title = "Card for an agent".into();
                    self.ins.text_body = cards::brief(&card);
                    self.popup = Some(Popup::Text(0));
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            (Detail::Compare, 'x') => self.ins.pins.clear(),
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// Keys that matter where the focus is, for the status line.
    pub fn context_hint(&self) -> &'static str {
        if self.focus == Focus::Code && self.ins.detail != Detail::Code {
            return self.detail_hint();
        }
        if self.focus != Focus::Tree {
            return "";
        }
        match self.view {
            View::Tree => "m mark understood · v views · P pin · < > history",
            View::Changes => "m accept · u model update · r refresh · v views",
            View::Decisions => "n new · s replace · a accept · C check working tree · Enter code",
            View::Cards => "n write spec · N from working tree · s status · b brief · c criteria",
            View::Criteria => {
                "n task · a criterion · l link element · t test · m reviewed · T re-vouch · V run tests · o open"
            }
            View::Ask => "n pin question · a answer again · x drop",
        }
    }

    /// Hints for the bottom border of the right panel.
    pub fn detail_hint(&self) -> &'static str {
        match self.ins.detail {
            Detail::Code => "v: more views · P pin · < > history",
            Detail::Relations => "j/k select · Enter open · P pin · < > history",
            Detail::Why => "n new · g reconstruct (AI) · a accept · s replace · Enter open",
            Detail::Contract => "d describe (AI) · e edit desired · i impact -> card · x drop",
            Detail::Trace => "p predict (AI) · e your input · x expected result · c card",
            Detail::Compare => "P pins the element in view · x clear",
        }
    }

    /// Builds the lines of the inspector for the current tab and subject.
    pub fn build_inspector(&mut self, width: usize) {
        let items = match self.ins.detail {
            Detail::Code => Vec::new(),
            Detail::Compare => self.compare_items(width),
            d => match self.ins.subject.clone() {
                None => vec![text_item(vec![dim(
                    "Select a file or a piece of code in the tree first.",
                )])],
                Some(s) => match d {
                    Detail::Relations => self.relation_items(&s, width),
                    Detail::Why => self.why_items(&s, width),
                    Detail::Contract => self.contract_items(&s, width),
                    Detail::Trace => self.trace_items(&s, width),
                    _ => Vec::new(),
                },
            },
        };
        self.ins.items = items;
        let n = self.ins.items.len();
        if self.ins.cursor >= n || self.ins.items.get(self.ins.cursor).is_some_and(|i| i.nav.is_none()) {
            self.ins.cursor = self.ins.items.iter().position(|i| i.nav.is_some()).unwrap_or(0);
        }
        // Keep the cursor visible.
        let h = self.ins.area.height.max(1) as usize;
        if self.ins.cursor < self.ins.scroll {
            self.ins.scroll = self.ins.cursor;
        } else if self.ins.cursor >= self.ins.scroll + h {
            self.ins.scroll = self.ins.cursor + 1 - h;
        }
        self.ins.scroll = self.ins.scroll.min(n.saturating_sub(h));
    }

    fn header_items(&self, s: &Subject) -> Vec<InspItem> {
        vec![
            text_item(vec![
                bold(s.name.clone()),
                dim(format!("  {} · {}:{}-{}", s.kind, s.path, s.start, s.end)),
            ]),
            blank(),
        ]
    }

    fn relation_items(&mut self, s: &Subject, width: usize) -> Vec<InspItem> {
        let mut out = self.header_items(s);
        let path = s.path.clone();
        let ctx = Arc::clone(&self.ctx);
        // Decisions, cards, criteria and the contract come from their own stores.
        let ds = decisions::for_element(&ctx, &s.id, &path);
        out.push(text_item(vec![bold("Why"), dim(format!("  {} decision(s)", ds.len()))]));
        for d in &ds {
            out.push(InspItem {
                line: line(vec![
                    plain("  "),
                    plain(fit(&d.title, width.saturating_sub(24))),
                    plain(" "),
                    badge(d.origin),
                    dim(format!(" {}", d.status.label())),
                ]),
                nav: Some(Nav::Decision(d.id.clone())),
            });
        }
        let cs = cards::for_element(&ctx, &s.id, &path);
        out.push(text_item(vec![bold("Changes"), dim(format!("  {} card(s)", cs.len()))]));
        for c in &cs {
            out.push(InspItem {
                line: line(vec![
                    plain("  "),
                    plain(fit(&c.title, width.saturating_sub(24))),
                    plain(" "),
                    badge(c.origin),
                    dim(format!(" {}", c.status.label())),
                ]),
                nav: Some(Nav::Card(c.id.clone())),
            });
        }
        let mut crit_lines = Vec::new();
        for t in criteria::list(&ctx) {
            for c in t.items.iter().filter(|c| c.implementation.contains_key(&s.id)) {
                let st = criteria::status(&ctx, c);
                crit_lines.push(InspItem {
                    line: line(vec![
                        plain("  "),
                        level_span("I", st.implementation),
                        plain(" "),
                        level_span("T", st.verification),
                        plain(" "),
                        level_span("R", st.review),
                        plain("  "),
                        plain(fit(&c.text, width.saturating_sub(30))),
                    ]),
                    nav: Some(Nav::Task(format!("{}/{}", t.id, c.id))),
                });
            }
        }
        out.push(text_item(vec![
            bold("Criteria"),
            dim(format!("  {} linked", crit_lines.len())),
        ]));
        out.extend(crit_lines);
        if !s.is_file {
            let rec = contracts::get(&ctx, &s.id);
            let hash = element::load(&ctx.root, &s.id).map(|e| e.hash);
            out.push(text_item(vec![
                bold("Contract"),
                dim(format!("  {}", contracts::state(&rec, hash.as_deref()).label())),
            ]));
        }
        out.push(blank());
        let Some(idx) = self.relations() else {
            out.push(text_item(vec![dim("relations index unavailable")]));
            return out;
        };
        let links: Vec<relations::Link> = if s.is_file {
            idx.of_file(&s.path)
        } else {
            let mut v = idx.calls(&s.id);
            v.extend(idx.called_by(&s.id));
            v.extend(idx.tested_by(&s.id));
            v
        };
        for (kind, title) in [("calledBy", "Called by"), ("calls", "Calls"), ("testedBy", "Tested by")] {
            let group: Vec<&relations::Link> = links.iter().filter(|l| l.kind == kind).collect();
            out.push(text_item(vec![
                bold(title),
                dim(format!("  {} · matched by name", group.len())),
            ]));
            for l in group {
                out.push(InspItem {
                    line: line(vec![
                        plain("  "),
                        plain(fit(&l.label, width / 3)),
                        dim(format!("  {}:{} ", l.path, l.line)),
                        badge(l.origin),
                    ]),
                    nav: Some(Nav::Element(l.id.clone())),
                });
            }
        }
        if s.is_file {
            out.push(blank());
            let els: Vec<&element::Element> = idx.elements.iter().filter(|e| e.path == s.path).collect();
            out.push(text_item(vec![bold("Elements"), dim(format!("  {}", els.len()))]));
            for e in els.iter().take(40) {
                out.push(InspItem {
                    line: line(vec![
                        plain("  "),
                        plain(fit(&e.name, width / 3)),
                        dim(format!("  {} {}-{}", e.kind, e.start, e.end)),
                    ]),
                    nav: Some(Nav::Element(e.id.clone())),
                });
            }
        }
        out
    }

    fn why_items(&mut self, s: &Subject, width: usize) -> Vec<InspItem> {
        let mut out = self.header_items(s);
        let ds = decisions::for_element(&self.ctx, &s.id, &s.path);
        if ds.is_empty() {
            out.push(text_item(vec![dim("No decision explains this yet.")]));
            out.push(text_item(vec![dim("n: write one (reason, cost, when to reconsider) · g: let the model reconstruct it from the code (marked AI).")]));
            return out;
        }
        for d in &ds {
            let style = if d.status == decisions::Status::Superseded {
                DIM
            } else {
                Style::new()
            };
            out.push(InspItem {
                line: line(vec![
                    bold(d.title.clone()),
                    plain("  "),
                    badge(d.origin),
                    dim(format!(" {} · {}", d.status.label(), d.id)),
                ]),
                nav: Some(Nav::Decision(d.id.clone())),
            });
            if d.origin == Origin::Ai {
                out.push(text_item(vec![
                    plain("  "),
                    Span::styled(
                        "reconstructed from the code, not recorded by anyone",
                        WARN.add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            field(&mut out, 2, "Decision", &d.decision, width, style);
            field(&mut out, 2, "Because", &d.context, width, style);
            field(&mut out, 2, "Costs", &d.cost, width, style);
            for a in &d.assumptions {
                field(&mut out, 2, "Assumes", a, width, style);
            }
            field(&mut out, 2, "Reconsider if", &d.revisit_if, width, style);
            if let Some(n) = &d.superseded_by {
                out.push(text_item(vec![plain("  "), dim(format!("superseded by {n}"))]));
            }
            out.push(blank());
        }
        out
    }

    fn contract_items(&mut self, s: &Subject, width: usize) -> Vec<InspItem> {
        let mut out = self.header_items(s);
        if s.is_file {
            out.push(text_item(vec![dim(
                "A contract belongs to a function or type: pick one in the Relations tab (Elements) or select a block.",
            )]));
            return out;
        }
        let rec = contracts::get(&self.ctx, &s.id);
        let hash = element::load(&self.ctx.root, &s.id).map(|e| e.hash);
        let state = contracts::state(&rec, hash.as_deref());
        out.push(text_item(vec![bold("State  "), plain(state.label())]));
        out.push(blank());
        let show = |out: &mut Vec<InspItem>, c: &contracts::Contract| {
            field(out, 2, "Responsibility", &c.responsibility, width, Style::new());
            field(out, 2, "Input", &c.input, width, Style::new());
            field(out, 2, "Output", &c.output, width, Style::new());
            for r in &c.rules {
                field(out, 2, "Rule", r, width, Style::new());
            }
            for x in &c.examples {
                field(out, 2, "Example", x, width, Style::new());
            }
        };
        match &rec.current {
            Some(c) => {
                let fresh = hash.as_deref().is_some_and(|h| c.is_current(h));
                out.push(text_item(vec![
                    bold("Now (what the code does)  "),
                    badge(c.origin),
                    dim(if fresh { "" } else { "  outdated: the code changed" }),
                ]));
                show(&mut out, &c.value);
            }
            None => out.push(text_item(vec![dim(
                "Not described yet: d asks the model to describe what the code does now.",
            )])),
        }
        out.push(blank());
        match &rec.desired {
            Some(d) => {
                out.push(text_item(vec![
                    bold("Wanted  "),
                    badge(Origin::Human),
                    dim("  a proposal until code and tests agree"),
                ]));
                show(&mut out, d);
                for (f, a, b) in contracts::diff(&rec) {
                    out.push(text_item(vec![
                        Span::styled("  ~ ", WARN),
                        plain(format!("{f}: ")),
                        Span::styled(fit(&a, width / 3), DIM),
                        plain(" -> "),
                        Span::styled(fit(&b, width / 3), Style::new().fg(Color::Green)),
                    ]));
                }
            }
            None => out.push(text_item(vec![dim(
                "Nothing wanted yet: e edits the contract; the code stays as it is until an agent implements it.",
            )])),
        }
        out
    }

    fn trace_items(&mut self, s: &Subject, width: usize) -> Vec<InspItem> {
        let mut out = self.header_items(s);
        if s.is_file {
            out.push(text_item(vec![dim(
                "An example run belongs to a function: pick one (Relations tab, Elements) or select a block.",
            )]));
            return out;
        }
        let hash = element::load(&self.ctx.root, &s.id).map(|e| e.hash);
        match trace::get(&self.ctx, &s.id) {
            None => out.push(text_item(vec![dim(
                "No example yet: p asks the model to walk through a typical input; e lets you give your own.",
            )])),
            Some(t) => {
                let fresh = hash.as_deref() == Some(t.hash.as_str());
                out.push(text_item(vec![
                    bold("Example run  "),
                    badge(t.origin),
                    dim(if fresh {
                        "  read from the code, not executed"
                    } else {
                        "  outdated: the code changed"
                    }),
                ]));
                field(&mut out, 2, "Input", &t.input, width, Style::new());
                for (n, st) in t.steps.iter().enumerate() {
                    let at = if st.line > 0 {
                        format!(" (line {})", st.line)
                    } else {
                        String::new()
                    };
                    let head = vec![plain("  "), bold(format!("{}. {}", n + 1, st.label)), dim(at)];
                    out.push(match st.line {
                        0 => text_item(head),
                        l => InspItem {
                            line: line(head),
                            nav: Some(Nav::Lines(s.path.clone(), l, l)),
                        },
                    });
                    field(&mut out, 5, "before", &st.before, width, DIM);
                    field(&mut out, 5, "after", &st.after, width, Style::new());
                    field(&mut out, 5, "because", &st.why, width, DIM);
                }
                field(
                    &mut out,
                    2,
                    "Result",
                    &t.result,
                    width,
                    Style::new().add_modifier(Modifier::BOLD),
                );
                if let Some(e) = &t.expected {
                    field(&mut out, 2, "You expect", e, width, Style::new().fg(Color::Green));
                }
            }
        }
        out.push(blank());
        out.push(text_item(vec![
            bold("Evidence  "),
            badge(Origin::Recorded),
            dim(" none: rekon does not run code"),
        ]));
        for t in criteria::list(&self.ctx) {
            for c in t.items.iter().filter(|c| c.implementation.contains_key(&s.id)) {
                if let Some(v) = &c.verification {
                    out.push(text_item(vec![
                        plain("  "),
                        badge(Origin::TestResult),
                        plain(format!(
                            " {} — {} test(s) {}",
                            c.text,
                            v.matched,
                            if v.passed { "pass" } else { "FAIL" }
                        )),
                    ]));
                }
            }
        }
        out
    }

    fn compare_items(&mut self, width: usize) -> Vec<InspItem> {
        let pins = self.ins.pins.clone();
        if pins.is_empty() {
            return vec![text_item(vec![dim(
                "Nothing to compare. P pins the element in view (up to 3), from any tab or lens.",
            )])];
        }
        let ctx = Arc::clone(&self.ctx);
        let idx = self.relations();
        struct Col {
            name: String,
            cells: Vec<String>,
            head: Vec<String>,
        }
        let cols: Vec<Col> = pins
            .iter()
            .map(|id| {
                let path = id.split_once("::").map_or(id.as_str(), |(p, _)| p).to_string();
                let el = element::load(&ctx.root, id);
                let (name, kind, range) = match &el {
                    Some(e) if e.kind != "file" => (e.name.clone(), e.kind.clone(), format!("{}-{}", e.start, e.end)),
                    _ => (path.clone(), "file".into(), "-".into()),
                };
                let (calls, called, tested) = match (&idx, &el) {
                    (Some(i), Some(e)) if e.kind != "file" => {
                        (i.calls(id).len(), i.called_by(id).len(), i.tested_by(id).len())
                    }
                    (Some(i), _) => {
                        let l = i.of_file(&path);
                        let n = |k: &str| l.iter().filter(|x| x.kind == k).count();
                        (n("calls"), n("calledBy"), n("testedBy"))
                    }
                    _ => (0, 0, 0),
                };
                let contract = contracts::get(&ctx, id);
                let cstate = contracts::state(&contract, el.as_ref().map(|e| e.hash.as_str()));
                let ds = decisions::for_element(&ctx, id, &path);
                let cs = cards::for_element(&ctx, id, &path);
                let summary = ctx.store.file_note(&path).summary.map(|s| s.text).unwrap_or_default();
                let change = review_status(&ctx, id);
                let head: Vec<String> = el
                    .as_ref()
                    .map(|e| e.text.lines().take(8).map(str::to_string).collect())
                    .unwrap_or_default();
                Col {
                    name,
                    cells: vec![
                        kind,
                        range,
                        calls.to_string(),
                        called.to_string(),
                        tested.to_string(),
                        format!(
                            "{} ({} open)",
                            ds.len(),
                            ds.iter().filter(|d| d.status == decisions::Status::Proposed).count()
                        ),
                        cs.len().to_string(),
                        cstate.label().to_string(),
                        change,
                        summary,
                    ],
                    head,
                }
            })
            .collect();
        let labels = [
            "kind",
            "lines",
            "calls",
            "called by",
            "tested by",
            "decisions",
            "cards",
            "contract",
            "since review",
            "file",
        ];
        let label_w = 13;
        let col_w = (width.saturating_sub(label_w) / cols.len()).max(8);
        let mut out = Vec::new();
        let mut names = vec![plain(format!("{:<label_w$}", ""))];
        for c in &cols {
            names.push(bold(format!("{:<col_w$}", fit(&c.name, col_w - 1))));
        }
        out.push(text_item(names));
        out.push(blank());
        for (r, label) in labels.iter().enumerate() {
            let differs = cols.windows(2).any(|w| w[0].cells[r] != w[1].cells[r]);
            let mut spans = vec![dim(format!("{label:<label_w$}"))];
            for c in &cols {
                let style = if differs && cols.len() > 1 { WARN } else { Style::new() };
                spans.push(Span::styled(format!("{:<col_w$}", fit(&c.cells[r], col_w - 1)), style));
            }
            out.push(text_item(spans));
        }
        out.push(blank());
        for n in 0..8 {
            let mut spans = vec![dim(format!("{:<label_w$}", if n == 0 { "code" } else { "" }))];
            for c in &cols {
                spans.push(plain(format!(
                    "{:<col_w$}",
                    fit(c.head.get(n).map_or("", String::as_str).trim_end(), col_w - 1)
                )));
            }
            out.push(text_item(spans));
        }
        out.push(blank());
        for (n, id) in pins.iter().enumerate() {
            out.push(InspItem {
                line: line(vec![plain(format!("{}. ", n + 1)), dim(id.clone())]),
                nav: Some(Nav::Element(id.clone())),
            });
        }
        out
    }
}

/// Status of an element against the review baseline, as short text.
fn review_status(ctx: &rekon_core::Ctx, id: &str) -> String {
    let base = rekon_core::review::load(&ctx.store);
    let path = id.split_once("::").map_or(id, |(p, _)| p);
    if !base.items.values().any(|r| r.path == path) {
        return "never reviewed".into();
    }
    match rekon_core::review::changes(ctx) {
        Ok(list) => list
            .iter()
            .find(|c| c.id == id || (id == path && c.path == path))
            .map_or("unchanged".to_string(), |c| c.status.label().to_string()),
        Err(_) => "?".into(),
    }
}
