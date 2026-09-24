//! `rekon apply`: descriptions written from outside (an agent), JSON on stdin.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::Ctx;
use crate::model::Author;
use crate::prompts::clean_text;
use crate::scan::{ROOT, Tree, normalize_prefix};
use crate::store::Target;

/// Descriptions longer than this are saved with a warning.
const MAX_WORDS: usize = 20;

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(default)]
    pub summaries: BTreeMap<String, String>,
    #[serde(default)]
    pub overview: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub written: Vec<String>,
    pub errors: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

pub fn parse(input: &str) -> Result<Input> {
    serde_json::from_str(input).context("invalid apply input: expected {\"summaries\": {...}, \"overview\": \"...\"}")
}

/// Saves every entry with the current key of its element. A bad entry fails alone.
pub fn apply(ctx: &Ctx, input: &Input, by: Author) -> Result<Report> {
    let (tree, _) = Tree::list(&ctx.root)?;
    let mut report = Report::default();
    let overview = input.overview.as_deref().map(str::trim).filter(|o| !o.is_empty());
    let mut overview_written = false;
    for (key, text) in &input.summaries {
        match apply_one(ctx, &tree, key, text, by, overview) {
            Ok((is_project, words)) => {
                overview_written |= is_project && overview.is_some();
                if words > MAX_WORDS {
                    report
                        .warnings
                        .push(format!("{key}: description has {words} words (style: at most 12)"));
                }
                report.written.push(key.clone());
            }
            Err(e) => report.errors.push((key.clone(), format!("{e:#}"))),
        }
    }
    if let Some(o) = overview
        && !overview_written
    {
        ctx.store.update_project(|n| {
            n.overview = Some(o.to_string());
            true
        })?;
        report.written.push("overview".into());
    }
    Ok(report)
}

/// Returns (is the project, word count).
fn apply_one(
    ctx: &Ctx,
    tree: &Tree,
    key: &str,
    text: &str,
    by: Author,
    overview: Option<&str>,
) -> Result<(bool, usize)> {
    let text = clean_text(text).context("empty description")?;
    let words = text.split_whitespace().count();
    let path = normalize_prefix(Some(key));
    if path.is_empty() {
        let k = tree.dir_key(ROOT);
        ctx.store.put_summary(Target::Project, &k, &text, by, overview)?;
        return Ok((true, words));
    }
    let wants_dir = key.ends_with('/') || key.ends_with('\\');
    let Some(i) = tree.get(&path) else {
        anyhow::bail!("not a file or folder of this repository");
    };
    let node = tree.node(i);
    if node.is_dir {
        ctx.store
            .put_summary(Target::Dir(&path), &tree.dir_key(i), &text, by, None)?;
    } else {
        if wants_dir {
            anyhow::bail!("is a file, not a folder");
        }
        let k = ctx.store.file_key(&path).context("cannot read the file")?;
        ctx.store.put_summary(Target::File(&path), &k, &text, by, None)?;
    }
    Ok((false, words))
}
