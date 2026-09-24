//! Splitting files into blocks (level 1) and blocks into sub-blocks, lazily.

use anyhow::{Result, bail};

use crate::Ctx;
use crate::hash;
use crate::model::Block;
use crate::prompts::{self, RawBlock, SegmentContext};
use crate::scan::Excluder;
use crate::store::Guard;

/// Repairs a model answer for the parent range `a..=b`:
/// drops empty or inverted blocks, clamps and sorts, closes gaps (the previous
/// block grows), removes overlaps (the next block starts after the previous one),
/// and returns an empty list (leaf) when fewer than two blocks remain.
pub fn repair(a: u32, b: u32, raw: Vec<RawBlock>) -> Vec<Block> {
    let (a64, b64) = (i64::from(a), i64::from(b));
    // 1. Empty descriptions and start > end.
    let mut items: Vec<(i64, i64, String)> = raw
        .into_iter()
        .filter(|r| r.start <= r.end)
        .filter_map(|r| Some((r.start, r.end, prompts::clean_text(&r.summary)?)))
        // Entirely outside the parent: nothing left to clamp.
        .filter(|(s, e, _)| *e >= a64 && *s <= b64)
        // 2. Clamp to the parent range, then sort.
        .map(|(s, e, t)| (s.max(a64), e.min(b64), t))
        .collect();
    items.sort_by_key(|(s, e, _)| (*s, *e));

    let mut out: Vec<(i64, i64, String)> = Vec::new();
    for (mut start, end, text) in items {
        match out.last_mut() {
            // 3. The first block starts at A.
            None => start = a64,
            Some(prev) => {
                if start > prev.1 + 1 {
                    // 4. Gap: the previous block grows up to the line before this one.
                    prev.1 = start - 1;
                } else if start <= prev.1 {
                    // 5. Overlap: start after the previous block; drop if nothing is left.
                    start = prev.1 + 1;
                    if start > end {
                        continue;
                    }
                }
            }
        }
        out.push((start, end, text));
    }
    // 3. The last block ends at B.
    if let Some(last) = out.last_mut() {
        last.1 = b64;
    }
    // 6. Fewer than two blocks: the parent becomes a leaf.
    if out.len() < 2 {
        return Vec::new();
    }
    // 7. New blocks are not split yet.
    out.into_iter()
        .map(|(s, e, t)| Block::new(s as u32, e as u32, t))
        .collect()
}

/// Blocks short enough become leaves right away, without the model.
fn mark_leaves(blocks: &mut [Block], leaf_max_lines: u32) {
    for b in blocks {
        if b.children.is_none() && b.line_count() <= leaf_max_lines {
            b.children = Some(Vec::new());
        }
    }
}

struct Source {
    key: String,
    text: String,
}

impl Source {
    fn lines(&self) -> Vec<&str> {
        self.text.lines().collect()
    }
}

