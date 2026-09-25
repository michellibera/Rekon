//! Row model shared by both panels: a tree flattened into the list of visible rows.
//! Selection and scrolling are indices into that list; a click hits row `y + offset`.

use std::collections::HashSet;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rekon_core::model::Block;
use rekon_core::scan::{ROOT, Tree};
use rekon_core::text;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Description state of a tree item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Desc {
    Fresh(String),
    Stale(String),
    Missing,
    /// Missing, and a job that will write it is running.
    Pending,
    /// Never described by the model (skipped, binary, too large).
    Static(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Dir,
    File,
    BlockHeader,
    CodeLine,
}

/// What a row points at.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowRef {
    /// Tree node by path.
    Node(String),
    /// Code block by line range.
    Block((u32, u32)),
    /// Code line number.
    Line(u32),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub depth: usize,
    pub kind: RowKind,
    pub target: RowRef,
    pub line: Line<'static>,
}

pub const DIM: Style = Style::new().fg(Color::DarkGray);
pub const WARN: Style = Style::new().fg(Color::Yellow);

/// Tree nodes visible with the given expanded folders: (node, depth), root excluded.
pub fn visible_nodes(tree: &Tree, expanded: &HashSet<String>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = tree.node(ROOT).children.iter().rev().map(|&c| (c, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        out.push((i, depth));
        let n = tree.node(i);
        if n.is_dir && expanded.contains(&n.path) {
            stack.extend(n.children.iter().rev().map(|&c| (c, depth + 1)));
        }
    }
    out
}

/// Cuts `s` to `width` terminal columns, ending with "…" when cut.
pub fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// One tree row: indent, marker, name (folders bold), two spaces, dim description.
pub fn tree_row(tree: &Tree, i: usize, depth: usize, expanded: bool, desc: &Desc, width: usize) -> Row {
    let n = tree.node(i);
    let stale = matches!(desc, Desc::Stale(_));
    let marker = match (stale, n.is_dir, expanded) {
        (true, _, _) => "⚠",
        (false, true, true) => "▾",
        (false, true, false) => "▸",
        (false, false, _) => " ",
    };
    let indent = "  ".repeat(depth);
    let name = if n.is_dir {
        format!("{}/", n.name)
    } else {
        n.name.clone()
    };
    let prefix_width = indent.width() + marker.width() + 1;
    let name = fit(&name, width.saturating_sub(prefix_width));
    let rest = width.saturating_sub(prefix_width + name.width() + 2);
    let (desc_text, desc_style) = match desc {
        Desc::Fresh(t) => (t.clone(), DIM),
        Desc::Stale(t) => (t.clone(), DIM),
        Desc::Missing => (text::NO_DESCRIPTION.to_string(), DIM.add_modifier(Modifier::ITALIC)),
        Desc::Pending => (text::PENDING.to_string(), DIM),
        Desc::Static(t) => (t.clone(), DIM.add_modifier(Modifier::ITALIC)),
    };
    let name_style = if n.is_dir {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    let mut spans = vec![
        Span::raw(indent),
        Span::styled(marker.to_string(), if stale { WARN } else { Style::new() }),
        Span::raw(" "),
        Span::styled(name, name_style),
    ];
    if rest > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(fit(&desc_text, rest), desc_style));
    }
    Row {
        depth,
        kind: if n.is_dir { RowKind::Dir } else { RowKind::File },
        target: RowRef::Node(n.path.clone()),
        line: Line::from(spans),
    }
}

/// Code line with its number: `{nr:>4} {code}`, prefixed by `prefix` spans.
pub fn code_row(nr: u32, depth: usize, prefix: Vec<Span<'static>>, code: Option<&Line<'static>>, raw: &str) -> Row {
    let mut spans = prefix;
    spans.push(Span::styled(format!("{nr:>4} "), DIM));
    match code {
        Some(line) => spans.extend(line.spans.iter().cloned()),
        None => spans.push(Span::raw(raw.replace('\t', "    "))),
    }
    Row {
        depth,
        kind: RowKind::CodeLine,
        target: RowRef::Line(nr),
        line: Line::from(spans),
    }
}

/// Colors of the bar in front of code lines, one per block level, cyclic.
const LEVEL_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::Blue,
    Color::Red,
];

pub fn level_color(depth: usize) -> Color {
    LEVEL_COLORS[depth % LEVEL_COLORS.len()]
}

/// Inputs of the code panel with blocks.
pub struct BlockView<'a> {
    pub blocks: &'a [Block],
    pub expanded: &'a dyn Fn((u32, u32)) -> bool,
    /// A split of this block is running.
    pub pending: &'a dyn Fn((u32, u32)) -> bool,
    /// Headers only (`o`).
    pub desc_only: bool,
    pub lines: &'a [String],
    pub highlighted: Option<&'a [Line<'static>]>,
    pub width: usize,
}

