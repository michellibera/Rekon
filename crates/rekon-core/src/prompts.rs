//! Building prompts and schemas, and validating the answers.

use serde_json::{Value, json};

use crate::backend::{LlmRequest, TaskKind};
use crate::config::Config;
use crate::model::Block;
use crate::scan::{self, Tree};

const SYSTEM: &str = include_str!("../prompts/system.md");
const OVERVIEW: &str = include_str!("../prompts/overview.md");
const FILES: &str = include_str!("../prompts/files.md");
const DIRS: &str = include_str!("../prompts/dirs.md");
const SEGMENT: &str = include_str!("../prompts/segment.md");

/// Section markers in the stdin input; the fake backend parses them too.
pub const FILE_MARKER: &str = "--- file: ";
pub const DIR_MARKER: &str = "--- folder: ";
pub const CODE_MARKER: &str = "--- code ---";

const MAX_PATH_LIST: usize = 150_000;
const MAX_README: usize = 20_000;
const MAX_MANIFESTS: usize = 20;
const MAX_MANIFEST: usize = 8_000;
const MAX_CODE_LINE: usize = 400;
const COLLAPSE_DEPTH: usize = 3;

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "pubspec.yaml",
    "Package.swift",
    "CMakeLists.txt",
    "Makefile",
    "deno.json",
    "Directory.Build.props",
];

pub fn system(language: &str, style: &str) -> String {
    SYSTEM.replace("{language}", language).replace("{style}", style.trim())
}

pub fn schema(kind: TaskKind) -> Value {
    let item = |a: &str, b: &str| {
        json!({ "type": "object", "required": [a, b],
                "properties": { a: { "type": "string" }, b: { "type": "string" } } })
    };
    match kind {
        TaskKind::Overview => json!({ "type": "object", "required": ["overview", "summary"],
            "properties": { "overview": { "type": "string" }, "summary": { "type": "string" } } }),
        TaskKind::Files => json!({ "type": "object", "required": ["files"],
            "properties": { "files": { "type": "array", "items": item("path", "summary") } } }),
        TaskKind::Dirs => json!({ "type": "object", "required": ["dirs"],
            "properties": { "dirs": { "type": "array", "items": item("path", "summary") } } }),
        TaskKind::Segment => json!({ "type": "object", "required": ["blocks"],
            "properties": { "blocks": { "type": "array", "items": { "type": "object",
                "required": ["start", "end", "summary"],
                "properties": { "start": { "type": "integer" }, "end": { "type": "integer" },
                                "summary": { "type": "string" } } } } } }),
    }
}

fn request(config: &Config, system: &str, kind: TaskKind, task: String, input: String) -> LlmRequest {
    let model = match kind {
        TaskKind::Overview => &config.models.overview,
        TaskKind::Files | TaskKind::Dirs => &config.models.tree,
        TaskKind::Segment => &config.models.blocks,
    };
    LlmRequest {
        kind,
        model: model.clone(),
        system: system.to_string(),
        task,
        input,
        schema: schema(kind),
    }
}

/// `{nr:>5} | {text}`, the text cut to 400 characters.
pub fn numbered_line(nr: u32, text: &str) -> String {
    let text: String = text.chars().take(MAX_CODE_LINE).collect();
    format!("{nr:>5} | {text}")
}

pub fn parse_numbered_line(line: &str) -> Option<(u32, &str)> {
    let (nr, text) = line
        .split_once(" | ")
        .or_else(|| line.strip_suffix(" |").map(|n| (n, "")))?;
    Some((nr.trim().parse().ok()?, text))
}

fn display_path(path: &str) -> &str {
    if path.is_empty() { "." } else { path }
}

/// One line per file: `path (N lines)` or `path — label`; folders deeper than
/// three levels are collapsed when the list is too long.
fn path_list(tree: &Tree) -> String {
    let line = |i: usize| {
        let n = tree.node(i);
        let f = n.file.as_ref().expect("file node");
        match (f.label(), f.lines) {
            (Some(label), _) => format!("{} — {label}", n.path),
            (None, Some(lines)) => format!("{} ({lines} lines)", n.path),
            (None, None) => n.path.clone(),
        }
    };
    let files: Vec<usize> = tree.subtree("").into_iter().filter(|&i| !tree.node(i).is_dir).collect();
    let full: Vec<String> = files.iter().map(|&i| line(i)).collect();
    let joined = full.join("\n");
    if joined.len() <= MAX_PATH_LIST {
        return joined;
    }
    let mut out: Vec<String> = Vec::new();
    for i in tree.subtree("") {
        let n = tree.node(i);
        let depth = tree.depth(i);
        if depth > COLLAPSE_DEPTH + 1 || (depth == COLLAPSE_DEPTH + 1 && !n.is_dir) {
            continue;
        }
        if n.is_dir && depth == COLLAPSE_DEPTH {
            let count = tree.subtree(&n.path).iter().filter(|&&j| !tree.node(j).is_dir).count();
            out.push(format!("{}/ ({count} files)", n.path));
        } else if !n.is_dir {
            out.push(line(i));
        }
    }
    let mut text = out.join("\n");
    if text.len() > MAX_PATH_LIST {
        text = truncate_chars(&text, MAX_PATH_LIST);
    }
    text
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}\n…", &s[..i]),
        None => s.to_string(),
    }
}

