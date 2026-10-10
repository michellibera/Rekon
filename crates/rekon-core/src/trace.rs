//! Worked example of an element: a concrete input, the successive states, the reason for
//! each branch and the result. Three kinds of knowledge are never mixed: a recorded
//! execution, a test result, and a model's prediction. This module makes predictions (always
//! `Predicted`); test results come from `criteria` and are shown next to them. A person
//! can change the input and state the result they expect; the expectation becomes a card
//! (a specification for a test and a change) and never touches the trace itself.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Ctx;
use crate::ai;
use crate::backend::TaskKind;
use crate::cards::{self, Basis, Card};
use crate::collection;
use crate::element;
use crate::model::Origin;
use crate::store::timestamp;

const KIND: &str = "traces";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Step {
    pub label: String,
    pub before: String,
    pub after: String,
    /// Why this branch was taken.
    pub why: String,
    /// Line of the step in the file (0: unknown).
    pub line: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Trace {
    pub element: String,
    pub input: String,
    pub steps: Vec<Step>,
    pub result: String,
    /// What the person expects instead (their wish, not a fact).
    #[serde(default)]
    pub expected: Option<String>,
    pub origin: Origin,
    /// Hash of the element the prediction was made for.
    pub hash: String,
    pub at: String,
}

fn key(element: &str) -> String {
    collection::key(element)
}

pub fn get(ctx: &Ctx, element: &str) -> Option<Trace> {
    collection::get(&ctx.store, KIND, &key(element))
}

fn save(ctx: &Ctx, t: &Trace) -> Result<()> {
    collection::put(&ctx.store, KIND, &key(&t.element), t)
}

/// Predicts a run of the element. Without `input` the model invents a typical example;
/// with it, the person's own example is walked through. The expectation survives a re-run.
pub fn predict(ctx: &Ctx, element_id: &str, input: Option<&str>) -> Result<Trace> {
    let e = element::load(&ctx.root, element_id).ok_or_else(|| anyhow!("element {element_id} not found"))?;
    let previous = get(ctx, element_id);
    let mut prompt = format!("ELEMENT: {} ({})\n\n{}\n", e.id, e.kind, e.text);
    if let Some(i) = input.and_then(ai::clean) {
        prompt.push_str(&format!("\nEXAMPLE INPUT (walk through exactly this): {i}\n"));
    }
    let step = ai::object(
        &["label", "before", "after", "why", "line"],
        serde_json::json!({"label": ai::string(), "before": ai::string(), "after": ai::string(),
            "why": ai::string(), "line": ai::integer()}),
    );
    let schema = ai::object(
        &["input", "steps", "result"],
        serde_json::json!({"input": ai::string(), "steps": ai::array(step), "result": ai::string()}),
    );
    let task = "Walk through this code element with a concrete input. Give the input (use the example input when \
                one is given, otherwise a typical realistic one), then the successive steps: for each the data \
                before and after and the reason the branch was taken, with the line number of the step if you can \
                tell, and finally the result. This is a prediction from reading the code, not a recorded run.";
    let v = ai::ask_json(ctx, TaskKind::Trace, task, prompt, schema)?;
    let steps: Vec<Step> = v
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let line = s.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            Some(Step {
                label: ai::text(s, "label")?,
                before: ai::text(s, "before").unwrap_or_default(),
                after: ai::text(s, "after").unwrap_or_default(),
                why: ai::text(s, "why").unwrap_or_default(),
                line: if line >= e.start && line <= e.end { line } else { 0 },
            })
        })
        .collect();
    let t = Trace {
        element: element_id.to_string(),
        input: input
            .and_then(ai::clean)
            .or_else(|| ai::text(&v, "input"))
            .ok_or_else(|| anyhow!("empty answer"))?,
        steps,
        result: ai::text(&v, "result").ok_or_else(|| anyhow!("empty answer"))?,
        expected: previous.and_then(|p| p.expected),
        origin: Origin::Predicted,
        hash: e.hash,
        at: timestamp(std::time::SystemTime::now()),
    };
    save(ctx, &t)?;
    Ok(t)
}

/// Records the result the person expects for this example.
pub fn set_expected(ctx: &Ctx, element_id: &str, expected: &str) -> Result<Trace> {
    let mut t = get(ctx, element_id).ok_or_else(|| anyhow!("predict a run of {element_id} first"))?;
    t.expected = ai::clean(expected);
    save(ctx, &t)?;
    Ok(t)
}

/// A proposed card from the difference between the predicted and the expected result: the
/// specification an agent turns into a test and a change.
pub fn expectation_card(ctx: &Ctx, element_id: &str) -> Result<Card> {
    let t = get(ctx, element_id).ok_or_else(|| anyhow!("no example for {element_id}"))?;
    let expected = t
        .expected
        .clone()
        .ok_or_else(|| anyhow!("state the expected result first"))?;
    let e = element::load(&ctx.root, element_id).ok_or_else(|| anyhow!("element {element_id} not found"))?;
    let mut c = Card::spec(
        &format!("Expected result: {}", e.name),
        &format!("For input \"{}\" the code gives (predicted): {}", t.input, t.result),
        &format!("For input \"{}\" the result is: {expected}", t.input),
    );
    c.consequences
        .push("Add a test for exactly this input and result".into());
    c.elements = vec![element_id.to_string()];
    c.basis.push(Basis {
        kind: "trace".into(),
        file: e.path,
        start_line: e.start,
        end_line: e.end,
        note: "predicted walk-through".into(),
    });
    c.source = "trace".into();
    cards::add(ctx, c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    const ID: &str = "a.rs::a::price";

    fn ctx() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rekon")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn price(x: u32) -> u32 {\n    x * 2\n}\n").unwrap();
        let config = Config {
            backend: "fake".into(),
            ..Config::default()
        };
        let ctx = Ctx::with_backend(dir.path(), config, Arc::new(FakeBackend::default()));
        (dir, ctx)
    }

    #[test]
    fn predictions_are_labelled_and_keep_the_expectation() {
        let (_d, ctx) = ctx();
        let t = predict(&ctx, ID, None).unwrap();
        assert_eq!(t.origin, Origin::Predicted);
        assert_eq!(t.steps.len(), 2);
        assert!(
            t.steps.iter().all(|s| s.line == 0),
            "lines outside the element are dropped"
        );
        set_expected(&ctx, ID, "Never below 10").unwrap();
        let again = predict(&ctx, ID, Some("x = 7")).unwrap();
        assert_eq!(again.input, "x = 7");
        assert_eq!(again.expected.as_deref(), Some("Never below 10"));
    }

    #[test]
    fn expectation_becomes_a_specification_card() {
        let (_d, ctx) = ctx();
        assert!(expectation_card(&ctx, ID).is_err());
        predict(&ctx, ID, None).unwrap();
        assert!(expectation_card(&ctx, ID).is_err(), "no expectation yet");
        set_expected(&ctx, ID, "Never below 10").unwrap();
        let c = expectation_card(&ctx, ID).unwrap();
        assert_eq!((c.origin, c.status), (Origin::Human, cards::Status::Proposed));
        assert!(c.after.contains("Never below 10"));
        assert_eq!(c.basis[0].kind, "trace");
    }
}
