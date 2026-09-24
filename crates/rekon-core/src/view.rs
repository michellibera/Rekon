//! Text and JSON views of the map, shared by `tree`, `show` and `context --hook`.

use serde_json::{Value, json};

use crate::Ctx;
use crate::model::{Author, Block, Freshness, freshness};
use crate::scan::{ROOT, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Fresh,
    Stale,
    Missing,
    /// Never described (skipped, binary, too large); `label` holds the reason.
    Static,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Fresh => "fresh",
            State::Stale => "stale",
            State::Missing => "missing",
            State::Static => "static",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// Depth relative to the listed prefix (children of the prefix = 0).
    pub depth: usize,
    pub summary: Option<String>,
    pub by: Option<Author>,
    pub state: State,
    pub label: Option<String>,
}

/// Description state of one tree node (not the root).
pub fn item(ctx: &Ctx, tree: &Tree, i: usize, depth: usize) -> Item {
    let n = tree.node(i);
    let (summary, state, label) = if n.is_dir {
        let s = ctx.store.dir_note(&n.path).summary;
        let st = state_of(freshness(s.as_ref(), Some(&tree.dir_key(i))));
        (s, st, None)
    } else {
        let f = n.file.as_ref();
        match f.and_then(|f| f.label()) {
            Some(label) => (None, State::Static, Some(label)),
            None => {
                let s = ctx.store.file_note(&n.path).summary;
                let st = state_of(freshness(s.as_ref(), f.and_then(|f| f.hash.as_deref())));
                (s, st, None)
            }
        }
    };
    Item {
        path: n.path.clone(),
        name: n.name.clone(),
        is_dir: n.is_dir,
        depth,
        by: summary.as_ref().map(|s| s.by),
        summary: summary.map(|s| s.text),
        state,
        label,
    }
}

fn state_of(f: Freshness) -> State {
    match f {
        Freshness::Fresh => State::Fresh,
        Freshness::Stale => State::Stale,
        Freshness::Missing => State::Missing,
    }
}