fn read_text(root: &std::path::Path, rel: &str, max: usize) -> Option<String> {
    let bytes = std::fs::read(root.join(rel)).ok()?;
    Some(truncate_chars(&String::from_utf8_lossy(&bytes), max))
}

pub fn overview_request(config: &Config, system: &str, root: &std::path::Path, tree: &Tree) -> LlmRequest {
    let mut input = format!("PATHS:\n{}\n", path_list(tree));
    let readme = tree.node(scan::ROOT).children.iter().copied().find(|&i| {
        let n = tree.node(i);
        !n.is_dir && n.name.to_lowercase().starts_with("readme") && n.file.as_ref().is_some_and(|f| f.is_text())
    });
    if let Some(i) = readme
        && let Some(text) = read_text(root, &tree.node(i).path, MAX_README)
    {
        input.push_str(&format!("\n--- README ({}) ---\n{text}\n", tree.node(i).path));
    }
    let manifests = tree
        .subtree("")
        .into_iter()
        .filter(|&i| {
            let n = tree.node(i);
            !n.is_dir
                && tree.depth(i) <= 2
                && (MANIFESTS.contains(&n.name.as_str()) || n.name.ends_with(".csproj") || n.name.ends_with(".sln"))
                && n.file.as_ref().is_some_and(|f| f.is_text())
        })
        .take(MAX_MANIFESTS);
    for i in manifests {
        if let Some(text) = read_text(root, &tree.node(i).path, MAX_MANIFEST) {
            input.push_str(&format!("\n--- manifest: {} ---\n{text}\n", tree.node(i).path));
        }
    }
    request(config, system, TaskKind::Overview, OVERVIEW.trim().to_string(), input)
}

/// First `head_lines` lines of a file, capped at `max_chars`.
pub fn file_head(root: &std::path::Path, rel: &str, head_lines: usize, max_chars: usize) -> String {
    let Ok(bytes) = std::fs::read(root.join(rel)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let head: Vec<&str> = text.lines().take(head_lines).collect();
    truncate_chars(&head.join("\n"), max_chars)
}

/// Description shown for an entry in folder listings.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub description: Option<String>,
}

