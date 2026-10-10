//! Editable contract of an element: responsibility, input, output, rules, examples. Two
//! layers stay apart: `current` (what the code does now, described by a model and stamped
//! with the element's hash) and `desired` (what a person wants). Editing the sentence only
//! creates a proposal; `impact` turns the difference into a card for an agent, and only
//! code plus verification make the wish a fact.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::Ctx;
use crate::ai;
use crate::backend::TaskKind;
use crate::cards::{self, Card};
use crate::collection;
use crate::element;
use crate::model::{Origin, Stamped};
use crate::store::timestamp;

const KIND: &str = "contracts";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Contract {
    pub responsibility: String,
    pub input: String,
    pub output: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub examples: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Record {
    pub element: String,
    pub current: Option<Stamped<Contract>>,
    pub desired: Option<Contract>,
    #[serde(default)]
    pub desired_at: String,
}

/// How the wish relates to the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing wished for (or nothing described yet).
    Descriptive,
    /// The wish differs from the current description: a proposal.
    Proposal,
    /// The wish equals the current description of the code.
    InSync,
    /// The description is older than the code.
    Stale,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Descriptive => "described",
            State::Proposal => "proposal",
            State::InSync => "in sync",
            State::Stale => "description outdated",
        }
    }
}

fn key(element: &str) -> String {
    collection::key(element)
}

pub fn get(ctx: &Ctx, element: &str) -> Record {
    collection::get(&ctx.store, KIND, &key(element)).unwrap_or_else(|| Record {
        element: element.to_string(),
        ..Record::default()
    })
}

fn save(ctx: &Ctx, r: &Record) -> Result<()> {
    collection::put(&ctx.store, KIND, &key(&r.element), r)
}

/// All records (for the lens and the hooks).
pub fn list(ctx: &Ctx) -> Vec<Record> {
    collection::list(&ctx.store, KIND)
}

/// State of a record against the current element hash (`None`: the element is gone).
pub fn state(r: &Record, current_hash: Option<&str>) -> State {
    let stale = match (&r.current, current_hash) {
        (Some(c), Some(h)) => !c.is_current(h),
        _ => false,
    };
    match (&r.current, &r.desired) {
        (_, None) => {
            if stale {
                State::Stale
            } else {
                State::Descriptive
            }
        }
        (Some(c), Some(d)) if !stale && &c.value == d => State::InSync,
        (Some(_), Some(_)) if stale => State::Stale,
        _ => State::Proposal,
    }
}

/// Field-by-field difference between the current and the desired contract.
pub fn diff(r: &Record) -> Vec<(&'static str, String, String)> {
    let (Some(c), Some(d)) = (&r.current, &r.desired) else {
        return Vec::new();
    };
    let (c, d) = (&c.value, &d);
    let mut out = Vec::new();
    let mut field = |name: &'static str, a: String, b: String| {
        if a != b {
            out.push((name, a, b));
        }
    };
    field("responsibility", c.responsibility.clone(), d.responsibility.clone());
    field("input", c.input.clone(), d.input.clone());
    field("output", c.output.clone(), d.output.clone());
    field("rules", c.rules.join("; "), d.rules.join("; "));
    field("examples", c.examples.join("; "), d.examples.join("; "));
    out
}

/// Asks the model to describe what the element does now (or returns the stamped one).
pub fn derive(ctx: &Ctx, element_id: &str, force: bool) -> Result<Record> {
    let e = element::load(&ctx.root, element_id).ok_or_else(|| anyhow!("element {element_id} not found"))?;
    let mut rec = get(ctx, element_id);
    if !force && rec.current.as_ref().is_some_and(|c| c.is_current(&e.hash)) {
        return Ok(rec);
    }
    let schema = ai::object(
        &["responsibility", "input", "output", "rules", "examples"],
        serde_json::json!({
            "responsibility": ai::string(), "input": ai::string(), "output": ai::string(),
            "rules": ai::strings(), "examples": ai::strings(),
        }),
    );
    let task = "Write the contract this code element fulfils today: its responsibility (one sentence), what it \
                takes, what it returns, the rules it enforces (only ones visible in the code) and one or two \
                concrete examples with numbers. Describe, do not judge.";
    let v = ai::ask_json(
        ctx,
        TaskKind::Contract,
        task,
        format!("ELEMENT: {} ({})\n\n{}\n", e.id, e.kind, e.text),
        schema,
    )?;
    let contract = Contract {
        responsibility: ai::text(&v, "responsibility").ok_or_else(|| anyhow!("empty answer"))?,
        input: ai::text(&v, "input").unwrap_or_default(),
        output: ai::text(&v, "output").unwrap_or_default(),
        rules: ai::texts(&v, "rules"),
        examples: ai::texts(&v, "examples"),
    };
    rec.current = Some(Stamped {
        hash: e.hash,
        origin: Origin::Ai,
        value: contract,
    });
    save(ctx, &rec)?;
    Ok(rec)
}

