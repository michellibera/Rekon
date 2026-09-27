//! Ontology view: an explorer of the stored graph. Nodes appear step by step:
//! expanding a node places only its hidden neighbors, in rows below it and as close
//! to it as free space allows. Nothing already visible moves, the zoom stays, and the
//! graph is never laid out again as a whole. Positions are world cells at zoom 100%;
//! the camera maps them to the screen in [`layout`].

mod draw;
mod layout;
mod nav;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ratatui::layout::Rect;
use rekon_core::ontology::graph::{self, GraphIndex};
use rekon_core::ontology::model::{Edge, Node};
use rekon_core::text;
use unicode_width::UnicodeWidthStr;

pub use draw::draw;
pub use layout::{Frame, IRect};
pub use nav::Dir;

/// Rows of a node box: border with the type, name, border.
pub const NODE_H: i32 = 3;
/// Rows between a node and the row it revealed: bus, relation label, arrow.
pub const V_GAP: i32 = 3;
/// Columns between neighbors in a row.
pub const H_GAP: i32 = 2;
/// Columns kept free in upper rows for the line to the rows below them.
const TRUNK_GAP: i32 = 3;
/// Neighbors revealed by one expansion; the rest wait behind "+N more".
pub const PAGE: usize = 12;
/// Longest name shown in a box, in columns.
pub const MAX_NAME: usize = 32;
const ZOOMS: [f64; 7] = [0.4, 0.55, 0.75, 1.0, 1.25, 1.5, 2.0];
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
const DERIVED_FROM: &str = "derivedFrom";

/// Selectable element of the graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Node(String),
    Edge(String),
    /// "+N more" marker of the node with this id.
    More(String),
}

/// A visible node: world position of its top-left corner, and how it got here.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub expanded: bool,
    /// Node whose expansion revealed this one (none for the root).
    pub parent: Option<String>,
}

/// "+N more" marker ending the rows revealed by a node.
#[derive(Clone, Debug, PartialEq)]
pub struct More {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub count: usize,
}

enum Drag {
    Pan {
        from: (u16, u16),
        cam: (f64, f64),
    },
    Node {
        id: String,
        from: (u16, u16),
        pos: (i32, i32),
    },
}

/// What a mouse press did.
#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    Single(Sel),
    Double(Sel),
}

pub struct Explorer {
    pub graph: Option<Arc<GraphIndex>>,
    /// Why the graph could not be read.
    pub error: Option<String>,
    mtime: Option<SystemTime>,
    pub nodes: HashMap<String, Placed>,
    /// Visible nodes in the order they appeared (drawing order).
    pub order: Vec<String>,
    pub more: HashMap<String, More>,
    /// Edges shown although neither end is expanded (data lineage).
    pub extra: HashSet<String>,
    pub sel: Option<Sel>,
    /// World point at the top-left corner of the view.
    pub cam: (f64, f64),
    pub zoom: f64,
    /// Inner area of the panel in the last frame.
    pub area: Rect,
    /// Layout of the last frame, for clicks and keyboard navigation.
    pub frame: Frame,
    /// On the next draw: bring the selection (and these nodes, if they fit) into view.
    pub reveal: Option<Vec<String>>,
    /// On the next draw: put the selection in the middle of the view, at this
    /// fraction of its height (0 = top).
    pub center: Option<f64>,
    /// Last places of hidden nodes, reused when they appear again.
    remembered: HashMap<String, (i32, i32)>,
    drag: Option<Drag>,
    last_click: Option<(Instant, Sel)>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            graph: None,
            error: None,
            mtime: None,
            nodes: HashMap::new(),
            order: Vec::new(),
            more: HashMap::new(),
            extra: HashSet::new(),
            sel: None,
            cam: (0.0, 0.0),
            zoom: 1.0,
            area: Rect::default(),
            frame: Frame::default(),
            reveal: None,
            center: None,
            remembered: HashMap::new(),
            drag: None,
            last_click: None,
        }
    }
}

/// Width of a node box: `│▸ name │` and the type in the top border.
pub fn node_width(n: &Node) -> i32 {
    let name = n.name.width().min(MAX_NAME) as i32;
    (name + 5).max(n.kind.width() as i32 + 4).max(10)
}

