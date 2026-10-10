//! Progress as met conditions instead of "80% done". Every criterion has three separate
//! answers: is it implemented, is it verified, and did the user review it. Each answer is
//! tied to the versions of the elements it was made for, so changing the code makes an old
//! test result or review stale (it stays in the history, but no longer counts).

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::Ctx;
use crate::cards::Card;
use crate::collection;
use crate::element;
use crate::model::Origin;
use crate::store::timestamp;

const KIND: &str = "criteria";

/// Element ids with the hash of the version they had at some moment.
pub type Versions = BTreeMap<String, String>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Verification {
    pub passed: bool,
    /// Tests that matched the criterion's patterns.
    pub matched: usize,
    pub at: String,
    /// Versions of the implementing elements when the tests ran.
    pub versions: Versions,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Review {
    pub at: String,
    pub versions: Versions,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Criterion {
    pub id: String,
    pub text: String,
    /// Elements that implement it, with the versions the person or agent vouched for.
    #[serde(default)]
    pub implementation: Versions,
    /// Substrings of test names that check it.
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub verification: Option<Verification>,
    #[serde(default)]
    pub review: Option<Review>,
    /// A decision about it is still open (nothing to implement yet).
    #[serde(default)]
    pub open: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub items: Vec<Criterion>,
    #[serde(default)]
    pub created: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Nothing yet.
    None,
    /// Done and up to date.
    Current,
    /// Was done, but the code changed since.
    Stale,
    /// Failed (verification only).
    Failing,
    /// Waiting for a decision (implementation only).
    Open,
}

/// The three answers about one criterion, computed against the current code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub implementation: Level,
    pub verification: Level,
    pub review: Level,
}

pub fn list(ctx: &Ctx) -> Vec<Task> {
    collection::list(&ctx.store, KIND)
}

pub fn get(ctx: &Ctx, query: &str) -> Option<Task> {
    collection::resolve(&ctx.store, KIND, query).map(|(_, t)| t)
}

fn save(ctx: &Ctx, t: &Task) -> Result<()> {
    collection::put(&ctx.store, KIND, &t.id, t)
}

pub fn new_task(ctx: &Ctx, title: &str, criteria: &[String]) -> Result<Task> {
    if title.trim().is_empty() {
        bail!("a task needs a title");
    }
    let mut t = Task {
        id: collection::next_id(&ctx.store, KIND, title),
        title: title.to_string(),
        items: Vec::new(),
        created: timestamp(std::time::SystemTime::now()),
    };
    for c in criteria {
        add_item(&mut t, c);
    }
    save(ctx, &t)?;
    Ok(t)
}

fn add_item(t: &mut Task, text: &str) -> String {
    let id = format!("c{}", t.items.len() + 1);
    t.items.push(Criterion {
        id: id.clone(),
        text: text.to_string(),
        ..Criterion::default()
    });
    id
}

/// Criteria for a card: what it wants and every consequence to handle.
pub fn from_card(ctx: &Ctx, card: &Card) -> Result<Task> {
    let mut texts = vec![card.after.clone()];
    texts.extend(card.consequences.iter().cloned());
    new_task(ctx, &card.title, &texts)
}

pub fn add(ctx: &Ctx, task: &str, text: &str) -> Result<Task> {
    update(ctx, task, |t| {
        add_item(t, text);
        Ok(())
    })
}

/// Read-modify-write of one task.
pub fn update(ctx: &Ctx, task: &str, f: impl FnOnce(&mut Task) -> Result<()>) -> Result<Task> {
    let mut t = get(ctx, task).ok_or_else(|| anyhow!("no task matches {task}"))?;
    f(&mut t)?;
    save(ctx, &t)?;
    Ok(t)
}

fn item<'a>(t: &'a mut Task, id: &str) -> Result<&'a mut Criterion> {
    t.items
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow!("no criterion {id}"))
}

fn current_versions(ctx: &Ctx, ids: impl IntoIterator<Item = String>) -> Versions {
    ids.into_iter()
        .filter_map(|id| Some((id.clone(), element::load(&ctx.root, &id)?.hash)))
        .collect()
}

/// Says that an element implements a criterion, in its current version.
pub fn link_implementation(ctx: &Ctx, task: &str, criterion: &str, element_id: &str) -> Result<Task> {
    let hash = element::load(&ctx.root, element_id)
        .ok_or_else(|| anyhow!("element {element_id} not found"))?
        .hash;
    update(ctx, task, |t| {
        let c = item(t, criterion)?;
        c.implementation.insert(element_id.to_string(), hash);
        c.open = false;
        Ok(())
    })
}

