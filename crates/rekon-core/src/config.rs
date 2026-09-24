use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Environment variable that overrides `backend` from config.json.
pub const BACKEND_ENV: &str = "REKON_BACKEND";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Models {
    pub overview: String,
    pub tree: String,
    pub blocks: String,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            overview: "sonnet".into(),
            tree: "haiku".into(),
            blocks: "sonnet".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct OpenCode {
    pub model: Option<String>,
    pub attach: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub language: String,
    pub backend: String,
    pub models: Models,
    pub bare: bool,
    pub workers: usize,
    pub timeout_secs: u64,
    pub max_file_bytes: u64,
    pub max_segment_lines: u32,
    pub leaf_max_lines: u32,
    pub head_lines: usize,
    pub batch_max_files: usize,
    pub batch_max_chars: usize,
    pub exclude: Vec<String>,
    pub editor_cmd: Option<String>,
    pub opencode: OpenCode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            language: "pl".into(),
            backend: "claude".into(),
            models: Models::default(),
            bare: false,
            workers: 3,
            timeout_secs: 180,
            max_file_bytes: 1_000_000,
            max_segment_lines: 1500,
            leaf_max_lines: 3,
            head_lines: 120,
            batch_max_files: 25,
            batch_max_chars: 60_000,
            exclude: [
                "*.lock",
                "package-lock.json",
                "pnpm-lock.yaml",
                "go.sum",
                "*.min.js",
                "*.min.css",
                "*.map",
                "*.svg",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            editor_cmd: None,
            opencode: OpenCode::default(),
        }
    }
}

impl Config {
    /// Reads `config.json` from the map directory; a missing file means defaults.
    /// `REKON_BACKEND` overrides the backend in both cases.
    pub fn load(map_dir: &Path) -> Result<Self> {
        let path = map_dir.join("config.json");
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        if let Ok(backend) = std::env::var(BACKEND_ENV)
            && !backend.is_empty()
        {
            config.backend = backend;
        }
        config.workers = config.workers.max(1);
        config.batch_max_files = config.batch_max_files.max(1);
        Ok(config)
    }

    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_keeps_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"workers": 5, "models": {"tree": "x"}}"#,
        )
        .unwrap();
        let c = Config::load(dir.path()).unwrap();
        assert_eq!(c.workers, 5);
        assert_eq!(c.models.tree, "x");
        assert_eq!(c.models.blocks, "sonnet");
        assert_eq!(c.max_segment_lines, 1500);
    }
}