impl Explorer {
    /// Reads `graph.json` again when it changed on disk; true when something changed.
    pub fn load(&mut self, map_dir: &Path) -> bool {
        let path = rekon_core::ontology::graph_path(map_dir);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == self.mtime {
            return false;
        }
        self.mtime = mtime;
        match graph::load(map_dir) {
            Ok(Some(g)) => {
                self.error = None;
                self.set_graph(Arc::new(g));
            }
            Ok(None) => *self = Self::default(),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        true
    }

    /// Takes a new graph, keeping visible what still exists, where it is.
    pub fn set_graph(&mut self, g: Arc<GraphIndex>) {
        self.nodes.retain(|id, _| g.node(id).is_some());
        let nodes = &self.nodes;
        self.order.retain(|id| nodes.contains_key(id));
        for (id, p) in self.nodes.iter_mut() {
            if p.parent.as_ref().is_some_and(|q| g.node(q).is_none()) {
                p.parent = None;
            }
            if let Some(n) = g.node(id) {
                p.w = node_width(n);
            }
        }
        self.more.clear();
        self.extra.retain(|e| g.edge(e).is_some());
        self.graph = Some(g);
        let valid = match &self.sel {
            Some(Sel::Node(id)) | Some(Sel::More(id)) => self.nodes.contains_key(id),
            Some(Sel::Edge(id)) => self.graph.as_ref().is_some_and(|g| g.edge(id).is_some()),
            None => true,
        };
        if !valid {
            self.sel = None;
        }
        if self.nodes.is_empty() {
            self.start();
        } else {
            // Neighbors hidden behind "+N more" are counted again.
            let expanded: Vec<String> = self
                .order
                .iter()
                .filter(|id| self.nodes[*id].expanded)
                .cloned()
                .collect();
            for id in expanded {
                let hidden = self.hidden(&id).len();
                if hidden > 0 {
                    self.place_more(&id, hidden);
                }
            }
        }
    }

    /// First view: the root with its neighbors.
    fn start(&mut self) {
        let Some(g) = self.graph.clone() else { return };
        let Some(root) = g.root().or(g.graph.nodes.first()) else {
            return;
        };
        let id = root.id.clone();
        self.nodes.insert(
            id.clone(),
            Placed {
                x: 0,
                y: 0,
                w: node_width(root),
                expanded: false,
                parent: None,
            },
        );
        self.order.push(id.clone());
        self.sel = Some(Sel::Node(id.clone()));
        self.expand(&id);
        self.center = Some(0.0);
    }

    pub fn root(&self) -> Option<String> {
        let g = self.graph.as_ref()?;
        g.root().map(|n| n.id.clone()).or_else(|| self.order.first().cloned())
    }

    // ----- what is shown -----

    /// Is this edge drawn: both ends visible and one of them expanded (or lineage)?
    pub fn edge_drawn(&self, e: &Edge) -> bool {
        match (self.nodes.get(&e.source), self.nodes.get(&e.target)) {
            (Some(s), Some(t)) => s.expanded || t.expanded || self.extra.contains(&e.id),
            _ => false,
        }
    }

    /// Neighbors of `id` that are not visible, in display order, each once.
    pub fn hidden(&self, id: &str) -> Vec<String> {
        let Some(g) = &self.graph else { return Vec::new() };
        let mut out: Vec<String> = Vec::new();
        for nb in g.neighbors(id) {
            if !self.nodes.contains_key(&nb.node.id) && !out.contains(&nb.node.id) {
                out.push(nb.node.id.clone());
            }
        }
        out
    }

    /// Would expanding `id` show anything: a hidden neighbor or an edge not drawn?
    pub fn expandable(&self, id: &str) -> bool {
        let (Some(g), Some(p)) = (&self.graph, self.nodes.get(id)) else {
            return false;
        };
        !p.expanded
            && g.neighbors(id)
                .iter()
                .any(|nb| !self.nodes.contains_key(&nb.node.id) || !self.edge_drawn(nb.edge))
    }

    // ----- expanding and collapsing -----

    /// Expands a node: the next page of its hidden neighbors appears below it.
    /// Returns true when the view changed.
    pub fn expand(&mut self, id: &str) -> bool {
        let Some(p) = self.nodes.get_mut(id) else { return false };
        let was = p.expanded;
        p.expanded = true;
        self.more.remove(id);
        let kids = self.hidden(id);
        let page: Vec<String> = kids.iter().take(PAGE).cloned().collect();
        let rest = kids.len() - page.len();
        self.place(id, &page, rest);
        self.reveal = Some(page.clone());
        !was || !page.is_empty()
    }

    /// "+N more": the next page of the node's neighbors, in rows below the others.
    pub fn show_more(&mut self, id: &str) {
        self.more.remove(id);
        let kids = self.hidden(id);
        let page: Vec<String> = kids.iter().take(PAGE).cloned().collect();
        self.place(id, &page, kids.len() - page.len());
        if let Some(first) = page.first() {
            self.sel = Some(Sel::Node(first.clone()));
        }
        self.reveal = Some(page);
    }

    /// Hides what the node revealed (and what that revealed), except nodes still
    /// next to another expanded node, which stay where they are.
    pub fn collapse(&mut self, id: &str) {
        let Some(g) = self.graph.clone() else { return };
        let Some(p) = self.nodes.get_mut(id) else { return };
        p.expanded = false;
        self.more.remove(id);
        let mut hide = self.descendants(id);
        loop {
            let keep = self.order.iter().filter(|h| hide.contains(*h)).find_map(|h| {
                g.neighbors(h)
                    .iter()
                    .find(|nb| {
                        let o = &nb.node.id;
                        o != id && !hide.contains(o) && self.nodes.get(o).is_some_and(|n| n.expanded)
                    })
                    .map(|nb| (h.clone(), nb.node.id.clone()))
            });
            let Some((k, new_parent)) = keep else { break };
            for d in self.descendants(&k) {
                hide.remove(&d);
            }
            hide.remove(&k);
            if let Some(n) = self.nodes.get_mut(&k) {
                n.parent = Some(new_parent);
            }
        }
        for h in &hide {
            if let Some(n) = self.nodes.remove(h) {
                self.remembered.insert(h.clone(), (n.x, n.y));
            }
            self.more.remove(h);
        }
        self.order.retain(|o| !hide.contains(o));
        self.extra.retain(|e| {
            g.edge(e)
                .is_some_and(|e| !hide.contains(&e.source) && !hide.contains(&e.target))
        });
        let lost = match &self.sel {
            Some(Sel::Node(n)) | Some(Sel::More(n)) => !self.nodes.contains_key(n),
            Some(Sel::Edge(e)) => g.edge(e).is_none_or(|e| !self.edge_drawn(e)),
            None => false,
        };
        if lost {
            self.sel = Some(Sel::Node(id.to_string()));
        }
        self.reveal = Some(Vec::new());
    }

    /// Nodes revealed by `id`, directly or further down.
    fn descendants(&self, id: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(cur) = stack.pop() {
            for (k, p) in &self.nodes {
                if p.parent.as_deref() == Some(cur.as_str()) && out.insert(k.clone()) {
                    stack.push(k.clone());
                }
            }
        }
        out
    }

    /// Reveals the data lineage of the selected node: `derivedFrom` in both
    /// directions, as far as it goes. Returns the number of nodes on the path.
    pub fn lineage(&mut self) -> usize {
        let Some(g) = self.graph.clone() else { return 0 };
        let Some(Sel::Node(start)) = self.sel.clone() else {
            return 0;
        };
        let mut queue = VecDeque::from([start.clone()]);
        let mut seen = HashSet::from([start]);
        let mut shown = Vec::new();
        while let Some(id) = queue.pop_front() {
            let links: Vec<_> = g
                .neighbors(&id)
                .into_iter()
                .filter(|nb| nb.edge.relation == DERIVED_FROM)
                .collect();
            let mut new: Vec<String> = Vec::new();
            for nb in &links {
                if !self.nodes.contains_key(&nb.node.id) && !new.contains(&nb.node.id) {
                    new.push(nb.node.id.clone());
                }
            }
            self.place(&id, &new, 0);
            for nb in links {
                self.extra.insert(nb.edge.id.clone());
                if seen.insert(nb.node.id.clone()) {
                    shown.push(nb.node.id.clone());
                    queue.push_back(nb.node.id.clone());
                }
            }
        }
        let count = shown.len();
        self.reveal = Some(shown);
        count
    }

    // ----- placing -----

    /// World rectangles taken by visible boxes.
    fn taken(&self) -> impl Iterator<Item = IRect> + '_ {
        self.nodes
            .values()
            .map(|p| IRect::new(p.x, p.y, p.w, NODE_H))
            .chain(self.more.values().map(|m| IRect::new(m.x, m.y, m.w, NODE_H)))
    }

