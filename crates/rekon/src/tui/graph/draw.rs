//! Drawing the explorer into the terminal buffer: edge routes as box-drawing lines
//! (joined where they meet), arrows, node boxes colored by type, relation labels.

use std::str::FromStr;

use ratatui::Frame as TuiFrame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Paragraph, Wrap};
use rekon_core::ontology::graph::GraphIndex;
use rekon_core::ontology::schema::Schema;
use rekon_core::text;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::layout::{self, EdgePath, IRect};
use super::{Explorer, MAX_NAME, Sel};

const EDGE: Style = Style::new().fg(Color::DarkGray);
const EDGE_SELECTED: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
const LABEL: Style = Style::new().fg(Color::Gray).add_modifier(Modifier::ITALIC);
const SELECTED: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
const SEL_FOCUSED: Color = Color::Rgb(26, 50, 74);
const SEL_UNFOCUSED: Color = Color::Rgb(46, 48, 54);
const DIM: Style = Style::new().fg(Color::DarkGray);

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// Box-drawing character joining the given sides of a cell.
fn glyph(mask: u8) -> char {
    match mask {
        m if m == UP | DOWN => '│',
        m if m == LEFT | RIGHT => '─',
        m if m == DOWN | RIGHT => '┌',
        m if m == DOWN | LEFT => '┐',
        m if m == UP | RIGHT => '└',
        m if m == UP | LEFT => '┘',
        m if m == UP | DOWN | RIGHT => '├',
        m if m == UP | DOWN | LEFT => '┤',
        m if m == DOWN | LEFT | RIGHT => '┬',
        m if m == UP | LEFT | RIGHT => '┴',
        m if m == UP | DOWN | LEFT | RIGHT => '┼',
        UP | DOWN => '│',
        _ => '─',
    }
}

/// Dotted variant for cells crossed only by edges outside the hierarchy.
fn dotted(mask: u8) -> char {
    match glyph(mask) {
        '│' => '┆',
        '─' => '┄',
        g => g,
    }
}

/// Side of `me` that `other` (a neighboring cell) is on.
fn side(me: (i32, i32), other: (i32, i32)) -> u8 {
    match (other.0 - me.0, other.1 - me.1) {
        (0, d) if d < 0 => UP,
        (0, _) => DOWN,
        (d, _) if d < 0 => LEFT,
        _ => RIGHT,
    }
}

/// Color of a node type from the schema (`#rrggbb` or a name), gray when unknown.
pub fn type_color(schema: &Schema, kind: &str) -> Color {
    schema
        .type_def(kind)
        .and_then(|t| t.color.as_deref())
        .and_then(|c| Color::from_str(c).ok())
        .unwrap_or(Color::Gray)
}

fn put(buf: &mut Buffer, area: Rect, x: i32, y: i32, ch: char, style: Style) {
    if IRect::from(area).contains(x, y) {
        let mut s = [0u8; 4];
        buf[(x as u16, y as u16)]
            .set_symbol(ch.encode_utf8(&mut s))
            .set_style(style);
    }
}

/// Writes `s` from `x`, cell by cell, clipped to `area`; returns the columns used.
fn put_str(buf: &mut Buffer, area: Rect, x: i32, y: i32, s: &str, style: Style) -> i32 {
    let mut cx = x;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0) as i32;
        if w == 0 {
            continue;
        }
        if IRect::from(area).contains(cx, y) && IRect::from(area).contains(cx + w - 1, y) {
            let mut b = [0u8; 4];
            buf[(cx as u16, y as u16)]
                .set_symbol(ch.encode_utf8(&mut b))
                .set_style(style);
        }
        cx += w;
    }
    cx - x
}

/// `s` cut to `width` columns with "…".
fn fit(s: &str, width: i32) -> String {
    super::super::rows::fit(s, width.max(0) as usize)
}

pub fn draw(f: &mut TuiFrame, ex: &mut Explorer, focused: bool, message: Option<&str>) {
    let area = ex.area;
    let Some(g) = ex.graph.clone() else {
        let text = message.map(str::to_string).or(ex.error.clone()).unwrap_or_default();
        f.render_widget(Paragraph::new(Span::styled(text, DIM)).wrap(Wrap { trim: true }), area);
        return;
    };
    ex.frame = layout::build(ex, &g);
    if let Some(fraction) = ex.center.take()
        && let Some(r) = ex.sel.as_ref().and_then(|s| ex.frame.rect(s))
    {
        let a = IRect::from(area);
        let top = a.y + 1 + (f64::from(a.h - r.h - 2) * fraction) as i32;
        ex.pan(r.center().0 - (a.x + a.w / 2), r.y - top);
        ex.frame = layout::build(ex, &g);
    }
    if let Some(ids) = ex.reveal.take()
        && bring_into_view(ex, &ids)
    {
        ex.frame = layout::build(ex, &g);
    }
    let buf = f.buffer_mut();
    let sel = ex.sel.clone();
    draw_edges(buf, &ex.frame, sel.as_ref());
    draw_nodes(buf, ex, &g, focused);
    for e in &ex.frame.edges {
        let hot = matches!(&sel, Some(Sel::Edge(id)) if *id == e.id);
        if let Some((x, y, ch)) = e.exit
            && ex.frame.nodes.iter().any(|n| !n.compact && n.rect.contains(x, y))
        {
            put(buf, area, x, y, ch, if hot { EDGE_SELECTED } else { EDGE });
        }
        if let Some(l) = &e.label {
            put_str(buf, area, l.x, l.y, &l.text, if hot { EDGE_SELECTED } else { LABEL });
        }
    }
    if (ex.zoom - 1.0).abs() > 1e-9 {
        let z = format!(" {:.0}% ", ex.zoom * 100.0);
        let x = i32::from(area.x) + i32::from(area.width) - z.width() as i32;
        put_str(buf, area, x, i32::from(area.y), &z, DIM);
    }
}

