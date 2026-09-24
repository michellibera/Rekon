//! External editor (`e`): `editor_cmd`, else `$VISUAL`, `$EDITOR`, `vi` (`notepad` on
//! Windows), with `+{line} {file}`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub struct EditRequest {
    pub path: String,
    pub abs: PathBuf,
    pub line: u32,
    /// Content hash before editing, to detect a change.
    pub hash_before: Option<String>,
}

fn default_editor() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() })
}

/// Program and arguments for editing `file` at `line`.
pub fn command(editor_cmd: Option<&str>, file: &Path, line: u32) -> Result<(String, Vec<String>)> {
    let cmd = editor_cmd.map(str::to_string).unwrap_or_else(default_editor);
    let mut words = shell_words::split(&cmd).with_context(|| format!("invalid editor command: {cmd}"))?;
    if words.is_empty() {
        bail!("empty editor command");
    }
    let program = words.remove(0);
    let name = Path::new(&program)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    // Notepad has no "+line" argument.
    if name != "notepad" {
        words.push(format!("+{line}"));
    }
    words.push(file.to_string_lossy().into_owned());
    Ok((program, words))
}

/// Runs the editor in the foreground and waits for it.
pub fn run(editor_cmd: Option<&str>, req: &EditRequest) -> Result<()> {
    let (program, args) = command(editor_cmd, &req.abs, req.line)?;
    let status = Command::new(&program)
        .args(&args)
        .status()
        .with_context(|| format!("cannot start {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_line_and_file_arguments() {
        let (p, a) = command(Some("nvim -u NONE"), Path::new("src/a.rs"), 15).unwrap();
        assert_eq!(p, "nvim");
        assert_eq!(a, ["-u", "NONE", "+15", "src/a.rs"]);
        let (p, a) = command(Some("'C:/Program Files/Notepad/notepad.exe'"), Path::new("a.rs"), 3).unwrap();
        assert_eq!(p, "C:/Program Files/Notepad/notepad.exe");
        assert_eq!(a, ["a.rs"]);
        assert!(command(Some("  "), Path::new("a"), 1).is_err());
    }
}
