//! Small collections of JSON documents in `.rekon/<kind>/` (decisions, cards, ...): one
//! file per item, listed in file-name order, written atomically under the store lock.

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::hash;
use crate::store::Store;

fn path(store: &Store, kind: &str, id: &str) -> std::path::PathBuf {
    store.dir().join(kind).join(format!("{id}.json"))
}

pub fn list<T: DeserializeOwned>(store: &Store, kind: &str) -> Vec<T> {
    let Ok(entries) = std::fs::read_dir(store.dir().join(kind)) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    names.sort();
    names
        .iter()
        .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
        .collect()
}

pub fn get<T: DeserializeOwned>(store: &Store, kind: &str, id: &str) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path(store, kind, id)).ok()?).ok()
}

pub fn put<T: Serialize>(store: &Store, kind: &str, id: &str, value: &T) -> Result<()> {
    store.write_json(&path(store, kind, id), value)
}

pub fn remove(store: &Store, kind: &str, id: &str) {
    let _ = std::fs::remove_file(path(store, kind, id));
}

/// Lowercase ASCII words joined with `-`, at most 40 characters.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(40).collect();
    out.trim_end_matches('-').to_string()
}

/// `0001-slug`, the number one above the highest existing one.
pub fn next_id(store: &Store, kind: &str, title: &str) -> String {
    let max = std::fs::read_dir(store.dir().join(kind))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.split('-').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let slug = slug(title);
    if slug.is_empty() {
        format!("{:04}", max + 1)
    } else {
        format!("{:04}-{slug}", max + 1)
    }
}

/// File-name safe key of an arbitrary id: readable prefix plus a short hash.
pub fn key(id: &str) -> String {
    let h = hash::hash_bytes(id.as_bytes());
    format!("{}-{}", slug(id), &h[..8])
}

/// Finds an item by its full id or by a unique prefix (e.g. just the number).
pub fn resolve<T: DeserializeOwned>(store: &Store, kind: &str, query: &str) -> Option<(String, T)> {
    let entries = std::fs::read_dir(store.dir().join(kind)).ok()?;
    let mut hits: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .filter(|id| id.starts_with(query))
        .collect();
    hits.sort();
    let id = if hits.iter().any(|h| h == query) {
        query.to_string()
    } else if hits.len() == 1 {
        hits.remove(0)
    } else {
        return None;
    };
    get(store, kind, &id).map(|v| (id, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_ids() {
        assert_eq!(slug("Use a queue, not HTTP!"), "use-a-queue-not-http");
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert_eq!(next_id(&store, "d", "First one"), "0001-first-one");
        put(&store, "d", "0001-first-one", &1u32).unwrap();
        assert_eq!(next_id(&store, "d", "Next"), "0002-next");
        assert_eq!(list::<u32>(&store, "d"), [1]);
        assert_eq!(resolve::<u32>(&store, "d", "0001").unwrap().0, "0001-first-one");
        assert!(key("a.rs::x").starts_with("a-rs-x-"));
    }
}
