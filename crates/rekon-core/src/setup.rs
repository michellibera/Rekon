//! `rekon setup`: installs the skill and the hooks in `~/.claude` once per machine,
//! so no repository files change.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::store::write_atomic;

const SKILL: &str = include_str!("../prompts/skill.md");

/// Hook events and the subcommand each one runs.
const HOOKS: [(&str, &str); 2] = [("Stop", "check --hook"), ("SessionStart", "context --hook")];

/// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot find the home folder (HOME)")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// Path of the running binary with `/` separators, so hook commands work in any shell.
pub fn exe_path() -> Result<String> {
    let exe = std::env::current_exe().context("cannot find the rekon binary")?;
    Ok(exe.to_string_lossy().replace('\\', "/"))
}

fn quoted(exe: &str) -> String {
    if exe.contains([' ', '\'', '"', '$', '&', '(', ')']) {
        format!("\"{exe}\"")
    } else {
        exe.to_string()
    }
}

/// Is this hook command one of ours (any path of a rekon binary)?
fn is_ours(command: &str, sub: &str) -> bool {
    command.ends_with(&format!(" {sub}")) && command.contains("rekon")
}

/// Settings with our hooks: old copies (e.g. with another binary path) removed,
/// current ones appended, other hooks untouched.
pub fn merged_settings(settings: &Value, exe: &str) -> Result<Value> {
    let mut settings = settings.clone();
    let root = settings.as_object_mut().context("settings.json is not a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("\"hooks\" in settings.json is not an object")?;
    for (event, sub) in HOOKS {
        let command = format!("{} {sub}", quoted(exe));
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("hooks.{event} in settings.json is not an array")
        };
        let already = groups.iter().any(|g| {
            g.get("hooks").and_then(Value::as_array).is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(Value::as_str) == Some(&command))
            })
        });
        if already {
            continue;
        }
        for group in groups.iter_mut() {
            if let Some(hs) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                hs.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| is_ours(c, sub))
                });
            }
        }
        groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
        groups.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
    }
    Ok(settings)
}

pub fn skill_text(exe: &str) -> String {
    SKILL.replace("{rekon}", &quoted(exe))
}

/// Installs (or with `dry_run` only lists) the changes. Returns their descriptions.
pub fn setup(claude_dir: &Path, exe: &str, dry_run: bool) -> Result<Vec<String>> {
    let mut changes = Vec::new();

    let skill_path = claude_dir.join("skills").join("rekon-init").join("SKILL.md");
    let skill = skill_text(exe);
    if std::fs::read_to_string(&skill_path).ok().as_deref() != Some(skill.as_str()) {
        changes.push(format!("write skill {}", skill_path.display()));
        if !dry_run {
            write_atomic(&skill_path, skill.as_bytes())?;
        }
    }

    let settings_path = claude_dir.join("settings.json");
    let current = match std::fs::read_to_string(&settings_path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON; not touching it", settings_path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", settings_path.display())),
    };
    let merged = merged_settings(&current, exe)?;
    if merged != current {
        for (event, sub) in HOOKS {
            if merged["hooks"][event] != current["hooks"][event] {
                changes.push(format!("add {event} hook: {} {sub}", quoted(exe)));
            }
        }
        if settings_path.exists() {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = settings_path.with_file_name(format!("settings.json.bak-{secs}"));
            changes.push(format!("back up settings to {}", backup.display()));
            if !dry_run {
                std::fs::copy(&settings_path, &backup).context("cannot back up settings.json")?;
            }
        }
        if !dry_run {
            let text = serde_json::to_string_pretty(&merged)? + "\n";
            write_atomic(&settings_path, text.as_bytes())?;
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/usr/local/bin/rekon";

    #[test]
    fn keeps_foreign_hooks_and_replaces_old_paths() {
        let settings = json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "other-tool" } ] },
                    { "hooks": [ { "type": "command", "command": "/old/rekon check --hook" } ] }
                ]
            }
        });
        let merged = merged_settings(&settings, EXE).unwrap();
        assert_eq!(merged["model"], "opus");
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "other-tool");
        assert_eq!(stop[1]["hooks"][0]["command"], "/usr/local/bin/rekon check --hook");
        assert_eq!(
            merged["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "/usr/local/bin/rekon context --hook"
        );
        assert_eq!(merged_settings(&merged, EXE).unwrap(), merged);
    }

    #[test]
    fn setup_twice_backs_up_and_does_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, r#"{"theme": "dark"}"#).unwrap();

        let planned = setup(dir.path(), EXE, true).unwrap();
        assert_eq!(planned.len(), 4, "{planned:?}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), r#"{"theme": "dark"}"#);
        assert!(!dir.path().join("skills").exists());

        let changes = setup(dir.path(), EXE, false).unwrap();
        assert_eq!(changes.len(), 4);
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.bak-")
            })
            .count();
        assert_eq!(backups, 1);
        assert!(
            std::fs::read_to_string(dir.path().join("skills/rekon-init/SKILL.md"))
                .unwrap()
                .contains(EXE)
        );

        assert!(setup(dir.path(), EXE, false).unwrap().is_empty());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let merged = merged_settings(&json!({}), "C:/Program Files/rekon.exe").unwrap();
        assert_eq!(
            merged["hooks"]["Stop"][0]["hooks"][0]["command"],
            "\"C:/Program Files/rekon.exe\" check --hook"
        );
    }

    #[test]
    fn invalid_settings_are_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), "{ not json").unwrap();
        assert!(setup(dir.path(), EXE, false).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
            "{ not json"
        );
    }
}
