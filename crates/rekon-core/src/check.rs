//! `rekon check --hook`: Stop hook of Claude Code. Blocks the end of a turn while
//! files changed in the session have outdated descriptions. Runs in every session,
//! so it only reads what it needs (target: under 300 ms).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::Config;
use crate::hash;
use crate::scan::{self, Excluder};
use crate::store::Store;
use crate::text;

/// Paths reported by `git status --porcelain -z` as modified, added, untracked,
/// renamed or copied (new names), relative to the repository root.
pub fn changed_paths(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .context("cannot run git status")?;
    anyhow::ensure!(out.status.success(), "git status failed");
    Ok(parse_porcelain(&out.stdout))
}

pub fn parse_porcelain(raw: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y) = (entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        if matches!(x, b'R' | b'C') {
            // The next field is the original path.
            fields.next();
        }
        if x == b'D' || y == b'D' {
            continue;
        }
        out.push(path);
    }
    out
}

/// Changed text files without a fresh description, then new folders without any.
pub fn stale_changes(root: &Path) -> Result<Vec<String>> {
    let store = Store::new(root);
    let config = Config::load(store.dir())?;
    let excluder = Excluder::new(&config.exclude)?;
    let mut files = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for path in changed_paths(root)? {
        if path.starts_with(&format!("{}/", crate::MAP_DIR)) || excluder.matched(&path).is_some() {
            continue;
        }
        let Some(key) = text_file_key(&root.join(&path), config.max_file_bytes) else {
            continue;
        };
        if store.file_note(&path).summary.is_none_or(|s| s.hash != key) {
            files.push(path.clone());
        }
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if !dirs.iter().any(|d| d == parent) && store.dir_note(parent).summary.is_none() {
                dirs.push(parent.to_string());
            }
            dir = parent;
        }
    }
    dirs.sort();
    files.extend(dirs.into_iter().map(|d| format!("{d}/")));
    Ok(files)
}

/// Content hash of a text file within the size limit; `None` for others.
fn text_file_key(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path).ok()?.read_to_end(&mut bytes).ok()?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    Some(hash::hash_bytes(&bytes))
}

/// Folder the hook runs for: `cwd` from the hook input, else the process folder.
pub fn hook_cwd(input: &Value) -> PathBuf {
    input
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// Output of the Stop hook, or `None` when it should stay silent.
pub fn stop_hook(input: &str, exe: &str) -> Result<Option<String>> {
    let input: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    if input.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let Ok(root) = scan::find_root(&hook_cwd(&input)) else {
        return Ok(None);
    };
    if !Store::new(&root).exists() {
        return Ok(None);
    }
    let stale = stale_changes(&root)?;
    if stale.is_empty() {
        return Ok(None);
    }
    let out = json!({ "decision": "block", "reason": text::stop_hook_reason(exe, &stale) });
    Ok(Some(out.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parsing() {
        let raw = b" M src/a.rs\0?? new/b.rs\0R  c2.rs\0c.rs\0 D gone.rs\0A  added.rs\0";
        assert_eq!(parse_porcelain(raw), ["src/a.rs", "new/b.rs", "c2.rs", "added.rs"]);
    }
}
