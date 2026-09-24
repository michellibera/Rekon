---
name: rekon-init
description: Builds or completes the rekon map (.rekon/) — project overview and one-sentence descriptions of files and folders. Use when the user asks to initialize or refresh the rekon map.
---

# rekon map from a session

1. If the repository has no `.rekon/`, run `{rekon} init --no-generate`.
2. Read `.rekon/style.md` and apply its rules to every description.
3. Run `{rekon} tree --stale --json` to see what is missing.
4. Overview first: read the README, manifests and entry points, then save `overview` and the description of `.` with `{rekon} apply`.
5. Then folder by folder: read files as needed and after each folder save, with one `{rekon} apply`, the descriptions of its files and of the folder itself.
6. Do not postpone saving to the end, so that context compaction or the end of the session does not take the progress away.
7. When context is running out, stop and tell the user that `rekon init` will fill in the rest.

`apply` reads JSON on stdin. A key ending with `/` is a folder, `.` is the whole project:

```bash
{rekon} apply <<'EOF'
{"summaries": {"src/auth.rs": "…", "src/": "…", ".": "…"}, "overview": "3-5 sentences"}
EOF
```
