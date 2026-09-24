//! Model backends: a model is called like a plain function through an agent CLI.

use std::sync::Arc;

use anyhow::{Result, bail};

use crate::config::Config;

pub mod claude;
pub mod fake;

/// Kind of task; used for logging and by the fake backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Overview,
    Files,
    Dirs,
    Segment,
}

impl TaskKind {
    pub fn name(self) -> &'static str {
        match self {
            TaskKind::Overview => "overview",
            TaskKind::Files => "files",
            TaskKind::Dirs => "dirs",
            TaskKind::Segment => "segment",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub kind: TaskKind,
    /// Alias from `config.models`, e.g. "haiku".
    pub model: String,
    /// System prompt built from style.md.
    pub system: String,
    /// Short task instruction.
    pub task: String,
    /// Content passed on stdin.
    pub input: String,
    /// Response schema.
    pub schema: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct LlmResponse {
    pub json: serde_json::Value,
    pub cost_usd: Option<f64>,
    /// Output tokens (including thinking), for the log.
    pub output_tokens: Option<u64>,
}

pub trait Backend: Send + Sync {
    fn ask(&self, req: &LlmRequest) -> Result<LlmResponse>;
}

/// Creates the backend named in the config (`REKON_BACKEND` already applied).
pub fn from_config(config: &Config) -> Result<Arc<dyn Backend>> {
    Ok(match config.backend.as_str() {
        "claude" => Arc::new(claude::ClaudeBackend::new(config)?),
        "fake" => Arc::new(fake::FakeBackend::from_env()),
        other => bail!("unknown backend \"{other}\" (expected claude or fake)"),
    })
}
