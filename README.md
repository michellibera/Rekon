# rekon

Terminal map of a repository. On the left: the project tree with a one-sentence
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
| 1 / 2 | left panel: tree / changes since your review |
| m | mark the file or folder as understood (changes view: accept the change) |
| u | changes view: ask the model what the change means for you |
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

### Review: what changed since I understood it

Opening a file never counts as understanding it. `m` (tree or code panel) marks the current
version of a file, or of every file in a folder, as understood; the baseline is kept in
`.rekon/review/` (old text in `.rekon/objects/`). Tab `2` then lists what differs, per element
(a function, type, ... from the outline, identified by path and qualified name):

| Status | Meaning |
| --- | --- |
| changed | real change of the element's own lines (children do not count) |
| format only | whitespace or `//` comments only: a skim is enough |
| renamed | same code under another name |
| added / removed | new or gone element of a file you reviewed |
| caller changed | the element is the same, but code that calls it changed (static, only files you reviewed) |

Selecting a change shows its lines in the code panel. `u` asks the model for an update of your
mental model (what held before, what is new, points tied to lines; marked `[AI]`, saved per pair
of versions); `m` accepts the change as the new baseline; `r` recomputes.

```sh
rekon review mark src/store.rs   # baseline
rekon review status [--json]     # what changed since
rekon review update <id>         # model update for one changed element
rekon review accept <id>
```

### Working with meaning: why, what changed, what is done

The left panel has lenses, the right panel has an inspector for the element you are looking at
(a function or type from the outline, or a whole file). Nothing here is packed next to the
descriptions: `v` / `V` (or a click on the tab under the right panel) switch its view.

| Left lens | What it lists | Main keys |
| --- | --- | --- |
| `1` Tree | files and folders | `m` mark as understood |
| `2` Changes | what changed since you last marked code as understood | `m` accept, `u` model update |
| `3` Why | decisions: reason, cost, assumptions, when to reconsider | `n` new, `s` replace, `a` accept, `C` check the working tree |
| `4` Cards | behaviour changes: before, after, consequences, basis | `n` write a spec, `N` from the working tree, `b` brief for an agent, `c` criteria |
| `5` Criteria | met conditions: implemented / tests / reviewed, separately | `l` link the element in view, `t` test, `m` reviewed, `V` run tests |
| `6` Ask | a pinned question: the elements that answer it, and what was left out | `n` pin (asks the model), `a` again |

| Right view (`v`) | What it shows |
| --- | --- |
| Code | the code in blocks (as before) |
| Relations | calls, callers and tests (static, matched by name), decisions, cards, criteria, contract; Enter jumps |
| Why | the decisions that explain the element; a reason reconstructed by a model is marked `[AI]` |
| Contract | what the code does now (`[AI]`, stamped with the element's version) next to what you want (`[human]`); `d` describe, `e` edit, `i` impact -> a proposed card |
| Example | a walk-through of a concrete input (`p` predict, `e` your input, `x` result you expect, `c` card); always `[predicted]` |
| Compare | the pinned elements side by side (`P` pins, up to three) |

`< >` go back and forward through the elements you visited. The header always shows what needs
attention (`Δ3 since review`, unconfirmed decisions).

Every fact carries its origin: `[human]`, `[AI]`, `[static]`, `[predicted]`, `[test]`,
`[recorded]`. A decision reconstructed by a model is a proposal until you accept it; a card
written as a specification is `proposed` until code exists; the contract you edit never changes
the code, it produces a card (`rekon contract impact`). A test result or a review stops counting
when the element it was made for changes, so "done" is never stale.

Agents use the same data from the CLI (`rekon decision add`, `rekon card add|brief`, `rekon
criteria ...`, `rekon relations <element>`); the SessionStart hook prints the decisions in force
and the specifications waiting for implementation, so an agent starts a session knowing them.
Set `test_command` in `.rekon/config.json` (default `cargo test --workspace` in a Cargo repository)
for `rekon criteria verify` / `V`; cargo and dotnet test output is understood.

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

- `crates/rekon-core` — scanning, notes store, prompts, backends, init, segmentation — no UI.
- `crates/rekon` — the `rekon` binary: CLI and TUI.
- `DECISIONS.md` — choices made where the plan left room.
