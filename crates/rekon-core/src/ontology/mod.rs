//! Ontology of the system: the repository mapped onto node types and relations
//! (`schema`), built from static facts and model answers grounded in code (indexing),
//! stored in `.rekon/ontology/` and read back for browsing.
//!
//! Indexing: repository → static facts + per-file model extraction (cached by content
//! hash) → merge → `graph.json`. Browsing: `graph::GraphIndex` over `graph.json`, no
//! model calls.

pub mod extract;
pub mod graph;
pub mod index;
pub mod manifest;
pub mod merge;
pub mod model;
pub mod outline;
pub mod schema;
pub mod view;

use std::path::{Path, PathBuf};

/// Folder of the ontology inside the map directory.
pub const DIR: &str = "ontology";

/// `.rekon/ontology/graph.json`.
pub fn graph_path(map_dir: &Path) -> PathBuf {
    map_dir.join(DIR).join("graph.json")
}

/// `.rekon/ontology/facts/<path>.json`: what was extracted from one file.
pub fn facts_path(map_dir: &Path, rel: &str) -> PathBuf {
    map_dir.join(DIR).join("facts").join(format!("{rel}.json"))
}

pub fn facts_dir(map_dir: &Path) -> PathBuf {
    map_dir.join(DIR).join("facts")
}