/// Items below `prefix` in tree order, down to `max_depth` levels (None = all).
pub fn items(ctx: &Ctx, tree: &Tree, prefix: &str, max_depth: Option<usize>) -> Vec<Item> {
    let Some(start) = tree.get(prefix) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if !tree.node(start).is_dir {
        out.push(item(ctx, tree, start, 0));
        return out;
    }
    let mut stack: Vec<(usize, usize)> = tree.node(start).children.iter().rev().map(|&c| (c, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        out.push(item(ctx, tree, i, depth));
        if tree.node(i).is_dir && max_depth.is_none_or(|m| depth + 1 < m) {
            stack.extend(tree.node(i).children.iter().rev().map(|&c| (c, depth + 1)));
        }
    }
    out
}

pub struct ProjectView {
    pub name: String,
    pub summary: Option<String>,
    pub overview: Option<String>,
    pub state: State,
}

pub fn project(ctx: &Ctx, tree: &Tree) -> ProjectView {
    let note = ctx.store.project_note();
    let state = state_of(freshness(note.summary.as_ref(), Some(&tree.dir_key(ROOT))));
    ProjectView {
        name: ctx
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string(),
        summary: note.summary.map(|s| s.text),
        overview: note.overview,
        state,
    }
}

fn item_line(it: &Item, flat: bool) -> String {
    let name = if flat { it.path.clone() } else { it.name.clone() };
    let name = if it.is_dir { format!("{name}/") } else { name };
    let indent = if flat { String::new() } else { "  ".repeat(it.depth) };
    let mark = if it.state == State::Stale { "⚠ " } else { "" };
    let desc = match (&it.label, &it.summary) {
        (Some(l), _) => l.clone(),
        (None, Some(s)) => s.clone(),
        (None, None) => format!("({})", crate::text::NO_DESCRIPTION),
    };
    format!("{indent}{mark}{name} — {desc}")
}

/// Overview and the tree with descriptions, one line per element.
pub fn tree_text(ctx: &Ctx, tree: &Tree, prefix: &str, max_depth: Option<usize>, stale_only: bool) -> String {
    let p = project(ctx, tree);
    let mut out = String::new();
    let mark = if p.state == State::Stale { "⚠ " } else { "" };
    out.push_str(&format!(
        "{mark}{} — {}\n",
        p.name,
        p.summary
            .as_deref()
            .unwrap_or(&format!("({})", crate::text::NO_DESCRIPTION))
    ));
    if let Some(o) = &p.overview {
        out.push_str(&format!("\n{}\n", o.trim()));
    }
    out.push('\n');
    for it in items(ctx, tree, prefix, max_depth) {
        if stale_only && !matches!(it.state, State::Stale | State::Missing) {
            continue;
        }
        out.push_str(&item_line(&it, stale_only));
        out.push('\n');
    }
    out
}

fn item_json(it: &Item) -> Value {
    json!({
        "path": if it.is_dir { format!("{}/", it.path) } else { it.path.clone() },
        "kind": if it.is_dir { "dir" } else { "file" },
        "depth": it.depth,
        "summary": it.summary,
        "by": it.by,
        "state": it.state.name(),
        "label": it.label,
    })
}

pub fn tree_json(ctx: &Ctx, tree: &Tree, prefix: &str, max_depth: Option<usize>, stale_only: bool) -> Value {
    let p = project(ctx, tree);
    let items: Vec<Value> = items(ctx, tree, prefix, max_depth)
        .iter()
        .filter(|it| !stale_only || matches!(it.state, State::Stale | State::Missing))
        .map(item_json)
        .collect();
    json!({
        "project": { "name": p.name, "summary": p.summary, "overview": p.overview, "state": p.state.name() },
        "items": items,
    })
}

fn blocks_text(blocks: &[Block], depth: usize, out: &mut String) {
    for b in blocks {
        let mark = match &b.children {
            None => "▸",
            Some(c) if c.is_empty() => "·",
            Some(_) => "▾",
        };
        out.push_str(&format!(
            "{}{mark} {}–{}  {}\n",
            "  ".repeat(depth),
            b.lines.0,
            b.lines.1,
            b.summary
        ));
        if let Some(c) = &b.children {
            blocks_text(c, depth + 1, out);
        }
    }
}

/// Note of a file or folder (with blocks), as text.
pub fn show_text(ctx: &Ctx, tree: &Tree, i: usize) -> String {
    let it = item(ctx, tree, i, 0);
    let mut out = item_line(&Item { depth: 0, ..it.clone() }, true);
    out.push('\n');
    if let Some(by) = it.by {
        out.push_str(&format!("state: {} · by: {}\n", it.state.name(), author(by)));
    } else {
        out.push_str(&format!("state: {}\n", it.state.name()));
    }
    if !it.is_dir {
        let note = ctx.store.file_note(&it.path);
        let key = tree.node(i).file.as_ref().and_then(|f| f.hash.clone());
        match note.blocks {
            Some(b) if Some(&b.hash) == key.as_ref() => {
                out.push_str("\nblocks:\n");
                blocks_text(&b.items, 1, &mut out);
            }
            Some(_) => out.push_str("\nblocks: outdated\n"),
            None => out.push_str("\nblocks: none\n"),
        }
    }
    out
}

pub fn show_json(ctx: &Ctx, tree: &Tree, i: usize) -> Value {
    let it = item(ctx, tree, i, 0);
    let mut v = item_json(&it);
    if !it.is_dir {
        let note = ctx.store.file_note(&it.path);
        let key = tree.node(i).file.as_ref().and_then(|f| f.hash.clone());
        let fresh = note.blocks.as_ref().is_some_and(|b| Some(&b.hash) == key.as_ref());
        v["blocks"] = json!(note.blocks.map(|b| b.items));
        v["blocks_state"] = json!(if fresh {
            "fresh"
        } else if v["blocks"].is_null() {
            "missing"
        } else {
            "stale"
        });
    }
    v
}

fn author(a: Author) -> &'static str {
    match a {
        Author::Auto => "auto",
        Author::Agent => "agent",
    }
}
