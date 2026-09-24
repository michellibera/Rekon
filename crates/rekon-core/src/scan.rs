//! Repository listing (from git), file classification and the in-memory tree.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::config::Config;
use crate::hash;
use crate::text;

/// Bytes inspected for a NUL byte when detecting binary files.
const BINARY_SNIFF: usize = 8000;

/// Repository root of `start` (`git rev-parse --show-toplevel`).
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(text::NOT_A_REPO);
    }
    let root = String::from_utf8(out.stdout).context("repository path is not UTF-8")?;
    Ok(PathBuf::from(root.trim()))
}

/// Tracked and untracked-but-not-ignored files, relative to `root`, with `/` separators.
/// Returns the paths and warnings about skipped entries.
pub fn git_files(root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
        .context("cannot run git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        match std::str::from_utf8(raw) {
            Ok(p) if is_map_path(p) => {}
            Ok(p) => paths.push(p.to_string()),
            Err(_) => warnings.push(format!("skipped non-UTF-8 path {}", String::from_utf8_lossy(raw))),
        }
    }
    paths.sort();
    paths.dedup();
    Ok((paths, warnings))
}

fn is_map_path(p: &str) -> bool {
    p == crate::MAP_DIR || p.starts_with(&format!("{}/", crate::MAP_DIR))
}

/// Matches paths against `exclude` patterns (full path or file name).
pub struct Excluder {
    set: GlobSet,
    patterns: Vec<String>,
}

impl Excluder {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for p in patterns {
            builder.add(Glob::new(p).with_context(|| format!("invalid exclude pattern {p}"))?);
        }
        Ok(Self {
            set: builder.build()?,
            patterns: patterns.to_vec(),
        })
    }

    /// The first pattern matching `path`, if any.
    pub fn matched(&self, path: &str) -> Option<&str> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut hits = self.set.matches(path);
        if hits.is_empty() {
            hits = self.set.matches(name);
        }
        hits.into_iter().min().map(|i| self.patterns[i].as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Skipped(String),
    TooLarge,
    Binary,
}

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub size: u64,
    pub mtime: Option<SystemTime>,
    /// Filled by [`analyze`]; `None` before the file was read.
    pub kind: Option<Kind>,
    pub lines: Option<u32>,
    /// Content hash, only for text files.
    pub hash: Option<String>,
}

impl FileInfo {
    pub fn is_text(&self) -> bool {
        self.kind == Some(Kind::Text)
    }

    /// Static label for files that are never described by the model.
    pub fn label(&self) -> Option<String> {
        match self.kind.as_ref()? {
            Kind::Text => None,
            Kind::Skipped(p) => Some(text::skipped(p)),
            Kind::TooLarge => Some(text::too_large(self.size)),
            Kind::Binary => Some(text::binary(self.size)),
        }
    }
}

fn stat(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.len(), meta.modified().ok()))
}

/// Classifies the file and fills kind, line count and hash.
pub fn analyze(abs: &Path, rel: &str, config: &Config, excluder: &Excluder, info: &mut FileInfo) {
    if let Some(p) = excluder.matched(rel) {
        info.kind = Some(Kind::Skipped(p.to_string()));
        return;
    }
    if info.size > config.max_file_bytes {
        info.kind = Some(Kind::TooLarge);
        return;
    }
    let mut bytes = Vec::with_capacity(info.size as usize);
    if std::fs::File::open(abs)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .is_err()
    {
        info.kind = None;
        return;
    }
    info.size = bytes.len() as u64;
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        info.kind = Some(Kind::Binary);
        return;
    }
    info.kind = Some(Kind::Text);
    info.lines = Some(count_lines(&bytes));
    info.hash = Some(hash::hash_bytes(&bytes));
}

/// Same count as `str::lines()`.
pub fn count_lines(bytes: &[u8]) -> u32 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u32;
    if bytes.last().is_some_and(|b| *b != b'\n') {
        newlines + 1
    } else {
        newlines
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    /// Relative path; `""` for the root.
    pub path: String,
    pub is_dir: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub file: Option<FileInfo>,
}

/// Folders and files of the repository; folders before files, alphabetical.
#[derive(Clone, Debug)]
pub struct Tree {
    pub nodes: Vec<Node>,
    index: HashMap<String, usize>,
}

pub const ROOT: usize = 0;

