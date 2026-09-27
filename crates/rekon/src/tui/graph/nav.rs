//! Spatial keyboard navigation: an arrow moves the selection to the nearest element
//! (node, edge label, "+N more") in that direction on the screen, not to the next
//! entry of a list.

use super::Sel;
use super::layout::Frame;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Distance between two intervals, 0 when they overlap.
fn gap(a: (i32, i32), b: (i32, i32)) -> i32 {
    if a.1 < b.0 {
        b.0 - a.1
    } else if b.1 < a.0 {
        a.0 - b.1
    } else {
        0
    }
}

/// Element to move to from `from`. Candidates lie ahead in `dir`; the best has the
/// smallest distance ahead plus twice the sideways distance (0 when aligned), and
/// those within 45° of `dir` win over the rest. A terminal row is about twice as
/// tall as a column is wide, so vertical distances count double.
pub fn next(frame: &Frame, from: &Sel, dir: Dir) -> Option<Sel> {
    let cur = frame.rect(from)?;
    let (cx, cy) = cur.center();
    let mut best: Option<((bool, i64), Sel)> = None;
    for (sel, r) in frame.elements() {
        if &sel == from {
            continue;
        }
        let (x, y) = r.center();
        let across_x = gap((r.x, r.right()), (cur.x, cur.right()));
        let across_y = gap((r.y, r.bottom()), (cur.y, cur.bottom()));
        let (ahead, aside) = match dir {
            Dir::Down => (i64::from(y - cy) * 2, i64::from(across_x)),
            Dir::Up => (i64::from(cy - y) * 2, i64::from(across_x)),
            Dir::Right => (i64::from(x - cx), i64::from(across_y) * 2),
            Dir::Left => (i64::from(cx - x), i64::from(across_y) * 2),
        };
        if ahead <= 0 {
            continue;
        }
        let key = (aside > ahead, ahead + 2 * aside);
        if best.as_ref().is_none_or(|(k, _)| key < *k) {
            best = Some((key, sel));
        }
    }
    best.map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::super::layout::{EdgePath, IRect, Label, NodeBox};
    use super::*;

    fn node(id: &str, x: i32, y: i32, w: i32) -> NodeBox {
        NodeBox {
            sel: Sel::Node(id.into()),
            rect: IRect::new(x, y, w, 3),
            compact: false,
        }
    }

    fn edge(id: &str, x: i32, y: i32, text: &str) -> EdgePath {
        EdgePath {
            id: id.into(),
            from: String::new(),
            points: Vec::new(),
            arrow: None,
            exit: None,
            label: Some(Label {
                x,
                y,
                text: text.into(),
            }),
            anchor: (x, y),
            more: false,
            cross: false,
        }
    }

    ///            [ root ]
    ///   calls      reads      emits
    /// [ a ]      [ b ]      [ c ]
    fn frame() -> Frame {
        Frame {
            nodes: vec![
                node("root", 20, 0, 12),
                node("a", 0, 6, 10),
                node("b", 21, 6, 10),
                node("c", 42, 6, 10),
            ],
            edges: vec![
                edge("e-a", 2, 4, "calls"),
                edge("e-b", 23, 4, "reads"),
                edge("e-c", 44, 4, "emits"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn arrows_follow_the_picture_not_the_list() {
        let f = frame();
        let n = |id: &str| Sel::Node(id.into());
        let e = |id: &str| Sel::Edge(id.into());
        assert_eq!(next(&f, &n("root"), Dir::Down), Some(e("e-b")), "the label right below");
        assert_eq!(next(&f, &e("e-b"), Dir::Down), Some(n("b")));
        assert_eq!(next(&f, &e("e-b"), Dir::Right), Some(e("e-c")));
        assert_eq!(next(&f, &n("b"), Dir::Left), Some(n("a")));
        assert_eq!(next(&f, &n("a"), Dir::Up), Some(e("e-a")));
        assert_eq!(next(&f, &e("e-a"), Dir::Up), Some(n("root")));
        assert_eq!(next(&f, &n("c"), Dir::Right), None);
        assert_eq!(next(&f, &n("root"), Dir::Up), None);
    }
}