/// A block description may add rows under short code up to this many lines.
const MAX_DESC_LINES: usize = 4;
/// Below this panel width blocks get no description column (the footer still has it).
const MIN_WIDTH_FOR_DESC: usize = 40;

/// Width of the description column to the right of the code.
fn desc_width(width: usize) -> usize {
    if width < MIN_WIDTH_FOR_DESC {
        0
    } else {
        (width * 2 / 5).clamp(16, 48)
    }
}

/// Word-wraps `s` to lines of at most `width` columns; overlong words are cut.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let word = fit(word, width);
        if cur.is_empty() {
            cur = word;
        } else if cur.width() + 1 + word.width() <= width {
            cur.push(' ');
            cur.push_str(&word);
        } else {
            out.push(std::mem::replace(&mut cur, word));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `s` wrapped into at most `rows` lines; the last one ends with "…" when text is left.
fn wrap_into(s: &str, width: usize, rows: usize) -> Vec<String> {
    let mut lines = wrap(s, width);
    if lines.len() > rows {
        lines.truncate(rows);
        if let Some(last) = lines.last_mut() {
            *last = fit(&format!("{last}…"), width);
        }
    }
    lines
}

/// Keeps the first `width` columns of `spans`; returns them with the width used.
fn clip(spans: Vec<Span<'static>>, width: usize) -> (Vec<Span<'static>>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = s.content.width();
        if used + w <= width {
            used += w;
            out.push(s);
            continue;
        }
        let mut cut = String::new();
        for c in s.content.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > width {
                break;
            }
            cut.push(c);
            used += cw;
        }
        out.push(Span::styled(cut, s.style));
        break;
    }
    (out, used)
}

/// Visible blocks, depth first. A block showing code (collapsed or leaf) is its code
/// lines, the first one carrying the marker instead of the bar
/// (`{indent}{marker} {nr:>4} {code}`), with the summary wrapped in a column on the
/// right (extra rows without code when the summary is longer than the code). A block
/// without code (expanded, or `o`) is a header line `{indent}{marker} {start}–{end}
/// {summary}` followed by its children one level deeper.
pub fn block_rows(view: &BlockView) -> Vec<Row> {
    let mut out = Vec::new();
    push_blocks(view, view.blocks, 0, &mut out);
    out
}

