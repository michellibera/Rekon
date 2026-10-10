//! Behaviour-change cards: one card is one change of meaning, however many files it
//! touches. Before -> after -> consequences, with the basis (code, tests, traces) that
//! supports it. Cards come from a diff (a model reads it, its basis is checked against the
//! files) or from a person as a specification; a card written as a specification is a
//! proposal until code exists, and only a person marks it verified.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Ctx;
use crate::ai;
use crate::backend::TaskKind;
use crate::collection;
use crate::decisions;
use crate::element;
use crate::model::Origin;
use crate::store::timestamp;

const KIND: &str = "cards";
const MAX_DIFF: usize = 40_000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A specification: the behaviour is wanted, the code does not do it yet.
    Proposed,
    /// The code does it; nobody checked.
    Implemented,
    /// A person or a passing test confirmed it.
    Verified,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Implemented => "implemented",
            Status::Verified => "verified",
        }
    }
}

/// What supports a card.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Basis {
    /// `code`, `test` or `trace`.
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Card {
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub consequences: Vec<String>,
    #[serde(default)]
    pub basis: Vec<Basis>,
    /// Element ids the change touches.
    #[serde(default)]
    pub elements: Vec<String>,
    pub origin: Origin,
    pub status: Status,
    /// Where it came from: a diff range or `spec`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub created: String,
}

impl Card {
    pub fn spec(title: &str, before: &str, after: &str) -> Self {
        Self {
            id: String::new(),
            title: title.to_string(),
            before: before.to_string(),
            after: after.to_string(),
            consequences: Vec::new(),
            basis: Vec::new(),
            elements: Vec::new(),
            origin: Origin::Human,
            status: Status::Proposed,
            source: "spec".into(),
            created: String::new(),
        }
    }
}

pub fn list(ctx: &Ctx) -> Vec<Card> {
    collection::list(&ctx.store, KIND)
}

pub fn get(ctx: &Ctx, query: &str) -> Option<Card> {
    collection::resolve(&ctx.store, KIND, query).map(|(_, c)| c)
}

pub fn add(ctx: &Ctx, mut c: Card) -> Result<Card> {
    if c.title.trim().is_empty() {
        bail!("a card needs a title");
    }
    c.id = collection::next_id(&ctx.store, KIND, &c.title);
    c.created = timestamp(std::time::SystemTime::now());
    collection::put(&ctx.store, KIND, &c.id.clone(), &c)?;
    Ok(c)
}

pub fn set_status(ctx: &Ctx, query: &str, status: Status) -> Result<Card> {
    let mut c = get(ctx, query).ok_or_else(|| anyhow!("no card matches {query}"))?;
    c.status = status;
    collection::put(&ctx.store, KIND, &c.id, &c)?;
    Ok(c)
}

/// Cards that touch an element (by id or by file).
pub fn for_element(ctx: &Ctx, id: &str, path: &str) -> Vec<Card> {
    list(ctx)
        .into_iter()
        .filter(|c| c.elements.iter().any(|e| e == id) || c.basis.iter().any(|b| b.file == path))
        .collect()
}

/// Text for the editor: `title:`, then sections `--- before`, `--- after`,
/// `--- consequences` (one per `-` line).
pub fn template(c: &Card) -> String {
    let cons: String = c.consequences.iter().map(|x| format!("- {x}\n")).collect();
    format!(
        "title: {}\n\n--- before\n{}\n\n--- after\n{}\n\n--- consequences\n{cons}",
        c.title, c.before, c.after
    )
}

pub fn parse_template(text: &str) -> Result<Card> {
    let mut title = String::new();
    let mut section = String::new();
    let mut bodies: std::collections::HashMap<String, Vec<&str>> = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("--- ") {
            section = name.trim().to_string();
        } else if section.is_empty() {
            if let Some(v) = line.strip_prefix("title:") {
                title = v.trim().to_string();
            }
        } else {
            bodies.entry(section.clone()).or_default().push(line);
        }
    }
    let body = |n: &str| {
        bodies
            .get(n)
            .map(|l| l.join("\n").trim().to_string())
            .unwrap_or_default()
    };
    let mut c = Card::spec(&title, &body("before"), &body("after"));
    c.consequences = body("consequences")
        .lines()
        .map(|l| l.trim().trim_start_matches('-').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if c.title.is_empty() || c.after.is_empty() {
        bail!("a card needs a title and an `after` section");
    }
    Ok(c)
}

/// The task for an agent that should implement a card.
pub fn brief(c: &Card) -> String {
    let mut out = format!(
        "# Task: {}\n\nCurrent behaviour: {}\nWanted behaviour: {}\n",
        c.title, c.before, c.after
    );
    if !c.consequences.is_empty() {
        out.push_str("\nConsequences to handle:\n");
        for x in &c.consequences {
            out.push_str(&format!("- {x}\n"));
        }
    }
    if !c.basis.is_empty() {
        out.push_str("\nRelevant code:\n");
        for b in &c.basis {
            out.push_str(&format!("- {}:{}-{} {}\n", b.file, b.start_line, b.end_line, b.note));
        }
    }
    out.push_str("\nWrite or update a test that shows the wanted behaviour, then implement it. Keep unrelated behaviour unchanged.\n");
    out
}

