//! Short decisions pinned to elements ("why is it like this?"): the decision, its reason,
//! its cost, the assumptions it rests on and the condition for reconsidering it. A
//! replaced decision stays in the history with a link to its successor. A reason is
//! either recorded by a person or agent (`Human`) or reconstructed by a model (`Ai`), and
//! the two are never mixed up. `check` asks whether a change breaks an assumption.

use std::process::Command;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Ctx;
use crate::ai;
use crate::backend::TaskKind;
use crate::collection;
use crate::element;
use crate::model::Origin;
use crate::store::timestamp;

const KIND: &str = "decisions";
/// Longest diff sent to the model.
const MAX_DIFF: usize = 30_000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Suggested (for instance reconstructed by a model), not confirmed by a person.
    Proposed,
    Accepted,
    Superseded,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Accepted => "accepted",
            Status::Superseded => "superseded",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Decision {
    #[serde(default)]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub context: String,
    pub decision: String,
    /// What we accept in exchange.
    #[serde(default)]
    pub cost: String,
    /// When to think about it again.
    #[serde(default)]
    pub revisit_if: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    pub status: Status,
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Element ids, or paths (a path covers the whole file).
    #[serde(default)]
    pub elements: Vec<String>,
    pub origin: Origin,
    #[serde(default)]
    pub created: String,
}

impl Decision {
    pub fn new(title: &str, decision: &str) -> Self {
        Self {
            id: String::new(),
            title: title.to_string(),
            context: String::new(),
            decision: decision.to_string(),
            cost: String::new(),
            revisit_if: String::new(),
            assumptions: Vec::new(),
            status: Status::Accepted,
            superseded_by: None,
            elements: Vec::new(),
            origin: Origin::Human,
            created: String::new(),
        }
    }

    /// Whether this decision explains the element `id` (in file `path`).
    pub fn covers(&self, id: &str, path: &str) -> bool {
        self.elements
            .iter()
            .any(|e| e == id || e == path || id.starts_with(&format!("{e}::")))
    }
}

pub fn list(ctx: &Ctx) -> Vec<Decision> {
    collection::list(&ctx.store, KIND)
}

pub fn get(ctx: &Ctx, query: &str) -> Option<Decision> {
    collection::resolve(&ctx.store, KIND, query).map(|(_, d)| d)
}

/// Decisions that explain an element, replaced ones last.
pub fn for_element(ctx: &Ctx, id: &str, path: &str) -> Vec<Decision> {
    let mut v: Vec<Decision> = list(ctx).into_iter().filter(|d| d.covers(id, path)).collect();
    v.sort_by_key(|d| d.status == Status::Superseded);
    v
}

/// Saves a new decision and returns it with its id. Origin and status are the caller's.
pub fn add(ctx: &Ctx, mut d: Decision) -> Result<Decision> {
    if d.title.trim().is_empty() || d.decision.trim().is_empty() {
        bail!("a decision needs a title and the decision itself");
    }
    d.id = collection::next_id(&ctx.store, KIND, &d.title);
    d.created = timestamp(std::time::SystemTime::now());
    collection::put(&ctx.store, KIND, &d.id.clone(), &d)?;
    Ok(d)
}

fn save(ctx: &Ctx, d: &Decision) -> Result<()> {
    collection::put(&ctx.store, KIND, &d.id, d)
}

/// Confirms a proposed decision (for instance a reconstructed one).
pub fn accept(ctx: &Ctx, query: &str) -> Result<Decision> {
    let mut d = get(ctx, query).ok_or_else(|| anyhow!("no decision matches {query}"))?;
    d.status = Status::Accepted;
    save(ctx, &d)?;
    Ok(d)
}

/// Replaces `old` with a new decision: the old one stays, marked and linked.
pub fn supersede(ctx: &Ctx, old: &str, new: Decision) -> Result<Decision> {
    let mut old = get(ctx, old).ok_or_else(|| anyhow!("no decision matches {old}"))?;
    let new = add(ctx, new)?;
    old.status = Status::Superseded;
    old.superseded_by = Some(new.id.clone());
    save(ctx, &old)?;
    Ok(new)
}

/// Text for the editor, and its reverse. Fields: `title:`, `status:`, `elements:` (comma
/// separated), then sections introduced by `--- name`.
pub fn template(d: &Decision) -> String {
    let list: String = d.assumptions.iter().map(|a| format!("- {a}\n")).collect();
    format!(
        "title: {}\nstatus: {}\nelements: {}\n\n--- context\n{}\n\n--- decision\n{}\n\n--- cost\n{}\n\n\
         --- revisit if\n{}\n\n--- assumptions\n{list}",
        d.title,
        d.status.label(),
        d.elements.join(", "),
        d.context,
        d.decision,
        d.cost,
        d.revisit_if,
    )
}