/// Vouches for the current versions of everything that implements it again.
pub fn touch(ctx: &Ctx, task: &str, criterion: &str) -> Result<Task> {
    let ctx2 = ctx;
    update(ctx, task, |t| {
        let c = item(t, criterion)?;
        c.implementation = current_versions(ctx2, c.implementation.keys().cloned());
        Ok(())
    })
}

pub fn link_test(ctx: &Ctx, task: &str, criterion: &str, pattern: &str) -> Result<Task> {
    update(ctx, task, |t| {
        let c = item(t, criterion)?;
        if !c.tests.iter().any(|p| p == pattern) {
            c.tests.push(pattern.to_string());
        }
        Ok(())
    })
}

pub fn set_open(ctx: &Ctx, task: &str, criterion: &str, open: bool) -> Result<Task> {
    update(ctx, task, |t| {
        item(t, criterion)?.open = open;
        Ok(())
    })
}

/// The user reviewed the criterion: the current versions of its elements.
pub fn mark_reviewed(ctx: &Ctx, task: &str, criterion: &str) -> Result<Task> {
    let ctx2 = ctx;
    update(ctx, task, |t| {
        let c = item(t, criterion)?;
        c.review = Some(Review {
            at: timestamp(std::time::SystemTime::now()),
            versions: current_versions(ctx2, c.implementation.keys().cloned()),
        });
        Ok(())
    })
}

fn level_of(recorded: &Versions, now: &Versions, have_ids: &Versions) -> Level {
    // Current when every implementing element still has the recorded version.
    if have_ids
        .keys()
        .all(|id| recorded.get(id) == now.get(id) && now.contains_key(id))
    {
        Level::Current
    } else {
        Level::Stale
    }
}

/// The three answers for a criterion.
pub fn status(ctx: &Ctx, c: &Criterion) -> Status {
    let now = current_versions(ctx, c.implementation.keys().cloned());
    let implementation = if c.implementation.is_empty() {
        if c.open { Level::Open } else { Level::None }
    } else {
        level_of(&c.implementation, &now, &c.implementation)
    };
    let verification = match &c.verification {
        None => Level::None,
        Some(v) if !v.passed => Level::Failing,
        Some(v) => level_of(&v.versions, &now, &c.implementation),
    };
    let review = match &c.review {
        None => Level::None,
        Some(r) => level_of(&r.versions, &now, &c.implementation),
    };
    Status {
        implementation,
        verification,
        review,
    }
}

/// Results of a test run: `(test name, passed)`.
pub type TestResults = Vec<(String, bool)>;

/// Test outcomes from cargo (`test a::b ... ok`) or dotnet (`Passed Name`, `Failed Name`) output.
pub fn parse_test_output(out: &str) -> TestResults {
    let mut res = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("test ") {
            if let Some((name, outcome)) = rest.rsplit_once(" ... ") {
                match outcome.trim() {
                    "ok" => res.push((name.trim().to_string(), true)),
                    "FAILED" => res.push((name.trim().to_string(), false)),
                    _ => {}
                }
            }
        } else if let Some(rest) = l.strip_prefix("Passed ") {
            res.push((rest.split(" [").next().unwrap_or(rest).trim().to_string(), true));
        } else if let Some(rest) = l.strip_prefix("Failed ") {
            res.push((rest.split(" [").next().unwrap_or(rest).trim().to_string(), false));
        }
    }
    res
}

/// Records verification of every criterion whose test patterns matched something. Returns
/// how many criteria were updated.
pub fn apply_results(ctx: &Ctx, results: &TestResults) -> Result<usize> {
    let mut updated = 0;
    for mut t in list(ctx) {
        let mut changed = false;
        for c in t.items.iter_mut().filter(|c| !c.tests.is_empty()) {
            let matched: Vec<&(String, bool)> = results
                .iter()
                .filter(|(name, _)| c.tests.iter().any(|p| name.contains(p.as_str())))
                .collect();
            if matched.is_empty() {
                continue;
            }
            c.verification = Some(Verification {
                passed: matched.iter().all(|(_, ok)| *ok),
                matched: matched.len(),
                at: timestamp(std::time::SystemTime::now()),
                versions: current_versions(ctx, c.implementation.keys().cloned()),
            });
            changed = true;
            updated += 1;
        }
        if changed {
            save(ctx, &t)?;
        }
    }
    Ok(updated)
}

/// The configured test command, or `cargo test --workspace` in a Cargo repository.
pub fn test_command(ctx: &Ctx) -> Option<String> {
    ctx.config.test_command.clone().or_else(|| {
        ctx.root
            .join("Cargo.toml")
            .exists()
            .then(|| "cargo test --workspace".to_string())
    })
}

