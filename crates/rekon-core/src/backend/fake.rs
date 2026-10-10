//! Deterministic backend without network, for tests and TUI work without costs.
//! It reads the same prompts the real backends get, so prompt building is exercised too.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use super::{Backend, LlmRequest, LlmResponse, TaskKind};
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
            TaskKind::Review => review(&req.input),
            kind => insight(kind, &req.input),
        };
        Ok(LlmResponse {
            json,
            cost_usd: Some(0.0),
            output_tokens: None,
        })
    }
}

/// Lines after `marker` up to the first blank line.
fn section<'a>(input: &'a str, marker: &str) -> Vec<&'a str> {
    input
        .lines()
        .skip_while(|l| *l != marker)
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .collect()
}

/// Fixed answers of the insight features (decisions, cards, contracts, questions, traces).
fn insight(kind: TaskKind, input: &str) -> serde_json::Value {
    use crate::ai::{DECISIONS_MARKER, DIFF_MARKER, ELEMENTS_MARKER, FILES_MARKER};
    match kind {
        TaskKind::Decision => json!({
            "title": "Opis testowy: decyzja",
            "context": "Opis testowy: kontekst",
            "decision": "Opis testowy: wybór",
            "cost": "Opis testowy: koszt",
            "revisit_if": "Opis testowy: warunek rewizji",
            "assumptions": ["Opis testowy: założenie"],
        }),
        TaskKind::Check => {
            let first = section(input, DECISIONS_MARKER)
                .first()
                .and_then(|l| l.split(" | ").next())
                .unwrap_or("")
                .to_string();
            json!({ "findings": [{"decision": first, "why": "Opis testowy: zmiana narusza założenie"}] })
        }
        TaskKind::Card => {
            let diff: Vec<&str> = input.lines().skip_while(|l| *l != DIFF_MARKER).skip(1).collect();
            let file = diff.iter().find_map(|l| l.strip_prefix("+++ b/")).unwrap_or("").to_string();
            let line = diff
                .iter()
                .find_map(|l| {
                    let rest = l.strip_prefix("@@ -")?;
                    let plus = rest.split(" +").nth(1)?;
                    plus.split([',', ' ']).next()?.parse::<u32>().ok()
                })
                .unwrap_or(1);
            json!({ "cards": [{
                "title": "Opis testowy: zmiana zachowania",
                "before": "Opis testowy: przed",
                "after": "Opis testowy: po",
                "consequences": ["Opis testowy: konsekwencja"],
                "basis": [{"file": file, "line": line, "end_line": line, "note": "Opis testowy: podstawa"}],
            }] })
        }
        TaskKind::Contract => json!({
            "responsibility": "Opis testowy: odpowiedzialność",
            "input": "Opis testowy: wejście",
            "output": "Opis testowy: wynik",
            "rules": ["Opis testowy: reguła"],
            "examples": ["Opis testowy: przykład"],
        }),
        TaskKind::Impact => json!({
            "implementation": ["Opis testowy: zmień implementację"],
            "tests": ["Opis testowy: dodaj test"],
            "dependencies": ["Opis testowy: sprawdź wywołujących"],
        }),
        TaskKind::Question => {
            let files: Vec<&str> = section(input, FILES_MARKER)
                .iter()
                .filter_map(|l| l.split(" | ").next())
                .collect();
            let chosen: Vec<_> = files.iter().take(2).map(|p| json!({"path": p, "reason": "Opis testowy: istotny"})).collect();
            let skipped: Vec<_> = files.iter().skip(2).take(1).map(|p| json!({"path": p, "reason": "Opis testowy: pominięty"})).collect();
            json!({ "files": chosen, "skipped": skipped })
        }
        TaskKind::Pick => {
            let items: Vec<_> = section(input, ELEMENTS_MARKER)
                .iter()
                .filter_map(|l| l.split(" | ").next())
                .take(2)
                .map(|id| json!({"element": id, "role": "Opis testowy: rola", "reason": "Opis testowy: powód"}))
                .collect();
            json!({ "items": items })
        }
        TaskKind::Trace => json!({
            "input": "Opis testowy: przykładowe wejście",
            "steps": [
                {"label": "krok 1", "before": "a", "after": "b", "why": "Opis testowy: powód gałęzi", "line": 0},
                {"label": "krok 2", "before": "b", "after": "c", "why": "Opis testowy: powód", "line": 0},
            ],
            "result": "Opis testowy: wynik",
        }),
        _ => json!({}),
    }
}

/// A fixed answer that points at the first line of the AFTER listing.
fn review(input: &str) -> serde_json::Value {
    let first = input
        .lines()
        .skip_while(|l| *l != crate::prompts::AFTER_MARKER)
        .skip(1)
        .find_map(parse_numbered_line)
        .map_or(1, |(n, _)| n);
    json!({
        "assumed": "Opis testowy: dotychczasowe założenie",
        "now": "Opis testowy: co się zmieniło",
        "points": [{"text": "Opis testowy: zmiana", "line": first}],
        "effort": "review",
    })
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
