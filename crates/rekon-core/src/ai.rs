//! Model calls of the insight features (decisions, cards, contracts, questions,
//! traces): a common system prompt and one helper to ask for JSON. The prompts live next
//! to the feature that uses them, the fake backend's answers in `backend/fake.rs`.

use anyhow::Result;
use serde_json::Value;

use crate::Ctx;
use crate::backend::{LlmRequest, TaskKind};

/// Section markers of the inputs; the fake backend reads them back.
pub const FILES_MARKER: &str = "FILES:";
pub const ELEMENTS_MARKER: &str = "ELEMENTS:";
pub const DECISIONS_MARKER: &str = "DECISIONS:";
pub const DIFF_MARKER: &str = "DIFF:";

/// Model answer for `task` over `input`, checked against `schema` by the backend.
pub fn ask_json(ctx: &Ctx, kind: TaskKind, task: &str, input: String, schema: Value) -> Result<Value> {
    let system = format!(
        "You help a programmer understand and change a codebase. Answer only according to the JSON \
         schema. Language of the texts: {}. Be concrete and short. Never invent files, symbols or \
         lines: refer only to what you were shown.",
        ctx.config.language
    );
    let req = LlmRequest {
        kind,
        model: ctx.config.models.review.clone(),
        system,
        task: task.to_string(),
        input,
        schema,
    };
    Ok(ctx.ask(&req)?.json)
}

/// Value of `key` as a cleaned one-line text.
pub fn text(v: &Value, key: &str) -> Option<String> {
    clean(v.get(key)?.as_str()?)
}

pub fn clean(s: &str) -> Option<String> {
    let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

pub fn texts(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| clean(x.as_str()?)).collect())
        .unwrap_or_default()
}

/// Schema of an object whose listed keys are all required.
pub fn object(required: &[&str], properties: Value) -> Value {
    serde_json::json!({ "type": "object", "required": required, "properties": properties })
}

pub fn string() -> Value {
    serde_json::json!({ "type": "string" })
}

pub fn integer() -> Value {
    serde_json::json!({ "type": "integer" })
}

pub fn array(items: Value) -> Value {
    serde_json::json!({ "type": "array", "items": items })
}

pub fn strings() -> Value {
    array(string())
}
