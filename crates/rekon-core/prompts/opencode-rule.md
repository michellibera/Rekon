## rekon map

When the repository has a `.rekon/` folder, keep its descriptions current. Before you finish
a task that changed files, run `{rekon} check`: it lists changed files (and new folders, ending
with `/`) whose descriptions are outdated. Describe each in one sentence following
`.rekon/style.md` and save them all with one command:

```bash
{rekon} apply <<'EOF'
{"summaries": {"src/example.rs": "…", "src/new-folder/": "…"}}
EOF
```

`{rekon} tree --depth 2` shows the project overview and the tree with descriptions.
