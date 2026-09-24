//! Reading and writing `.rekon/`: atomic writes, a cross-process lock, and the
//! guarded writes that keep automatic jobs from overwriting fresher data.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::hash;
use crate::model::{Author, Block, Blocks, DirNote, FileNote, NOTE_VERSION, ProjectNote, Summary};

pub const DEFAULT_STYLE: &str = include_str!("../prompts/style.md");

/// Which element a summary belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    File(&'a str),
    Dir(&'a str),
    Project,
}

/// State remembered by an automatic job when it starts.
#[derive(Clone, Debug)]
pub struct Guard {
    /// Key of the element when the job started.
    pub start_key: String,
    /// Overwrite even when the note already has a fresh part (`r`, `--force`).
    pub force: bool,
}

pub struct Store {
    root: PathBuf,
    dir: PathBuf,
    lock: Mutex<()>,
    log_lock: Mutex<()>,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dir: root.join(crate::MAP_DIR),
            lock: Mutex::new(()),
            log_lock: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn exists(&self) -> bool {
        self.dir.is_dir()
    }

    pub fn file_note_path(&self, rel: &str) -> PathBuf {
        self.dir.join("notes").join("files").join(format!("{rel}.json"))
    }

    pub fn dir_note_path(&self, rel: &str) -> PathBuf {
        self.dir.join("notes").join("dirs").join(format!("{rel}.json"))
    }

    pub fn project_path(&self) -> PathBuf {
        self.dir.join("project.json")
    }

    pub fn style(&self) -> String {
        std::fs::read_to_string(self.dir.join("style.md")).unwrap_or_else(|_| DEFAULT_STYLE.to_string())
    }

    pub fn file_note(&self, rel: &str) -> FileNote {
        self.read_or_default(&self.file_note_path(rel))
    }

    pub fn dir_note(&self, rel: &str) -> DirNote {
        self.read_or_default(&self.dir_note_path(rel))
    }

    pub fn project_note(&self) -> ProjectNote {
        self.read_or_default(&self.project_path())
    }

    /// Summary of a target, as stored.
    pub fn summary(&self, target: Target) -> Option<Summary> {
        match target {
            Target::File(p) => self.file_note(p).summary,
            Target::Dir(p) => self.dir_note(p).summary,
            Target::Project => self.project_note().summary,
        }
    }

