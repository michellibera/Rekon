//! User-facing texts shared by the CLI, the TUI and the hooks, kept in one place.

pub fn skipped(pattern: &str) -> String {
    format!("Skipped ({pattern})")
}

pub fn too_large(size: u64) -> String {
    format!("Too large to describe ({})", human_size(size))
}

pub fn binary(size: u64) -> String {
    format!("Binary file ({})", human_size(size))
}

pub const NO_DESCRIPTION: &str = "no description";
pub const PENDING: &str = "…";
pub const NOT_A_REPO: &str = "rekon needs a git repository (git rev-parse --show-toplevel failed)";
pub const NO_MAP: &str = "no .rekon/ map in this repository; run `rekon init` first";
pub const NO_GRAPH: &str = "no ontology in this repository yet; run `rekon ontology index` first";

pub fn claude_missing(err: &str) -> String {
    format!(
        "cannot run `claude --version` ({err}). Install Claude Code, or set \"backend\" in \
         .rekon/config.json (or REKON_BACKEND) to another backend, e.g. \"fake\" for testing"
    )
}

pub fn progress_line(files: (usize, usize), dirs: (usize, usize), errors: usize, cost: f64) -> String {
    format!(
        "Files {}/{} · Folders {}/{} · Errors {} · ~${:.2}",
        files.0, files.1, dirs.0, dirs.1, errors, cost
    )
}

pub fn ontology_progress(done: usize, total: usize, errors: usize, cost: f64) -> String {
    format!("Ontology {done}/{total} files · Errors {errors} · ~${cost:.2}")
}

pub fn stop_hook_reason(exe: &str, paths: &[String]) -> String {
    const MAX: usize = 20;
    let mut listed = paths.iter().take(MAX).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > MAX {
        listed.push_str(&format!(" and {} more", paths.len() - MAX));
    }
    let example: Vec<String> = paths
        .iter()
        .take(MAX)
        .map(|p| format!("{}: \"…\"", serde_json::to_string(p).unwrap_or_default()))
        .collect();
    format!(
        "The rekon map has outdated descriptions of files changed in this session: {listed}.\n\
         Update them with one command, following .rekon/style.md:\n\
         {exe} apply <<'EOF'\n\
         {{\"summaries\": {{{}}}}}\n\
         EOF",
        example.join(", ")
    )
}

// TUI texts.

pub const TREE_TITLE: &str = "Tree";
pub const CODE_TITLE_EMPTY: &str = "Code";
pub const OVERVIEW_TITLE: &str = "Project overview";
pub const HELP_TITLE: &str = "Keys";
pub const NO_OVERVIEW: &str = "No overview yet. Run `rekon init` (or wait for the background init).";
pub const SELECT_FILE: &str = "Select a file in the tree to see its code.";
pub const SPLITTING: &str = "splitting into blocks…";
pub const HELP_HINT: &str = "? help";
pub const BLOCKS_OUTDATED: &str = "blocks outdated, r splits again";
pub const NO_BLOCKS: &str = "no blocks, r splits";

pub fn too_long_for_blocks(lines: u32, max: u32) -> String {
    format!("{lines} lines, more than max_segment_lines ({max}): no blocks")
}

pub fn status_line(jobs: usize, errors: usize, last_error: Option<&str>, cost: f64) -> String {
    let errors = match last_error {
        Some(e) if errors > 0 => format!("errors: {errors} ({e})"),
        _ => format!("errors: {errors}"),
    };
    format!("jobs: {jobs} · {errors} · session cost ~${cost:.2} · {HELP_HINT}")
}

