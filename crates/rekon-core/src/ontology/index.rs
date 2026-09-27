//! Indexing: scan → static facts → model extraction of changed files (in parallel,
//! each file's facts saved as soon as they are complete) → merge → `graph.json`.
//! Unchanged files never go to the model again: their facts are keyed by content
//! hash and by the fingerprint of the schema and the extractor.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;

use super::extract::{self, Part, Source};
use super::model::{FACTS_VERSION, FileFacts, GRAPH_VERSION, Graph, Node, RawEdge, Scan};
use super::schema::Schema;
use super::{facts_dir, facts_path, graph_path, manifest, merge, outline};
use crate::Ctx;
use crate::jobs::run_parallel;
use crate::scan::{Excluder, ROOT, Tree};
use crate::store::{self, write_json_atomic};

/// Bumped when the prompt or the grounding changes, so older facts are redone.
const EXTRACTOR_VERSION: u32 = 2;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Sends files to the model even when their facts are fresh.
    pub force: bool,
    /// Limits extraction to these files; the graph is still built from all facts.
    pub paths: Option<Vec<String>>,
    /// Only reports what would be sent to the model (`Report::planned`).
    pub dry_run: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub errors: usize,
    pub last_error: Option<String>,
    pub cost: f64,
}

impl Progress {
    pub fn line(&self) -> String {
        crate::text::ontology_progress(self.done, self.total, self.errors, self.cost)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Files in scope.
    pub files: usize,
    /// Files sent to the model.
    pub analyzed: usize,
    /// Files whose facts were still fresh.
    pub reused: usize,
    pub errors: usize,
    pub nodes: usize,
    pub edges: usize,
    pub cost: f64,
    /// Model requests of this run, as file ranges (`path:start-end`).
    pub planned: Vec<Vec<String>>,
}

/// Facts are valid only for the schema and extractor they were made with.
pub fn fingerprint(schema: &Schema) -> String {
    crate::hash::hash_bytes(format!("{EXTRACTOR_VERSION}\n{}", schema.fingerprint()).as_bytes())
}

pub fn read_facts(map_dir: &Path, rel: &str) -> Option<FileFacts> {
    let text = std::fs::read_to_string(facts_path(map_dir, rel)).ok()?;
    serde_json::from_str(&text).ok()
}

fn is_fresh(facts: &FileFacts, source: &Source, print: &str) -> bool {
    facts.version == FACTS_VERSION && facts.hash == source.hash && facts.schema == print
}

/// Files the ontology covers: text files matching `ontology.include` and not
/// `ontology.exclude` (tests by default).
pub fn scope(ctx: &Ctx, tree: &Tree) -> Result<Vec<usize>> {
    let include = Excluder::new(&ctx.config.ontology.include)?;
    let exclude = Excluder::new(&ctx.config.ontology.exclude)?;
    Ok((0..tree.nodes.len())
        .filter(|&i| {
            let n = tree.node(i);
            !n.is_dir
                && n.file.as_ref().is_some_and(|f| f.is_text())
                && include.matched(&n.path).is_some()
                && exclude.matched(&n.path).is_none()
        })
        .collect())
}

fn load_source(ctx: &Ctx, rel: &str) -> Option<Source> {
    let bytes = std::fs::read(ctx.root.join(rel)).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let hash = crate::hash::hash_bytes(&bytes);
    let summary = ctx
        .store
        .file_note(rel)
        .summary
        .filter(|s| s.hash == hash)
        .map(|s| s.text);
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let code_lines = outline::tests_start(rel, &text).map_or(lines.len() as u32, |start| start - 1);
    Some(Source {
        path: rel.to_string(),
        outline: outline::outline(rel, &text),
        code_lines,
        lines,
        hash,
        summary,
    })
}

/// Requests of at most `max_lines` lines and `max_files` files; a longer file goes
/// alone, in parts cut before a definition that would cross the end of a part (the
/// outermost one, e.g. an `impl` rather than a method in it), unless that would make
/// the part shorter than a quarter of `max_lines`. A file at most a quarter longer
/// than `max_lines`, or a last part shorter than that quarter, is not split off.
fn batches<'a>(files: &[&'a Source], max_lines: u32, max_files: usize) -> Vec<Vec<Part<'a>>> {
    let max_lines = max_lines.max(50);
    let mut out = Vec::new();
    let mut current: Vec<Part> = Vec::new();
    let mut lines = 0;
    let slack = max_lines / 4;
    for &s in files {
        let n = s.code_lines;
        if n > max_lines + slack {
            let mut start = 1;
            while start <= n {
                let mut end = (start + max_lines - 1).min(n);
                if n - end <= slack {
                    end = n;
                }
                if end < n
                    && let Some(cut) = s
                        .outline
                        .iter()
                        .filter(|it| it.start > start && it.start <= end && it.end > end)
                        .filter(|it| it.start - start >= max_lines / 4)
                        .min_by_key(|it| (it.depth, std::cmp::Reverse(it.start)))
                        .map(|it| it.start - 1)
                {
                    end = cut;
                }
                out.push(vec![Part {
                    source: s,
                    range: (start, end),
                }]);
                start = end + 1;
            }
            continue;
        }
        if !current.is_empty() && (lines + n > max_lines || current.len() >= max_files.max(1)) {
            out.push(std::mem::take(&mut current));
            lines = 0;
        }
        current.push(Part {
            source: s,
            range: (1, n),
        });
        lines += n;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Facts of a file collected from its parts until all of them are answered.
#[derive(Default)]
struct Pending {
    parts_left: usize,
    failed: bool,
    nodes: Vec<Node>,
    edges: Vec<RawEdge>,
}

struct Runner<'a> {
    ctx: &'a Ctx,
    schema: &'a Schema,
    print: String,
    system: String,
    project: Option<String>,
    known: Vec<extract::Known>,
    pending: Mutex<HashMap<String, Pending>>,
    progress: Mutex<Progress>,
    cost_before: f64,
    on_progress: &'a (dyn Fn(&Progress) + Sync),
}