    fn free(&self, r: &IRect) -> bool {
        self.taken().all(|t| !t.intersects(r))
    }

    /// Top-left corner for a row `width` wide near `(x, y)`: the first spot that
    /// overlaps no box (label and arrow rows above it included), trying sideways
    /// shifts first, then rows further down.
    fn find_spot(&self, x: i32, y: i32, width: i32) -> (i32, i32) {
        for step in 0..12 {
            let yy = y + step * (NODE_H + V_GAP);
            for k in 0..=60 {
                let dx = if k % 2 == 1 { (k + 1) / 2 * 3 } else { -(k / 2) * 3 };
                let r = IRect::new(x + dx - 1, yy - 2, width + 2, NODE_H + 2);
                if self.free(&r) {
                    return (x + dx, yy);
                }
            }
        }
        let bottom = self.taken().map(|t| t.bottom()).max().unwrap_or(y);
        (x, bottom + V_GAP + 1)
    }

    /// Places `kids` (not visible yet) in rows centered under `parent`; `more`
    /// hidden neighbors get a "+N more" marker at the end.
    fn place(&mut self, parent: &str, kids: &[String], more: usize) {
        let Some(g) = self.graph.clone() else { return };
        let Some(p) = self.nodes.get(parent).cloned() else {
            return;
        };
        let widths: Vec<i32> = kids.iter().map(|k| g.node(k).map_or(10, node_width)).collect();
        // Places they had before, when all of them are still free.
        let before: Option<Vec<(i32, i32)>> = kids.iter().map(|k| self.remembered.get(k).copied()).collect();
        if let Some(spots) = before.filter(|_| more == 0 && !kids.is_empty()) {
            let rects: Vec<IRect> = spots
                .iter()
                .zip(&widths)
                .map(|(&(x, y), &w)| IRect::new(x, y, w, NODE_H))
                .collect();
            let apart = rects
                .iter()
                .enumerate()
                .all(|(i, r)| rects[i + 1..].iter().all(|o| !o.intersects(r)));
            if apart && rects.iter().all(|r| self.free(&r.grow(1, 1))) {
                for (k, r) in kids.iter().zip(rects) {
                    self.insert(k, r.x, r.y, r.w, parent);
                }
                return;
            }
        }
        // Slot of a kid: its box, or its relation label when that is wider.
        let mut slots: Vec<(Option<&String>, i32, i32)> = kids
            .iter()
            .zip(&widths)
            .map(|(k, &w)| {
                let label = link_label(&g, parent, k).width() as i32;
                (Some(k), w, w.max(label + 2))
            })
            .collect();
        if more > 0 {
            let w = text::more_neighbors(more).width() as i32 + 4;
            slots.push((None, w, w));
        }
        if slots.is_empty() {
            return;
        }
        let max_row = ((f64::from(self.area.width) / self.zoom) as i32).max(60);
        let mut rows: Vec<Vec<(Option<&String>, i32, i32)>> = vec![Vec::new()];
        let mut width = 0;
        for s in slots {
            if rows.last().is_some_and(|r| !r.is_empty()) && width + H_GAP + s.2 > max_row {
                rows.push(Vec::new());
                width = 0;
            }
            width += if width == 0 { s.2 } else { H_GAP + s.2 };
            rows.last_mut().expect("a row").push(s);
        }
        let pcx = p.x + p.w / 2;
        let mut y = p.y + NODE_H + V_GAP;
        let count = rows.len();
        for (ri, row) in rows.into_iter().enumerate() {
            // Upper rows leave a gap in the middle for the line to the rows below.
            let trunk = ri + 1 < count;
            let half = row.len() / 2;
            let total = row.iter().map(|s| s.2).sum::<i32>()
                + H_GAP * (row.len() as i32 - 1)
                + if trunk { TRUNK_GAP + H_GAP } else { 0 };
            let (x0, yy) = self.find_spot(pcx - total / 2, y, total);
            let mut cx = x0;
            for (k, (id, w, slot)) in row.into_iter().enumerate() {
                if trunk && k == half {
                    cx += TRUNK_GAP + H_GAP;
                }
                let nx = cx + (slot - w) / 2;
                match id {
                    Some(id) => self.insert(id, nx, yy, w, parent),
                    None => {
                        self.more.insert(
                            parent.to_string(),
                            More {
                                x: nx,
                                y: yy,
                                w,
                                count: more,
                            },
                        );
                    }
                }
                cx += slot + H_GAP;
            }
            y = yy + NODE_H + V_GAP;
        }
    }

