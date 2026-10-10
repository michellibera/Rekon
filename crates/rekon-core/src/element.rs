//! Elements of a source file: the units a person reviews and rekon compares over time.
//! An element is a definition from the outline (function, type, ...) with a stable id
//! made of the path and the qualified name, so it survives edits above it. Three
//! fingerprints tell how it changed: `hash` (exact), `norm` (ignoring whitespace and
//! `//` comments) and `shape` (`norm` with the element's own name blanked, so a rename
//! shows as the same shape). Only the element's own lines count, not its children's.

use crate::hash;
use crate::outline;

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// `path::qualified name`; a repeated name gets `#2`, `#3`, ...; a file without
    /// definitions is one element with the id `path`.
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    /// 1-based, inclusive; children are inside the range but not in the fingerprints.
    pub start: u32,
    pub end: u32,
    pub hash: String,
    pub norm: String,
    pub shape: String,
    /// Own lines of the element: the text that `hash` covers.
    pub text: String,
}

/// Elements of a file, in source order.
pub fn elements(path: &str, text: &str) -> Vec<Element> {
    let lines: Vec<&str> = text.lines().collect();
    let items = outline::outline(path, text);
    if items.is_empty() {
        return vec![make(
            path,
            path,
            path,
            "file",
            1,
            lines.len().max(1) as u32,
            &lines,
            &[],
        )];
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for it in &items {
        let n = seen.entry(it.qualified.clone()).and_modify(|n| *n += 1).or_insert(1);
        let id = if *n == 1 {
            format!("{path}::{}", it.qualified)
        } else {
            format!("{path}::{}#{n}", it.qualified)
        };
        // Direct and indirect children: ranges nested inside this one.
        let children: Vec<(u32, u32)> = items
            .iter()
            .filter(|c| {
                c.depth > it.depth && c.start >= it.start && c.end <= it.end && (c.start, c.end) != (it.start, it.end)
            })
            .map(|c| (c.start, c.end))
            .collect();
        out.push(make(&id, path, &it.name, it.kind, it.start, it.end, &lines, &children));
    }
    out
}

/// Whether `text` contains a call of `name`: `name(` not preceded by an identifier character.
pub fn calls(text: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    text.match_indices(&needle).any(|(i, _)| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// Like [`calls`], but a method-style call (`x.name(`) only counts for a long name: a short
/// one such as `insert` or `extend` is far more likely to be a standard library method than
/// the element that happens to share its name.
pub fn calls_strict(text: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    text.match_indices(&needle).any(|(i, _)| match text[..i].chars().next_back() {
        None => true,
        Some('.') => name.chars().count() >= 8,
        Some(c) => !(c.is_alphanumeric() || c == '_'),
    })
}

/// Current version of the element with this id (`path::name`, or a plain path for a file).
pub fn load(root: &std::path::Path, id: &str) -> Option<Element> {
    let path = id.split_once("::").map_or(id, |(p, _)| p);
    let text = std::fs::read(root.join(path)).ok()?;
    elements(path, &String::from_utf8_lossy(&text)).into_iter().find(|e| e.id == id)
}

/// The innermost element of `path` that contains `line`.
pub fn at_line(root: &std::path::Path, path: &str, line: u32) -> Option<Element> {
    let text = std::fs::read(root.join(path)).ok()?;
    elements(path, &String::from_utf8_lossy(&text))
        .into_iter()
        .filter(|e| e.start <= line && line <= e.end)
        .min_by_key(|e| e.end - e.start)
}

#[allow(clippy::too_many_arguments)]
fn make(
    id: &str,
    path: &str,
    name: &str,
    kind: &str,
    start: u32,
    end: u32,
    lines: &[&str],
    skip: &[(u32, u32)],
) -> Element {
    let own: Vec<&str> = (start..=end)
        .filter(|n| !skip.iter().any(|&(a, b)| *n >= a && *n <= b))
        .filter_map(|n| lines.get(n as usize - 1).copied())
        .collect();
    let text = own.join("\n");
    let norm = normalize(&own);
    let shape = if name.is_empty() {
        norm.clone()
    } else {
        norm.replace(name, "_")
    };
    Element {
        id: id.to_string(),
        path: path.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        start,
        end,
        hash: hash::hash_bytes(text.as_bytes()),
        norm: hash::hash_bytes(norm.as_bytes()),
        shape: hash::hash_bytes(shape.as_bytes()),
        text,
    }
}

/// Code without whitespace and without lines that are only a `//` comment.
fn normalize(lines: &[&str]) -> String {
    lines
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str =
        "pub struct S;\n\nimpl S {\n    pub fn run(&self) {\n        go();\n    }\n\n    pub fn stop(&self) {}\n}\n";

    fn find<'a>(v: &'a [Element], name: &str) -> &'a Element {
        v.iter().find(|e| e.name == name).unwrap()
    }

    #[test]
    fn ids_are_path_and_qualified_name() {
        let e = elements("src/s.rs", A);
        assert!(
            e.iter().any(|e| e.id == "src/s.rs::S::run"),
            "{:?}",
            e.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parent_fingerprint_ignores_children() {
        let before = elements("s.rs", A);
        let after = elements("s.rs", &A.replace("go();", "go(); more();"));
        assert_eq!(find(&before, "S").hash, find(&after, "S").hash);
        assert_ne!(find(&before, "run").hash, find(&after, "run").hash);
    }

    #[test]
    fn formatting_and_comments_keep_norm() {
        let a = elements("s.rs", A);
        let b = elements("s.rs", &A.replace("        go();", "    // note\n    go( );"));
        let (x, y) = (find(&a, "run"), find(&b, "run"));
        assert_ne!(x.hash, y.hash);
        assert_eq!(x.norm, y.norm);
    }

    #[test]
    fn rename_keeps_shape() {
        let a = elements("s.rs", A);
        let b = elements("s.rs", &A.replace("stop", "halt"));
        let (x, y) = (find(&a, "stop"), find(&b, "halt"));
        assert_ne!(x.norm, y.norm);
        assert_eq!(x.shape, y.shape);
    }

    #[test]
    fn file_without_definitions_is_one_element() {
        let e = elements("notes.md", "# a\nb\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].id, "notes.md");
    }
}
