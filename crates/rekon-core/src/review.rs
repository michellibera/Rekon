//! Personal review baseline: for every element the version the user last marked as
//! understood (opening a file never counts). Comparing it with the current code tells
//! what changed since, and how much it matters: formatting only, a rename, a real
//! change, or an unchanged element whose caller changed. A model can turn a real change
//! into an update of the mental model. Data: `.rekon/review/baseline.json`, old text in
//! `.rekon/objects/`, model updates in `.rekon/review/updates/`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::Ctx;
use crate::element::{self, Element, calls};
use crate::model::{Origin, Stamped};
use crate::prompts;
use crate::store::{Store, timestamp};

pub const BASELINE_VERSION: u32 = 1;

/// An element as it was when the user marked it as understood.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reviewed {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub hash: String,
    pub norm: String,
    pub shape: String,
    /// Key of the saved text in `.rekon/objects/`.
    pub snapshot: String,
    pub at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Baseline {
    pub version: u32,
    pub items: BTreeMap<String, Reviewed>,
}

pub fn baseline_path(store: &Store) -> PathBuf {
    store.dir().join("review").join("baseline.json")
}

pub fn load(store: &Store) -> Baseline {
    store.read_json(&baseline_path(store))
}

/// How an element differs from the version the user reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Unchanged,
    /// Whitespace or `//` comments only.
    FormatOnly,
    Changed,
    /// Same code under another name (the new id).
    Renamed(String),
    Removed,
    /// New element in a file the user reviewed.
    Added,
    /// The element is the same, but code that calls it changed.
    CallerChanged,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Unchanged => "unchanged",
            Status::FormatOnly => "format only",
            Status::Changed => "changed",
            Status::Renamed(_) => "renamed",
            Status::Removed => "removed",
            Status::Added => "added",
            Status::CallerChanged => "caller changed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Id in the baseline (for `Added`: the new id).
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub status: Status,
    /// Current lines; 0 when the element is gone.
    pub start: u32,
    pub end: u32,
    /// Key of the reviewed text (`None` for `Added`) and of the current one.
    pub snapshot: Option<String>,
    pub new_hash: Option<String>,
    /// Changed elements that call this one (`CallerChanged`); found statically.
    pub callers: Vec<String>,
}

impl Change {
    pub fn origin(&self) -> Origin {
        Origin::Static
    }
}

fn reviewed(e: &Element, snapshot: String, at: &str) -> Reviewed {
    Reviewed {
        path: e.path.clone(),
        name: e.name.clone(),
        kind: e.kind.clone(),
        hash: e.hash.clone(),
        norm: e.norm.clone(),
        shape: e.shape.clone(),
        snapshot,
        at: at.to_string(),
    }
}

fn read_elements(ctx: &Ctx, path: &str) -> Vec<Element> {
    match std::fs::read(ctx.root.join(path)) {
        Ok(bytes) => element::elements(path, &String::from_utf8_lossy(&bytes)),
        Err(_) => Vec::new(),
    }
}

fn now() -> String {
    timestamp(std::time::SystemTime::now())
}

/// Marks every element of a file as understood in its current version; returns their count.
pub fn mark_file(ctx: &Ctx, path: &str) -> Result<usize> {
    let elements = read_elements(ctx, path);
    if elements.is_empty() {
        bail!("cannot read {path}");
    }
    let at = now();
    let mut entries = Vec::new();
    for e in &elements {
        entries.push((e.id.clone(), reviewed(e, ctx.store.put_object(&e.text)?, &at)));
    }
    let n = entries.len();
    ctx.store.update_json(&baseline_path(&ctx.store), |b: &mut Baseline| {
        b.version = BASELINE_VERSION;
        b.items.retain(|_, r| r.path != path);
        b.items.extend(entries);
    })?;
    Ok(n)
}

/// Accepts one change: the element (or its rename target, or its removal) becomes the
/// new reviewed version.
pub fn accept(ctx: &Ctx, change: &Change) -> Result<()> {
    let new_id = match &change.status {
        Status::Renamed(to) => Some(to.as_str()),
        Status::Removed => None,
        _ => Some(change.id.as_str()),
    };
    let entry = match new_id {
        Some(id) => {
            let e = read_elements(ctx, &change.path)
                .into_iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow!("{id} not found in {}", change.path))?;
            let snapshot = ctx.store.put_object(&e.text)?;
            Some((e.id.clone(), reviewed(&e, snapshot, &now())))
        }
        None => None,
    };
    ctx.store.update_json(&baseline_path(&ctx.store), |b: &mut Baseline| {
        b.version = BASELINE_VERSION;
        b.items.remove(&change.id);
        if let Some((id, r)) = entry {
            b.items.insert(id, r);
        }
    })
}