fn read_source(ctx: &Ctx, rel: &str) -> Result<Source> {
    let excluder = Excluder::new(&ctx.config.exclude)?;
    if let Some(p) = excluder.matched(rel) {
        bail!("{rel} is excluded by pattern {p}");
    }
    let bytes = std::fs::read(ctx.root.join(rel)).map_err(|e| anyhow::anyhow!("cannot read {rel}: {e}"))?;
    if bytes.len() as u64 > ctx.config.max_file_bytes {
        bail!("{rel} is larger than max_file_bytes");
    }
    if bytes[..bytes.len().min(8000)].contains(&0) {
        bail!("{rel} is a binary file");
    }
    Ok(Source {
        key: hash::hash_bytes(&bytes),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Level-1 blocks of a file: the stored ones when fresh (and not `force`),
/// otherwise a new split of lines 1–N saved with a guarded write.
pub fn level1(ctx: &Ctx, rel: &str, force: bool) -> Result<Vec<Block>> {
    let src = read_source(ctx, rel)?;
    let note = ctx.store.file_note(rel);
    if !force && let Some(blocks) = note.blocks.as_ref().filter(|b| b.hash == src.key) {
        return Ok(blocks.items.clone());
    }
    let lines = src.lines();
    let n = lines.len() as u32;
    if n > ctx.config.max_segment_lines {
        bail!(
            "{rel} has {n} lines, more than max_segment_lines ({})",
            ctx.config.max_segment_lines
        );
    }
    let items = if n == 0 {
        Vec::new()
    } else {
        let project = ctx.store.project_note().summary.map(|s| s.text);
        let file = note.summary.as_ref().map(|s| s.text.clone());
        let sctx = SegmentContext {
            project: project.as_deref(),
            path: rel,
            file: file.as_deref(),
            level1: &[],
            trail: &[],
        };
        let req = prompts::segment_request(&ctx.config, &ctx.system_prompt(), &sctx, &lines, (1, n));
        let raw = prompts::parse_blocks(&ctx.ask(&req)?.json);
        let fallback = raw
            .iter()
            .find_map(|r| prompts::clean_text(&r.summary))
            .or(file)
            .unwrap_or_default();
        let mut items = repair(1, n, raw);
        if items.is_empty() {
            // A file the model did not split: one leaf block over the whole file.
            let mut whole = Block::new(1, n, fallback);
            whole.children = Some(Vec::new());
            items.push(whole);
        }
        mark_leaves(&mut items, ctx.config.leaf_max_lines);
        items
    };
    let guard = Guard {
        start_key: src.key.clone(),
        force,
    };
    if !ctx.store.put_level1(rel, &guard, items.clone())? {
        ctx.store
            .log(&format!("segment {rel}: result dropped by guarded write"));
    }
    Ok(items)
}

/// Children of the block with `range`: stored when present (and not `force`),
/// otherwise a new split saved with a guarded write. Empty = leaf.
pub fn split_block(ctx: &Ctx, rel: &str, range: (u32, u32), force: bool) -> Result<Vec<Block>> {
    let src = read_source(ctx, rel)?;
    let note = ctx.store.file_note(rel);
    let Some(blocks) = note.blocks.as_ref().filter(|b| b.hash == src.key) else {
        bail!("blocks of {rel} are missing or outdated; split the file first");
    };
    let Some(block) = blocks.find(range) else {
        bail!("{rel} has no block with lines {}-{}", range.0, range.1);
    };
    if !force && let Some(children) = &block.children {
        return Ok(children.clone());
    }
    let guard = Guard {
        start_key: src.key.clone(),
        force,
    };
    if block.line_count() <= ctx.config.leaf_max_lines {
        ctx.store.put_children(rel, &guard, range, Vec::new())?;
        return Ok(Vec::new());
    }
    let lines = src.lines();
    let project = ctx.store.project_note().summary.map(|s| s.text);
    let file = note.summary.as_ref().map(|s| s.text.clone());
    let trail = blocks.path_to(range);
    let sctx = SegmentContext {
        project: project.as_deref(),
        path: rel,
        file: file.as_deref(),
        level1: &blocks.items,
        trail: &trail,
    };
    let req = prompts::segment_request(&ctx.config, &ctx.system_prompt(), &sctx, &lines, range);
    let raw = prompts::parse_blocks(&ctx.ask(&req)?.json);
    let mut children = repair(range.0, range.1, raw);
    mark_leaves(&mut children, ctx.config.leaf_max_lines);
    if !ctx.store.put_children(rel, &guard, range, children.clone())? {
        ctx.store.log(&format!(
            "segment {rel} {}-{}: result dropped by guarded write",
            range.0, range.1
        ));
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(items: &[(i64, i64, &str)]) -> Vec<RawBlock> {
        items
            .iter()
            .map(|&(start, end, s)| RawBlock {
                start,
                end,
                summary: s.to_string(),
            })
            .collect()
    }

    fn ranges(blocks: &[Block]) -> Vec<(u32, u32)> {
        blocks.iter().map(|b| b.lines).collect()
    }

    #[test]
    fn step1_drops_empty_and_inverted() {
        let out = repair(
            1,
            10,
            raw(&[(1, 4, "a"), (5, 3, "inverted"), (5, 7, "  "), (5, 10, "b")]),
        );
        assert_eq!(ranges(&out), [(1, 4), (5, 10)]);
        assert_eq!(out[1].summary, "b");
    }

    #[test]
    fn step2_clamps_and_sorts() {
        let out = repair(10, 20, raw(&[(15, 30, "b"), (0, 14, "a"), (40, 50, "outside")]));
        assert_eq!(ranges(&out), [(10, 14), (15, 20)]);
        assert_eq!(out[0].summary, "a");
    }

    #[test]
    fn step3_first_starts_at_a_last_ends_at_b() {
        let out = repair(1, 10, raw(&[(3, 5, "a"), (6, 8, "b")]));
        assert_eq!(ranges(&out), [(1, 5), (6, 10)]);
    }

    #[test]
    fn step4_gap_extends_previous() {
        let out = repair(1, 10, raw(&[(1, 3, "a"), (6, 10, "b")]));
        assert_eq!(ranges(&out), [(1, 5), (6, 10)]);
    }

    #[test]
    fn step5_overlap_moves_next_or_drops_it() {
        let out = repair(
            1,
            10,
            raw(&[(1, 5, "a"), (4, 8, "b"), (6, 7, "inside b"), (9, 10, "c")]),
        );
        assert_eq!(ranges(&out), [(1, 5), (6, 8), (9, 10)]);
        let out = repair(1, 10, raw(&[(1, 8, "a"), (2, 5, "swallowed"), (9, 10, "b")]));
        assert_eq!(ranges(&out), [(1, 8), (9, 10)]);
    }

    #[test]
    fn step6_fewer_than_two_blocks_is_leaf() {
        assert!(repair(1, 10, raw(&[(1, 10, "all")])).is_empty());
        assert!(repair(1, 10, raw(&[(1, 5, "a"), (2, 4, "swallowed")])).is_empty());
        assert!(repair(1, 10, Vec::new()).is_empty());
    }

    #[test]
    fn step7_new_blocks_are_unsplit_and_cleaned() {
        let out = repair(1, 4, raw(&[(1, 2, "a\nb"), (3, 4, "c")]));
        assert!(out.iter().all(|b| b.children.is_none()));
        assert_eq!(out[0].summary, "a b");
    }

    #[test]
    fn repaired_blocks_cover_range_exactly() {
        let out = repair(5, 40, raw(&[(38, 60, "c"), (1, 9, "a"), (12, 20, "b"), (19, 25, "b2")]));
        assert_eq!(out.first().unwrap().lines.0, 5);
        assert_eq!(out.last().unwrap().lines.1, 40);
        for w in out.windows(2) {
            assert_eq!(w[0].lines.1 + 1, w[1].lines.0);
        }
    }

    #[test]
    fn short_blocks_become_leaves() {
        let mut blocks = vec![Block::new(1, 3, "a"), Block::new(4, 10, "b")];
        mark_leaves(&mut blocks, 3);
        assert!(blocks[0].is_leaf());
        assert!(blocks[1].children.is_none());
    }
}
