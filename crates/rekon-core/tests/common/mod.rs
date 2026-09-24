#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use rekon_core::Ctx;
use rekon_core::backend::fake::FakeBackend;
use rekon_core::config::Config;

pub fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A small repository: nested folders, a lock file, a binary file, an ignored file.
pub fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, "README.md", "# Shop\n\nA test shop.\n");
    write(root, "Cargo.toml", "[package]\nname = \"shop\"\n");
    write(root, "Cargo.lock", "# lock\n");
    write(root, ".gitignore", "target/\n");
    write(root, "target/debug/x", "ignored\n");
    write(
        root,
        "src/main.rs",
        "fn main() {\n    run();\n}\n\nfn run() {\n    println!(\"hi\");\n}\n",
    );
    write(root, "src/api/orders.rs", &orders());
    write(root, "src/api/mod.rs", "pub mod orders;\n");
    std::fs::write(root.join("logo.png"), [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
    dir
}

pub fn orders() -> String {
    let mut s = String::from("use std::io;\n\n");
    for f in 0..3 {
        s.push_str(&format!(
            "fn f{f}() {{\n    let a = {f};\n    let b = a + 1;\n    println!(\"{{}}\", b);\n}}\n\n"
        ));
    }
    s
}

pub fn ctx(root: &Path) -> (Ctx, Arc<FakeBackend>) {
    rekon_core::init::prepare(root).unwrap();
    let fake = Arc::new(FakeBackend::default());
    let config = Config::load(&root.join(".rekon")).unwrap();
    let config = Config {
        backend: "fake".into(),
        ..config
    };
    (Ctx::with_backend(root, config, fake.clone()), fake)
}