/// Records what a person wants. Nothing about the code changes.
pub fn set_desired(ctx: &Ctx, element_id: &str, desired: Contract) -> Result<Record> {
    let mut rec = get(ctx, element_id);
    rec.desired = Some(desired);
    rec.desired_at = timestamp(std::time::SystemTime::now());
    save(ctx, &rec)?;
    Ok(rec)
}

/// Drops the wish (the proposal is abandoned).
pub fn clear_desired(ctx: &Ctx, element_id: &str) -> Result<()> {
    let mut rec = get(ctx, element_id);
    rec.desired = None;
    save(ctx, &rec)
}

/// Template for the editor (the desired contract, starting from the current one).
pub fn template(r: &Record) -> String {
    let c = r
        .desired
        .clone()
        .or_else(|| r.current.as_ref().map(|c| c.value.clone()))
        .unwrap_or_default();
    let list = |v: &[String]| v.iter().map(|x| format!("- {x}\n")).collect::<String>();
    format!(
        "# Desired contract of {}. Change what you want; the code stays as it is until an agent implements it.\n\
         responsibility: {}\ninput: {}\noutput: {}\n\n--- rules\n{}\n--- examples\n{}",
        r.element,
        c.responsibility,
        c.input,
        c.output,
        list(&c.rules),
        list(&c.examples)
    )
}