    /// "+N more" alone (after a reload): below the node's lowest revealed row.
    fn place_more(&mut self, parent: &str, count: usize) {
        let Some(p) = self.nodes.get(parent).cloned() else {
            return;
        };
        let w = text::more_neighbors(count).width() as i32 + 4;
        let (x, y) = self.find_spot(p.x + p.w / 2 - w / 2, p.y + NODE_H + V_GAP, w);
        self.more.insert(parent.to_string(), More { x, y, w, count });
    }

    fn insert(&mut self, id: &str, x: i32, y: i32, w: i32, parent: &str) {
        self.remembered.remove(id);
        self.nodes.insert(
            id.to_string(),
            Placed {
                x,
                y,
                w,
                expanded: false,
                parent: Some(parent.to_string()),
            },
        );
        self.order.push(id.to_string());
    }

    // ----- selection and keys -----

    pub fn select(&mut self, sel: Sel) {
        self.sel = Some(sel);
        self.reveal.get_or_insert_with(Vec::new);
    }

    pub fn select_root(&mut self) {
        if let Some(root) = self.root() {
            self.select(Sel::Node(root));
            self.center = Some(0.0);
        }
    }

    /// Moves the selection to the nearest element in `dir` on the screen.
    pub fn go(&mut self, dir: Dir) {
        let Some(from) = self.sel.clone() else {
            self.select_root();
            return;
        };
        if let Some(next) = nav::next(&self.frame, &from, dir) {
            self.select(next);
        }
    }