impl Runner<'_> {
    fn update(&self, f: impl FnOnce(&mut Progress)) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut p);
        p.cost = self.ctx.cost.usd() - self.cost_before;
        (self.on_progress)(&p);
    }

    fn save(&self, source: &Source, nodes: Vec<Node>, edges: Vec<RawEdge>) {
        let facts = FileFacts {
            version: FACTS_VERSION,
            hash: source.hash.clone(),
            schema: self.print.clone(),
            nodes,
            edges,
        };
        if let Err(e) = write_json_atomic(&facts_path(self.ctx.store.dir(), &source.path), &facts) {
            self.error(1, format!("ontology {}: {e:#}", source.path));
        }
    }

    fn error(&self, count: usize, message: String) {
        self.ctx.store.log(&format!("error: {message}"));
        self.update(|p| {
            p.errors += count;
            p.last_error = Some(message);
        });
    }

    fn run_batch(&self, parts: Vec<Part>) {
        let req = extract::request(
            &self.ctx.config,
            &self.system,
            self.schema,
            self.project.as_deref(),
            &self.known,
            &parts,
        );
        let answer = self.ctx.ask(&req);
        let names: Vec<&str> = parts.iter().map(|p| p.source.path.as_str()).collect();
        let mut extracted = match answer {
            Ok(resp) => {
                let ex = extract::parse(&resp.json, self.schema, &parts);
                if ex.dropped > 0 {
                    self.ctx.store.log(&format!(
                        "ontology {}: dropped {} elements without valid evidence",
                        names.join(", "),
                        ex.dropped
                    ));
                }
                Some(ex.files)
            }
            Err(e) => {
                self.error(0, format!("ontology {}: {e:#}", names.join(", ")));
                None
            }
        };
        for part in &parts {
            let path = &part.source.path;
            let finished = {
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                let entry = pending.entry(path.clone()).or_default();
                match extracted.as_mut().and_then(|files| files.remove(path)) {
                    Some((nodes, edges)) => {
                        entry.nodes.extend(nodes);
                        entry.edges.extend(edges);
                    }
                    None if extracted.is_none() => entry.failed = true,
                    None => {}
                }
                entry.parts_left = entry.parts_left.saturating_sub(1);
                (entry.parts_left == 0).then(|| std::mem::take(entry))
            };
            let Some(done) = finished else { continue };
            if done.failed {
                self.update(|p| {
                    p.done += 1;
                    p.errors += 1;
                });
            } else {
                self.save(part.source, done.nodes, done.edges);
                self.update(|p| p.done += 1);
            }
        }
    }
}

