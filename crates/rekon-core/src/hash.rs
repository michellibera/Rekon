//! Freshness keys: blake3 of file contents, and of sorted child names for folders.

use std::io::Read;
use std::path::Path;

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Key of a folder (or of the project, for the root): blake3 of the sorted child
/// names, one per line, folder names ending with `/`.
pub fn dir_key<'a>(children: impl IntoIterator<Item = (&'a str, bool)>) -> String {
    let mut names: Vec<String> = children
        .into_iter()
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name.to_string() })
        .collect();
    names.sort();
    hash_bytes(names.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_key_is_order_independent_and_marks_folders() {
        let a = dir_key([("b.rs", false), ("a", true)]);
        let b = dir_key([("a", true), ("b.rs", false)]);
        assert_eq!(a, b);
        assert_ne!(a, dir_key([("a", false), ("b.rs", false)]));
    }

    #[test]
    fn file_hash_matches_bytes_hash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(hash_file(&p).unwrap(), hash_bytes(b"hello"));
    }
}