pub fn parse_template(text: &str) -> Result<Decision> {
    let mut d = Decision::new("", "");
    let mut section = String::new();
    let mut bodies: std::collections::HashMap<String, Vec<&str>> = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("--- ") {
            section = name.trim().to_string();
            continue;
        }
        if section.is_empty() {
            if let Some(v) = line.strip_prefix("title:") {
                d.title = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("status:") {
                d.status = match v.trim() {
                    "proposed" => Status::Proposed,
                    "superseded" => Status::Superseded,
                    _ => Status::Accepted,
                };
            } else if let Some(v) = line.strip_prefix("elements:") {
                d.elements = v
                    .split(',')
                    .map(|e| e.trim().to_string())
                    .filter(|e| !e.is_empty())
                    .collect();
            }
        } else {
            bodies.entry(section.clone()).or_default().push(line);
        }
    }
    let body = |name: &str| {
        bodies
            .get(name)
            .map(|l| l.join("\n").trim().to_string())
            .unwrap_or_default()
    };
    d.context = body("context");
    d.decision = body("decision");
    d.cost = body("cost");
    d.revisit_if = body("revisit if");
    d.assumptions = body("assumptions")
        .lines()
        .map(|l| l.trim().trim_start_matches('-').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if d.title.is_empty() || d.decision.is_empty() {
        bail!("the decision needs a title and a decision section");
    }
    Ok(d)
}

/// Asks the model to reconstruct why the element is the way it is. The result is a
/// proposal marked `Ai`: the reason is inferred from code, not recorded.
pub fn reconstruct(ctx: &Ctx, element_id: &str) -> Result<Decision> {
    let e = element::load(&ctx.root, element_id).ok_or_else(|| anyhow!("element {element_id} not found"))?;
    let schema = ai::object(
        &["title", "context", "decision", "cost", "revisit_if", "assumptions"],
        serde_json::json!({
            "title": ai::string(), "context": ai::string(), "decision": ai::string(),
            "cost": ai::string(), "revisit_if": ai::string(), "assumptions": ai::strings(),
        }),
    );
    let task = "Reconstruct the most likely design decision behind this code element: the decision, the \
                context that forced it, what it costs, the assumptions it rests on and when it should be \
                reconsidered. You only see the code, so say what the code implies; do not present guesses as facts.";
    let input = format!("ELEMENT: {} ({})\n\n{}\n", e.id, e.kind, e.text);
    let v = ai::ask_json(ctx, TaskKind::Decision, task, input, schema)?;
    let mut d = Decision::new(
        &ai::text(&v, "title").ok_or_else(|| anyhow!("empty answer"))?,
        &ai::text(&v, "decision").ok_or_else(|| anyhow!("empty answer"))?,
    );
    d.context = ai::text(&v, "context").unwrap_or_default();
    d.cost = ai::text(&v, "cost").unwrap_or_default();
    d.revisit_if = ai::text(&v, "revisit_if").unwrap_or_default();
    d.assumptions = ai::texts(&v, "assumptions");
    d.elements = vec![e.id];
    d.status = Status::Proposed;
    d.origin = Origin::Ai;
    add(ctx, d)
}

/// A decision whose assumption the change may break.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub decision: Decision,
    pub why: String,
}