/// Everything that differs from the baseline, by path and position. Only files with a
/// baseline are compared; callers are looked for in those files too.
pub fn changes(ctx: &Ctx) -> Result<Vec<Change>> {
    let baseline = load(&ctx.store);
    let paths: Vec<&str> = {
        let mut p: Vec<&str> = baseline.items.values().map(|r| r.path.as_str()).collect();
        p.dedup();
        p.sort_unstable();
        p.dedup();
        p
    };
    let mut out: Vec<Change> = Vec::new();
    let mut current: Vec<Element> = Vec::new();
    for path in paths {
        let now = read_elements(ctx, path);
        let mut used: HashSet<String> = HashSet::new();
        for (id, r) in baseline.items.iter().filter(|(_, r)| r.path == path) {
            let mut c = Change {
                id: id.clone(),
                path: path.to_string(),
                name: r.name.clone(),
                kind: r.kind.clone(),
                status: Status::Removed,
                start: 0,
                end: 0,
                snapshot: Some(r.snapshot.clone()),
                new_hash: None,
                callers: Vec::new(),
            };
            let same = now.iter().find(|e| &e.id == id);
            let renamed = || {
                now.iter()
                    .find(|e| !baseline.items.contains_key(&e.id) && e.kind == r.kind && e.shape == r.shape)
            };
            if let Some(e) = same {
                used.insert(e.id.clone());
                c.status = if e.hash == r.hash {
                    Status::Unchanged
                } else if e.norm == r.norm {
                    Status::FormatOnly
                } else {
                    Status::Changed
                };
                (c.start, c.end, c.new_hash) = (e.start, e.end, Some(e.hash.clone()));
            } else if let Some(e) = renamed() {
                used.insert(e.id.clone());
                c.status = Status::Renamed(e.id.clone());
                (c.start, c.end, c.new_hash) = (e.start, e.end, Some(e.hash.clone()));
            }
            out.push(c);
        }
        for e in &now {
            if !used.contains(&e.id) && !baseline.items.contains_key(&e.id) {
                out.push(Change {
                    id: e.id.clone(),
                    path: path.to_string(),
                    name: e.name.clone(),
                    kind: e.kind.clone(),
                    status: Status::Added,
                    start: e.start,
                    end: e.end,
                    snapshot: None,
                    new_hash: Some(e.hash.clone()),
                    callers: Vec::new(),
                });
            }
        }
        current.extend(now);
    }
    // Unchanged elements called from something that changed.
    let moved: Vec<&Element> = current
        .iter()
        .filter(|e| {
            out.iter().any(|c| {
                matches!(c.status, Status::Changed | Status::Added | Status::Renamed(_))
                    && (c.id == e.id || matches!(&c.status, Status::Renamed(to) if to == &e.id))
            })
        })
        .collect();
    for c in out.iter_mut().filter(|c| c.status == Status::Unchanged) {
        if c.name.chars().count() < 4 || c.kind == "file" {
            continue;
        }
        c.callers = moved
            .iter()
            .filter(|e| e.id != c.id && calls(&e.text, &c.name))
            .map(|e| e.id.clone())
            .collect();
        if !c.callers.is_empty() {
            c.status = Status::CallerChanged;
        }
    }
    out.retain(|c| c.status != Status::Unchanged);
    out.sort_by(|a, b| (&a.path, a.start, &a.id).cmp(&(&b.path, b.start, &b.id)));
    Ok(out)
}

/// What a model says about a real change: the assumption that held, what is new, and
/// the concrete points, each tied to a line.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Update {
    pub assumed: String,
    pub now: String,
    pub points: Vec<Point>,
    /// `skim`, `review` or `careful`.
    pub effort: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Point {
    pub text: String,
    pub line: u32,
}

fn update_path(store: &Store, change: &Change) -> Option<PathBuf> {
    let key = format!("{}_{}", change.snapshot.as_ref()?, change.new_hash.as_ref()?);
    Some(store.dir().join("review").join("updates").join(format!("{key}.json")))
}