    fn read_or_default<T: DeserializeOwned + Default>(&self, path: &Path) -> T {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                self.log(&format!("warning: ignoring invalid note {}: {e}", path.display()));
                T::default()
            }),
            Err(_) => T::default(),
        }
    }

    /// Runs `f` under the in-process mutex and the `.rekon/.lock` file lock.
    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(".lock"))
            .context("cannot open .rekon/.lock")?;
        file.lock().context("cannot lock .rekon/.lock")?;
        let result = f();
        let _ = file.unlock();
        result
    }

    /// Read-modify-write of one note under the lock; `f` returns whether to write.
    fn update<T>(&self, path: &Path, f: impl FnOnce(&mut T) -> bool) -> Result<bool>
    where
        T: Serialize + DeserializeOwned + Default + VersionedNote,
    {
        self.locked(|| {
            let mut note: T = self.read_or_default(path);
            if !f(&mut note) {
                return Ok(false);
            }
            note.set_version();
            write_json_atomic(path, &note)?;
            Ok(true)
        })
    }

    pub fn update_file(&self, rel: &str, f: impl FnOnce(&mut FileNote) -> bool) -> Result<bool> {
        self.update(&self.file_note_path(rel), f)
    }

    pub fn update_dir(&self, rel: &str, f: impl FnOnce(&mut DirNote) -> bool) -> Result<bool> {
        self.update(&self.dir_note_path(rel), f)
    }

    pub fn update_project(&self, f: impl FnOnce(&mut ProjectNote) -> bool) -> Result<bool> {
        self.update(&self.project_path(), f)
    }

    /// Current key of a file (content hash), `None` when unreadable.
    pub fn file_key(&self, rel: &str) -> Option<String> {
        hash::hash_file(&self.root.join(rel)).ok()
    }

    /// Guarded write of an automatic summary. `current_key` is evaluated under the
    /// lock (for files it defaults to the content hash when `None` is passed).
    /// The result is dropped when the key changed since the job started, or when the
    /// note already has a fresh summary for that key (unless `force`).
    pub fn put_summary_guarded(
        &self,
        target: Target,
        guard: &Guard,
        current_key: Option<&dyn Fn() -> Option<String>>,
        text: &str,
        overview: Option<&str>,
    ) -> Result<bool> {
        let current = || match (current_key, target) {
            (Some(f), _) => f(),
            (None, Target::File(p)) => self.file_key(p),
            (None, _) => None,
        };
        let accept = |existing: &Option<Summary>| {
            current().as_deref() == Some(guard.start_key.as_str())
                && (guard.force || existing.as_ref().is_none_or(|s| s.hash != guard.start_key))
        };
        let summary = Summary {
            hash: guard.start_key.clone(),
            text: text.to_string(),
            by: Author::Auto,
        };
        match target {
            Target::File(p) => self.update_file(p, |n| {
                accept(&n.summary) && {
                    n.summary = Some(summary);
                    true
                }
            }),
            Target::Dir(p) => self.update_dir(p, |n| {
                accept(&n.summary) && {
                    n.summary = Some(summary);
                    true
                }
            }),
            Target::Project => self.update_project(|n| {
                accept(&n.summary) && {
                    n.summary = Some(summary);
                    n.overview = overview.map(str::to_string);
                    true
                }
            }),
        }
    }

    /// Unconditional write (used by `apply`): the summary gets the given key.
    pub fn put_summary(&self, target: Target, key: &str, text: &str, by: Author, overview: Option<&str>) -> Result<()> {
        let summary = Summary {
            hash: key.to_string(),
            text: text.to_string(),
            by,
        };
        match target {
            Target::File(p) => self.update_file(p, |n| {
                n.summary = Some(summary);
                true
            }),
            Target::Dir(p) => self.update_dir(p, |n| {
                n.summary = Some(summary);
                true
            }),
            Target::Project => self.update_project(|n| {
                n.summary = Some(summary);
                if let Some(o) = overview {
                    n.overview = Some(o.to_string());
                }
                true
            }),
        }?;
        Ok(())
    }

    /// Guarded write of level-1 blocks.
    pub fn put_level1(&self, rel: &str, guard: &Guard, items: Vec<Block>) -> Result<bool> {
        self.update_file(rel, |n| {
            let fresh = n.blocks.as_ref().is_some_and(|b| b.hash == guard.start_key);
            if self.file_key(rel).as_deref() != Some(guard.start_key.as_str()) || (fresh && !guard.force) {
                return false;
            }
            n.blocks = Some(Blocks {
                hash: guard.start_key.clone(),
                items,
            });
            true
        })
    }

    /// Guarded write of the children of the block with `range`. Requires fresh
    /// blocks and an existing block that is not split yet (unless `force`).
    pub fn put_children(&self, rel: &str, guard: &Guard, range: (u32, u32), children: Vec<Block>) -> Result<bool> {
        self.update_file(rel, |n| {
            if self.file_key(rel).as_deref() != Some(guard.start_key.as_str()) {
                return false;
            }
            let Some(blocks) = n.blocks.as_mut().filter(|b| b.hash == guard.start_key) else {
                return false;
            };
            match blocks.find_mut(range) {
                Some(b) if b.children.is_none() || guard.force => {
                    b.children = Some(children);
                    true
                }
                _ => false,
            }
        })
    }

    pub fn remove_file_note(&self, rel: &str) -> Result<()> {
        self.locked(|| remove_if_exists(&self.file_note_path(rel)))
    }

    pub fn remove_dir_note(&self, rel: &str) -> Result<()> {
        self.locked(|| remove_if_exists(&self.dir_note_path(rel)))
    }

    /// Relative paths of all stored file notes.
    pub fn file_note_paths(&self) -> Vec<String> {
        list_notes(&self.dir.join("notes").join("files"))
    }

    /// Relative paths of all stored folder notes.
    pub fn dir_note_paths(&self) -> Vec<String> {
        list_notes(&self.dir.join("notes").join("dirs"))
    }

    /// Appends one line to `.rekon/rekon.log`. Never fails loudly.
    pub fn log(&self, line: &str) {
        if !self.exists() {
            return;
        }
        let _guard = self.log_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("rekon.log"))
        {
            let _ = writeln!(f, "{} {}", timestamp(SystemTime::now()), line.replace('\n', " "));
        }
    }
}

pub trait VersionedNote {
    fn set_version(&mut self);
}

impl VersionedNote for FileNote {
    fn set_version(&mut self) {
        self.version = NOTE_VERSION;
    }
}

impl VersionedNote for DirNote {
    fn set_version(&mut self) {
        self.version = NOTE_VERSION;
    }
}

impl VersionedNote for ProjectNote {
    fn set_version(&mut self) {
        self.version = NOTE_VERSION;
    }
}

