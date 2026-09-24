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