/// The saved update for this exact pair of versions, if there is one.
pub fn cached_update(store: &Store, change: &Change) -> Option<Stamped<Update>> {
    let path = update_path(store, change)?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Asks the model for the update of the mental model (or returns the saved one).
pub fn update(ctx: &Ctx, change: &Change) -> Result<Stamped<Update>> {
    if let Some(u) = cached_update(&ctx.store, change) {
        return Ok(u);
    }
    let (Some(snapshot), Some(new_hash), Some(path)) =
        (&change.snapshot, &change.new_hash, update_path(&ctx.store, change))
    else {
        bail!("nothing to compare: {} has no reviewed version", change.id);
    };
    if !matches!(change.status, Status::Changed | Status::Renamed(_)) {
        bail!("{} is {}: nothing to explain", change.id, change.status.label());
    }
    let before = ctx
        .store
        .object(snapshot)
        .ok_or_else(|| anyhow!("the reviewed version of {} is missing", change.id))?;
    let text = std::fs::read_to_string(ctx.root.join(&change.path))?;
    let lines: Vec<&str> = text.lines().collect();
    let (a, b) = (change.start as usize, change.end as usize);
    let after = lines.get(a.saturating_sub(1)..b.min(lines.len())).unwrap_or(&[]);
    let req = prompts::review_request(
        &ctx.config,
        &ctx.system_prompt(),
        &change.id,
        &before,
        after,
        change.start,
    );
    let json = ctx.ask(&req)?.json;
    let text = |k: &str| json.get(k).and_then(|v| v.as_str()).and_then(prompts::clean_text);
    let points = json
        .get("points")
        .and_then(|p| p.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let line = u32::try_from(p.get("line")?.as_u64()?).ok()?;
                    let t = prompts::clean_text(p.get("text")?.as_str()?)?;
                    (line >= change.start && line <= change.end).then_some(Point { text: t, line })
                })
                .collect()
        })
        .unwrap_or_default();
    let effort = match json.get("effort").and_then(|v| v.as_str()) {
        Some(e @ ("skim" | "review" | "careful")) => e.to_string(),
        _ => "review".to_string(),
    };
    let stamped = Stamped {
        hash: new_hash.clone(),
        origin: Origin::Ai,
        value: Update {
            assumed: text("assumed").ok_or_else(|| anyhow!("empty answer"))?,
            now: text("now").ok_or_else(|| anyhow!("empty answer"))?,
            points,
            effort,
        },
    };
    crate::store::write_json_atomic(&path, &stamped)?;
    Ok(stamped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    const SRC: &str = "fn save(o: u32) {\n    charge(o);\n}\n\nfn charge(o: u32) {\n    remit(o);\n}\n\nfn confirm() {\n    send();\n}\n\nfn remit(o: u32) {\n    bank(o);\n}\n";

    fn setup() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rekon")).unwrap();
        std::fs::write(dir.path().join("a.rs"), SRC).unwrap();
        let config = Config {
            backend: "fake".into(),
            ..Config::default()
        };
        let ctx = Ctx::with_backend(dir.path(), config, Arc::new(FakeBackend::default()));
        (dir, ctx)
    }

    fn write(dir: &tempfile::TempDir, text: &str) {
        std::fs::write(dir.path().join("a.rs"), text).unwrap();
    }

    fn status(ctx: &Ctx, name: &str) -> Option<Status> {
        changes(ctx)
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.status)
    }

    #[test]
    fn nothing_changed_after_marking() {
        let (_d, ctx) = setup();
        assert_eq!(mark_file(&ctx, "a.rs").unwrap(), 4);
        assert!(changes(&ctx).unwrap().is_empty());
    }

    #[test]
    fn classifies_changes() {
        let (d, ctx) = setup();
        mark_file(&ctx, "a.rs").unwrap();
        let text = SRC
            .replace("    send();", "    // note\n    send( );")
            .replace("remit(o);", "remit(o); retry(o);");
        write(&d, &text);
        assert_eq!(status(&ctx, "confirm"), Some(Status::FormatOnly));
        assert_eq!(status(&ctx, "charge"), Some(Status::Changed));
        let remit = changes(&ctx).unwrap().into_iter().find(|c| c.name == "remit").unwrap();
        assert_eq!(remit.status, Status::CallerChanged);
        assert_eq!(remit.callers, ["a.rs::a::charge"]);
    }

    #[test]
    fn detects_rename_add_remove() {
        let (d, ctx) = setup();
        mark_file(&ctx, "a.rs").unwrap();
        write(
            &d,
            &SRC.replace("confirm", "acknowledge")
                .replace("fn save", "fn extra() {}\n\nfn save"),
        );
        assert_eq!(
            status(&ctx, "confirm"),
            Some(Status::Renamed("a.rs::a::acknowledge".into()))
        );
        assert_eq!(status(&ctx, "extra"), Some(Status::Added));
        write(&d, "");
        assert_eq!(status(&ctx, "save"), Some(Status::Removed));
    }

    #[test]
    fn accepting_resets_the_baseline() {
        let (d, ctx) = setup();
        mark_file(&ctx, "a.rs").unwrap();
        write(&d, &SRC.replace("remit(o);", "remit(o); retry(o);"));
        let c = changes(&ctx).unwrap().into_iter().find(|c| c.name == "charge").unwrap();
        accept(&ctx, &c).unwrap();
        assert!(status(&ctx, "charge").is_none());
    }

    #[test]
    fn update_is_grounded_and_cached() {
        let (d, ctx) = setup();
        mark_file(&ctx, "a.rs").unwrap();
        write(&d, &SRC.replace("remit(o);", "remit(o); retry(o);"));
        let c = changes(&ctx).unwrap().into_iter().find(|c| c.name == "charge").unwrap();
        let u = update(&ctx, &c).unwrap();
        assert_eq!(u.origin, Origin::Ai);
        assert!(u.value.points.iter().all(|p| p.line >= c.start && p.line <= c.end));
        assert_eq!(u.value.points.len(), 1);
        let again = update(&ctx, &c).unwrap();
        assert_eq!(u, again);
        assert!(cached_update(&ctx.store, &c).is_some());
    }

    #[test]
    fn update_refuses_format_only() {
        let (d, ctx) = setup();
        mark_file(&ctx, "a.rs").unwrap();
        write(&d, &SRC.replace("send();", "send( );"));
        let c = changes(&ctx)
            .unwrap()
            .into_iter()
            .find(|c| c.name == "confirm")
            .unwrap();
        assert!(update(&ctx, &c).is_err());
    }
}
