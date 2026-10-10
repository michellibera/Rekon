//! Static relations between elements, found without a model: who calls an element, whom it
//! calls, which tests exercise it. Every relation is named, points at a line, and carries
//! its origin (`Static`: matched by name, so an ambiguous name is skipped, not guessed).

use std::collections::HashMap;

use anyhow::Result;

use crate::Ctx;
use crate::element::{self, Element};
use crate::model::Origin;
use crate::scan::{self, Excluder};

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// `calls`, `calledBy`, `testedBy`.
    pub kind: &'static str,
    /// Id of the element on the other end.
    pub id: String,
    pub label: String,
    pub path: String,
    /// Line of the call.
    pub line: u32,
    pub origin: Origin,
}

pub struct Index {
    pub elements: Vec<Element>,
    tests: Vec<bool>,
    /// Lines of every indexed file, to point at the exact line of a call.
    files: HashMap<String, Vec<String>>,
    by_name: HashMap<String, Vec<usize>>,
}

/// Names shorter than this are too common to match by name.
const MIN_NAME: usize = 4;

pub fn build(ctx: &Ctx) -> Result<Index> {
    let (paths, _) = scan::git_files(&ctx.root)?;
    let include = Excluder::new(&ctx.config.scope.include)?;
    let tests = Excluder::new(&ctx.config.scope.exclude)?;
    let excluded = Excluder::new(&ctx.config.exclude)?;
    let mut elements = Vec::new();
    let mut is_test = Vec::new();
    let mut files = HashMap::new();
    for path in paths {
        if include.matched(&path).is_none() || excluded.matched(&path).is_some() {
            continue;
        }
        let Ok(bytes) = std::fs::read(ctx.root.join(&path)) else {
            continue;
        };
        if bytes.len() as u64 > ctx.config.max_file_bytes {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let test_file = tests.matched(&path).is_some();
        for e in element::elements(&path, &text) {
            if e.kind == "file" {
                continue;
            }
            is_test.push(test_file || e.id.contains("::tests::") || e.name.starts_with("test_"));
            elements.push(e);
        }
        files.insert(path, text.lines().map(str::to_string).collect());
    }
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in elements.iter().enumerate() {
        by_name.entry(e.name.clone()).or_default().push(i);
    }
    Ok(Index {
        elements,
        tests: is_test,
        files,
        by_name,
    })
}

impl Index {
    pub fn position(&self, id: &str) -> Option<usize> {
        self.elements.iter().position(|e| e.id == id)
    }

    pub fn is_test(&self, i: usize) -> bool {
        self.tests[i]
    }

    /// Elements that other code can refer to by name: unique, long enough, not tests.
    fn nameable(&self, i: usize) -> bool {
        let e = &self.elements[i];
        e.name.chars().count() >= MIN_NAME
            && !self.tests[i]
            && self
                .by_name
                .get(&e.name)
                .is_some_and(|v| v.iter().filter(|&&j| !self.tests[j]).count() == 1)
    }

    /// First line of `from` (within its own range) that calls `name`.
    fn call_line(&self, from: &Element, name: &str) -> u32 {
        let lines = self.files.get(&from.path);
        (from.start..=from.end)
            .find(|&n| {
                lines
                    .and_then(|l| l.get(n as usize - 1))
                    .is_some_and(|l| element::calls_strict(l, name))
            })
            .unwrap_or(from.start)
    }

    /// Elements matching a query: the exact id, a name, or the tail of an id.
    pub fn find(&self, query: &str) -> Vec<&Element> {
        let exact: Vec<&Element> = self.elements.iter().filter(|e| e.id == query).collect();
        if !exact.is_empty() {
            return exact;
        }
        let tail = format!("::{query}");
        self.elements
            .iter()
            .filter(|e| e.name == query || e.id.ends_with(&tail))
            .collect()
    }

    /// What the element calls.
    pub fn calls(&self, id: &str) -> Vec<Link> {
        let Some(i) = self.position(id) else { return Vec::new() };
        let from = &self.elements[i];
        (0..self.elements.len())
            .filter(|&j| j != i && self.nameable(j) && element::calls_strict(&from.text, &self.elements[j].name))
            .map(|j| {
                let t = &self.elements[j];
                Link {
                    kind: "calls",
                    id: t.id.clone(),
                    label: t.name.clone(),
                    path: from.path.clone(),
                    line: self.call_line(from, &t.name),
                    origin: Origin::Static,
                }
            })
            .collect()
    }

    fn callers(&self, id: &str, tests: bool, kind: &'static str) -> Vec<Link> {
        let Some(i) = self.position(id) else { return Vec::new() };
        if !self.nameable(i) {
            return Vec::new();
        }
        let name = &self.elements[i].name;
        (0..self.elements.len())
            .filter(|&j| j != i && self.tests[j] == tests && element::calls_strict(&self.elements[j].text, name))
            .map(|j| {
                let c = &self.elements[j];
                Link {
                    kind,
                    id: c.id.clone(),
                    label: c.name.clone(),
                    path: c.path.clone(),
                    line: self.call_line(c, name),
                    origin: Origin::Static,
                }
            })
            .collect()
    }

    /// Non-test code that calls the element.
    pub fn called_by(&self, id: &str) -> Vec<Link> {
        self.callers(id, false, "calledBy")
    }

    /// Tests that call the element.
    pub fn tested_by(&self, id: &str) -> Vec<Link> {
        self.callers(id, true, "testedBy")
    }

    /// Relations of a whole file: those of every element in it, without the ones between
    /// its own elements.
    pub fn of_file(&self, path: &str) -> Vec<Link> {
        let ids: Vec<&str> = self
            .elements
            .iter()
            .filter(|e| e.path == path)
            .map(|e| e.id.as_str())
            .collect();
        let mut out: Vec<Link> = Vec::new();
        for id in &ids {
            for l in self
                .calls(id)
                .into_iter()
                .chain(self.called_by(id))
                .chain(self.tested_by(id))
            {
                let other = l.id.split_once("::").map_or(l.id.as_str(), |(p, _)| p);
                if other != path && !out.iter().any(|o| o.kind == l.kind && o.id == l.id) {
                    out.push(l);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::config::Config;
    use std::sync::Arc;

    fn repo() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "pub fn checkout(o: u32) {\n    charge(o);\n}\n\npub fn charge(o: u32) {\n    o + 1;\n}\n\npub fn new() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("tests/t.rs"), "fn pays() {\n    checkout(1);\n}\n").unwrap();
        std::fs::create_dir_all(root.join(".rekon")).unwrap();
        let ctx = Ctx::with_backend(
            root,
            Config {
                backend: "fake".into(),
                ..Config::default()
            },
            Arc::new(FakeBackend::default()),
        );
        (dir, ctx)
    }

    #[test]
    fn finds_callers_callees_and_tests() {
        let (_d, ctx) = repo();
        let idx = build(&ctx).unwrap();
        let checkout = idx.elements.iter().find(|e| e.name == "checkout").unwrap().id.clone();
        let charge = idx.elements.iter().find(|e| e.name == "charge").unwrap().id.clone();
        let calls = idx.calls(&checkout);
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].id.as_str(), calls[0].line), (charge.as_str(), 2));
        assert_eq!(idx.called_by(&charge)[0].label, "checkout");
        assert_eq!(idx.tested_by(&checkout)[0].path, "tests/t.rs");
        assert!(idx.called_by(&checkout).is_empty());
        // Short and ambiguous names are never guessed.
        let new = idx.elements.iter().find(|e| e.name == "new").unwrap().id.clone();
        assert!(idx.called_by(&new).is_empty());
    }

    #[test]
    fn file_relations_skip_the_file_itself() {
        let (_d, ctx) = repo();
        let idx = build(&ctx).unwrap();
        let links = idx.of_file("src/a.rs");
        assert!(links.iter().all(|l| l.kind == "testedBy"), "{links:?}");
        assert_eq!(links.len(), 1);
    }
}