/// Writes JSON to a temporary file in the same folder, then renames it into place.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)? + "\n";
    write_atomic(path, text.as_bytes())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path without parent")?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("note");
    let tmp = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn list_notes(base: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(rel) = path
                .strip_prefix(base)
                .ok()
                .and_then(|r| r.to_str())
                .and_then(|r| r.strip_suffix(".json"))
            {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.starts_with('.') || !name.contains(".tmp-") {
                    out.push(rel.replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` in UTC.
pub fn timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rekon")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        let store = Store::new(dir.path());
        (dir, store)
    }

    #[test]
    fn note_paths_do_not_collide() {
        let (_d, s) = setup();
        assert!(s.file_note_path("src/api").ends_with("notes/files/src/api.json"));
        assert!(s.dir_note_path("src/api").ends_with("notes/dirs/src/api.json"));
        assert_ne!(s.file_note_path("x"), s.dir_note_path("x"));
    }

    #[test]
    fn guarded_write_rejects_changed_key() {
        let (d, s) = setup();
        let key = s.file_key("src/a.rs").unwrap();
        let guard = Guard {
            start_key: key.clone(),
            force: false,
        };
        std::fs::write(d.path().join("src/a.rs"), "fn b() {}\n").unwrap();
        assert!(
            !s.put_summary_guarded(Target::File("src/a.rs"), &guard, None, "x", None)
                .unwrap()
        );
        assert!(s.file_note("src/a.rs").summary.is_none());
    }

    #[test]
    fn guarded_write_keeps_agent_summary_unless_forced() {
        let (_d, s) = setup();
        let key = s.file_key("src/a.rs").unwrap();
        s.put_summary(Target::File("src/a.rs"), &key, "by agent", Author::Agent, None)
            .unwrap();
        let guard = Guard {
            start_key: key.clone(),
            force: false,
        };
        assert!(
            !s.put_summary_guarded(Target::File("src/a.rs"), &guard, None, "auto", None)
                .unwrap()
        );
        assert_eq!(s.file_note("src/a.rs").summary.unwrap().text, "by agent");
        let forced = Guard { force: true, ..guard };
        assert!(
            s.put_summary_guarded(Target::File("src/a.rs"), &forced, None, "auto", None)
                .unwrap()
        );
        let summary = s.file_note("src/a.rs").summary.unwrap();
        assert_eq!((summary.text.as_str(), summary.by), ("auto", Author::Auto));
    }

    #[test]
    fn guarded_write_replaces_stale_summary() {
        let (_d, s) = setup();
        s.put_summary(Target::File("src/a.rs"), "old-key", "old", Author::Agent, None)
            .unwrap();
        let guard = Guard {
            start_key: s.file_key("src/a.rs").unwrap(),
            force: false,
        };
        assert!(
            s.put_summary_guarded(Target::File("src/a.rs"), &guard, None, "new", None)
                .unwrap()
        );
    }

    #[test]
    fn dir_guard_uses_given_key() {
        let (_d, s) = setup();
        let guard = Guard {
            start_key: "k1".into(),
            force: false,
        };
        let now_k2 = || Some("k2".to_string());
        assert!(
            !s.put_summary_guarded(Target::Dir("src"), &guard, Some(&now_k2), "x", None)
                .unwrap()
        );
        let now_k1 = || Some("k1".to_string());
        assert!(
            s.put_summary_guarded(Target::Dir("src"), &guard, Some(&now_k1), "x", None)
                .unwrap()
        );
        assert_eq!(s.dir_note_paths(), ["src"]);
    }

    #[test]
    fn children_need_fresh_unsplit_block() {
        let (_d, s) = setup();
        let key = s.file_key("src/a.rs").unwrap();
        let guard = Guard {
            start_key: key.clone(),
            force: false,
        };
        let items = vec![Block::new(1, 5, "a"), Block::new(6, 9, "b")];
        assert!(s.put_level1("src/a.rs", &guard, items).unwrap());
        let kids = vec![Block::new(1, 2, "x"), Block::new(3, 5, "y")];
        assert!(!s.put_children("src/a.rs", &guard, (1, 4), kids.clone()).unwrap());
        assert!(s.put_children("src/a.rs", &guard, (1, 5), kids.clone()).unwrap());
        // Second split of the same block is ignored without force.
        assert!(!s.put_children("src/a.rs", &guard, (1, 5), vec![]).unwrap());
        let note = s.file_note("src/a.rs");
        assert_eq!(
            note.blocks
                .unwrap()
                .find((1, 5))
                .unwrap()
                .children
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        // Level 1 is not replaced while fresh.
        assert!(!s.put_level1("src/a.rs", &guard, vec![]).unwrap());
    }

    #[test]
    fn concurrent_updates_do_not_lose_writes() {
        let (_d, s) = setup();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let s = &s;
                scope.spawn(move || {
                    s.update_project(|n| {
                        let mut o = n.overview.clone().unwrap_or_default();
                        o.push_str(&i.to_string());
                        n.overview = Some(o);
                        true
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(s.project_note().overview.unwrap().len(), 8);
    }

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_790_294_400)),
            "2026-09-25T00:00:00Z"
        );
    }
}