pub fn parse_template(text: &str) -> Result<Contract> {
    let mut c = Contract::default();
    let mut section = "";
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("--- ") {
            section = if name.trim() == "rules" { "rules" } else { "examples" };
            continue;
        }
        let item = || line.trim().trim_start_matches('-').trim().to_string();
        match section {
            "" => {
                if let Some(v) = line.strip_prefix("responsibility:") {
                    c.responsibility = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("input:") {
                    c.input = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("output:") {
                    c.output = v.trim().to_string();
                }
            }
            "rules" if !item().is_empty() => c.rules.push(item()),
            "examples" if !item().is_empty() => c.examples.push(item()),
            _ => {}
        }
    }
    if c.responsibility.is_empty() {
        bail!("the contract needs a responsibility");
    }
    Ok(c)
}

/// Turns the difference between the current and the desired contract into a proposed
/// card: what to change in the implementation, the tests and the callers.
pub fn impact(ctx: &Ctx, element_id: &str) -> Result<Card> {
    let rec = get(ctx, element_id);
    let changes = diff(&rec);
    let (Some(current), false) = (&rec.current, changes.is_empty()) else {
        bail!("nothing to do: describe the element (derive) and change its desired contract first");
    };
    let e = element::load(&ctx.root, element_id).ok_or_else(|| anyhow!("element {element_id} not found"))?;
    let mut input = format!("ELEMENT: {}\n\nCODE:\n{}\n\nWANTED CHANGES:\n", e.id, e.text);
    for (field, a, b) in &changes {
        input.push_str(&format!("- {field}: from \"{a}\" to \"{b}\"\n"));
    }
    if let Ok(idx) = crate::relations::build(ctx) {
        let callers: Vec<String> = idx
            .called_by(element_id)
            .into_iter()
            .map(|l| format!("{}:{}", l.path, l.line))
            .collect();
        if !callers.is_empty() {
            input.push_str(&format!("\nCALLED FROM: {}\n", callers.join(", ")));
        }
    }
    let schema = ai::object(
        &["implementation", "tests", "dependencies"],
        serde_json::json!({"implementation": ai::strings(), "tests": ai::strings(), "dependencies": ai::strings()}),
    );
    let task = "The programmer wants this element's contract to change. List concretely what must change in the \
                implementation, which tests to add or update, and which callers or dependencies to check.";
    let v = ai::ask_json(ctx, TaskKind::Impact, task, input, schema)?;
    let mut card = Card::spec(
        &format!("Contract change: {}", e.name),
        &current.value.responsibility,
        &rec.desired
            .as_ref()
            .map(|d| d.responsibility.clone())
            .unwrap_or_default(),
    );
    for (field, a, b) in &changes {
        card.consequences.push(format!("{field}: \"{a}\" -> \"{b}\""));
    }
    for (label, key) in [
        ("Implementation", "implementation"),
        ("Tests", "tests"),
        ("Dependencies", "dependencies"),
    ] {
        card.consequences
            .extend(ai::texts(&v, key).into_iter().map(|t| format!("{label}: {t}")));
    }
    card.elements = vec![element_id.to_string()];
    card.basis.push(cards::Basis {
        kind: "code".into(),
        file: e.path.clone(),
        start_line: e.start,
        end_line: e.end,
        note: "current implementation".into(),
    });
    card.origin = Origin::Ai;
    card.source = "contract".into();
    cards::add(ctx, card)
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
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn discount(p: u32) -> u32 {\n    p - p / 5\n}\n",
        )
        .unwrap();
        let config = Config {
            backend: "fake".into(),
            ..Config::default()
        };
        let ctx = Ctx::with_backend(dir.path(), config, Arc::new(FakeBackend::default()));
        (dir, ctx)
    }

    const ID: &str = "a.rs::a::discount";

    #[test]
    fn current_and_desired_are_separate_layers() {
        let (_d, ctx) = ctx();
        let rec = derive(&ctx, ID, false).unwrap();
        assert_eq!(rec.current.as_ref().unwrap().origin, Origin::Ai);
        let hash = element::load(&ctx.root, ID).unwrap().hash;
        assert_eq!(state(&rec, Some(&hash)), State::Descriptive);

        let mut want = rec.current.clone().unwrap().value;
        want.rules.push("The final price is never negative".into());
        let rec = set_desired(&ctx, ID, want).unwrap();
        assert_eq!(state(&rec, Some(&hash)), State::Proposal);
        assert_eq!(diff(&rec).len(), 1);
        assert_eq!(diff(&rec)[0].0, "rules");

        std::fs::write(_d.path().join("a.rs"), "pub fn discount(p: u32) -> u32 {\n    p\n}\n").unwrap();
        let h2 = element::load(&ctx.root, ID).unwrap().hash;
        assert_eq!(state(&rec, Some(&h2)), State::Stale);
    }

    #[test]
    fn impact_makes_a_proposed_card_for_an_agent() {
        let (_d, ctx) = ctx();
        assert!(impact(&ctx, ID).is_err());
        let rec = derive(&ctx, ID, false).unwrap();
        let mut want = rec.current.unwrap().value;
        want.output = "Never negative".into();
        set_desired(&ctx, ID, want).unwrap();
        let card = impact(&ctx, ID).unwrap();
        assert_eq!((card.origin, card.status), (Origin::Ai, cards::Status::Proposed));
        assert!(card.consequences.iter().any(|c| c.starts_with("Tests:")));
        assert!(card.consequences.iter().any(|c| c.starts_with("output:")));
    }

    #[test]
    fn template_roundtrip() {
        let mut c = Contract::default();
        c.responsibility = "Computes a discount".into();
        c.input = "cart".into();
        c.rules = vec!["a".into(), "b".into()];
        c.examples = vec!["100 -> 80".into()];
        let r = Record {
            element: ID.into(),
            desired: Some(c.clone()),
            ..Record::default()
        };
        assert_eq!(
            parse_template(&template(&r)).unwrap(),
            Contract {
                output: String::new(),
                ..c
            }
        );
        assert!(parse_template("input: x\n").is_err());
    }
}