fn push_blocks(view: &BlockView, blocks: &[Block], depth: usize, out: &mut Vec<Row>) {
    for b in blocks {
        let has_children = matches!(&b.children, Some(c) if !c.is_empty());
        let expanded = (view.expanded)(b.lines);
        let pending = (view.pending)(b.lines);
        let marker = if pending {
            "⏳"
        } else if b.is_leaf() {
            "·"
        } else if expanded && has_children {
            "▾"
        } else {
            "▸"
        };
        let indent = "  ".repeat(depth);
        let bar = Style::new().fg(level_color(depth));
        let shows_children = expanded && has_children;
        if shows_children || view.desc_only {
            let range = format!("{}–{}", b.lines.0, b.lines.1);
            let used = indent.width() + marker.width() + 1 + range.width() + 2;
            let summary = fit(&b.summary, view.width.saturating_sub(used));
            out.push(Row {
                depth,
                kind: RowKind::BlockHeader,
                target: RowRef::Block(b.lines),
                line: Line::from(vec![
                    Span::raw(indent),
                    Span::styled(marker.to_string(), bar),
                    Span::raw(" "),
                    Span::styled(range, DIM),
                    Span::raw("  "),
                    Span::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
                ]),
            });
            if shows_children {
                push_blocks(view, b.children.as_deref().unwrap_or_default(), depth + 1, out);
            }
            continue;
        }

        let desc_w = desc_width(view.width);
        let code_w = view
            .width
            .saturating_sub(indent.width() + desc_w + usize::from(desc_w > 0));
        let count = (b.lines.1 + 1).saturating_sub(b.lines.0) as usize;
        let desc = wrap_into(&b.summary, desc_w, count.max(MAX_DESC_LINES));
        // A short block with a longer description gets extra rows without code.
        for k in 0..count.max(desc.len()) {
            let nr = b.lines.0 + k as u32;
            // A marker is narrower than the bar and its space, except the 2-column ⏳.
            let gutter = if k > 0 {
                "│ ".to_string()
            } else if marker.width() >= 2 {
                marker.to_string()
            } else {
                format!("{marker} ")
            };
            let mut row = if k < count {
                let i = nr as usize - 1;
                let raw = view.lines.get(i).map_or("", String::as_str);
                code_row(
                    nr,
                    depth,
                    vec![Span::styled(gutter, bar)],
                    view.highlighted.and_then(|h| h.get(i)),
                    raw,
                )
            } else {
                Row {
                    depth,
                    kind: RowKind::CodeLine,
                    target: RowRef::Line(b.lines.1),
                    line: Line::from(Span::styled(gutter, bar)),
                }
            };
            let (body, used) = clip(row.line.spans, code_w);
            let mut spans = vec![Span::raw(indent.clone())];
            spans.extend(body);
            spans.push(Span::raw(" ".repeat(code_w - used)));
            if let Some(d) = desc.get(k) {
                spans.push(Span::raw(" "));
                spans.push(Span::raw(d.clone()));
            }
            row.line = Line::from(spans);
            if k == 0 {
                row.kind = RowKind::BlockHeader;
                row.target = RowRef::Block(b.lines);
            }
            out.push(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rekon_core::scan::FileInfo;

    fn tree() -> Tree {
        Tree::from_entries(
            ["src/api/orders.rs", "src/main.rs", "README.md"]
                .iter()
                .map(|p| (p.to_string(), FileInfo::default())),
        )
    }

    #[test]
    fn visible_nodes_follow_expansion() {
        let t = tree();
        let paths = |exp: &[&str]| {
            let expanded: HashSet<String> = exp.iter().map(|s| s.to_string()).collect();
            visible_nodes(&t, &expanded)
                .iter()
                .map(|&(i, d)| format!("{d}:{}", t.node(i).path))
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&[]), ["0:src", "0:README.md"]);
        assert_eq!(paths(&["src"]), ["0:src", "1:src/api", "1:src/main.rs", "0:README.md"]);
        assert_eq!(
            paths(&["src/api"]),
            ["0:src", "0:README.md"],
            "hidden under a collapsed parent"
        );
    }

    #[test]
    fn fit_cuts_with_ellipsis() {
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("zażółć", 3), "za…");
        assert_eq!(fit("abc", 0), "");
    }

    fn text(row: &Row) -> String {
        let t: String = row.line.spans.iter().map(|s| s.content.as_ref()).collect();
        t.trim_end().to_string()
    }

    fn nested() -> Vec<Block> {
        let mut b = Block::new(3, 6, "Second");
        b.children = Some(vec![Block::new(3, 4, "Inner a"), Block::new(5, 6, "Inner b")]);
        b.children.as_mut().unwrap()[1].children = Some(Vec::new());
        vec![Block::new(1, 2, "First"), b]
    }

    fn render(expanded: &[(u32, u32)], pending: &[(u32, u32)], desc_only: bool) -> Vec<String> {
        let lines: Vec<String> = (1..=6).map(|i| format!("line{i}")).collect();
        let blocks = nested();
        let exp = |r: (u32, u32)| expanded.contains(&r);
        let pen = |r: (u32, u32)| pending.contains(&r);
        let view = BlockView {
            blocks: &blocks,
            expanded: &exp,
            pending: &pen,
            desc_only,
            lines: &lines,
            highlighted: None,
            width: 40,
        };
        block_rows(&view).iter().map(text).collect()
    }

    #[test]
    fn collapsed_blocks_show_their_code() {
        assert_eq!(
            render(&[], &[], false),
            [
                "▸    1 line1            First",
                "│    2 line2",
                "▸    3 line3            Second",
                "│    4 line4",
                "│    5 line5",
                "│    6 line6",
            ]
        );
    }

    #[test]
    fn expanded_block_shows_children_one_level_deeper() {
        assert_eq!(
            render(&[(3, 6)], &[], false),
            [
                "▸    1 line1            First",
                "│    2 line2",
                "▾ 3–6  Second",
                "  ▸    3 line3          Inner a",
                "  │    4 line4",
                "  ·    5 line5          Inner b",
                "  │    6 line6",
            ]
        );
    }

    #[test]
    fn wrap_breaks_on_words_and_marks_leftover_text() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefgh ij", 4), ["abc…", "ij"]);
        assert_eq!(wrap_into("one two three four", 7, 1), ["one tw…"]);
        assert!(wrap("anything", 0).is_empty());
    }

    #[test]
    fn long_summaries_add_rows_under_short_code() {
        let lines = vec!["x".to_string()];
        let blocks = vec![Block::new(1, 1, "a summary long enough for two lines")];
        let no = |_: (u32, u32)| false;
        let view = BlockView {
            blocks: &blocks,
            expanded: &no,
            pending: &no,
            desc_only: false,
            lines: &lines,
            highlighted: None,
            width: 40,
        };
        let rows = block_rows(&view);
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            [
                "▸    1 x                a summary long",
                "│                       enough for two",
                "│                       lines",
            ]
        );
        assert_eq!(rows[0].kind, RowKind::BlockHeader);
        assert!(rows[1..].iter().all(|r| r.kind == RowKind::CodeLine));
    }

    #[test]
    fn descriptions_only_and_pending_marker() {
        assert_eq!(
            render(&[(3, 6)], &[(3, 4)], true),
            ["▸ 1–2  First", "▾ 3–6  Second", "  ⏳ 3–4  Inner a", "  · 5–6  Inner b"]
        );
    }
}
