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
    Ok(out)
}
