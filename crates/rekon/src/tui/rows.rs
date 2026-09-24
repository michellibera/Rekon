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

/// Header per visible block (`{indent}{marker} {start}–{end}  {summary}`); under a
/// collapsed block or a leaf its code lines (`{indent}│ {nr:>4} {code}`); an expanded
/// block shows its children one level deeper instead of its code.
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
        let range = format!("{}–{}", b.lines.0, b.lines.1);
        let used = indent.width() + marker.width() + 1 + range.width() + 2;
        let summary = fit(&b.summary, view.width.saturating_sub(used));
        out.push(Row {
            depth,
            kind: RowKind::BlockHeader,
            target: RowRef::Block(b.lines),
            line: Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(marker.to_string(), Style::new().fg(level_color(depth))),
                Span::raw(" "),
                Span::styled(range, DIM),
                Span::raw("  "),
                Span::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
            ]),
        });
        if expanded && has_children {
            push_blocks(view, b.children.as_deref().unwrap_or_default(), depth + 1, out);
        } else if !view.desc_only {
            for nr in b.lines.0..=b.lines.1 {
                let i = nr as usize - 1;
                let prefix = vec![
                    Span::raw(indent.clone()),
                    Span::styled("│ ", Style::new().fg(level_color(depth))),
                ];
                let raw = view.lines.get(i).map_or("", String::as_str);
                out.push(code_row(
                    nr,
                    depth,
                    prefix,
                    view.highlighted.and_then(|h| h.get(i)),
                    raw,
                ));
            }
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
        row.line.spans.iter().map(|s| s.content.as_ref()).collect()
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
                "▸ 1–2  First",
                "│    1 line1",
                "│    2 line2",
                "▸ 3–6  Second",
                "│    3 line3",
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
                "▸ 1–2  First",
                "│    1 line1",
                "│    2 line2",
                "▾ 3–6  Second",
                "  ▸ 3–4  Inner a",
                "  │    3 line3",
                "  │    4 line4",
                "  · 5–6  Inner b",
                "  │    5 line5",
                "  │    6 line6",
            ]
        );
    }

    #[test]
    fn descriptions_only_and_pending_marker() {
        assert_eq!(
            render(&[(3, 6)], &[(3, 4)], true),
            ["▸ 1–2  First", "▾ 3–6  Second", "  ⏳ 3–4  Inner a", "  · 5–6  Inner b"]
        );
    }
}
