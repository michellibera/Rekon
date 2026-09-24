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

`REKON_BACKEND=fake` answers deterministically without a model (tests, UI work);
`REKON_FAKE_DELAY_MS=800` makes it slow enough to see progress markers.

### TUI

`rekon` without a subcommand opens the TUI (a missing map is built in the background).

| Key | Action |
| --- | --- |
| ↑/↓, k/j | previous/next item (code panel: block header) |
| →/l, Enter | expand folder, open file, expand block |
| ←/h | collapse or go to parent |
| Tab | switch panel |
| PgUp/PgDn | scroll by a page |
| o | descriptions only in the code panel |
| w | tree at full width |
| i | project overview |
| e | open `$EDITOR` at the selected block |
| r | regenerate the selected item |
| R | refresh all outdated tree descriptions |
| ? | help |
| q | quit |

Mouse: a click selects and expands or collapses, the wheel scrolls the panel under the cursor.
`⚠` marks a description older than the code.

### With Claude Code

```sh
rekon setup --dry-run   # show what would change in ~/.claude
rekon setup             # install the rekon-init skill and two hooks (once per machine)
```

- Stop hook (`rekon check --hook`): when files changed in the session have outdated
  descriptions, the agent is asked to update them with one `rekon apply`.
- SessionStart hook (`rekon context --hook`): the agent gets the overview and tree to depth 2.
- `/rekon-init` in a session builds the map with the agent itself.

`rekon apply` reads JSON on stdin; a key ending with `/` is a folder, `.` is the project:

```sh
rekon apply <<'EOF'
{"summaries": {"src/auth.rs": "Verifies JWT tokens", "src/": "Application code", ".": "Shop backend"},
 "overview": "Optional: 3-5 sentences."}
EOF
```

### With OpenCode

`"backend": "opencode"` in `.rekon/config.json` (or `REKON_BACKEND=opencode`) calls
`opencode run` instead of `claude -p`; `"opencode": {"model": "provider/model"}` picks the
model, `"attach": "http://localhost:4096"` reuses a running `opencode serve`.
`rekon setup --opencode` adds a rule to `~/.config/opencode/AGENTS.md` asking the agent to
keep descriptions current.

Run `setup` from the binary you will keep (e.g. after `cargo install --path crates/rekon`):
hook commands store its full path.

## Layout

- `crates/rekon-core` — scanning, notes store, prompts, backends, init, segmentation (no UI).
- `crates/rekon` — the `rekon` binary: CLI.
- `DECISIONS.md` — choices made where the plan left room.
