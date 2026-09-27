//! Deterministic backend without network, for tests and TUI work without costs.
//! It reads the same prompts the real backends get, so prompt building is exercised too.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use super::{Backend, LlmRequest, LlmResponse, TaskKind};
use crate::ontology::extract::{KNOWN_MARKER, OUTLINE_MARKER, SYMBOLS_MARKER};
use crate::prompts::{CODE_MARKER, DIR_MARKER, FILE_MARKER, parse_numbered_line};

#[derive(Default)]
pub struct FakeBackend {
    /// Artificial latency per call (`REKON_FAKE_DELAY_MS`).
    pub delay: Duration,
    /// Paths left out of `files` answers (to test retries).
    pub omit: Mutex<Vec<String>>,
    /// Number of calls made.
    pub calls: AtomicUsize,
}

pub fn description(path: &str) -> String {
    format!("Opis testowy: {path}")
}

impl FakeBackend {
    pub fn from_env() -> Self {
        let delay = std::env::var("REKON_FAKE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(Duration::ZERO, Duration::from_millis);
        Self {
            delay,
            ..Default::default()
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Backend for FakeBackend {
    fn ask(&self, req: &LlmRequest) -> Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let json = match req.kind {
            TaskKind::Overview => json!({
                "overview": "Opis testowy: przegląd projektu",
                "summary": description("projekt"),
            }),
            TaskKind::Files => {
                let omit = self.omit.lock().unwrap().clone();
                let files: Vec<_> = marked(&req.input, FILE_MARKER)
                    .into_iter()
                    .filter(|p| !omit.contains(p))
                    .map(|p| json!({"path": p, "summary": description(&p)}))
                    .collect();
                json!({ "files": files })
            }
            TaskKind::Dirs => {
                let dirs: Vec<_> = marked(&req.input, DIR_MARKER)
                    .into_iter()
                    .map(|p| json!({"path": p, "summary": description(&p)}))
                    .collect();
                json!({ "dirs": dirs })
            }
            TaskKind::Segment => json!({ "blocks": split(&req.input) }),
            TaskKind::Ontology => ontology(&req.input),
        };
        Ok(LlmResponse {
            json,
            cost_usd: Some(0.0),
            output_tokens: None,
        })
    }
}

/// Values of lines like `--- file: src/a.rs ---`.
fn marked(input: &str, marker: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|l| l.strip_prefix(marker)?.strip_suffix(" ---").map(str::to_string))
        .collect()
}

/// Cuts the numbered code after blank lines into 2–7 blocks.
fn split(input: &str) -> Vec<serde_json::Value> {
    let code: Vec<(u32, &str)> = input
        .lines()
        .skip_while(|l| *l != CODE_MARKER)
        .skip(1)
        .filter_map(parse_numbered_line)
        .collect();
    let (Some(first), Some(last)) = (code.first(), code.last()) else {
        return Vec::new();
    };
    let (a, b) = (first.0, last.0);
    // Group starts: a line that follows a blank line and is not blank itself.
    let mut starts = vec![a];
    for w in code.windows(2) {
        if w[0].1.trim().is_empty() && !w[1].1.trim().is_empty() {
            starts.push(w[1].0);
        }
    }
    if starts.len() < 2 && b > a {
        starts = vec![a, a + (b - a).div_ceil(2)];
    }
    if starts.len() > 7 {
        let n = starts.len();
        starts = (0..7).map(|i| starts[i * n / 7]).collect();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let e = starts.get(i + 1).map_or(b, |n| n - 1);
            json!({"start": s, "end": e, "summary": format!("Opis testowy: linie {s}-{e}")})
        })
        .collect()
}

/// One file of an ontology request, as the fake reads it back.
#[derive(Default)]
struct OntologyFile {
    path: String,
    /// (start, end, kind, name) from the OUTLINE section.
    items: Vec<(u32, u32, String, String)>,
    code: Vec<(u32, String)>,
}

fn last_segment(name: &str) -> &str {
    name.rsplit([':', '.']).find(|s| !s.is_empty()).unwrap_or(name)
}

/// For each file: a Component named after the file (`orders module`), a DataEntity
/// per type and a Process per function of the outline (`reads`/`executes` from the
/// Component), and `calls` edges to types of other files and known elements that
/// the code mentions.
fn ontology(input: &str) -> serde_json::Value {
    let mut files: Vec<OntologyFile> = Vec::new();
    let mut known: Vec<(String, String)> = Vec::new();
    let mut section = "";
    for line in input.lines() {
        if line.starts_with(KNOWN_MARKER) || line.starts_with(SYMBOLS_MARKER) {
            section = "known";
        } else if let Some(rest) = line.strip_prefix(FILE_MARKER) {
            files.push(OntologyFile {
                path: rest.strip_suffix(" ---").unwrap_or(rest).to_string(),
                ..Default::default()
            });
            section = "file";
        } else if line == OUTLINE_MARKER {
            section = "outline";
        } else if line == CODE_MARKER {
            section = "code";
        } else if let Some(rest) = line.strip_prefix("- ").filter(|_| section == "known") {
            // `{type or kind} {name} ({path})`
            let rest = rest.rsplit_once(" (").map_or(rest, |(r, _)| r);
            if let Some((label, name)) = rest.split_once(' ') {
                let kind = if label.starts_with(char::is_uppercase) {
                    label
                } else {
                    "Component"
                };
                known.push((kind.to_string(), name.to_string()));
            }
        } else if let Some(rest) = line.strip_prefix("- ").filter(|_| section == "outline") {
            // `{start}-{end} {kind} {name}`
            let mut words = rest.splitn(3, ' ');
            let (Some(range), Some(kind), Some(name)) = (words.next(), words.next(), words.next()) else {
                continue;
            };
            let Some((a, b)) = range.split_once('-') else { continue };
            if let (Ok(a), Ok(b), Some(f)) = (a.parse(), b.parse(), files.last_mut()) {
                f.items.push((a, b, kind.to_string(), name.to_string()));
            }
        } else if section == "code"
            && let (Some((nr, text)), Some(f)) = (parse_numbered_line(line), files.last_mut())
        {
            f.code.push((nr, text.to_string()));
        }
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let reference = |kind: &str, name: &str| json!({"type": kind, "name": name});
    let evidence = |file: &str, a: u32, b: u32, symbol: Option<&str>| {
        let mut e = json!({"file": file, "start": a, "end": b, "reason": "Opis testowy"});
        if let Some(s) = symbol {
            e["symbol"] = json!(s);
        }
        json!([e])
    };
    for f in &files {
        let (Some(first), Some(last)) = (f.code.first(), f.code.last()) else {
            continue;
        };
        let file = f.path.rsplit('/').next().unwrap_or(&f.path);
        let module = format!("{} module", file.split('.').next().unwrap_or(file));
        nodes.push(
            json!({"type": "Component", "name": module, "description": description(&f.path),
            "confidence": 0.9, "evidence": evidence(&f.path, first.0, last.0, None)}),
        );
        for (a, b, kind, name) in &f.items {
            let (node_type, relation) = match kind.as_str() {
                "struct" | "enum" | "class" | "interface" | "trait" | "record" | "type" | "union" => {
                    ("DataEntity", "reads")
                }
                "fn" | "method" | "def" => ("Process", "executes"),
                _ => continue,
            };
            let symbol = last_segment(name);
            nodes.push(
                json!({"type": node_type, "name": name, "description": description(name),
                "confidence": 0.8, "evidence": evidence(&f.path, *a, *b, Some(symbol))}),
            );
            edges.push(json!({"source": reference("Component", &module), "relation": relation,
                "target": reference(node_type, name), "confidence": 0.8,
                "evidence": evidence(&f.path, *a, *a, Some(symbol))}));
        }
        let others = files.iter().filter(|g| g.path != f.path).flat_map(|g| {
            g.items
                .iter()
                .filter(|it| matches!(it.2.as_str(), "struct" | "enum" | "class" | "interface" | "trait"))
                .map(|it| ("DataEntity".to_string(), it.3.clone()))
        });
        let targets: Vec<(String, String)> = known.iter().cloned().chain(others).collect();
        for (kind, name) in &targets {
            let key = last_segment(name);
            if let Some((nr, _)) = f.code.iter().find(|(_, text)| text.contains(key)) {
                edges.push(json!({"source": reference("Component", &module), "relation": "calls",
                    "target": reference(kind, name), "confidence": 0.7,
                    "evidence": evidence(&f.path, *nr, *nr, Some(key))}));
            }
        }
    }
    json!({ "nodes": nodes, "edges": edges })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(lines: &[&str], from: u32) -> String {
        let mut s = format!("context\n{CODE_MARKER}\n");
        for (i, l) in lines.iter().enumerate() {
            s.push_str(&crate::prompts::numbered_line(from + i as u32, l));
            s.push('\n');
        }
        s
    }

    #[test]
    fn splits_after_blank_lines() {
        let blocks = split(&numbered(&["a", "b", "", "c", "", "d"], 10));
        let ranges: Vec<_> = blocks
            .iter()
            .map(|b| (b["start"].as_u64().unwrap(), b["end"].as_u64().unwrap()))
            .collect();
        assert_eq!(ranges, [(10, 12), (13, 14), (15, 15)]);
    }

    #[test]
    fn halves_without_blank_lines_and_caps_at_seven() {
        let blocks = split(&numbered(&["a", "b", "c", "d"], 1));
        assert_eq!(blocks.len(), 2);
        let many: Vec<&str> = (0..40).map(|i| if i % 2 == 1 { "" } else { "x" }).collect();
        let blocks = split(&numbered(&many, 1));
        assert_eq!(blocks.len(), 7);
        assert_eq!(blocks[6]["end"], 40);
    }
}