/// Cards from a diff (working tree against HEAD by default). Each card's basis is checked
/// against the files: unknown files and lines outside the file are dropped.
pub fn from_diff(ctx: &Ctx, range: Option<&str>) -> Result<Vec<Card>> {
    let diff = decisions::git_diff(ctx, range)?;
    if diff.trim().is_empty() {
        bail!("no changes to describe");
    }
    let known = decisions::diff_paths(&diff);
    let clipped: String = diff.chars().take(MAX_DIFF).collect();
    let basis_schema = ai::object(
        &["file", "line", "end_line", "note"],
        serde_json::json!({"file": ai::string(), "line": ai::integer(), "end_line": ai::integer(), "note": ai::string()}),
    );
    let schema = ai::object(
        &["cards"],
        serde_json::json!({ "cards": ai::array(ai::object(
            &["title", "before", "after", "consequences", "basis"],
            serde_json::json!({"title": ai::string(), "before": ai::string(), "after": ai::string(),
                "consequences": ai::strings(), "basis": ai::array(basis_schema)}))) }),
    );
    let task = "Describe the changes of behaviour in this diff as cards. One card is one change of meaning, even \
                when it touches many files; formatting and renames are not cards. For each card give the \
                behaviour before, the behaviour after, the consequences for users or other code, and the \
                basis: file and line range in the NEW version that shows it. Say nothing you cannot see in the diff.";
    let v = ai::ask_json(
        ctx,
        TaskKind::Card,
        task,
        format!("{}\n{clipped}\n", ai::DIFF_MARKER),
        schema,
    )?;
    let mut out = Vec::new();
    for raw in v.get("cards").and_then(Value::as_array).into_iter().flatten() {
        let (Some(title), Some(before), Some(after)) =
            (ai::text(raw, "title"), ai::text(raw, "before"), ai::text(raw, "after"))
        else {
            continue;
        };
        let mut card = Card::spec(&title, &before, &after);
        card.origin = Origin::Ai;
        card.status = Status::Implemented;
        card.source = range.unwrap_or("git diff HEAD").to_string();
        card.consequences = ai::texts(raw, "consequences");
        for b in raw.get("basis").and_then(Value::as_array).into_iter().flatten() {
            let Some(file) = ai::text(b, "file").filter(|f| known.contains(f)) else {
                continue;
            };
            let total = std::fs::read_to_string(ctx.root.join(&file)).map_or(0, |t| t.lines().count() as u32);
            let start = b.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let end = (b.get("end_line").and_then(Value::as_u64).unwrap_or(0) as u32).max(start);
            if start == 0 || start > total {
                continue;
            }
            card.basis.push(Basis {
                kind: "code".into(),
                file: file.clone(),
                start_line: start,
                end_line: end.min(total),
                note: ai::text(b, "note").unwrap_or_default(),
            });
            if let Some(e) = element::at_line(&ctx.root, &file, start)
                && !card.elements.contains(&e.id)
            {
                card.elements.push(e.id);
            }
        }
        out.push(add(ctx, card)?);
    }
    if out.is_empty() {
        bail!("the model found no change of behaviour in the diff");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    fn repo() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.rs"), "fn checkout() {\n    charge();\n}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        std::fs::create_dir_all(root.join(".rekon")).unwrap();
        let ctx = Ctx::with_backend(
            root,
            Config {
                backend: "fake".into(),
                ..Config::default()
            },
            Arc::new(FakeBackend::default()),
        );
        (dir, ctx)
    }

    #[test]
    fn cards_come_from_a_diff_with_checked_basis() {
        let (d, ctx) = repo();
        assert!(from_diff(&ctx, None).is_err(), "no changes yet");
        std::fs::write(
            d.path().join("a.rs"),
            "fn checkout() {\n    charge();\n    retry();\n}\n",
        )
        .unwrap();
        let cards = from_diff(&ctx, None).unwrap();
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!((c.origin, c.status), (Origin::Ai, Status::Implemented));
        assert_eq!(c.basis.len(), 1);
        assert_eq!(c.basis[0].file, "a.rs");
        assert_eq!(c.elements, ["a.rs::a::checkout"]);
        assert_eq!(for_element(&ctx, "a.rs::a::checkout", "a.rs").len(), 1);
    }

    #[test]
    fn spec_cards_are_proposals_and_verified_by_people() {
        let (_d, ctx) = repo();
        let mut c = Card::spec("Retry", "Fails at once", "Retries three times");
        c.consequences = vec!["Show a separate status".into()];
        let c = add(&ctx, c).unwrap();
        assert_eq!((c.origin, c.status), (Origin::Human, Status::Proposed));
        assert!(brief(&c).contains("Retries three times"));
        assert_eq!(
            set_status(&ctx, &c.id, Status::Verified).unwrap().status,
            Status::Verified
        );
    }

    #[test]
    fn template_roundtrip() {
        let mut c = Card::spec("T", "b", "a");
        c.consequences = vec!["x".into(), "y".into()];
        let back = parse_template(&template(&c)).unwrap();
        assert_eq!(
            (back.title.as_str(), back.before.as_str(), back.after.as_str()),
            ("T", "b", "a")
        );
        assert_eq!(back.consequences, c.consequences);
        assert!(parse_template("title: x\n").is_err());
    }
}
