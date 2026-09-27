//! Node types and relations of the ontology, as data. The built-in definitions
//! (`prompts/ontology.json`) can be extended or overridden per repository in
//! `.rekon/ontology/schema.json`; ids are stable machine-readable identifiers.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const BUILTIN: &str = include_str!("../../prompts/ontology.json");

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TypeDef {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Display hint: a color name or `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Asked from the model; false for types rekon derives itself (SourceCode).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub extract: bool,
    /// Elements of this type live in source files: rekon links them to their files
    /// with `implementedBy`, and may unify them by name across types.
    #[serde(default, skip_serializing_if = "is_false")]
    pub code: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationDef {
    pub id: String,
    /// The relation read from target to source, e.g. `consumedBy` for `consumes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub extract: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Schema {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    #[serde(default)]
    pub relations: Vec<RelationDef>,
}

pub const SOURCE_CODE: &str = "SourceCode";
pub const SYSTEM: &str = "System";
pub const COMPONENT: &str = "Component";
pub const DEPENDENCY: &str = "Dependency";
pub const CONTAINS: &str = "contains";
pub const DEPENDS_ON: &str = "dependsOn";
pub const IMPLEMENTED_BY: &str = "implementedBy";

impl Schema {
    pub fn builtin() -> Self {
        serde_json::from_str(BUILTIN).expect("built-in ontology is valid JSON")
    }

    /// Built-in definitions, extended (or overridden by id) with
    /// `<map_dir>/ontology/schema.json` when it exists.
    pub fn load(map_dir: &Path) -> Result<Self> {
        let mut schema = Self::builtin();
        let path = map_dir.join(super::DIR).join("schema.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let extra: Schema =
                    serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
                schema.extend(extra);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
        Ok(schema)
    }

    /// Adds types and relations; an entry with an existing id replaces it.
    pub fn extend(&mut self, other: Schema) {
        for t in other.types {
            match self.types.iter_mut().find(|x| x.id == t.id) {
                Some(x) => *x = t,
                None => self.types.push(t),
            }
        }
        for r in other.relations {
            match self.relations.iter_mut().find(|x| x.id == r.id) {
                Some(x) => *x = r,
                None => self.relations.push(r),
            }
        }
    }

    pub fn type_def(&self, id: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.id == id)
    }

    pub fn relation(&self, id: &str) -> Option<&RelationDef> {
        self.relations.iter().find(|r| r.id == id)
    }

    pub fn is_code(&self, kind: &str) -> bool {
        self.type_def(kind).is_some_and(|t| t.code)
    }

    /// Canonical type id for a name given by the model (case and separators ignored).
    pub fn canonical_type(&self, name: &str) -> Option<&str> {
        let key = loose(name);
        self.types.iter().find(|t| loose(&t.id) == key).map(|t| t.id.as_str())
    }

    /// Canonical relation id for a name given by the model; `true` when the name was
    /// the inverse (`consumedBy`), so source and target have to be swapped.
    pub fn canonical_relation(&self, name: &str) -> Option<(&str, bool)> {
        let key = loose(name);
        if let Some(r) = self.relations.iter().find(|r| loose(&r.id) == key) {
            return Some((r.id.as_str(), false));
        }
        self.relations
            .iter()
            .find(|r| r.inverse.as_deref().is_some_and(|i| loose(i) == key))
            .map(|r| (r.id.as_str(), true))
    }

    /// Name of `relation` read from target to source, when the schema defines one.
    pub fn inverse(&self, relation: &str) -> Option<&str> {
        self.relation(relation)?.inverse.as_deref()
    }

    /// Changes whenever the definitions change; stored with extracted facts, so a new
    /// schema makes them stale.
    pub fn fingerprint(&self) -> String {
        crate::hash::hash_bytes(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }
}

fn loose(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_the_minimal_ontology() {
        let s = Schema::builtin();
        for t in [
            "Actor",
            "System",
            "Component",
            "Interface",
            "Process",
            "DataEntity",
            "DataTransformation",
            "DataStore",
            "Event",
            "Dependency",
            "Observation",
            "FailureMode",
            "SourceCode",
            "Owner",
        ] {
            assert!(s.type_def(t).is_some(), "{t}");
        }
        for r in [
            "invokes",
            "calls",
            "produces",
            "consumes",
            "reads",
            "writes",
            "transforms",
            "storedIn",
            "dependsOn",
            "triggers",
            "implementedBy",
            "observableBy",
            "mayFailWith",
            "causes",
            "affects",
            "ownedBy",
            "derivedFrom",
            "handledBy",
            "executes",
            "emits",
        ] {
            assert!(s.relation(r).is_some(), "{r}");
        }
        assert!(!s.type_def(SOURCE_CODE).unwrap().extract);
        assert!(s.is_code("Component") && !s.is_code("Actor"));
    }

    #[test]
    fn names_from_the_model_are_canonicalized() {
        let s = Schema::builtin();
        assert_eq!(s.canonical_type("data_entity"), Some("DataEntity"));
        assert_eq!(s.canonical_type("FAILURE MODE"), Some("FailureMode"));
        assert_eq!(s.canonical_type("Widget"), None);
        assert_eq!(s.canonical_relation("stored_in"), Some(("storedIn", false)));
        assert_eq!(s.canonical_relation("consumedBy"), Some(("consumes", true)));
        assert_eq!(s.inverse("consumes"), Some("consumedBy"));
    }

    #[test]
    fn repository_schema_extends_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ontology")).unwrap();
        std::fs::write(
            dir.path().join("ontology/schema.json"),
            r##"{"types": [{"id": "DataEntityField", "description": "Field", "color": "#ffffff", "code": true},
                          {"id": "Actor", "description": "Changed"}],
                "relations": [{"id": "hasField", "inverse": "fieldOf"}]}"##,
        )
        .unwrap();
        let s = Schema::load(dir.path()).unwrap();
        assert!(s.is_code("DataEntityField"));
        assert_eq!(s.type_def("Actor").unwrap().description, "Changed");
        assert_eq!(s.canonical_relation("fieldOf"), Some(("hasField", true)));
        assert_ne!(s.fingerprint(), Schema::builtin().fingerprint());
    }
}
