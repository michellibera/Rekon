//! OpenCode CLI (`opencode run`). There is no `--json-schema`, so the system prompt, the
//! task, the schema and the input go into an attached file, and the JSON is taken out of
//! the answer text and checked here.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::claude::{TempWorkDir, find_program, run_with_input};
use super::{Backend, LlmRequest, LlmResponse};
use crate::config::Config;

/// Short, single-line message: on Windows `opencode` is a `.cmd` shim and arguments
/// with newlines cannot be passed safely through cmd.exe.
const MESSAGE: &str = "Follow the instructions in the attached file. Answer only with the JSON object.";

pub struct OpenCodeBackend {
    program: String,
    model: Option<String>,
    attach: Option<String>,
    timeout: Duration,
}

impl OpenCodeBackend {
    pub fn new(config: &Config) -> Result<Self> {
        let program = find_program("opencode").map_err(|e| {
            anyhow::anyhow!(
                "cannot run `opencode --version` ({e}). Install OpenCode or set \"backend\" in .rekon/config.json"
            )
        })?;
        Ok(Self {
            program,
            model: config.opencode.model.clone(),
            attach: config.opencode.attach.clone(),
            timeout: Duration::from_secs(config.timeout_secs),
        })
    }

    /// Arguments of one call. The message comes first: `--file` takes several values
    /// and would swallow a message placed after it.
    pub fn args(&self, file: &str) -> Vec<String> {
        let mut args = vec!["run".to_string(), MESSAGE.to_string(), "--format".into(), "json".into()];
        if let Some(m) = &self.model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(a) = &self.attach {
            args.extend(["--attach".into(), a.clone()]);
        }
        args.extend(["--file".into(), file.to_string()]);
        args
    }
}

/// Content of the attached file.
pub fn prompt_file(req: &LlmRequest) -> String {
    format!(
        "{}\n\nTASK: {}\n\nAnswer with ONLY one JSON object (no prose, no code fences) valid against this JSON Schema:\n{}\n\nINPUT:\n{}\n",
        req.system.trim(),
        req.task.trim(),
        req.schema,
        req.input
    )
}

impl Backend for OpenCodeBackend {
    fn ask(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let workdir = TempWorkDir::new()?;
        let file = workdir.path().join("request.md");
        std::fs::write(&file, prompt_file(req)).context("cannot write the request file")?;
        let mut cmd = Command::new(&self.program);
        cmd.args(self.args(&file.to_string_lossy()))
            .current_dir(workdir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (success, stdout, stderr) = run_with_input(cmd, b"", self.timeout)?;
        if !success {
            bail!(
                "opencode exited with an error: {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        parse_events(&stdout, &req.schema)
    }
}

/// Collects the answer text and cost from `--format json` events (one JSON per line),
/// extracts the JSON object and checks the schema's required top-level fields.
pub fn parse_events(stdout: &[u8], schema: &Value) -> Result<LlmResponse> {
    let mut text = String::new();
    let mut cost = 0.0;
    let mut output_tokens = 0;
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(event.pointer("/part/text").and_then(Value::as_str).unwrap_or("")),
            Some("step_finish") => {
                cost += event.pointer("/part/cost").and_then(Value::as_f64).unwrap_or(0.0);
                output_tokens += event
                    .pointer("/part/tokens/output")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            Some("error") => bail!("opencode reported an error: {}", event.get("error").unwrap_or(&event)),
            _ => {}
        }
    }
    let json = extract_json(&text).with_context(|| format!("no JSON object in the opencode answer: {text:.300}"))?;
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for key in required.iter().filter_map(Value::as_str) {
        if json.get(key).is_none() {
            bail!("opencode answer misses the field \"{key}\"");
        }
    }
    Ok(LlmResponse {
        json,
        cost_usd: Some(cost),
        output_tokens: Some(output_tokens),
    })
}

/// The first `{` to the last `}` of the text, parsed as JSON.
fn extract_json(text: &str) -> Option<Value> {
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    serde_json::from_str(text.get(start..=end)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_stream() {
        let out = br#"{"type":"step_start","part":{}}
{"type":"text","part":{"type":"text","text":"```json\n{\"blocks\": [{\"start\": 1, \"end\": 2, \"summary\": \"x\"}]}\n```"}}
{"type":"step_finish","part":{"cost":0.002,"tokens":{"output":26}}}"#;
        let schema = serde_json::json!({"required": ["blocks"]});
        let r = parse_events(out, &schema).unwrap();
        assert_eq!(r.json["blocks"][0]["end"], 2);
        assert_eq!(r.cost_usd, Some(0.002));
        assert_eq!(r.output_tokens, Some(26));
    }

    #[test]
    fn missing_required_field_is_error() {
        let out = br#"{"type":"text","part":{"text":"{\"other\": 1}"}}"#;
        let schema = serde_json::json!({"required": ["files"]});
        assert!(parse_events(out, &schema).unwrap_err().to_string().contains("files"));
        assert!(parse_events(b"", &schema).is_err());
    }

    #[test]
    fn message_comes_before_file() {
        let b = OpenCodeBackend {
            program: "opencode".into(),
            model: Some("anthropic/claude-haiku-4-5".into()),
            attach: None,
            timeout: Duration::from_secs(1),
        };
        let args = b.args("/tmp/request.md");
        assert_eq!(args[..2], ["run".to_string(), MESSAGE.to_string()]);
        assert_eq!(
            args[args.len() - 2..],
            ["--file".to_string(), "/tmp/request.md".to_string()]
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "anthropic/claude-haiku-4-5")
        );
    }
}
