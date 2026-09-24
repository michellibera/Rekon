//! Claude Code CLI (`claude -p`) with tools disabled and a JSON schema for the answer.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use wait_timeout::ChildExt;

use super::{Backend, LlmRequest, LlmResponse};
use crate::config::Config;
use crate::text;

pub struct ClaudeBackend {
    program: String,
    bare: bool,
    timeout: Duration,
}

impl ClaudeBackend {
    /// Checks `claude --version` so a missing CLI is reported before any work starts.
    pub fn new(config: &Config) -> Result<Self> {
        let program = find_program("claude").map_err(|e| anyhow::anyhow!(text::claude_missing(&e)))?;
        Ok(Self {
            program,
            bare: config.bare,
            timeout: Duration::from_secs(config.timeout_secs),
        })
    }

    /// Arguments of one call. Built in one place so flag changes touch one function.
    pub fn args(req: &LlmRequest, bare: bool) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-p".into(),
            req.task.clone(),
            "--output-format".into(),
            "json".into(),
            "--json-schema".into(),
            req.schema.to_string(),
            "--model".into(),
            req.model.clone(),
            "--system-prompt".into(),
            req.system.clone(),
            "--tools".into(),
            String::new(),
            "--disallowedTools".into(),
            "mcp__*".into(),
            "--no-session-persistence".into(),
            // One-line descriptions need little reasoning; without this the call
            // inherits the user's default effort (e.g. high) and gets several times slower.
            "--effort".into(),
            "low".into(),
        ];
        if bare {
            args.push("--bare".into());
        }
        args
    }
}

impl Backend for ClaudeBackend {
    fn ask(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let workdir = TempWorkDir::new()?;
        let mut cmd = Command::new(&self.program);
        cmd.args(Self::args(req, self.bare))
            .current_dir(workdir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (status, stdout, stderr) = run_with_input(cmd, req.input.as_bytes(), self.timeout)?;
        parse_output(status, &stdout, &stderr)
    }
}

/// Finds a working program name by running `<name> --version`. On Windows, npm
/// installs only a `.cmd` shim, which `Command` does not find without the extension.
pub fn find_program(name: &str) -> std::result::Result<String, String> {
    let mut candidates = vec![name.to_string()];
    if cfg!(windows) {
        candidates.push(format!("{name}.cmd"));
    }
    let mut last = String::new();
    for c in candidates {
        match Command::new(&c).arg("--version").stdin(Stdio::null()).output() {
            Ok(o) if o.status.success() => return Ok(c),
            Ok(o) => last = String::from_utf8_lossy(&o.stderr).trim().to_string(),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Parses the `--output-format json` result of `claude -p`.
pub fn parse_output(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<LlmResponse> {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(stdout).ok();
    let detail = || {
        parsed
            .as_ref()
            .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
            .unwrap_or_else(|| {
                let e = String::from_utf8_lossy(stderr);
                let e = e.trim();
                if e.is_empty() {
                    String::from_utf8_lossy(stdout).trim().to_string()
                } else {
                    e.to_string()
                }
            })
    };
    if !success {
        bail!("claude exited with an error: {}", truncate(&detail(), 500));
    }
    let Some(v) = parsed.as_ref() else {
        bail!("claude returned invalid JSON: {}", truncate(&detail(), 500));
    };
    if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
        bail!("claude reported an error: {}", truncate(&detail(), 500));
    }
    let Some(json) = v.get("structured_output").filter(|s| !s.is_null()) else {
        bail!("claude returned no structured_output: {}", truncate(&detail(), 500));
    };
    Ok(LlmResponse {
        json: json.clone(),
        cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()),
        output_tokens: v.pointer("/usage/output_tokens").and_then(|t| t.as_u64()),
    })
}

/// Runs a prepared command, feeding `input` on stdin, killing it after `timeout`.
/// Returns (success, stdout, stderr).
pub fn run_with_input(mut cmd: Command, input: &[u8], timeout: Duration) -> Result<(bool, Vec<u8>, Vec<u8>)> {
    let mut child: Child = cmd.spawn().context("cannot start the model CLI")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut out_pipe = child.stdout.take().context("no stdout")?;
    let mut err_pipe = child.stderr.take().context("no stderr")?;
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });
    let status = match child.wait_timeout(timeout)? {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("model CLI timed out after {} s", timeout.as_secs());
        }
    };
    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok((status.success(), stdout, stderr))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Fresh empty folder outside the repository, removed on drop. Without `--bare`
/// a session would load hooks, MCP servers and CLAUDE.md from its working folder.
pub struct TempWorkDir(PathBuf);

static WORKDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempWorkDir {
    pub fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "rekon-{}-{nanos}-{}",
            std::process::id(),
            WORKDIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).context("cannot create a temporary folder")?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TaskKind;

    #[test]
    fn args_have_no_shell_and_all_flags() {
        let req = LlmRequest {
            kind: TaskKind::Files,
            model: "haiku".into(),
            system: "sys 'quoted' \"x\"".into(),
            task: "do it".into(),
            input: String::new(),
            schema: serde_json::json!({"type": "object"}),
        };
        let args = ClaudeBackend::args(&req, false);
        assert_eq!(&args[..2], ["-p", "do it"]);
        assert!(args.windows(2).any(|w| w[0] == "--tools" && w[1].is_empty()));
        assert!(args.contains(&"sys 'quoted' \"x\"".to_string()));
        assert!(!args.contains(&"--bare".to_string()));
        assert!(ClaudeBackend::args(&req, true).contains(&"--bare".to_string()));
    }

    #[test]
    fn parses_structured_output() {
        let out = br#"{"type":"result","is_error":false,"total_cost_usd":0.01,"structured_output":{"a":1}}"#;
        let r = parse_output(true, out, b"").unwrap();
        assert_eq!(r.json["a"], 1);
        assert_eq!(r.cost_usd, Some(0.01));
    }

    #[test]
    fn missing_structured_output_is_error() {
        let out = br#"{"type":"result","is_error":false,"result":"text only"}"#;
        let e = parse_output(true, out, b"").unwrap_err().to_string();
        assert!(e.contains("no structured_output"), "{e}");
        let e = parse_output(false, b"", b"boom").unwrap_err().to_string();
        assert!(e.contains("boom"), "{e}");
    }
}
