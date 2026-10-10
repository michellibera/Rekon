//! `rekon context --hook`: SessionStart hook of Claude Code. Prints the project
//! overview and the tree to depth 2, so the agent knows the structure from the start.
//! Plain stdout of a SessionStart hook is added to the session context.

use anyhow::Result;
use serde_json::Value;

use crate::Ctx;
use crate::check::hook_cwd;
use crate::scan::{self, Tree};
use crate::view;

const MAX_LINES: usize = 150;
const DEPTH: usize = 2;

pub fn session_start_hook(input: &str, exe: &str) -> Result<Option<String>> {
    let input: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let Ok(root) = scan::find_root(&hook_cwd(&input)) else {
        return Ok(None);
    };
    let ctx = Ctx::at_root(&root)?;
    if !ctx.store.exists() {
        return Ok(None);
    }
    Ok(Some(render(&ctx, exe)?))
}

pub fn render(ctx: &Ctx, exe: &str) -> Result<String> {
    let (mut tree, _) = Tree::list(&ctx.root)?;
    // Only files that are shown need classification (labels for skipped files).
    let shown: Vec<usize> = (0..tree.nodes.len())
        .filter(|&i| !tree.node(i).is_dir && tree.depth(i) <= DEPTH)
        .collect();
    tree.analyze_all(&ctx.root, &ctx.config, Some(&shown))?;
    let body = view::tree_text(ctx, &tree, "", Some(DEPTH), false);
    let mut lines: Vec<&str> = body.lines().collect();
    let total = lines.len();
    let mut out = format!(
        "Map of this repository from rekon (.rekon/): project overview and tree to depth 2 with \
         one-line descriptions. More: `{exe} tree <folder>`, `{exe} show <path>`.\n\n"
    );
    if total > MAX_LINES {
        lines.truncate(MAX_LINES);
    }
    out.push_str(&lines.join("\n"));
    out.push('\n');
    if total > MAX_LINES {
        out.push_str(&format!("… {} more lines ({exe} tree)\n", total - MAX_LINES));
    }
    out.push_str(&insight_section(ctx, exe));
    Ok(out)
}

/// What the person and the agent share beyond descriptions: decisions in force,
/// specifications waiting for implementation, and how to record new ones. Empty when
/// nothing was recorded yet, so a plain map stays plain.
fn insight_section(ctx: &Ctx, exe: &str) -> String {
    use crate::{cards, decisions};
    let ds: Vec<_> = decisions::list(ctx)
        .into_iter()
        .filter(|d| d.status != decisions::Status::Superseded)
        .collect();
    let specs: Vec<_> = cards::list(ctx)
        .into_iter()
        .filter(|c| c.status == cards::Status::Proposed && c.origin == crate::model::Origin::Human)
        .collect();
    let changed = crate::review::changes(ctx).map_or(0, |c| c.len());
    if ds.is_empty() && specs.is_empty() && changed == 0 {
        return String::new();
    }
    let mut out = String::from("\nDecisions and specifications kept in rekon:\n");
    if !ds.is_empty() {
        out.push_str("Decisions in force (do not undo one silently; if a change breaks its assumptions, say so and replace it):\n");
        for d in ds.iter().take(15) {
            let mark = if d.status == decisions::Status::Proposed { " (unconfirmed, inferred by a model)" } else { "" };
            let on = if d.elements.is_empty() { String::new() } else { format!(" [{}]", d.elements.join(", ")) };
            out.push_str(&format!("- {}: {}{on}{mark}. Reconsider if: {}\n", d.id, d.decision, if d.revisit_if.is_empty() { "-" } else { &d.revisit_if }));
        }
    }
    if !specs.is_empty() {
        out.push_str(&format!("Specifications written by the user, waiting for implementation (`{exe} card brief <id>`):\n"));
        for c in specs.iter().take(10) {
            out.push_str(&format!("- {}: {}\n", c.id, c.title));
        }
    }
    if changed > 0 {
        out.push_str(&format!("{changed} element(s) changed since the user last reviewed them (`{exe} review status`); explain non-obvious changes.\n"));
    }
    out.push_str(&format!(
        "Record what you decide: `{exe} decision add` (JSON on stdin: title, decision, context, cost, revisit_if, assumptions[], elements[]) \
         for a design choice with a reason; `{exe} card add` (title, before, after, consequences[]) for a change of behaviour.\n"
    ));
    out
}
