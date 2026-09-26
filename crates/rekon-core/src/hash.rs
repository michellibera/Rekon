//! Freshness keys: blake3 of file contents, and of sorted child names for folders.
//! Contents are hashed with CRLF read as LF, so a checkout that only converts line
//! endings (git `core.autocrlf`) does not make descriptions look outdated.

use std::io::Read;
use std::path::Path;

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut cr = false;
    update_lf(&mut hasher, bytes, &mut cr);
    finish(hasher, cr)
}

pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut cr = false;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        update_lf(&mut hasher, &buf[..n], &mut cr);
    }
    Ok(finish(hasher, cr))
}

/// Feeds `chunk` with every CRLF as LF. `cr` carries a CR that ended the previous
/// chunk, since its LF may start the next one.
fn update_lf(hasher: &mut blake3::Hasher, chunk: &[u8], cr: &mut bool) {
    if std::mem::take(cr) && chunk.first() != Some(&b'\n') {
        hasher.update(b"\r");
    }
    let mut start = 0;
    for (i, &b) in chunk.iter().enumerate() {
        if b != b'\r' {
            continue;
        }
        match chunk.get(i + 1) {
            Some(b'\n') => {}
            Some(_) => continue,
            None => *cr = true,
        }
        hasher.update(&chunk[start..i]);
        start = i + 1;
    }
    hasher.update(&chunk[start..]);
}

fn finish(mut hasher: blake3::Hasher, cr: bool) -> String {
    if cr {
        hasher.update(b"\r");
    }
    hasher.finalize().to_hex().to_string()
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

    fn plain(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

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

    #[test]
    fn line_endings_do_not_change_the_hash() {
        assert_eq!(hash_bytes(b"a\r\nb\r\n"), hash_bytes(b"a\nb\n"));
        assert_eq!(hash_bytes(b"a\r\n"), plain(b"a\n"), "LF files keep their old keys");
        assert_eq!(hash_bytes(b"a\rb"), plain(b"a\rb"), "a lone CR stays");
        assert_eq!(hash_bytes(b"a\r"), plain(b"a\r"), "a trailing CR stays");
    }

    #[test]
    fn crlf_split_between_chunks_is_still_lf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let mut crlf = vec![b'x'; 64 * 1024 - 1];
        crlf.extend_from_slice(b"\r\nend\r");
        std::fs::write(&p, &crlf).unwrap();
        let mut lf = vec![b'x'; 64 * 1024 - 1];
        lf.extend_from_slice(b"\nend\r");
        assert_eq!(hash_file(&p).unwrap(), plain(&lf));
    }
}