    /// ↓: expands a node with something to show, else moves down.
    pub fn down(&mut self) {
        match self.sel.clone() {
            Some(Sel::Node(id)) if self.expandable(&id) => {
                self.expand(&id);
            }
            Some(Sel::More(id)) => self.show_more(&id),
            _ => self.go(Dir::Down),
        }
    }

    /// Space: expands or collapses the selected node.
    pub fn toggle(&mut self) {
        match self.sel.clone() {
            Some(Sel::Node(id)) if self.nodes.get(&id).is_some_and(|p| p.expanded) => self.collapse(&id),
            Some(Sel::Node(id)) => {
                self.expand(&id);
            }
            Some(Sel::More(id)) => self.show_more(&id),
            _ => {}
        }
    }

    /// Backspace: collapses the selected node, or the node that revealed it.
    pub fn collapse_selected(&mut self) {
        let target = match self.sel.clone() {
            Some(Sel::Node(id)) if self.nodes.get(&id).is_some_and(|p| p.expanded) => Some(id),
            Some(Sel::Node(id)) => self.nodes.get(&id).and_then(|p| p.parent.clone()),
            Some(Sel::More(id)) => Some(id),
            Some(Sel::Edge(id)) => self.frame.edges.iter().find(|e| e.id == id).map(|e| e.from.clone()),
            None => None,
        };
        if let Some(id) = target {
            self.collapse(&id);
            self.select(Sel::Node(id));
        }
    }

    /// Esc: the node one level up (the one that revealed the selection); at the root
    /// the selection is cleared.
    pub fn up_level(&mut self) {
        let next = match self.sel.clone() {
            Some(Sel::Node(id)) => self.nodes.get(&id).and_then(|p| p.parent.clone()).map(Sel::Node),
            Some(Sel::Edge(id)) => self
                .frame
                .edges
                .iter()
                .find(|e| e.id == id)
                .map(|e| Sel::Node(e.from.clone())),
            Some(Sel::More(id)) => Some(Sel::Node(id)),
            None => None,
        };
        match next {
            Some(s) => self.select(s),
            None => self.sel = None,
        }
    }

    // ----- camera -----

    /// Moves the view by screen cells.
    pub fn pan(&mut self, dx: i32, dy: i32) {
        self.cam.0 += f64::from(dx) / self.zoom;
        self.cam.1 += f64::from(dy) / self.zoom;
    }