impl Tree {
    /// Builds the tree from a git listing, with size and mtime but without reading files.
    pub fn list(root: &Path) -> Result<(Self, Vec<String>)> {
        let (paths, warnings) = git_files(root)?;
        let entries = paths.into_iter().filter_map(|p| {
            let (size, mtime) = stat(&root.join(&p))?;
            Some((
                p,
                FileInfo {
                    size,
                    mtime,
                    ..Default::default()
                },
            ))
        });
        Ok((Self::from_entries(entries), warnings))
    }

    /// Lists and analyzes every file.
    pub fn scan(root: &Path, config: &Config) -> Result<(Self, Vec<String>)> {
        let (mut tree, warnings) = Self::list(root)?;
        tree.analyze_all(root, config, None)?;
        Ok((tree, warnings))
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, FileInfo)>) -> Self {
        let mut tree = Tree {
            nodes: vec![Node {
                name: String::new(),
                path: String::new(),
                is_dir: true,
                parent: None,
                children: Vec::new(),
                file: None,
            }],
            index: HashMap::new(),
        };
        tree.index.insert(String::new(), ROOT);
        for (path, info) in entries {
            let mut parent = ROOT;
            let parts: Vec<&str> = path.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                let sub = parts[..=i].join("/");
                let is_file = i == parts.len() - 1;
                parent = match tree.index.get(&sub) {
                    Some(&idx) => idx,
                    None => {
                        let idx = tree.nodes.len();
                        tree.nodes.push(Node {
                            name: part.to_string(),
                            path: sub.clone(),
                            is_dir: !is_file,
                            parent: Some(parent),
                            children: Vec::new(),
                            file: is_file.then(|| info.clone()),
                        });
                        tree.nodes[parent].children.push(idx);
                        tree.index.insert(sub, idx);
                        idx
                    }
                };
            }
        }
        tree.sort();
        tree
    }

    fn sort(&mut self) {
        for i in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[i].children);
            children.sort_by(|&a, &b| {
                let (a, b) = (&self.nodes[a], &self.nodes[b]);
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.name.cmp(&b.name))
            });
            self.nodes[i].children = children;
        }
    }

    /// Analyzes files that are not analyzed yet (or all when `only` is `None` and
    /// they lack a kind), in parallel.
    pub fn analyze_all(&mut self, root: &Path, config: &Config, only: Option<&[usize]>) -> Result<()> {
        let excluder = Excluder::new(&config.exclude)?;
        let targets: Vec<usize> = match only {
            Some(ids) => ids.to_vec(),
            None => (0..self.nodes.len())
                .filter(|&i| self.nodes[i].file.as_ref().is_some_and(|f| f.kind.is_none()))
                .collect(),
        };
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let chunk = targets.len().div_ceil(threads).max(1);
        let results: Vec<(usize, FileInfo)> = std::thread::scope(|s| {
            let handles: Vec<_> = targets
                .chunks(chunk)
                .map(|ids| {
                    let nodes = &self.nodes;
                    let excluder = &excluder;
                    s.spawn(move || {
                        ids.iter()
                            .filter_map(|&i| {
                                let mut info = nodes[i].file.clone()?;
                                analyze(&root.join(&nodes[i].path), &nodes[i].path, config, excluder, &mut info);
                                Some((i, info))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        for (i, info) in results {
            self.nodes[i].file = Some(info);
        }
        Ok(())
    }

    /// Re-stats one file and re-analyzes it when mtime or size changed.
    /// Returns true when the file info changed.
    pub fn refresh_file(&mut self, i: usize, root: &Path, config: &Config, excluder: &Excluder) -> bool {
        let node = &self.nodes[i];
        let Some(old) = node.file.as_ref() else { return false };
        let Some((size, mtime)) = stat(&root.join(&node.path)) else {
            return false;
        };
        if old.kind.is_some() && old.size == size && old.mtime == mtime {
            return false;
        }
        let mut info = FileInfo {
            size,
            mtime,
            ..Default::default()
        };
        analyze(&root.join(&node.path), &node.path, config, excluder, &mut info);
        let changed = info.hash != old.hash || info.kind != old.kind;
        self.nodes[i].file = Some(info);
        changed
    }

    /// Carries analysis results over from an older tree for files whose size and
    /// mtime did not change, so a rescan only reads what changed.
    pub fn reuse_from(&mut self, old: &Tree) {
        for node in &mut self.nodes {
            let (Some(info), Some(&j)) = (node.file.as_mut(), old.index.get(&node.path)) else {
                continue;
            };
            if let Some(prev) = &old.nodes[j].file
                && prev.kind.is_some()
                && prev.size == info.size
                && prev.mtime == info.mtime
            {
                *info = prev.clone();
            }
        }
    }

    pub fn get(&self, path: &str) -> Option<usize> {
        self.index.get(path.trim_end_matches('/')).copied()
    }

    pub fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    /// Key of a folder: hash of sorted child names.
    pub fn dir_key(&self, i: usize) -> String {
        hash::dir_key(self.nodes[i].children.iter().map(|&c| {
            let n = &self.nodes[c];
            (n.name.as_str(), n.is_dir)
        }))
    }

    pub fn depth(&self, i: usize) -> usize {
        let p = &self.nodes[i].path;
        if p.is_empty() { 0 } else { p.matches('/').count() + 1 }
    }

    /// Nodes in the subtree of `prefix` (a folder or file path; `""` = everything).
    pub fn subtree(&self, prefix: &str) -> Vec<usize> {
        let Some(start) = self.get(prefix) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.nodes[i].children.iter().rev());
        }
        out
    }

    pub fn file_count(&self) -> usize {
        self.nodes.iter().filter(|n| !n.is_dir).count()
    }
}

/// Normalizes a user-given path prefix: `.`/`./x/` → `""`/`x`.
pub fn normalize_prefix(prefix: Option<&str>) -> String {
    let p = prefix.unwrap_or("").replace('\\', "/");
    let p = p.trim_start_matches("./").trim_end_matches('/');
    if p == "." { String::new() } else { p.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(paths: &[&str]) -> Tree {
        Tree::from_entries(paths.iter().map(|p| (p.to_string(), FileInfo::default())))
    }

    #[test]
    fn folders_before_files_alphabetical() {
        let t = entries(&["b.rs", "src/main.rs", "a.rs", "src/api/x.rs", "Zed/y"]);
        let names: Vec<_> = t.nodes[ROOT]
            .children
            .iter()
            .map(|&i| t.nodes[i].name.as_str())
            .collect();
        assert_eq!(names, ["src", "Zed", "a.rs", "b.rs"]);
        let src = t.get("src").unwrap();
        assert!(t.nodes[src].is_dir);
        assert_eq!(t.depth(t.get("src/api/x.rs").unwrap()), 3);
        assert_eq!(t.depth(ROOT), 0);
    }

    #[test]
    fn subtree_and_keys() {
        let t = entries(&["src/a.rs", "src/b/c.rs", "d.rs"]);
        let sub: Vec<_> = t.subtree("src").iter().map(|&i| t.nodes[i].path.clone()).collect();
        assert_eq!(sub, ["src", "src/b", "src/b/c.rs", "src/a.rs"]);
        assert_eq!(
            t.dir_key(t.get("src/").unwrap()),
            hash::dir_key([("b", true), ("a.rs", false)])
        );
    }

    #[test]
    fn classification_order() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            max_file_bytes: 10,
            ..Config::default()
        };
        let ex = Excluder::new(&config.exclude).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            let mut info = FileInfo {
                size: bytes.len() as u64,
                ..Default::default()
            };
            analyze(&p, name, &config, &ex, &mut info);
            info
        };
        assert_eq!(
            write("Cargo.lock", b"0123456789abc").kind,
            Some(Kind::Skipped("*.lock".into()))
        );
        assert_eq!(write("big.txt", b"0123456789abc").kind, Some(Kind::TooLarge));
        assert_eq!(write("bin", b"ab\0c").kind, Some(Kind::Binary));
        let text = write("t.rs", b"a\nb");
        assert_eq!(text.kind, Some(Kind::Text));
        assert_eq!(text.lines, Some(2));
        assert!(text.hash.is_some());
    }

    #[test]
    fn exclude_matches_nested_names() {
        let ex = Excluder::new(&["package-lock.json".to_string(), "*.min.js".to_string()]).unwrap();
        assert_eq!(ex.matched("web/package-lock.json"), Some("package-lock.json"));
        assert_eq!(ex.matched("a/b/x.min.js"), Some("*.min.js"));
        assert_eq!(ex.matched("a/b/x.js"), None);
    }

    #[test]
    fn line_counting() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\n\nb"), 3);
    }

    #[test]
    fn prefixes() {
        assert_eq!(normalize_prefix(None), "");
        assert_eq!(normalize_prefix(Some(".")), "");
        assert_eq!(normalize_prefix(Some("./src/api/")), "src/api");
    }
}
