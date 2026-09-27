//! Screen layout of one frame: node boxes and edge routes in terminal cells, from the
//! world positions, the camera and the zoom. Drawing, clicks and keyboard navigation
//! all read this one layout, so what is selected is always what is seen.

use std::collections::{BTreeMap, HashMap, HashSet};

use ratatui::layout::Rect;
use rekon_core::ontology::graph::GraphIndex;
use rekon_core::ontology::model::Edge;
use unicode_width::UnicodeWidthStr;

use super::{Explorer, NODE_H, Sel};

/// Separator of relation names that share one route.
pub const SEP: &str = " · ";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w - 1
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h - 1
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    pub fn intersects(&self, o: &IRect) -> bool {
        self.x <= o.right() && o.x <= self.right() && self.y <= o.bottom() && o.y <= self.bottom()
    }

    pub fn grow(&self, dx: i32, dy: i32) -> IRect {
        IRect::new(self.x - dx, self.y - dy, self.w + 2 * dx, self.h + 2 * dy)
    }

    pub fn union(&self, o: &IRect) -> IRect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        IRect::new(
            x,
            y,
            self.right().max(o.right()) - x + 1,
            self.bottom().max(o.bottom()) - y + 1,
        )
    }
}

impl From<Rect> for IRect {
    fn from(r: Rect) -> Self {
        IRect::new(i32::from(r.x), i32::from(r.y), i32::from(r.width), i32::from(r.height))
    }
}

#[derive(Clone, Debug)]
pub struct NodeBox {
    pub sel: Sel,
    pub rect: IRect,
    /// One line without a border (zoomed out).
    pub compact: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub x: i32,
    pub y: i32,
    pub text: String,
}

impl Label {
    pub fn rect(&self) -> IRect {
        IRect::new(self.x, self.y, self.text.width() as i32, 1)
    }
}

#[derive(Clone, Debug)]
pub struct EdgePath {
    /// Edge id; `more:<node>` for the line to a "+N more" marker.
    pub id: String,
    /// Node the route starts at (the one that revealed the other or is expanded).
    pub from: String,
    /// Corners of the route: the first next to `from`, the last next to `to`.
    pub points: Vec<(i32, i32)>,
    /// Arrow drawn on the last point.
    pub arrow: Option<char>,
    /// Where the route leaves the border of `from`, and the side it goes to
    /// (`┬` down, `┴` up, `├` right, `┤` left).
    pub exit: Option<(i32, i32, char)>,
    pub label: Option<Label>,
    /// Point of the edge for navigation when it has no label.
    pub anchor: (i32, i32),
    /// Line to a "+N more" marker, not an edge of the graph.
    pub more: bool,
    /// Joins two nodes of which neither revealed the other: drawn dotted, so it does
    /// not read as part of the hierarchy.
    pub cross: bool,
}

impl EdgePath {
    pub fn rect(&self) -> IRect {
        self.label
            .as_ref()
            .map(Label::rect)
            .unwrap_or(IRect::new(self.anchor.0, self.anchor.1, 1, 1))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub area: Rect,
    pub nodes: Vec<NodeBox>,
    pub edges: Vec<EdgePath>,
}

impl Frame {
    pub fn node(&self, sel: &Sel) -> Option<&NodeBox> {
        self.nodes.iter().find(|n| &n.sel == sel)
    }

    /// Rectangle of an element: its box, or its label (anchor) for an edge.
    pub fn rect(&self, sel: &Sel) -> Option<IRect> {
        match sel {
            Sel::Edge(id) => self.edges.iter().find(|e| &e.id == id).map(EdgePath::rect),
            _ => self.node(sel).map(|n| n.rect),
        }
    }

    /// Every selectable element with its rectangle.
    pub fn elements(&self) -> impl Iterator<Item = (Sel, IRect)> + '_ {
        self.nodes.iter().map(|n| (n.sel.clone(), n.rect)).chain(
            self.edges
                .iter()
                .filter(|e| !e.more)
                .map(|e| (Sel::Edge(e.id.clone()), e.rect())),
        )
    }

