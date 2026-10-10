//! Pinned questions ("How do we prevent double charges?"). A question gets a view: the
//! elements that answer it, each with the role it plays and the reason, plus what was
//! left out. Two model calls keep it cheap: first the files (from their one-line
//! descriptions), then the elements of the chosen files. The scope that was NOT examined
//! is part of the answer, because a confident answer that silently skipped half the
//! repository is worse than none.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Ctx;
use crate::ai;
use crate::backend::TaskKind;
use crate::collection;
use crate::element;
use crate::model::Origin;
use crate::scan::{self, Excluder};
use crate::store::timestamp;

const KIND: &str = "questions";
const MAX_FILES_LISTED: usize = 600;
const MAX_FILES_CHOSEN: usize = 12;
const MAX_ELEMENTS_PER_FILE: usize = 60;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub element: String,
    pub path: String,
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub role: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Omitted {
    /// A file or a folder.
    pub path: String,
    pub reason: String,
    /// The model chose to skip it (else it simply was not examined).
    pub deliberate: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Answer {
    pub items: Vec<Item>,
    pub omitted: Vec<Omitted>,
    /// Files whose elements the model looked at, out of `total` files in scope.
    pub examined: usize,
    pub total: usize,
    pub at: String,
    pub origin: Origin,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub created: String,
    #[serde(default)]
    pub answer: Option<Answer>,
}

pub fn list(ctx: &Ctx) -> Vec<Question> {
    collection::list(&ctx.store, KIND)
}

pub fn get(ctx: &Ctx, query: &str) -> Option<Question> {
    collection::resolve(&ctx.store, KIND, query).map(|(_, q)| q)
}

/// Pins a question (without answering it yet).
pub fn pin(ctx: &Ctx, text: &str) -> Result<Question> {
    let text = ai::clean(text).ok_or_else(|| anyhow!("empty question"))?;
    let q = Question {
        id: collection::next_id(&ctx.store, KIND, &text),
        text,
        created: timestamp(std::time::SystemTime::now()),
        answer: None,
    };
    collection::put(&ctx.store, KIND, &q.id, &q)?;
    Ok(q)
}

pub fn remove(ctx: &Ctx, query: &str) -> Result<()> {
    let (id, _): (String, Question) =
        collection::resolve(&ctx.store, KIND, query).ok_or_else(|| anyhow!("no question matches {query}"))?;
    collection::remove(&ctx.store, KIND, &id);
    Ok(())
}

/// Source files in scope, with their one-line descriptions.
fn scope(ctx: &Ctx) -> Result<Vec<(String, String)>> {
    let (paths, _) = scan::git_files(&ctx.root)?;
    let include = Excluder::new(&ctx.config.scope.include)?;
    let excluded = Excluder::new(&ctx.config.exclude)?;
    Ok(paths
        .into_iter()
        .filter(|p| include.matched(p).is_some() && excluded.matched(p).is_none())
        .map(|p| {
            let summary = ctx.store.file_note(&p).summary.map(|s| s.text).unwrap_or_default();
            (p, summary)
        })
        .collect())
}

fn first_segment(path: &str) -> &str {
    path.split_once('/').map_or("(root files)", |(d, _)| d)
}

/// Answers a pinned question (again, when it already has an answer).
pub fn answer(ctx: &Ctx, query: &str) -> Result<Question> {
    let mut q = get(ctx, query).ok_or_else(|| anyhow!("no question matches {query}"))?;
    let files = scope(ctx)?;
    if files.is_empty() {
        bail!("no source files in scope");
    }
    // 1. Which files?
    let mut input = format!("QUESTION: {}\n\n{}\n", q.text, ai::FILES_MARKER);
    for (p, s) in files.iter().take(MAX_FILES_LISTED) {
        let s: String = s.chars().take(100).collect();
        input.push_str(&format!("{p} | {s}\n"));
    }
    let schema = ai::object(
        &["files", "skipped"],
        serde_json::json!({
            "files": ai::array(ai::object(&["path", "reason"], serde_json::json!({"path": ai::string(), "reason": ai::string()}))),
            "skipped": ai::array(ai::object(&["path", "reason"], serde_json::json!({"path": ai::string(), "reason": ai::string()}))),
        }),
    );
    let task = format!(
        "Pick at most {MAX_FILES_CHOSEN} files (from the list only) that hold the answer to the question. Also list \
         files or folders you looked at and deliberately skipped because they seem related but are not, with the reason."
    );
    let v = ai::ask_json(ctx, TaskKind::Question, &task, input, schema)?;
    let known = |p: &str| files.iter().any(|(f, _)| f == p);
    let mut chosen: Vec<String> = Vec::new();
    for f in v.get("files").and_then(Value::as_array).into_iter().flatten() {
        if let Some(p) = ai::text(f, "path").filter(|p| known(p) && !chosen.contains(p)) {
            chosen.push(p);
        }
    }
    chosen.truncate(MAX_FILES_CHOSEN);
    let mut omitted: Vec<Omitted> = Vec::new();
    for s in v.get("skipped").and_then(Value::as_array).into_iter().flatten() {
        let (Some(p), Some(reason)) = (ai::text(s, "path"), ai::text(s, "reason")) else {
            continue;
        };
        let dir = format!("{}/", p.trim_end_matches('/'));
        if (known(&p) || files.iter().any(|(f, _)| f.starts_with(&dir))) && !chosen.contains(&p) {
            omitted.push(Omitted {
                path: p,
                reason,
                deliberate: true,
            });
        }
    }
    if chosen.is_empty() {
        bail!("the model found no file that answers the question");
    }

    // 2. Which elements of those files?
    let mut listing = String::new();
    let mut valid: Vec<element::Element> = Vec::new();
    for p in &chosen {
        let Ok(bytes) = std::fs::read(ctx.root.join(p)) else {
            continue;
        };
        for e in element::elements(p, &String::from_utf8_lossy(&bytes))
            .into_iter()
            .take(MAX_ELEMENTS_PER_FILE)
        {
            let head: String = e.text.lines().next().unwrap_or("").trim().chars().take(80).collect();
            listing.push_str(&format!("{} | {} | {}-{} | {head}\n", e.id, e.kind, e.start, e.end));
            valid.push(e);
        }
    }
    let input = format!("QUESTION: {}\n\n{}\n{listing}", q.text, ai::ELEMENTS_MARKER);
    let schema = ai::object(
        &["items"],
        serde_json::json!({ "items": ai::array(ai::object(
            &["element", "role", "reason"],
            serde_json::json!({"element": ai::string(), "role": ai::string(), "reason": ai::string()}))) }),
    );
    let task = "Pick the elements (by exact id from the list) that take part in the answer to the question, in the \
                order someone should read them. For each: its role (e.g. entry point, validation, storage) and \
                why it matters for the question.";
    let v = ai::ask_json(ctx, TaskKind::Pick, task, input, schema)?;
    let mut items: Vec<Item> = Vec::new();
    for raw in v.get("items").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = ai::text(raw, "element") else { continue };
        let Some(e) = valid.iter().find(|e| e.id == id) else {
            continue;
        };
        if items.iter().any(|i| i.element == id) {
            continue;
        }
        items.push(Item {
            element: id,
            path: e.path.clone(),
            name: e.name.clone(),
            start: e.start,
            end: e.end,
            role: ai::text(raw, "role").unwrap_or_default(),
            reason: ai::text(raw, "reason").unwrap_or_default(),
        });
    }

    // What was not examined, deterministically.
    for p in &chosen {
        if !items.iter().any(|i| &i.path == p) {
            omitted.push(Omitted {
                path: p.clone(),
                reason: "read, nothing relevant to the question".into(),
                deliberate: true,
            });
        }
    }
    let mut untouched: Vec<&str> = Vec::new();
    for (p, _) in &files {
        let seg = first_segment(p);
        let covered = chosen.iter().any(|c| first_segment(c) == seg)
            || omitted
                .iter()
                .any(|o| o.path == seg || o.path.starts_with(&format!("{seg}/")));
        if !covered && !untouched.contains(&seg) {
            untouched.push(seg);
        }
    }
    for seg in untouched {
        omitted.push(Omitted {
            path: seg.to_string(),
            reason: "not examined".into(),
            deliberate: false,
        });
    }

    q.answer = Some(Answer {
        items,
        omitted,
        examined: chosen.len(),
        total: files.len(),
        at: timestamp(std::time::SystemTime::now()),
        origin: Origin::Ai,
    });
    collection::put(&ctx.store, KIND, &q.id, &q)?;
    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    #[test]
    fn answer_lists_items_and_the_scope_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::create_dir_all(root.join(".rekon")).unwrap();
        for (p, t) in [
            ("api/pay.rs", "pub fn charge() {\n    go();\n}\n"),
            ("api/order.rs", "pub fn place_order() {\n    charge();\n}\n"),
            ("api/other.rs", "pub fn unrelated() {}\n"),
            ("web/page.rs", "pub fn render_page() {}\n"),
        ] {
            std::fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            std::fs::write(root.join(p), t).unwrap();
        }
        let ctx = Ctx::with_backend(
            root,
            Config {
                backend: "fake".into(),
                ..Config::default()
            },
            Arc::new(FakeBackend::default()),
        );
        let q = pin(&ctx, "How do we avoid double charges?").unwrap();
        assert!(q.answer.is_none());
        let q = answer(&ctx, &q.id).unwrap();
        let a = q.answer.unwrap();
        assert_eq!(a.origin, Origin::Ai);
        assert_eq!((a.examined, a.total), (2, 4));
        assert!(!a.items.is_empty());
        assert!(a.items.iter().all(|i| i.start >= 1 && i.end >= i.start));
        // A deliberately skipped file and a folder nobody looked at are both reported.
        assert!(a.omitted.iter().any(|o| o.deliberate));
        assert!(
            a.omitted.iter().any(|o| !o.deliberate && o.path == "web"),
            "{:?}",
            a.omitted
        );
        remove(&ctx, &q.id).unwrap();
        assert!(list(&ctx).is_empty());
    }
}