/// Moves the camera as little as possible so the selection, and the listed nodes
/// when everything fits, are visible. Returns true when it moved.
fn bring_into_view(ex: &mut Explorer, ids: &[String]) -> bool {
    let a = IRect::from(ex.area);
    let Some(sel) = ex.sel.as_ref().and_then(|s| ex.frame.rect(s)) else {
        return false;
    };
    let mut want = sel;
    for id in ids {
        if let Some(n) = ex.frame.node(&Sel::Node(id.clone())) {
            let u = want.union(&n.rect);
            if u.w <= a.w - 2 && u.h < a.h {
                want = u;
            }
        }
    }
    let shift = |lo: i32, hi: i32, min: i32, max: i32| {
        if hi - lo > max - min || lo < min {
            lo - min
        } else if hi > max {
            hi - max
        } else {
            0
        }
    };
    let dx = shift(want.x, want.right(), a.x + 1, a.right() - 1);
    let dy = shift(want.y - 2, want.bottom(), a.y, a.bottom());
    if dx == 0 && dy == 0 {
        return false;
    }
    ex.pan(dx, dy);
    true
}

fn draw_edges(buf: &mut Buffer, frame: &layout::Frame, sel: Option<&Sel>) {
    let area = frame.area;
    let (w, h) = (usize::from(area.width), usize::from(area.height));
    if w == 0 || h == 0 {
        return;
    }
    let mut mask = vec![0u8; w * h];
    // Cells of hierarchy edges; the other ones are dotted.
    let mut solid = vec![false; w * h];
    let mut hot = vec![false; w * h];
    let index = |x: i32, y: i32| -> Option<usize> {
        IRect::from(area)
            .contains(x, y)
            .then(|| (y - i32::from(area.y)) as usize * w + (x - i32::from(area.x)) as usize)
    };
    let selected = |e: &EdgePath| matches!(sel, Some(Sel::Edge(id)) if *id == e.id);
    for e in &frame.edges {
        let cells = layout::cells(&e.points);
        let into_box = match e.exit.map(|x| x.2) {
            Some('┬') => UP,
            Some('┴') => DOWN,
            Some('├') => LEFT,
            _ => RIGHT,
        };
        for (k, &c) in cells.iter().enumerate() {
            let mut bits = 0;
            if k > 0 {
                bits |= side(c, cells[k - 1]);
            } else {
                bits |= into_box;
            }
            if k + 1 < cells.len() {
                bits |= side(c, cells[k + 1]);
            }
            if let Some(i) = index(c.0, c.1) {
                mask[i] |= bits;
                solid[i] |= !e.cross;
                hot[i] |= selected(e);
            }
        }
    }
    for (i, &m) in mask.iter().enumerate() {
        if m == 0 {
            continue;
        }
        let (x, y) = (i32::from(area.x) + (i % w) as i32, i32::from(area.y) + (i / w) as i32);
        let ch = if solid[i] { glyph(m) } else { dotted(m) };
        put(buf, area, x, y, ch, if hot[i] { EDGE_SELECTED } else { EDGE });
    }
    for e in &frame.edges {
        if let (Some(arrow), Some(&(x, y))) = (e.arrow, e.points.last()) {
            let style = if selected(e) { EDGE_SELECTED } else { EDGE };
            put(buf, area, x, y, arrow, style);
        }
    }
}

