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