    /// Elements under a cell: a box first, then edges by label, then by route.
    pub fn hit(&self, x: i32, y: i32) -> Vec<Sel> {
        if let Some(n) = self.nodes.iter().rev().find(|n| n.rect.contains(x, y)) {
            return vec![n.sel.clone()];
        }
        let mut out: Vec<Sel> = self
            .edges
            .iter()
            .filter(|e| !e.more && e.label.as_ref().is_some_and(|l| l.rect().contains(x, y)))
            .map(|e| Sel::Edge(e.id.clone()))
            .collect();
        for e in self.edges.iter().filter(|e| !e.more) {
            let sel = Sel::Edge(e.id.clone());
            if !out.contains(&sel) && cells(&e.points).contains(&(x, y)) {
                out.push(sel);
            }
        }
        out
    }
}

/// Every cell of a route through `points` (straight segments between corners).
pub fn cells(points: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = Vec::new();
    let mut push = |c: (i32, i32)| {
        if out.last() != Some(&c) {
            out.push(c);
        }
    };
    let Some(&first) = points.first() else {
        return out;
    };
    push(first);
    for w in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        let (mut x, mut y) = (x0, y0);
        while (x, y) != (x1, y1) {
            if x != x1 {
                x += (x1 - x).signum();
            } else {
                y += (y1 - y).signum();
            }
            push((x, y));
        }
    }
    out
}

/// Route from box `a` to box `b`: down, up or sideways, with the arrow at `b`, and
/// label spots from the best (a row of its own, above the arrow) to fallbacks
/// (on the route itself); `true` marks a row of its own.
struct Route {
    points: Vec<(i32, i32)>,
    arrow: char,
    exit: (i32, i32, char),
    spots: Vec<(i32, i32, bool)>,
}

fn route(a: IRect, b: IRect) -> Option<Route> {
    let (acx, acy) = a.center();
    let (bcx, bcy) = b.center();
    if b.y >= a.bottom() + 2 {
        let start = (acx, a.bottom() + 1);
        let ym = (b.y - 3).max(start.1);
        let mut spots = Vec::new();
        if b.y - 2 > ym {
            spots.push((bcx, b.y - 2, true));
        }
        spots.push(((acx + bcx) / 2, ym, false));
        return Some(Route {
            points: vec![start, (acx, ym), (bcx, ym), (bcx, b.y - 1)],
            arrow: '▼',
            exit: (acx, a.bottom(), '┬'),
            spots,
        });
    }
    if b.bottom() <= a.y - 2 {
        let start = (acx, a.y - 1);
        let ym = (b.bottom() + 3).min(start.1);
        let mut spots = Vec::new();
        if b.bottom() + 2 < ym {
            spots.push((bcx, b.bottom() + 2, true));
        }
        spots.push(((acx + bcx) / 2, ym, false));
        return Some(Route {
            points: vec![start, (acx, ym), (bcx, ym), (bcx, b.bottom() + 1)],
            arrow: '▲',
            exit: (acx, a.y, '┴'),
            spots,
        });
    }
    let side = |from: i32, to: i32, arrow: char, exit: (i32, i32, char)| {
        let xm = (from + to) / 2;
        Route {
            points: vec![(from, acy), (xm, acy), (xm, bcy), (to, bcy)],
            arrow,
            exit,
            spots: vec![((from + xm) / 2, acy, false), ((xm + to) / 2, bcy, false)],
        }
    };
    if b.x >= a.right() + 3 {
        return Some(side(a.right() + 1, b.x - 1, '▶', (a.right(), acy, '├')));
    }
    if b.right() <= a.x - 3 {
        return Some(side(a.x - 1, b.right() + 1, '◀', (a.x, acy, '┤')));
    }
    None
}