fn entries_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| {
            let name = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            match &e.description {
                Some(d) => format!("- {name} — {d}"),
                None => format!("- {name}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Batch of files from one folder: `heads` = (path, beginning of the file).
pub fn files_request(
    config: &Config,
    system: &str,
    overview: &str,
    folder: &str,
    entries: &[Entry],
    heads: &[(String, String)],
) -> LlmRequest {
    let mut input = format!(
        "PROJECT OVERVIEW:\n{overview}\n\nFOLDER: {}\nENTRIES:\n{}\n\nFILES TO DESCRIBE:\n",
        display_path(folder),
        entries_text(entries)
    );
    for (path, head) in heads {
        input.push_str(&format!("{FILE_MARKER}{path} ---\n{head}\n\n"));
    }
    request(config, system, TaskKind::Files, FILES.trim().to_string(), input)
}

/// Batch of folders: each with its children and their descriptions.
pub fn dirs_request(config: &Config, system: &str, overview: &str, dirs: &[(String, Vec<Entry>)]) -> LlmRequest {
    let mut input = format!("PROJECT OVERVIEW:\n{overview}\n\n");
    for (path, entries) in dirs {
        input.push_str(&format!("{DIR_MARKER}{path} ---\n{}\n\n", entries_text(entries)));
    }
    request(config, system, TaskKind::Dirs, DIRS.trim().to_string(), input)
}

pub struct SegmentContext<'a> {
    pub project: Option<&'a str>,
    pub path: &'a str,
    pub file: Option<&'a str>,
    /// Level-1 blocks (empty at level 1).
    pub level1: &'a [Block],
    /// Descriptions from level 1 down to the block being split (empty at level 1).
    pub trail: &'a [&'a Block],
}

pub fn segment_request(
    config: &Config,
    system: &str,
    ctx: &SegmentContext,
    lines: &[&str],
    range: (u32, u32),
) -> LlmRequest {
    let mut input = String::new();
    if let Some(p) = ctx.project {
        input.push_str(&format!("PROJECT: {p}\n"));
    }
    input.push_str(&format!("FILE: {}", ctx.path));
    if let Some(f) = ctx.file {
        input.push_str(&format!(" — {f}"));
    }
    input.push('\n');
    if !ctx.level1.is_empty() {
        input.push_str("\nLEVEL 1 BLOCKS:\n");
        for b in ctx.level1 {
            input.push_str(&format!("- {}-{} {}\n", b.lines.0, b.lines.1, b.summary));
        }
    }
    if !ctx.trail.is_empty() {
        input.push_str("\nDESCRIPTIONS DOWN TO THE BLOCK BEING SPLIT:\n");
        for (depth, b) in ctx.trail.iter().enumerate() {
            input.push_str(&format!(
                "{}- {}-{} {}\n",
                "  ".repeat(depth),
                b.lines.0,
                b.lines.1,
                b.summary
            ));
        }
    }
    input.push_str(&format!("\n{CODE_MARKER}\n"));
    for nr in range.0..=range.1 {
        let text = lines.get(nr as usize - 1).copied().unwrap_or("");
        input.push_str(&numbered_line(nr, text));
        input.push('\n');
    }
    let task = SEGMENT
        .trim()
        .replace("{start}", &range.0.to_string())
        .replace("{end}", &range.1.to_string());
    request(config, system, TaskKind::Segment, task, input)
}

/// One-line description: newlines become spaces, whitespace collapsed; empty → `None`.
pub fn clean_text(s: &str) -> Option<String> {
    let text = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// `(path, summary)` pairs from a `files`/`dirs` answer; empty descriptions dropped,
/// trailing slashes removed from paths.
pub fn parse_path_summaries(json: &Value, key: &str) -> Vec<(String, String)> {
    json.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|it| {
                    let path = it.get("path")?.as_str()?.trim().trim_end_matches('/').to_string();
                    let summary = clean_text(it.get("summary")?.as_str()?)?;
                    Some((path, summary))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A block as returned by the model, before repair.
#[derive(Clone, Debug, PartialEq)]
pub struct RawBlock {
    pub start: i64,
    pub end: i64,
    pub summary: String,
}

pub fn parse_blocks(json: &Value) -> Vec<RawBlock> {
    json.get("blocks")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|it| {
                    Some(RawBlock {
                        start: it.get("start")?.as_i64()?,
                        end: it.get("end")?.as_i64()?,
                        summary: it.get("summary")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_lines_roundtrip_and_cut() {
        let long = "x".repeat(500);
        let line = numbered_line(7, &long);
        assert!(line.starts_with("    7 | "));
        assert_eq!(parse_numbered_line(&line).unwrap().1.len(), 400);
        assert_eq!(parse_numbered_line(&numbered_line(12, "")), Some((12, "")));
        assert_eq!(parse_numbered_line("   3 | a | b"), Some((3, "a | b")));
    }

    #[test]
    fn answers_are_cleaned() {
        let json = json!({"files": [
            {"path": "a.rs", "summary": " Line one\nline two "},
            {"path": "b.rs", "summary": "   "},
            {"path": "c/", "summary": "C"},
            {"summary": "no path"}
        ]});
        assert_eq!(
            parse_path_summaries(&json, "files"),
            [
                ("a.rs".to_string(), "Line one line two".to_string()),
                ("c".to_string(), "C".to_string())
            ]
        );
    }

    #[test]
    fn system_prompt_contains_language_and_style() {
        let s = system("pl", "# Style\n- short");
        assert!(s.contains("Language of the descriptions: pl."));
        assert!(s.ends_with("- short\n") || s.ends_with("- short"));
    }

    #[test]
    fn segment_input_has_context_only_below_level_one() {
        let config = Config::default();
        let lines = ["a", "b", "c"];
        let ctx = SegmentContext {
            project: Some("P"),
            path: "x.rs",
            file: Some("F"),
            level1: &[],
            trail: &[],
        };
        let req = segment_request(&config, "sys", &ctx, &lines, (1, 3));
        assert!(!req.input.contains("LEVEL 1"));
        assert!(req.input.contains("    3 | c"));
        assert!(req.task.contains("lines 1-3"));
        assert_eq!(req.model, "sonnet");
        let l1 = [Block::new(1, 2, "top")];
        let trail = [&l1[0]];
        let ctx = SegmentContext {
            level1: &l1,
            trail: &trail,
            ..ctx
        };
        let req = segment_request(&config, "sys", &ctx, &lines, (1, 2));
        assert!(req.input.contains("LEVEL 1 BLOCKS:\n- 1-2 top"));
        assert!(!req.input.contains("    3 | c"));
    }
}