/// Runs the tests and records the results as verification (origin `TestResult`).
pub fn verify(ctx: &Ctx) -> Result<(usize, usize)> {
    let cmd = test_command(ctx).ok_or_else(|| anyhow!("set \"test_command\" in .rekon/config.json"))?;
    let words = shell_split(&cmd);
    let (program, args) = words.split_first().ok_or_else(|| anyhow!("empty test_command"))?;
    let out = Command::new(program)
        .args(args)
        .current_dir(&ctx.root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| anyhow!("cannot run {program}: {e}"))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let results = parse_test_output(&text);
    let n = apply_results(ctx, &results)?;
    Ok((results.len(), n))
}

/// Splits a command on spaces, keeping double-quoted parts together.
fn shell_split(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in cmd.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Where a status answer comes from, for labels in the UI.
pub fn origin_of_verification() -> Origin {
    Origin::TestResult
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    fn ctx() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rekon")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn retry() {\n    go();\n}\n").unwrap();
        let config = Config {
            backend: "fake".into(),
            ..Config::default()
        };
        let ctx = Ctx::with_backend(dir.path(), config, Arc::new(FakeBackend::default()));
        (dir, ctx)
    }

    const ID: &str = "a.rs::a::retry";

    #[test]
    fn three_separate_answers_and_staleness() {
        let (d, ctx) = ctx();
        let t = new_task(
            &ctx,
            "Payments",
            &["Retry after a short failure".into(), "No double charge".into()],
        )
        .unwrap();
        let s = status(&ctx, &t.items[0]);
        assert_eq!(
            (s.implementation, s.verification, s.review),
            (Level::None, Level::None, Level::None)
        );

        link_implementation(&ctx, &t.id, "c1", ID).unwrap();
        link_test(&ctx, &t.id, "c1", "retry_works").unwrap();
        mark_reviewed(&ctx, &t.id, "c1").unwrap();
        apply_results(&ctx, &vec![("tests::retry_works".into(), true)]).unwrap();
        let t = get(&ctx, &t.id).unwrap();
        let s = status(&ctx, &t.items[0]);
        assert_eq!(
            (s.implementation, s.verification, s.review),
            (Level::Current, Level::Current, Level::Current)
        );

        std::fs::write(d.path().join("a.rs"), "pub fn retry() {\n    go();\n    again();\n}\n").unwrap();
        let s = status(&ctx, &t.items[0]);
        assert_eq!(
            (s.implementation, s.verification, s.review),
            (Level::Stale, Level::Stale, Level::Stale)
        );

        // Vouching again renews the implementation only; tests and review must be redone.
        let t = touch(&ctx, &t.id, "c1").unwrap();
        let s = status(&ctx, &t.items[0]);
        assert_eq!(
            (s.implementation, s.verification, s.review),
            (Level::Current, Level::Stale, Level::Stale)
        );

        apply_results(&ctx, &vec![("tests::retry_works".into(), false)]).unwrap();
        let t = get(&ctx, &t.id).unwrap();
        assert_eq!(status(&ctx, &t.items[0]).verification, Level::Failing);
    }

    #[test]
    fn open_decision_is_not_missing_work() {
        let (_d, ctx) = ctx();
        let t = new_task(&ctx, "T", &["Manual resume".into()]).unwrap();
        let t = set_open(&ctx, &t.id, "c1", true).unwrap();
        assert_eq!(status(&ctx, &t.items[0]).implementation, Level::Open);
    }

    #[test]
    fn parses_cargo_and_dotnet_output() {
        let r = parse_test_output(
            "running 2 tests\ntest a::b ... ok\ntest a::c ... FAILED\ntest a::d ... ignored\n  Passed Shop.Tests.Pays [12 ms]\n  Failed Shop.Tests.Fails [1 ms]\n",
        );
        assert_eq!(
            r,
            vec![
                ("a::b".to_string(), true),
                ("a::c".to_string(), false),
                ("Shop.Tests.Pays".to_string(), true),
                ("Shop.Tests.Fails".to_string(), false)
            ]
        );
        assert_eq!(
            shell_split("dotnet test --logger \"console;verbosity=normal\""),
            ["dotnet", "test", "--logger", "console;verbosity=normal"]
        );
    }

    #[test]
    fn criteria_from_a_card() {
        let (_d, ctx) = ctx();
        let mut c = Card::spec("Retry", "fails", "retries");
        c.consequences = vec!["show status".into()];
        let t = from_card(&ctx, &c).unwrap();
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.items[1].text, "show status");
    }
}
