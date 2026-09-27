//! Ontology view: an explorer of the stored graph. Nodes appear step by step:
//! expanding a node places only its hidden neighbors, in rows below it and as close
//! to it as free space allows. Nothing already visible moves, the zoom stays, and the
//! graph is never laid out again as a whole. Positions are world cells at zoom 100%;
//! the camera maps them to the screen in [`layout`].

mod draw;
mod layout;
mod nav;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ratatui::layout::Rect;
use rekon_core::ontology::graph::{self, GraphIndex, Neighbor};
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
/// Hidden neighbors of one relation from this many on wait behind a group marker.
const GROUP_MIN: usize = 5;
/// Picking a filter reveals at most this many hidden matching nodes.
pub const REVEAL_MAX: usize = 200;
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
    /// Group marker of the node's hidden neighbors joined by one relation (label).
    Group(String, String),
}

/// A pick in the filters: a node type or a relation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Filter {
    Kind(String),
    Relation(String),
}

/// What a place in a row of revealed neighbors holds.
enum Slot<'a> {
    Node(&'a String),
    /// Group marker: relation label, hidden neighbors.
    Group(&'a String, usize),
    More,
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
    /// Group markers by (node, relation label).
    pub groups: BTreeMap<(String, String), More>,
    /// Node types and relations picked in the filters; none picked shows everything.
    pub filters: HashSet<Filter>,
    /// With filters: the nodes that match them, the only ones shown.
    relevant: Option<HashSet<String>>,
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
            groups: BTreeMap::new(),
            filters: HashSet::new(),
            relevant: None,
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
        self.groups.clear();
        self.extra.retain(|e| g.edge(e).is_some());
        self.graph = Some(g);
        self.relevant = self.find_relevant();
        let valid = match &self.sel {
            Some(Sel::Node(id)) | Some(Sel::More(id)) | Some(Sel::Group(id, _)) => self.nodes.contains_key(id),
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
                self.place_markers(&id);
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

    /// Can this node be shown with the filters?
    pub fn shown(&self, id: &str) -> bool {
        self.relevant.as_ref().is_none_or(|r| r.contains(id))
    }

    /// Edges of a node to nodes the filters let through, with their other ends.
    fn links(&self, id: &str) -> Vec<Neighbor<'_>> {
        let Some(g) = &self.graph else { return Vec::new() };
        g.neighbors(id)
            .into_iter()
            .filter(|nb| self.shown(&nb.node.id))
            .collect()
    }

    /// Is a relation picked, or no relation at all?
    fn relation_passes(&self, relation: &str) -> bool {
        let picked = self.filters.iter().any(|f| matches!(f, Filter::Relation(_)));
        !picked || self.filters.contains(&Filter::Relation(relation.to_string()))
    }

    /// Nodes matching the filters: of a picked type, and with a picked relation the
    /// two ends of its edges; `None` with nothing picked.
    fn find_relevant(&self) -> Option<HashSet<String>> {
        let g = self.graph.as_ref()?;
        if self.filters.is_empty() {
            return None;
        }
        let by_kind = self.filters.iter().any(|f| matches!(f, Filter::Kind(_)));
        let by_relation = self.filters.iter().any(|f| matches!(f, Filter::Relation(_)));
        let kind_ok = |kind: &str| !by_kind || self.filters.contains(&Filter::Kind(kind.to_string()));
        let mut out = HashSet::new();
        if by_relation {
            for e in g.graph.edges.iter().filter(|e| self.relation_passes(&e.relation)) {
                if let (Some(s), Some(t)) = (g.node(&e.source), g.node(&e.target))
                    && kind_ok(&s.kind)
                    && kind_ok(&t.kind)
                {
                    out.insert(s.id.clone());
                    out.insert(t.id.clone());
                }
            }
        } else {
            out.extend(g.graph.nodes.iter().filter(|n| kind_ok(&n.kind)).map(|n| n.id.clone()));
        }
        Some(out)
    }

    /// Places the matching nodes not visible yet, expanding the graph from the root
    /// along the shortest path to each (nodes on the way are placed too, and shown
    /// again without filters). Returns false when it stopped at [`REVEAL_MAX`].
    fn reveal_relevant(&mut self) -> bool {
        let (Some(g), Some(relevant)) = (self.graph.clone(), self.relevant.clone()) else {
            return true;
        };
        let Some(root) = self.root() else { return true };
        let mut came_from: HashMap<String, String> = HashMap::new();
        let mut queue = VecDeque::from([root.clone()]);
        let mut seen = HashSet::from([root]);
        while let Some(id) = queue.pop_front() {
            for nb in g.neighbors(&id) {
                if seen.insert(nb.node.id.clone()) {
                    came_from.insert(nb.node.id.clone(), id.clone());
                    queue.push_back(nb.node.id.clone());
                }
            }
        }
        let targets: Vec<String> = g
            .graph
            .nodes
            .iter()
            .filter(|n| relevant.contains(&n.id) && !self.nodes.contains_key(&n.id))
            .map(|n| n.id.clone())
            .collect();
        let mut complete = true;
        for (k, target) in targets.into_iter().enumerate() {
            if k >= REVEAL_MAX {
                complete = false;
                break;
            }
            // Up to the first visible node, then placed back down from it.
            let mut path = Vec::new();
            let mut cur = target;
            while !self.nodes.contains_key(&cur) {
                path.push(cur.clone());
                match came_from.get(&cur) {
                    Some(prev) => cur = prev.clone(),
                    None => break,
                }
            }
            if !self.nodes.contains_key(&cur) {
                continue;
            }
            for child in path.into_iter().rev() {
                self.place(&cur, std::slice::from_ref(&child), 0, &[]);
                if let Some(p) = self.nodes.get_mut(&cur) {
                    p.expanded = true;
                }
                cur = child;
            }
        }
        // Edges of the picked relations between shown nodes are drawn even when
        // neither end is expanded.
        for e in &g.graph.edges {
            if self.relation_passes(&e.relation)
                && relevant.contains(&e.source)
                && relevant.contains(&e.target)
                && self.nodes.contains_key(&e.source)
                && self.nodes.contains_key(&e.target)
            {
                self.extra.insert(e.id.clone());
            }
        }
        complete
    }

    /// Node types and relations of the graph with their counts.
    pub fn filter_entries(&self) -> Vec<(Filter, usize)> {
        let Some(g) = &self.graph else { return Vec::new() };
        let mut counts: BTreeMap<Filter, usize> = BTreeMap::new();
        for n in &g.graph.nodes {
            *counts.entry(Filter::Kind(n.kind.clone())).or_default() += 1;
        }
        for e in &g.graph.edges {
            *counts.entry(Filter::Relation(e.relation.clone())).or_default() += 1;
        }
        counts.into_iter().collect()
    }

    /// Picks or unpicks a node type or relation: only matching nodes are shown, the
    /// hidden ones revealed. Visible nodes keep their places. Returns false when the
    /// reveal stopped at [`REVEAL_MAX`].
    pub fn toggle_filter(&mut self, f: Filter) -> bool {
        if !self.filters.remove(&f) {
            self.filters.insert(f);
        }
        self.relevant = self.find_relevant();
        let complete = self.reveal_relevant();
        self.more.clear();
        self.groups.clear();
        let expanded: Vec<String> = self
            .order
            .iter()
            .filter(|id| self.nodes[*id].expanded && self.shown(id))
            .cloned()
            .collect();
        for id in expanded {
            self.place_markers(&id);
        }
        let g = self.graph.clone();
        let valid = match &self.sel {
            Some(Sel::Node(id)) => self.shown(id),
            Some(Sel::More(id)) => self.more.contains_key(id),
            Some(Sel::Group(id, label)) => self.groups.contains_key(&(id.clone(), label.clone())),
            Some(Sel::Edge(id)) => g.as_ref().and_then(|g| g.edge(id)).is_some_and(|e| self.edge_drawn(e)),
            None => true,
        };
        if !valid {
            let first = self.order.iter().find(|id| self.shown(id)).cloned();
            self.sel = first.map(Sel::Node);
        }
        self.reveal = Some(Vec::new());
        complete
    }

    /// Is this edge drawn: both ends visible and shown, one of them expanded (or lineage)?
    pub fn edge_drawn(&self, e: &Edge) -> bool {
        if !self.shown(&e.source) || !self.shown(&e.target) || !self.relation_passes(&e.relation) {
            return false;
        }
        match (self.nodes.get(&e.source), self.nodes.get(&e.target)) {
            (Some(s), Some(t)) => s.expanded || t.expanded || self.extra.contains(&e.id),
            _ => false,
        }
    }

    /// Neighbors of `id` that are not visible, in display order, each once.
    pub fn hidden(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for nb in self.links(id) {
            if !self.nodes.contains_key(&nb.node.id) && !out.contains(&nb.node.id) {
                out.push(nb.node.id.clone());
            }
        }
        out
    }

    /// Would expanding `id` show anything: a hidden neighbor or an edge not drawn?
    pub fn expandable(&self, id: &str) -> bool {
        let Some(p) = self.nodes.get(id) else {
            return false;
        };
        !p.expanded
            && self
                .links(id)
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
        self.groups.retain(|(p, _), _| p != id);
        let (loose, groups) = self.split(id);
        let page: Vec<String> = loose.iter().take(PAGE).cloned().collect();
        let rest = loose.len() - page.len();
        self.place(id, &page, rest, &groups);
        self.reveal = Some(page.clone());
        !was || !page.is_empty() || !groups.is_empty()
    }

    /// Relation of `kid` as read from `id` (the first one when there are several).
    fn relation_to(&self, id: &str, kid: &str) -> Option<String> {
        let g = self.graph.as_ref()?;
        self.links(id)
            .into_iter()
            .find(|nb| nb.node.id == kid)
            .map(|nb| g.relation_label(nb.edge, nb.outgoing))
    }

    /// Hidden neighbors of `id` split into loose ones and groups (relation label,
    /// count) of relations with at least [`GROUP_MIN`] of them, when there are more
    /// hidden neighbors than that.
    fn split(&self, id: &str) -> (Vec<String>, Vec<(String, usize)>) {
        let kids = self.hidden(id);
        if kids.len() <= GROUP_MIN {
            return (kids, Vec::new());
        }
        let labels: Vec<Option<String>> = kids.iter().map(|k| self.relation_to(id, k)).collect();
        let mut counts: Vec<(String, usize)> = Vec::new();
        for l in labels.iter().flatten() {
            match counts.iter_mut().find(|(c, _)| c == l) {
                Some((_, n)) => *n += 1,
                None => counts.push((l.clone(), 1)),
            }
        }
        counts.retain(|(_, n)| *n >= GROUP_MIN);
        let grouped = |l: &Option<String>| l.as_ref().is_some_and(|l| counts.iter().any(|(c, _)| c == l));
        let loose = kids
            .into_iter()
            .zip(&labels)
            .filter(|(_, l)| !grouped(l))
            .map(|(k, _)| k)
            .collect();
        (loose, counts)
    }

    /// Opens a group marker: all its neighbors appear below the node.
    pub fn open_group(&mut self, id: &str, label: &str) {
        self.groups.remove(&(id.to_string(), label.to_string()));
        let kids: Vec<String> = self
            .hidden(id)
            .into_iter()
            .filter(|k| self.relation_to(id, k).as_deref() == Some(label))
            .collect();
        self.place(id, &kids, 0, &[]);
        if let Some(first) = kids.first() {
            self.sel = Some(Sel::Node(first.clone()));
        }
        self.reveal = Some(kids);
    }

    /// "+N more": the next page of the node's neighbors, in rows below the others.
    pub fn show_more(&mut self, id: &str) {
        self.more.remove(id);
        let (loose, _) = self.split(id);
        let page: Vec<String> = loose.iter().take(PAGE).cloned().collect();
        self.place(id, &page, loose.len() - page.len(), &[]);
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
        self.groups.retain(|(p, _), _| p != id);
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
        self.groups.retain(|(p, _), _| !hide.contains(p));
        self.order.retain(|o| !hide.contains(o));
        self.extra.retain(|e| {
            g.edge(e)
                .is_some_and(|e| !hide.contains(&e.source) && !hide.contains(&e.target))
        });
        let lost = match &self.sel {
            Some(Sel::Node(n)) | Some(Sel::More(n)) | Some(Sel::Group(n, _)) => !self.nodes.contains_key(n),
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
            self.place(&id, &new, 0, &[]);
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
            .chain(self.groups.values().map(|m| IRect::new(m.x, m.y, m.w, NODE_H)))
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

    /// Places `kids` (not visible yet) in rows centered under `parent`, then a marker
    /// per group of `groups`; `more` hidden neighbors get a "+N more" marker at the end.
    fn place(&mut self, parent: &str, kids: &[String], more: usize, groups: &[(String, usize)]) {
        let Some(g) = self.graph.clone() else { return };
        let Some(p) = self.nodes.get(parent).cloned() else {
            return;
        };
        let widths: Vec<i32> = kids.iter().map(|k| g.node(k).map_or(10, node_width)).collect();
        // Places they had before, when all of them are still free.
        let before: Option<Vec<(i32, i32)>> = kids.iter().map(|k| self.remembered.get(k).copied()).collect();
        if let Some(spots) = before.filter(|_| more == 0 && groups.is_empty() && !kids.is_empty()) {
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
        let mut slots: Vec<(Slot, i32, i32)> = kids
            .iter()
            .zip(&widths)
            .map(|(k, &w)| {
                let label = link_label(&g, parent, k).width() as i32;
                (Slot::Node(k), w, w.max(label + 2))
            })
            .collect();
        for (label, count) in groups {
            let w = text::relation_group(*count).width() as i32 + 4;
            slots.push((Slot::Group(label, *count), w, w.max(label.width() as i32 + 2)));
        }
        if more > 0 {
            let w = text::more_neighbors(more).width() as i32 + 4;
            slots.push((Slot::More, w, w));
        }
        if slots.is_empty() {
            return;
        }
        let max_row = ((f64::from(self.area.width) / self.zoom) as i32).max(60);
        let mut rows: Vec<Vec<(Slot, i32, i32)>> = vec![Vec::new()];
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
                let marker = |count| More { x: nx, y: yy, w, count };
                match id {
                    Slot::Node(id) => self.insert(id, nx, yy, w, parent),
                    Slot::Group(label, count) => {
                        self.groups.insert((parent.to_string(), label.clone()), marker(count));
                    }
                    Slot::More => {
                        self.more.insert(parent.to_string(), marker(more));
                    }
                }
                cx += slot + H_GAP;
            }
            y = yy + NODE_H + V_GAP;
        }
    }

    /// Markers alone (after a reload or a filter change): groups and "+N more" of the
    /// node's hidden neighbors, below its lowest revealed row.
    fn place_markers(&mut self, parent: &str) {
        let Some(p) = self.nodes.get(parent).cloned() else {
            return;
        };
        let (loose, groups) = self.split(parent);
        let spot = |ex: &Self, w: i32| ex.find_spot(p.x + p.w / 2 - w / 2, p.y + NODE_H + V_GAP, w);
        for (label, count) in groups {
            let w = text::relation_group(count).width() as i32 + 4;
            let (x, y) = spot(self, w);
            self.groups.insert((parent.to_string(), label), More { x, y, w, count });
        }
        if !loose.is_empty() {
            let count = loose.len();
            let w = text::more_neighbors(count).width() as i32 + 4;
            let (x, y) = spot(self, w);
            self.more.insert(parent.to_string(), More { x, y, w, count });
        }
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

    /// Space: expands or collapses the selected node.
    pub fn toggle(&mut self) {
        match self.sel.clone() {
            Some(Sel::Node(id)) if self.nodes.get(&id).is_some_and(|p| p.expanded) => self.collapse(&id),
            Some(Sel::Node(id)) => {
                self.expand(&id);
            }
            Some(Sel::More(id)) => self.show_more(&id),
            Some(Sel::Group(id, label)) => self.open_group(&id, &label),
            _ => {}
        }
    }

    /// Backspace: collapses the selected node, or the node that revealed it.
    pub fn collapse_selected(&mut self) {
        let target = match self.sel.clone() {
            Some(Sel::Node(id)) if self.nodes.get(&id).is_some_and(|p| p.expanded) => Some(id),
            Some(Sel::Node(id)) => self.nodes.get(&id).and_then(|p| p.parent.clone()),
            Some(Sel::More(id)) | Some(Sel::Group(id, _)) => Some(id),
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
            Some(Sel::More(id)) | Some(Sel::Group(id, _)) => Some(Sel::Node(id)),
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