fn draw_nodes(buf: &mut Buffer, ex: &Explorer, g: &GraphIndex, focused: bool) {
    let area = ex.area;
    let schema = g.schema();
    // The selected box last, so it is on top.
    let mut boxes: Vec<&layout::NodeBox> = ex.frame.nodes.iter().collect();
    boxes.sort_by_key(|b| ex.sel.as_ref() == Some(&b.sel));
    for b in boxes {
        let selected = ex.sel.as_ref() == Some(&b.sel);
        let fill = selected.then_some(if focused { SEL_FOCUSED } else { SEL_UNFOCUSED });
        let r = b.rect;
        match &b.sel {
            Sel::More(parent) => {
                let count = ex.more.get(parent).map_or(0, |m| m.count);
                let label = text::more_neighbors(count);
                let border = if selected { SELECTED } else { DIM };
                if b.compact {
                    put_str(buf, area, r.x, r.y, &fit(&label, r.w), border);
                } else {
                    frame_box(buf, area, r, DASHED, border, fill);
                    put_str(buf, area, r.x + 2, r.y + 1, &fit(&label, r.w - 3), border);
                }
            }
            Sel::Node(id) => {
                let Some(n) = g.node(id) else { continue };
                let color = type_color(schema, &n.kind);
                let marker = if ex.nodes.get(id).is_some_and(|p| p.expanded) {
                    "▾"
                } else if ex.expandable(id) {
                    "▸"
                } else {
                    "·"
                };
                let name_style = if selected {
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::White)
                };
                let name: String = fit(&n.name, MAX_NAME as i32);
                if b.compact {
                    let style = Style::new().fg(color).add_modifier(if selected {
                        Modifier::REVERSED | Modifier::BOLD
                    } else {
                        Modifier::empty()
                    });
                    put_str(buf, area, r.x, r.y, &fit(&format!("{marker}{name}"), r.w), style);
                    continue;
                }
                let border = if selected { SELECTED } else { Style::new().fg(color) };
                let root = ex.graph.as_ref().and_then(|g| g.graph.root.as_deref()) == Some(id.as_str());
                let referenced = n.metadata.get("referenced").and_then(|v| v.as_bool()) == Some(true);
                let lines = if root {
                    DOUBLE
                } else if referenced {
                    DASHED
                } else {
                    PLAIN
                };
                frame_box(buf, area, r, lines, border, fill);
                put_str(buf, area, r.x + 1, r.y, &fit(&n.kind, r.w - 2), Style::new().fg(color));
                let used = put_str(buf, area, r.x + 1, r.y + 1, marker, border);
                put_str(
                    buf,
                    area,
                    r.x + 1 + used + 1,
                    r.y + 1,
                    &fit(&name, r.w - 4 - used),
                    name_style,
                );
                if !ex.nodes.get(id).is_some_and(|p| p.expanded) {
                    let hidden = ex.hidden(id).len();
                    if hidden > 0 {
                        let hint = format!("+{hidden}");
                        let x = r.right() - hint.width() as i32 - 1;
                        if x > r.x {
                            put_str(buf, area, x, r.bottom(), &hint, Style::new().fg(color));
                        }
                    }
                }
            }
            Sel::Edge(_) => {}
        }
    }
}

/// Lines of a box border: horizontal, vertical, then the corners ┌ ┐ └ ┘.
type Lines = [char; 6];
const PLAIN: Lines = ['─', '│', '┌', '┐', '└', '┘'];
/// The root: the analyzed system.
const DOUBLE: Lines = ['═', '║', '╔', '╗', '╚', '╝'];
/// Elements only referenced by other code, and "+N more".
const DASHED: Lines = ['╌', '╎', '┌', '┐', '└', '┘'];

/// Border of a box drawn with `lines`, its inside filled.
fn frame_box(buf: &mut Buffer, area: Rect, r: IRect, lines: Lines, style: Style, fill: Option<Color>) {
    let [horizontal, vertical, tl, tr, bl, br] = lines;
    let inside = match fill {
        Some(bg) => Style::new().bg(bg),
        None => Style::new(),
    };
    for y in r.y..=r.bottom() {
        for x in r.x..=r.right() {
            let top = y == r.y;
            let bottom = y == r.bottom();
            let ch = match (x == r.x, x == r.right(), top, bottom) {
                (true, _, true, _) => tl,
                (_, true, true, _) => tr,
                (true, _, _, true) => bl,
                (_, true, _, true) => br,
                (_, _, true, _) | (_, _, _, true) => horizontal,
                (true, _, _, _) | (_, true, _, _) => vertical,
                _ => ' ',
            };
            let st = if ch == ' ' { inside } else { style.patch(inside) };
            put(buf, area, x, y, ch, st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junctions_join_the_right_sides() {
        assert_eq!(glyph(UP | DOWN), '│');
        assert_eq!(glyph(DOWN | LEFT | RIGHT), '┬');
        assert_eq!(glyph(UP | DOWN | LEFT | RIGHT), '┼');
        assert_eq!(glyph(RIGHT), '─');
        assert_eq!(dotted(UP | DOWN), '┆');
        assert_eq!(dotted(DOWN | RIGHT), '┌');
        assert_eq!(side((5, 5), (5, 4)), UP);
        assert_eq!(side((5, 5), (6, 5)), RIGHT);
        let s = Schema::builtin();
        assert_eq!(type_color(&s, "Component"), Color::Rgb(0x5f, 0xaf, 0xd7));
        assert_eq!(type_color(&s, "Unknown"), Color::Gray);
    }
}
