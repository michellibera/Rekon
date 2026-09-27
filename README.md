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
| 1 / 2 | left panel: tree / ontology graph |
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

### Ontology

`2` switches the left panel from the tree to the ontology: the system as a graph of typed
elements (Actor, System, Component, Interface, Process, DataEntity, DataTransformation,
DataStore, Event, Dependency, Observation, FailureMode, SourceCode, Owner) joined by relations
(`calls`, `reads`, `writes`, `emits`, `consumes`, `handledBy`, `derivedFrom`, `mayFailWith`, …).
Every node and every relation has evidence: the file and lines of code that prove it, shown in
the code panel.

```sh
rekon ontology index --dry-run  # which files would go to the model, in how many requests
rekon ontology index            # analyze changed files, rebuild .rekon/ontology/graph.json
rekon ontology show             # the root and counts per type
rekon ontology show Store       # a node with its evidence and relations (--json)
```

Indexing and browsing are separate: the TUI only reads `graph.json`, and `R` in the ontology
view runs the analysis in the background. A file goes to the model again only when its content
changed: what was found in it is kept in `.rekon/ontology/facts/`, keyed by the content hash and
the schema.

How the graph is built:

- Static facts, without the model: packages and dependencies from manifests (Cargo.toml,
  package.json, *.csproj, go.mod, pyproject.toml, requirements.txt, with the declaring lines),
  an outline of every file (types, functions, classes and their line ranges), source files.
- The model maps each file onto the ontology. Its evidence is checked against the file: lines
  must be in the part it was shown, a cited symbol must be in those lines (else the evidence
  moves to where the symbol is, or loses the symbol and some confidence). Elements left without
  evidence are dropped.
- A deterministic merge joins everything: an element seen in several files is one node with all
  its evidence, code elements link to their source files (`implementedBy`), components go into
  their packages (`contains`), and every node can be reached from the root System.

| Key (ontology view) | Action |
| --- | --- |
| arrows, hjkl | move to the nearest element in that direction (nodes and relation labels) |
| Space / Backspace | expand (neighbors appear below it, nothing else moves) or collapse / collapse the node (or the one above it) |
| Enter | evidence of the node or relation in the code panel; again: the next one |
| [ / ] | previous / next evidence |
| Esc | the node one level up |
| + / - / 0 | zoom in / out / 100% |
| c / Home | center on the selection / go to the root |
| f | filters: only nodes of the picked types, joined by the picked relations; hidden ones are revealed (none picked: everything) |
| L | reveal the data lineage (`derivedFrom`) of the selection |
| R / r | analyze changed files / the selection's files again |

Mouse: a click selects a node or a relation, a double click expands, dragging moves a node or
the view, the wheel zooms.

Focus: the selection, its neighbors and their relations stay bright, the rest is grayed out.
Groups: when a node has more than 5 hidden neighbors, those joined by one relation (5 or more)
wait behind a marker with their count; the relation is on the line to it. Space or Enter opens it.

The ontology is data. `.rekon/ontology/schema.json` adds or overrides node types and relations
(same format as `crates/rekon-core/prompts/ontology.json`, e.g. a `DataEntityField` type with a
`hasField` relation); the UI takes colors and relation names from the graph. In
`.rekon/config.json`, `ontology` sets which files are analyzed (`include`, `exclude`; tests are
left out) and the size of a request, `models.ontology` the model (default `sonnet`).

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

- `crates/rekon-core` — scanning, notes store, prompts, backends, init, segmentation, and the
  ontology (`src/ontology`: schema, static outline and manifests, extraction, merge, graph) —
  no UI.
- `crates/rekon` — the `rekon` binary: CLI and TUI (`src/tui/graph`: the ontology explorer).
- `DECISIONS.md` — choices made where the plan left room.