    /// One zoom step in or out, keeping the world point under `at` (default: the
    /// selection, else the middle of the view) in place.
    pub fn zoom_step(&mut self, steps: i32, at: Option<(u16, u16)>) {
        let i = ZOOMS.iter().position(|z| (*z - self.zoom).abs() < 1e-9).unwrap_or(3) as i32;
        let next = ZOOMS[(i + steps).clamp(0, ZOOMS.len() as i32 - 1) as usize];
        self.zoom_to(next, at);
    }

    pub fn zoom_to(&mut self, zoom: f64, at: Option<(u16, u16)>) {
        let a = self.area;
        let (sx, sy) = at
            .map(|(x, y)| (i32::from(x), i32::from(y)))
            .or_else(|| self.sel.as_ref().and_then(|s| self.frame.rect(s)).map(|r| r.center()))
            .unwrap_or((
                i32::from(a.x) + i32::from(a.width) / 2,
                i32::from(a.y) + i32::from(a.height) / 2,
            ));
        let (ox, oy) = (f64::from(sx - i32::from(a.x)), f64::from(sy - i32::from(a.y)));
        let (wx, wy) = (self.cam.0 + ox / self.zoom, self.cam.1 + oy / self.zoom);
        self.zoom = zoom;
        self.cam = (wx - ox / zoom, wy - oy / zoom);
    }

    // ----- mouse -----

    /// Press of the left button at a screen cell: selects what is there (repeated
    /// presses cycle through overlapping edges) and starts dragging a node or the view.
    pub fn press(&mut self, x: u16, y: u16) -> Option<Click> {
        let hits = self.frame.hit(i32::from(x), i32::from(y));
        let Some(first) = hits.first().cloned() else {
            self.drag = Some(Drag::Pan {
                from: (x, y),
                cam: self.cam,
            });
            return None;
        };
        let current = self
            .sel
            .as_ref()
            .filter(|s| matches!(s, Sel::Edge(_)) && matches!(first, Sel::Edge(_)) && hits.len() > 1)
            .and_then(|s| hits.iter().position(|h| h == s));
        let pick = match current {
            Some(i) => hits[(i + 1) % hits.len()].clone(),
            None => first,
        };
        let double = self
            .last_click
            .as_ref()
            .is_some_and(|(t, s)| *s == pick && t.elapsed() < DOUBLE_CLICK);
        self.last_click = (!double).then(|| (Instant::now(), pick.clone()));
        self.sel = Some(pick.clone());
        if let Sel::Node(id) = &pick
            && let Some(p) = self.nodes.get(id)
        {
            self.drag = Some(Drag::Node {
                id: id.clone(),
                from: (x, y),
                pos: (p.x, p.y),
            });
        }
        Some(if double {
            Click::Double(pick)
        } else {
            Click::Single(pick)
        })
    }

    /// Mouse moved with the button down: moves the dragged node or the view.
    pub fn drag_to(&mut self, x: u16, y: u16) {
        let zoom = self.zoom;
        match &self.drag {
            Some(Drag::Pan { from, cam }) => {
                let dx = f64::from(i32::from(x) - i32::from(from.0));
                let dy = f64::from(i32::from(y) - i32::from(from.1));
                self.cam = (cam.0 - dx / zoom, cam.1 - dy / zoom);
            }
            Some(Drag::Node { id, from, pos }) => {
                let dx = (f64::from(i32::from(x) - i32::from(from.0)) / zoom).round() as i32;
                let dy = (f64::from(i32::from(y) - i32::from(from.1)) / zoom).round() as i32;
                let (id, pos) = (id.clone(), *pos);
                if let Some(p) = self.nodes.get_mut(&id) {
                    p.x = pos.0 + dx;
                    p.y = pos.1 + dy;
                }
            }
            None => {}
        }
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

/// Relations between two nodes read from `from`, e.g. `reads · writes`.
pub fn link_label(g: &GraphIndex, from: &str, to: &str) -> String {
    g.neighbors(from)
        .iter()
        .filter(|nb| nb.node.id == to)
        .map(|nb| g.relation_label(nb.edge, nb.outgoing))
        .collect::<Vec<_>>()
        .join(" · ")
}