/// End that a drawn edge starts from: the node that revealed the other one, else the
/// expanded end (the smaller id when both are).
fn orientation(ex: &Explorer, e: &Edge) -> (String, String) {
    let (s, t) = (&e.source, &e.target);
    let (ps, pt) = (&ex.nodes[s], &ex.nodes[t]);
    let from_source = if pt.parent.as_ref() == Some(s) {
        true
    } else if ps.parent.as_ref() == Some(t) {
        false
    } else if ps.expanded && pt.expanded {
        s < t
    } else {
        ps.expanded || !pt.expanded
    };
    if from_source {
        (s.clone(), t.clone())
    } else {
        (t.clone(), s.clone())
    }
}

/// Edges joining one pair of nodes (from, to), each with its label read from `from`.
type Group<'a> = ((String, String), Vec<(&'a Edge, String)>);

pub fn build(ex: &Explorer, g: &GraphIndex) -> Frame {
    let area = ex.area;
    let z = ex.zoom;
    let compact = z < 0.75;
    let sx = |x: i32| i32::from(area.x) + ((f64::from(x) - ex.cam.0) * z).round() as i32;
    let sy = |y: i32| i32::from(area.y) + ((f64::from(y) - ex.cam.1) * z).round() as i32;
    let size = |w: i32| -> (i32, i32) {
        if z >= 1.0 {
            return (w, NODE_H);
        }
        let min = if compact { 3.0 } else { 8.0 };
        (
            (f64::from(w) * z).round().max(min) as i32,
            if compact { 1 } else { NODE_H },
        )
    };
    let mut frame = Frame {
        area,
        ..Default::default()
    };
    let mut rects: HashMap<&str, IRect> = HashMap::new();
    for id in ex.order.iter().filter(|id| ex.shown(id)) {
        let p = &ex.nodes[id];
        let (w, h) = size(p.w);
        let r = IRect::new(sx(p.x), sy(p.y), w, h);
        rects.insert(id, r);
        frame.nodes.push(NodeBox {
            sel: Sel::Node(id.clone()),
            rect: r,
            compact,
        });
    }
    // Lines to markers: (line id, node, marker box, relation label).
    let mut markers: Vec<(String, &String, IRect, Option<&String>)> = Vec::new();
    let mut more: Vec<(&String, &super::More)> = ex.more.iter().filter(|(p, _)| ex.shown(p)).collect();
    more.sort_by(|a, b| a.0.cmp(b.0));
    for (parent, m) in more {
        let (w, h) = size(m.w);
        let r = IRect::new(sx(m.x), sy(m.y), w, h);
        frame.nodes.push(NodeBox {
            sel: Sel::More(parent.clone()),
            rect: r,
            compact,
        });
        markers.push((format!("more:{parent}"), parent, r, None));
    }
    for ((parent, label), m) in ex.groups.iter().filter(|((p, _), _)| ex.shown(p)) {
        let (w, h) = size(m.w);
        let r = IRect::new(sx(m.x), sy(m.y), w, h);
        frame.nodes.push(NodeBox {
            sel: Sel::Group(parent.clone(), label.clone()),
            rect: r,
            compact,
        });
        markers.push((format!("group:{parent}:{label}"), parent, r, Some(label)));
    }

    // Edges between visible nodes with an expanded end, grouped by the pair they join.
    let mut groups: BTreeMap<(String, String), Vec<(&Edge, String)>> = BTreeMap::new();
    let mut seen = HashSet::new();
    for id in &ex.order {
        for nb in g.neighbors(id) {
            if !ex.edge_drawn(nb.edge) || !seen.insert(nb.edge.id.as_str()) {
                continue;
            }
            let (from, to) = orientation(ex, nb.edge);
            let label = g.relation_label(nb.edge, nb.edge.source == from);
            groups.entry((from, to)).or_default().push((nb.edge, label));
        }
    }
    let mut taken: Vec<IRect> = frame.nodes.iter().map(|n| n.rect).collect();
    // Routes of the revealing edges first, so their labels get the spots above the arrows.
    let mut ordered: Vec<Group> = groups.into_iter().collect();
    ordered.sort_by_key(|((from, to), _)| ex.nodes[to].parent.as_ref() != Some(from));
    for ((from, to), edges) in ordered {
        let (Some(&a), Some(&b)) = (rects.get(from.as_str()), rects.get(to.as_str())) else {
            continue;
        };
        let r = route(a, b);
        let points = r.as_ref().map(|r| r.points.clone()).unwrap_or_default();
        let (ac, bc) = (a.center(), b.center());
        let middle = cells(&points)
            .get(cells(&points).len() / 2)
            .copied()
            .unwrap_or(((ac.0 + bc.0) / 2, (ac.1 + bc.1) / 2));
        let full: String = edges.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join(SEP);
        let width = full.width() as i32;
        // Zoomed out only rows of their own are used: labels on crowded routes
        // run into each other.
        let spots = r.as_ref().map_or(&[][..], |r| &r.spots[..]);
        let spot = spots
            .iter()
            .filter(|s| s.2 || !compact)
            .map(|&(cx, y, _)| IRect::new(cx - width / 2, y, width, 1))
            .find(|l| taken.iter().all(|t| !t.intersects(&l.grow(1, 0))));
        if let Some(l) = spot {
            taken.push(l);
        }
        let mut x = spot.map_or(0, |l| l.x);
        let cross = ex.nodes[to.as_str()].parent.as_ref() != Some(&from);
        for (k, (edge, text)) in edges.iter().enumerate() {
            let text = if k == 0 { text.clone() } else { format!("{SEP}{text}") };
            let label = spot.map(|l| Label {
                x,
                y: l.y,
                text: text.clone(),
            });
            x += text.width() as i32;
            let anchor = label.as_ref().map_or(middle, |l| l.rect().center());
            frame.edges.push(EdgePath {
                id: edge.id.clone(),
                from: from.clone(),
                points: points.clone(),
                arrow: r.as_ref().map(|r| r.arrow),
                exit: r.as_ref().map(|r| r.exit),
                label,
                anchor,
                more: false,
                cross,
            });
        }
    }
    for (id, parent, m, relation) in markers {
        let Some(&a) = rects.get(parent.as_str()) else { continue };
        let Some(r) = route(a, m) else { continue };
        let label = relation.and_then(|text| {
            let width = text.width() as i32;
            let spot = r
                .spots
                .iter()
                .filter(|s| s.2 || !compact)
                .map(|&(cx, y, _)| IRect::new(cx - width / 2, y, width, 1))
                .find(|l| taken.iter().all(|t| !t.intersects(&l.grow(1, 0))))?;
            taken.push(spot);
            Some(Label {
                x: spot.x,
                y: spot.y,
                text: text.clone(),
            })
        });
        let middle = cells(&r.points)[cells(&r.points).len() / 2];
        frame.edges.push(EdgePath {
            id,
            from: parent.clone(),
            points: r.points,
            arrow: Some(r.arrow),
            exit: Some(r.exit),
            label,
            anchor: middle,
            more: true,
            cross: false,
        });
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_go_down_up_and_sideways() {
        let a = IRect::new(10, 0, 10, 3);
        let below = IRect::new(0, 6, 8, 3);
        let r = route(a, below).unwrap();
        assert_eq!(r.points, [(15, 3), (15, 3), (4, 3), (4, 5)]);
        assert_eq!((r.arrow, r.exit), ('▼', (15, 2, '┬')));
        assert_eq!(r.spots[0], (4, 4, true), "label right above the arrow");
        let r = route(below, a).unwrap();
        assert_eq!(r.arrow, '▲');
        assert_eq!(r.points.last(), Some(&(15, 3)));
        let right = IRect::new(40, 1, 6, 3);
        let r = route(a, right).unwrap();
        assert_eq!(
            (r.arrow, r.points[0], *r.points.last().unwrap()),
            ('▶', (20, 1), (39, 2))
        );
        assert!(route(a, IRect::new(12, 1, 4, 3)).is_none(), "overlapping boxes");
    }

    #[test]
    fn cells_follow_corners() {
        assert_eq!(
            cells(&[(0, 0), (0, 2), (2, 2)]),
            [(0, 0), (0, 1), (0, 2), (1, 2), (2, 2)]
        );
        assert_eq!(cells(&[(1, 1), (1, 1)]), [(1, 1)]);
    }
}