pub const HELP: &[(&str, &str)] = &[
    ("1 / 2", "tree view / ontology view of the left panel"),
    ("↑/↓, k/j", "previous/next item (in the code panel: block header)"),
    ("→/l, Enter", "expand folder, open file, expand block"),
    ("←/h", "collapse or go to parent"),
    ("Tab", "switch panel"),
    ("PgUp/PgDn", "scroll by a page"),
    ("o", "descriptions only in the code panel"),
    ("w", "tree at full width (toggle)"),
    ("i", "project overview"),
    ("e", "open $EDITOR at the selected block"),
    ("r", "regenerate the selected item"),
    ("R", "refresh all outdated tree descriptions"),
    ("?", "this help"),
    ("q", "quit (Esc closes a window)"),
    ("mouse", "click selects and expands or collapses, wheel scrolls"),
];

// Ontology view.

pub const TAB_TREE: &str = "Tree";
pub const TAB_ONTOLOGY: &str = "Ontology";
pub const NO_ONTOLOGY: &str = "No ontology yet. R maps this repository onto the ontology with the model (like      `rekon ontology index`); later runs analyze only changed files.";
pub const ONTOLOGY_RUNNING: &str = "Analyzing the repository; the graph appears when the analysis ends.";
pub const NO_EVIDENCE: &str = "no evidence for this element";
pub const HELP_ONTOLOGY_TITLE: &str = "Ontology view";

/// Title of the code panel showing evidence: what it proves, which one, where, why.
pub fn evidence_title(element: &str, index: usize, total: usize, location: &str, reason: &str) -> String {
    let base = format!("{element} · evidence {index}/{total}: {location}");
    if reason.is_empty() {
        base
    } else {
        format!("{base} — {reason}")
    }
}

pub fn more_neighbors(count: usize) -> String {
    format!("+{count} more")
}

pub const NO_LINEAGE: &str = "no data lineage (derivedFrom) from this element";

pub fn ontology_done(analyzed: usize, nodes: usize, edges: usize, errors: usize) -> String {
    format!("ontology: {analyzed} files analyzed · {nodes} nodes · {edges} edges · {errors} errors")
}

/// Footer line of a node selected in the graph.
pub fn node_line(kind: &str, name: &str, description: &str, confidence: f64, evidence: usize, id: &str) -> String {
    format!("{kind} · {name} — {description} · confidence {confidence} · evidence {evidence} · {id}")
}

/// Footer line of an edge selected in the graph.
pub fn edge_line(title: &str, confidence: f64, evidence: usize) -> String {
    format!("{title} · confidence {confidence} · evidence {evidence} · Enter shows the code")
}

pub fn more_line(count: usize, name: &str) -> String {
    format!("{count} more neighbors of {name}: ↓, Space or Enter shows them")
}

pub const HELP_ONTOLOGY: &[(&str, &str)] = &[
    ("arrows, hjkl", "move to the nearest element in that direction"),
    ("↓ on ▸ node", "expand: its neighbors appear below it"),
    ("Space", "expand or collapse the selected node"),
    ("Backspace", "collapse the node (or the node above it)"),
    ("Enter", "show the evidence in the code panel (again: the next one)"),
    ("[ / ]", "previous / next evidence"),
    ("Esc", "select the node one level up"),
    ("+ / - / 0", "zoom in / out / back to 100%"),
    ("c / Home", "center on the selection / go to the root"),
    ("L", "reveal the data lineage (derivedFrom) of the selection"),
    ("R / r", "analyze changed files / the selection's files again"),
    (
        "mouse",
        "click selects, double click expands, drag pans or moves a node, wheel zooms",
    ),
];

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(3 * 1024 + 512), "3.5 KB");
        assert_eq!(human_size(50 * 1024 * 1024), "50 MB");
    }

    #[test]
    fn reason_lists_at_most_twenty() {
        let paths: Vec<String> = (0..23).map(|i| format!("f{i}.rs")).collect();
        let r = stop_hook_reason("rekon", &paths);
        assert!(r.contains("f19.rs and 3 more"));
        assert!(!r.contains("\"f20.rs\""));
        assert!(r.contains("rekon apply <<'EOF'"));
    }
}
