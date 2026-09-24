# rekon

Terminal map of a repository for one person. On the left: the project tree with a one-sentence
description of every file and folder. On the right: the code of the open file split into logical
blocks with short descriptions; expanding a block splits it further, down to blocks of a few lines.

Descriptions live in `.rekon/` (hidden from git through `.git/info/exclude`) and are regenerated
only when the code changes. Tree descriptions are created upfront by `rekon init`; blocks are
created lazily, when a file is opened or a block expanded.

The model is called through the Claude Code CLI (`claude -p`), with tools disabled and a JSON
schema for the answer.

## Build

```sh
cargo build --release
# binary: target/release/rekon
```

Rust 1.89 or newer.

## Use

```sh
rekon init              # in a git repository: create .rekon/ and describe everything
rekon tree --depth 2    # overview and tree with descriptions
rekon show src/main.rs  # note of a file, with blocks
rekon segment src/main.rs [--lines 15-40]
```

`REKON_BACKEND=fake` answers deterministically without a model (tests, UI work).

## Layout

- `crates/rekon-core` — scanning, notes store, prompts, backends, init, segmentation (no UI).
- `crates/rekon` — the `rekon` binary: CLI.
- `DECISIONS.md` — choices made where the plan left room.