fn repo_name(ctx: &Ctx) -> String {
    ctx.root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository")
        .to_string()
}

/// Full pipeline; see the module docs.
pub fn run(ctx: &Ctx, opts: &Options, on_progress: &(dyn Fn(&Progress) + Sync)) -> Result<Report> {
    let (tree, warnings) = Tree::scan(&ctx.root, &ctx.config)?;
    for w in warnings {
        ctx.store.log(&format!("warning: {w}"));
    }
    let schema = Schema::load(ctx.store.dir())?;
    let print = fingerprint(&schema);
    let map_dir = ctx.store.dir().to_path_buf();
    let paths: Vec<String> = scope(ctx, &tree)?
        .into_iter()
        .map(|i| tree.node(i).path.clone())
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let mut sources: Vec<Source> = run_parallel(threads, paths, |p| load_source(ctx, &p))
        .into_iter()
        .flatten()
        .collect();
    sources.sort_by(|a, b| a.path.cmp(&b.path));

    let wanted = |path: &str| opts.paths.as_ref().is_none_or(|ps| ps.iter().any(|p| p == path));
    let fresh_before: HashSet<&str> = sources
        .iter()
        .filter(|s| read_facts(&map_dir, &s.path).is_some_and(|f| is_fresh(&f, s, &print)))
        .map(|s| s.path.as_str())
        .collect();
    let (empty, stale): (Vec<&Source>, Vec<&Source>) = sources
        .iter()
        .filter(|s| wanted(&s.path) && (opts.force || !fresh_before.contains(s.path.as_str())))
        .partition(|s| s.code_lines == 0);
    let selected: HashSet<&str> = empty.iter().chain(&stale).map(|s| s.path.as_str()).collect();
    let reused = fresh_before.iter().filter(|p| !selected.contains(*p)).count();

    let previous = super::graph::read(&map_dir);
    let runner = Runner {
        ctx,
        schema: &schema,
        print: print.clone(),
        system: ctx.system_prompt(),
        project: ctx.store.project_note().summary.map(|s| s.text),
        known: extract::known_elements(&sources, previous.as_ref()),
        pending: Mutex::new(HashMap::new()),
        progress: Mutex::new(Progress::default()),
        cost_before: ctx.cost.usd(),
        on_progress,
    };
    for s in &empty {
        runner.save(s, Vec::new(), Vec::new());
    }
    let batches = batches(
        &stale,
        ctx.config.ontology.batch_lines,
        ctx.config.ontology.batch_max_files,
    );
    let planned: Vec<Vec<String>> = batches
        .iter()
        .map(|b| {
            b.iter()
                .map(|p| format!("{}:{}-{}", p.source.path, p.range.0, p.range.1))
                .collect()
        })
        .collect();
    if opts.dry_run {
        return Ok(Report {
            files: sources.len(),
            analyzed: stale.len(),
            reused,
            planned,
            ..Report::default()
        });
    }
    if !batches.is_empty() {
        // Report a missing CLI once, before any work.
        ctx.backend()?;
    }
    {
        let mut pending = runner.pending.lock().unwrap_or_else(|e| e.into_inner());
        for part in batches.iter().flatten() {
            pending.entry(part.source.path.clone()).or_default().parts_left += 1;
        }
    }
    runner.update(|p| p.total = stale.len());
    run_parallel(ctx.config.workers, batches, |b| runner.run_batch(b));

    // Facts of files that left the scope.
    let kept: HashSet<&str> = sources.iter().map(|s| s.path.as_str()).collect();
    for p in store::list_notes(&facts_dir(&map_dir)) {
        if !kept.contains(p.as_str()) {
            let _ = std::fs::remove_file(facts_path(&map_dir, &p));
        }
    }

    let facts: Vec<(String, FileFacts)> = sources
        .iter()
        .filter_map(|s| {
            let f = read_facts(&map_dir, &s.path).filter(|f| is_fresh(f, s, &print))?;
            Some((s.path.clone(), f))
        })
        .collect();
    let all_files: Vec<String> = tree
        .nodes
        .iter()
        .filter(|n| !n.is_dir)
        .map(|n| n.path.clone())
        .collect();
    let manifests = manifest::read(&ctx.root, &all_files);
    let readme = tree.node(ROOT).children.iter().map(|&c| tree.node(c)).find_map(|n| {
        let f = n.file.as_ref().filter(|f| f.is_text())?;
        n.name
            .to_lowercase()
            .starts_with("readme")
            .then(|| (n.path.as_str(), f.lines.unwrap_or(1)))
    });
    let lines = |p: &str| tree.get(p).and_then(|i| tree.node(i).file.as_ref()?.lines);
    let summary = ctx.store.project_note().summary.map(|s| s.text);
    let repo = repo_name(ctx);
    let merged = merge::merge(&merge::Input {
        schema: &schema,
        repo: &repo,
        summary: summary.as_deref(),
        manifests: &manifests,
        readme,
        facts: &facts,
        lines: &lines,
    });
    let progress = runner.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let report = Report {
        files: sources.len(),
        analyzed: stale.len(),
        reused,
        errors: progress.errors,
        nodes: merged.nodes.len(),
        edges: merged.edges.len(),
        cost: progress.cost,
        planned,
    };
    let graph = Graph {
        version: GRAPH_VERSION,
        schema,
        root: Some(merged.root),
        scan: Scan {
            at: store::timestamp(std::time::SystemTime::now()),
            files: report.files,
            analyzed: report.analyzed,
            reused: report.reused,
            errors: report.errors,
            cost_usd: (report.cost * 10_000.0).round() / 10_000.0,
        },
        nodes: merged.nodes,
        edges: merged.edges,
    };
    write_json_atomic(&graph_path(&map_dir), &graph)?;
    ctx.store.log(&format!(
        "ontology: {} files, {} analyzed, {} errors, {} nodes, {} edges",
        report.files, report.analyzed, report.errors, report.nodes, report.edges
    ));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(path: &str, n: usize, items: &[(u32, u32, usize)]) -> Source {
        Source {
            path: path.into(),
            hash: "h".into(),
            lines: (0..n).map(|i| format!("l{i}")).collect(),
            code_lines: n as u32,
            outline: items
                .iter()
                .map(|&(start, end, depth)| outline::Item {
                    kind: "fn",
                    name: "f".into(),
                    qualified: "f".into(),
                    start,
                    end,
                    depth,
                })
                .collect(),
            summary: None,
        }
    }

    #[test]
    fn batches_group_small_files_and_cut_long_ones_between_definitions() {
        let a = src("a.rs", 40, &[]);
        let b = src("b.rs", 50, &[]);
        let c = src("c.rs", 30, &[]);
        let long = src("long.rs", 260, &[(1, 90, 0), (91, 130, 0), (131, 260, 0)]);
        let files = [&a, &b, &long, &c];
        let got: Vec<Vec<String>> = batches(&files, 100, 5)
            .iter()
            .map(|b| {
                b.iter()
                    .map(|p| format!("{}:{}-{}", p.source.path, p.range.0, p.range.1))
                    .collect()
            })
            .collect();
        assert_eq!(
            got,
            [
                vec!["long.rs:1-90"],
                vec!["long.rs:91-130"],
                vec!["long.rs:131-230"],
                vec!["long.rs:231-260"],
                vec!["a.rs:1-40", "b.rs:1-50"],
                vec!["c.rs:1-30"],
            ]
        );
        // One long impl: cut before a method, not in the middle of one.
        let imp = src("imp.rs", 300, &[(5, 300, 0), (10, 70, 1), (71, 140, 1), (141, 300, 1)]);
        let parts: Vec<(u32, u32)> = batches(&[&imp], 100, 5).iter().map(|b| b[0].range).collect();
        assert_eq!(parts, [(1, 70), (71, 140), (141, 240), (241, 300)]);
        // A short tail stays with the part before it; tests at the end are left out.
        let tail = src("tail.rs", 120, &[]);
        assert_eq!(batches(&[&tail], 100, 5)[0][0].range, (1, 120));
        let mut tested = src("tested.rs", 150, &[]);
        tested.code_lines = 60;
        assert_eq!(batches(&[&tested], 100, 5)[0][0].range, (1, 60));
    }
}