/// Diff of the working tree against HEAD, or of `range` (`git diff <range>`).
pub fn git_diff(ctx: &Ctx, range: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&ctx.root).args(["diff", "--no-color", "-U2"]);
    match range {
        Some(r) => cmd.arg(r),
        None => cmd.arg("HEAD"),
    };
    let out = cmd.output()?;
    if !out.status.success() {
        bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Paths named in a unified diff.
pub fn diff_paths(diff: &str) -> Vec<String> {
    let mut v: Vec<String> = diff
        .lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(str::to_string)
        .collect();
    v.dedup();
    v
}

/// Checks a diff against the active decisions that concern the changed files (or none in
/// particular); no model call when nothing applies.
pub fn check(ctx: &Ctx, diff: &str) -> Result<Vec<Finding>> {
    let paths = diff_paths(diff);
    let relevant: Vec<Decision> = list(ctx)
        .into_iter()
        .filter(|d| d.status == Status::Accepted && !d.assumptions.is_empty())
        .filter(|d| {
            d.elements.is_empty()
                || d.elements.iter().any(|e| {
                    let p = e.split_once("::").map_or(e.as_str(), |(p, _)| p);
                    paths.iter().any(|x| x == p)
                })
        })
        .collect();
    if relevant.is_empty() || diff.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut input = format!("{}\n", ai::DECISIONS_MARKER);
    for d in &relevant {
        input.push_str(&format!(
            "{} | {} | assumptions: {}\n",
            d.id,
            d.title,
            d.assumptions.join("; ")
        ));
    }
    let clipped: String = diff.chars().take(MAX_DIFF).collect();
    input.push_str(&format!("\n{}\n{clipped}\n", ai::DIFF_MARKER));
    let schema = ai::object(
        &["findings"],
        serde_json::json!({ "findings": ai::array(ai::object(
            &["decision", "why"], serde_json::json!({ "decision": ai::string(), "why": ai::string() }))) }),
    );
    let task = "Decide which of the listed decisions this diff violates or makes questionable, because it breaks \
                one of their assumptions or their revisit condition. Return only real conflicts (none is fine), \
                naming the decision id and the concrete reason.";
    let v = ai::ask_json(ctx, TaskKind::Check, task, input, schema)?;
    Ok(v.get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let id = ai::text(f, "decision")?;
            let why = ai::text(f, "why")?;
            let d = relevant.iter().find(|d| d.id == id)?.clone();
            Some(Finding { decision: d, why })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    fn ctx() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rekon")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn checkout() {\n    queue();\n}\n").unwrap();
        let config = Config {
            backend: "fake".into(),
            ..Config::default()
        };
        let ctx = Ctx::with_backend(dir.path(), config, Arc::new(FakeBackend::default()));
        (dir, ctx)
    }

    #[test]
    fn template_roundtrip() {
        let mut d = Decision::new("Use a queue", "Send mail through a queue");
        d.elements = vec!["a.rs::checkout".into(), "b.rs".into()];
        d.assumptions = vec!["The API is sometimes down".into(), "Delay is fine".into()];
        d.cost = "Delay".into();
        let back = parse_template(&template(&d)).unwrap();
        assert_eq!(back.title, d.title);
        assert_eq!(back.elements, d.elements);
        assert_eq!(back.assumptions, d.assumptions);
        assert_eq!(back.decision, d.decision);
        assert!(parse_template("title: x\n").is_err());
    }

    #[test]
    fn decisions_cover_elements_and_files() {
        let (_d, ctx) = ctx();
        let mut d = Decision::new("Queue", "Use a queue");
        d.elements = vec!["a.rs".into()];
        add(&ctx, d).unwrap();
        assert_eq!(for_element(&ctx, "a.rs::checkout", "a.rs").len(), 1);
        assert!(for_element(&ctx, "b.rs::x", "b.rs").is_empty());
    }

    #[test]
    fn superseded_decisions_keep_a_link_and_sort_last() {
        let (_d, ctx) = ctx();
        let mut a = Decision::new("Old", "Old way");
        a.elements = vec!["a.rs".into()];
        let old = add(&ctx, a).unwrap();
        let mut b = Decision::new("New", "New way");
        b.elements = vec!["a.rs".into()];
        let new = supersede(&ctx, &old.id, b).unwrap();
        let v = for_element(&ctx, "a.rs::checkout", "a.rs");
        assert_eq!(v[0].id, new.id);
        assert_eq!(v[1].status, Status::Superseded);
        assert_eq!(v[1].superseded_by.as_deref(), Some(new.id.as_str()));
    }

    #[test]
    fn reconstruction_is_marked_as_a_proposal_by_ai() {
        let (_d, ctx) = ctx();
        let d = reconstruct(&ctx, "a.rs::a::checkout").unwrap();
        assert_eq!((d.origin, d.status), (Origin::Ai, Status::Proposed));
        assert_eq!(d.elements, ["a.rs::a::checkout"]);
        assert_eq!(accept(&ctx, &d.id).unwrap().status, Status::Accepted);
    }

    #[test]
    fn check_only_asks_when_a_decision_applies() {
        let (_d, ctx) = ctx();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-x\n+y\n";
        assert!(check(&ctx, diff).unwrap().is_empty());
        let mut d = Decision::new("Queue", "Use a queue");
        d.elements = vec!["a.rs::checkout".into()];
        d.assumptions = vec!["API is down sometimes".into()];
        let d = add(&ctx, d).unwrap();
        let f = check(&ctx, diff).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].decision.id, d.id);
        assert!(
            check(&ctx, "diff --git a/z.rs b/z.rs\n+++ b/z.rs\n")
                .unwrap()
                .is_empty()
        );
    }
}
