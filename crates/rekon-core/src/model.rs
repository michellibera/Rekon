//! Serialized notes stored in `.rekon/`.

use serde::{Deserialize, Serialize};

pub const NOTE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    Auto,
    Agent,
}

/// Where a fact shown to the user comes from. The UI marks every fact with its origin,
/// so a guess of the model is never taken for a recorded one.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Written by the user.
    Human,
    /// Produced (or reconstructed) by a model.
    Ai,
    /// Read from the code by rekon itself, without a model.
    Static,
    /// Observed in a real run.
    Recorded,
    /// Result of a test run.
    TestResult,
    /// Predicted by a model, not observed.
    Predicted,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Ai => "AI",
            Origin::Static => "static",
            Origin::Recorded => "recorded",
            Origin::TestResult => "test",
            Origin::Predicted => "predicted",
        }
    }
}

/// A value together with the key (hash) of the element it was made for. It stops being
/// current when the element changes; the old value may stay as history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stamped<T> {
    pub hash: String,
    pub origin: Origin,
    pub value: T,
}

impl<T> Stamped<T> {
    pub fn is_current(&self, key: &str) -> bool {
        self.hash == key
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Summary {
    pub hash: String,
    pub text: String,
    pub by: Author,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Block {
    /// 1-based, inclusive.
    pub lines: (u32, u32),
    pub summary: String,
    /// `None` = not split yet, `Some([])` = leaf.
    pub children: Option<Vec<Block>>,
}

impl Block {
    pub fn new(start: u32, end: u32, summary: impl Into<String>) -> Self {
        Self {
            lines: (start, end),
            summary: summary.into(),
            children: None,
        }
    }

    pub fn line_count(&self) -> u32 {
        self.lines.1 - self.lines.0 + 1
    }

    pub fn is_leaf(&self) -> bool {
        matches!(&self.children, Some(c) if c.is_empty())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Blocks {
    pub hash: String,
    pub items: Vec<Block>,
}

impl Blocks {
    /// Finds a block by its range anywhere in the tree. Ranges are unique because
    /// children always split their parent into at least two parts.
    pub fn find(&self, range: (u32, u32)) -> Option<&Block> {
        find_in(&self.items, range)
    }

    pub fn find_mut(&mut self, range: (u32, u32)) -> Option<&mut Block> {
        find_in_mut(&mut self.items, range)
    }

    /// Blocks from level 1 down to the block with `range`, inclusive; empty when absent.
    pub fn path_to(&self, range: (u32, u32)) -> Vec<&Block> {
        let mut path = Vec::new();
        let mut level = &self.items;
        while let Some(b) = level.iter().find(|b| contains(b.lines, range)) {
            path.push(b);
            if b.lines == range {
                return path;
            }
            match &b.children {
                Some(c) if !c.is_empty() => level = c,
                _ => break,
            }
        }
        Vec::new()
    }
}

fn contains(outer: (u32, u32), inner: (u32, u32)) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

fn find_in(items: &[Block], range: (u32, u32)) -> Option<&Block> {
    let b = items.iter().find(|b| contains(b.lines, range))?;
    if b.lines == range {
        return Some(b);
    }
    find_in(b.children.as_deref()?, range)
}

fn find_in_mut(items: &mut [Block], range: (u32, u32)) -> Option<&mut Block> {
    let b = items.iter_mut().find(|b| contains(b.lines, range))?;
    if b.lines == range {
        return Some(b);
    }
    find_in_mut(b.children.as_deref_mut()?, range)
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FileNote {
    pub version: u32,
    pub summary: Option<Summary>,
    pub blocks: Option<Blocks>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DirNote {
    pub version: u32,
    pub summary: Option<Summary>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ProjectNote {
    pub version: u32,
    pub summary: Option<Summary>,
    pub overview: Option<String>,
}

/// Freshness of one part of a note relative to the current key of its element.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    Missing,
    Stale,
    Fresh,
}

/// `key: None` means the key is not known yet (hashing in progress); the note is
/// then trusted until proven otherwise.
pub fn freshness(summary: Option<&Summary>, key: Option<&str>) -> Freshness {
    match (summary, key) {
        (None, _) => Freshness::Missing,
        (Some(_), None) => Freshness::Fresh,
        (Some(s), Some(k)) if s.hash == k => Freshness::Fresh,
        (Some(_), Some(_)) => Freshness::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Blocks {
        let mut a = Block::new(1, 10, "a");
        a.children = Some(vec![Block::new(1, 4, "a1"), Block::new(5, 10, "a2")]);
        Blocks {
            hash: "h".into(),
            items: vec![a, Block::new(11, 20, "b")],
        }
    }

    #[test]
    fn find_by_range() {
        let t = tree();
        assert_eq!(t.find((5, 10)).unwrap().summary, "a2");
        assert_eq!(t.find((1, 10)).unwrap().summary, "a");
        assert!(t.find((5, 9)).is_none());
        let path: Vec<_> = t.path_to((5, 10)).iter().map(|b| b.summary.clone()).collect();
        assert_eq!(path, ["a", "a2"]);
        assert!(t.path_to((5, 9)).is_empty());
    }

    #[test]
    fn note_json_shape() {
        let note = FileNote {
            version: 1,
            summary: Some(Summary {
                hash: "x".into(),
                text: "t".into(),
                by: Author::Auto,
            }),
            blocks: Some(tree()),
        };
        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(json["summary"]["by"], "auto");
        assert_eq!(json["blocks"]["items"][0]["lines"], serde_json::json!([1, 10]));
        assert_eq!(json["blocks"]["items"][1]["children"], serde_json::Value::Null);
    }

    #[test]
    fn freshness_states() {
        let s = Summary {
            hash: "a".into(),
            text: "t".into(),
            by: Author::Auto,
        };
        assert_eq!(freshness(None, Some("a")), Freshness::Missing);
        assert_eq!(freshness(Some(&s), Some("a")), Freshness::Fresh);
        assert_eq!(freshness(Some(&s), Some("b")), Freshness::Stale);
        assert_eq!(freshness(Some(&s), None), Freshness::Fresh);
    }
}

/// Lines of code that back a statement, shown in the code panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    /// Repository path with `/` separators.
    pub file: String,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    pub symbol: Option<String>,
    pub reason: String,
}

impl Evidence {
    pub fn location(&self) -> String {
        if self.start_line == self.end_line {
            format!("{}:{}", self.file, self.start_line)
        } else {
            format!("{}:{}-{}", self.file, self.start_line, self.end_line)
        }
    }
}
