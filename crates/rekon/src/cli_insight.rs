//! CLI of the insight features: decisions ("why"), cards, contracts, criteria, questions,
//! examples (traces) and relations. Agents write with JSON on stdin, people with `--edit`.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;
use rekon_core::cards::{self, Card};
use rekon_core::contracts::{self, Contract};
use rekon_core::criteria::{self, Level};
use rekon_core::decisions::{self, Decision};
use rekon_core::model::Origin;
use rekon_core::{Ctx, element, questions, relations, trace};
use serde_json::Value;

use crate::tui::editor;

#[derive(Subcommand)]
pub enum DecisionCmd {
    /// Record a decision: JSON on stdin {"title","decision","context","cost","revisit_if","assumptions":[],"elements":[]}, or --edit
    Add {
        /// Write it in $EDITOR instead of stdin
        #[arg(long)]
        edit: bool,
        /// Elements it explains (ids or paths)
        #[arg(long = "element")]
        elements: Vec<String>,
    },
    /// List decisions
    List,
    /// Show one decision
    Show { id: String },
    /// Confirm a proposed decision (for instance a reconstructed one)
    Accept { id: String },
    /// Replace a decision with a new one (JSON on stdin, or --edit); the old one stays in the history
    Supersede {
        id: String,
        #[arg(long)]
        edit: bool,
    },
    /// Ask the model to reconstruct why an element is the way it is (saved as a proposal marked AI)
    Reconstruct { element: String },
    /// Check a diff (working tree against HEAD by default) against the assumptions of the decisions
    Check {
        /// Git revision range, e.g. main..HEAD
        range: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CardCmd {
    /// Describe the behaviour changes of a diff as cards (model)
    New {
        range: Option<String>,
    },
    /// Add a card written by you: JSON on stdin {"title","before","after","consequences":[]}, or --edit
    Add {
        #[arg(long)]
        edit: bool,
    },
    List,
    Show {
        id: String,
    },
    /// The task text for an agent that should implement the card
    Brief {
        id: String,
    },
    /// Mark the card as implemented / verified
    Implemented {
        id: String,
    },
    Verify {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ContractCmd {
    /// Show the current and the desired contract of an element
    Show { element: String },
    /// Have the model describe what the element does now
    Derive {
        element: String,
        #[arg(long)]
        force: bool,
    },
    /// Set the desired contract: JSON on stdin {"responsibility","input","output","rules":[],"examples":[]}, or --edit
    Desire {
        element: String,
        #[arg(long)]
        edit: bool,
    },
    /// Turn the difference into a proposed card (what to change in code, tests, callers)
    Impact { element: String },
    /// Drop the desired contract
    Drop { element: String },
}

#[derive(Subcommand)]
pub enum CriteriaCmd {
    /// New task with criteria
    New {
        title: String,
        /// One criterion per --item
        #[arg(long = "item")]
        items: Vec<String>,
    },
    /// New task from a card (its wanted behaviour and consequences)
    FromCard {
        card: String,
    },
    List,
    Show {
        task: String,
    },
    Add {
        task: String,
        text: String,
    },
    /// Say that an element implements a criterion (in its current version)
    Implements {
        task: String,
        criterion: String,
        element: String,
    },
    /// Say which tests check a criterion (part of a test name)
    Test {
        task: String,
        criterion: String,
        pattern: String,
    },
    /// Vouch again for the current version of what implements a criterion
    Touch {
        task: String,
        criterion: String,
    },
    /// Mark a criterion as reviewed by you (current versions)
    Review {
        task: String,
        criterion: String,
    },
    /// Mark a criterion as waiting for a decision
    Open {
        task: String,
        criterion: String,
        #[arg(long)]
        close: bool,
    },
    /// Run the tests (config test_command) and record the results
    Verify,
}

#[derive(Subcommand)]
pub enum AskCmd {
    /// Pin a question and answer it
    Q {
        text: String,
    },
    List,
    Show {
        id: String,
    },
    /// Answer a pinned question again
    Again {
        id: String,
    },
    Drop {
        id: String,
    },
}

fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

fn edit(ctx: &Ctx, initial: &str) -> Result<String> {
    editor::edit_temp(ctx.config.editor_cmd.as_deref(), initial, "md")
}

/// An element id from what the user typed: an exact id, a unique name or id suffix, or a file path.
pub fn resolve(ctx: &Ctx, cwd: &Path, arg: &str, allow_file: bool) -> Result<String> {
    if element::load(&ctx.root, arg).is_some() {
        return Ok(arg.to_string());
    }
    let rel = crate::rel_prefix(ctx, cwd, Some(arg));
    if allow_file && !rel.is_empty() && ctx.root.join(&rel).is_file() {
        return Ok(rel);
    }
    let idx = relations::build(ctx)?;
    let hits = idx.find(arg);
    match hits.as_slice() {
        [e] => Ok(e.id.clone()),
        [] => Err(crate::Usage(format!("no element matches {arg}")).into()),
        many => Err(crate::Usage(format!(
            "{} elements match {arg}: {}",
            many.len(),
            many.iter()
                .take(8)
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn list_of(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn origin_tag(o: Origin) -> String {
    format!("[{}]", o.label())
}

fn print_decision(d: &Decision) {
    println!("{} {} — {} ({})", origin_tag(d.origin), d.id, d.title, d.status.label());
    for (label, text) in [
        ("Context", &d.context),
        ("Decision", &d.decision),
        ("Cost", &d.cost),
        ("Revisit if", &d.revisit_if),
    ] {
        if !text.is_empty() {
            println!("  {label}: {text}");
        }
    }
    for a in &d.assumptions {
        println!("  Assumes: {a}");
    }
    if !d.elements.is_empty() {
        println!("  Explains: {}", d.elements.join(", "));
    }
    if let Some(n) = &d.superseded_by {
        println!("  Superseded by: {n}");
    }
}

fn decision_input(edit_it: bool, ctx: &Ctx, elements: &[String], base: Option<&Decision>) -> Result<Decision> {
    let mut d = if edit_it {
        let mut start = base.cloned().unwrap_or_else(|| Decision::new("", ""));
        if start.elements.is_empty() {
            start.elements = elements.to_vec();
        }
        decisions::parse_template(&edit(ctx, &decisions::template(&start))?)?
    } else {
        let v: Value = serde_json::from_str(&read_stdin()?).context("invalid JSON on stdin")?;
        let mut d = Decision::new(&s(&v, "title"), &s(&v, "decision"));
        d.context = s(&v, "context");
        d.cost = s(&v, "cost");
        d.revisit_if = s(&v, "revisit_if");
        d.assumptions = list_of(&v, "assumptions");
        d.elements = list_of(&v, "elements");
        d
    };
    for e in elements {
        if !d.elements.contains(e) {
            d.elements.push(e.clone());
        }
    }
    d.origin = Origin::Human;
    Ok(d)
}

pub fn decision(ctx: &Ctx, cwd: &Path, cmd: DecisionCmd) -> Result<u8> {
    match cmd {
        DecisionCmd::Add { edit: e, elements } => {
            let d = decisions::add(ctx, decision_input(e, ctx, &elements, None)?)?;
            println!("Recorded {}", d.id);
        }
        DecisionCmd::List => {
            for d in decisions::list(ctx) {
                println!("{:<12} {} {} {}", d.status.label(), d.id, origin_tag(d.origin), d.title);
            }
        }
        DecisionCmd::Show { id } => {
            print_decision(&decisions::get(ctx, &id).ok_or_else(|| crate::Usage(format!("no decision matches {id}")))?);
        }
        DecisionCmd::Accept { id } => println!("Accepted {}", decisions::accept(ctx, &id)?.id),
        DecisionCmd::Supersede { id, edit: e } => {
            let old = decisions::get(ctx, &id).ok_or_else(|| crate::Usage(format!("no decision matches {id}")))?;
            let new = decisions::supersede(ctx, &id, decision_input(e, ctx, &[], Some(&old))?)?;
            println!("{} replaces {}", new.id, old.id);
        }
        DecisionCmd::Reconstruct { element } => {
            let id = resolve(ctx, cwd, &element, false)?;
            print_decision(&decisions::reconstruct(ctx, &id)?);
            println!("(inferred from the code: check it, then `rekon decision accept`)");
        }
        DecisionCmd::Check { range } => {
            let diff = decisions::git_diff(ctx, range.as_deref())?;
            let findings = decisions::check(ctx, &diff)?;
            if findings.is_empty() {
                println!("No decision is affected by this change.");
            }
            for f in &findings {
                println!("{} {} — {}", f.decision.id, f.decision.title, f.why);
                println!("  Revisit if: {}", f.decision.revisit_if);
            }
            return Ok(u8::from(!findings.is_empty()));
        }
    }
    Ok(0)
}

fn print_card(c: &Card) {
    println!("{} {} — {} ({})", origin_tag(c.origin), c.id, c.title, c.status.label());
    println!("  Before: {}\n  After:  {}", c.before, c.after);
    for x in &c.consequences {
        println!("  Consequence: {x}");
    }
    for b in &c.basis {
        println!(
            "  Basis ({}): {}:{}-{} {}",
            b.kind, b.file, b.start_line, b.end_line, b.note
        );
    }
}

pub fn card(ctx: &Ctx, cmd: CardCmd) -> Result<u8> {
    let find = |id: &str| cards::get(ctx, id).ok_or_else(|| crate::Usage(format!("no card matches {id}")));
    match cmd {
        CardCmd::New { range } => {
            for c in cards::from_diff(ctx, range.as_deref())? {
                print_card(&c);
            }
        }
        CardCmd::Add { edit: e } => {
            let c = if e {
                cards::parse_template(&edit(ctx, &cards::template(&Card::spec("", "", "")))?)?
            } else {
                let v: Value = serde_json::from_str(&read_stdin()?).context("invalid JSON on stdin")?;
                let mut c = Card::spec(&s(&v, "title"), &s(&v, "before"), &s(&v, "after"));
                c.consequences = list_of(&v, "consequences");
                c.elements = list_of(&v, "elements");
                c
            };
            println!("Added {}", cards::add(ctx, c)?.id);
        }
        CardCmd::List => {
            for c in cards::list(ctx) {
                println!("{:<12} {} {} {}", c.status.label(), c.id, origin_tag(c.origin), c.title);
            }
        }
        CardCmd::Show { id } => print_card(&find(&id)?),
        CardCmd::Brief { id } => print!("{}", cards::brief(&find(&id)?)),
        CardCmd::Implemented { id } => println!("{}", cards::set_status(ctx, &id, cards::Status::Implemented)?.id),
        CardCmd::Verify { id } => println!("{}", cards::set_status(ctx, &id, cards::Status::Verified)?.id),
    }
    Ok(0)
}

fn print_contract(label: &str, c: &Contract) {
    println!(
        "{label}:\n  Responsibility: {}\n  Input: {}\n  Output: {}",
        c.responsibility, c.input, c.output
    );
    for r in &c.rules {
        println!("  Rule: {r}");
    }
    for x in &c.examples {
        println!("  Example: {x}");
    }
}

pub fn contract(ctx: &Ctx, cwd: &Path, cmd: ContractCmd) -> Result<u8> {
    match cmd {
        ContractCmd::Show { element } => {
            let id = resolve(ctx, cwd, &element, false)?;
            let rec = contracts::get(ctx, &id);
            let hash = element::load(&ctx.root, &id).map(|e| e.hash);
            println!("{id} — {}", contracts::state(&rec, hash.as_deref()).label());
            match &rec.current {
                Some(c) => print_contract(&format!("Current {}", origin_tag(c.origin)), &c.value),
                None => println!("(not described yet: `rekon contract derive`)"),
            }
            if let Some(d) = &rec.desired {
                print_contract("Desired [human]", d);
                for (f, a, b) in contracts::diff(&rec) {
                    println!("  ~ {f}: \"{a}\" -> \"{b}\"");
                }
            }
        }
        ContractCmd::Derive { element, force } => {
            let id = resolve(ctx, cwd, &element, false)?;
            let rec = contracts::derive(ctx, &id, force)?;
            print_contract("Current [AI]", &rec.current.expect("derived").value);
        }
        ContractCmd::Desire { element, edit: e } => {
            let id = resolve(ctx, cwd, &element, false)?;
            let c = if e {
                contracts::parse_template(&edit(ctx, &contracts::template(&contracts::get(ctx, &id)))?)?
            } else {
                let v: Value = serde_json::from_str(&read_stdin()?).context("invalid JSON on stdin")?;
                Contract {
                    responsibility: s(&v, "responsibility"),
                    input: s(&v, "input"),
                    output: s(&v, "output"),
                    rules: list_of(&v, "rules"),
                    examples: list_of(&v, "examples"),
                }
            };
            contracts::set_desired(ctx, &id, c)?;
            println!("Desired contract saved for {id} (a proposal: the code is unchanged)");
        }
        ContractCmd::Impact { element } => {
            let id = resolve(ctx, cwd, &element, false)?;
            print_card(&contracts::impact(ctx, &id)?);
        }
        ContractCmd::Drop { element } => {
            let id = resolve(ctx, cwd, &element, false)?;
            contracts::clear_desired(ctx, &id)?;
        }
    }
    Ok(0)
}

pub fn level_mark(l: Level) -> &'static str {
    match l {
        Level::None => "-",
        Level::Current => "ok",
        Level::Stale => "stale",
        Level::Failing => "FAIL",
        Level::Open => "open",
    }
}

fn print_task(ctx: &Ctx, t: &criteria::Task) {
    println!("{} — {}", t.id, t.title);
    println!("  {:<4} {:<7} {:<7} {:<7}  criterion", "", "impl", "tests", "review");
    for c in &t.items {
        let st = criteria::status(ctx, c);
        println!(
            "  {:<4} {:<7} {:<7} {:<7}  {}",
            c.id,
            level_mark(st.implementation),
            level_mark(st.verification),
            level_mark(st.review),
            c.text
        );
    }
}

pub fn criteria_cmd(ctx: &Ctx, cwd: &Path, cmd: CriteriaCmd) -> Result<u8> {
    let find = |id: &str| criteria::get(ctx, id).ok_or_else(|| crate::Usage(format!("no task matches {id}")));
    match cmd {
        CriteriaCmd::New { title, items } => print_task(ctx, &criteria::new_task(ctx, &title, &items)?),
        CriteriaCmd::FromCard { card } => {
            let c = cards::get(ctx, &card).ok_or_else(|| crate::Usage(format!("no card matches {card}")))?;
            print_task(ctx, &criteria::from_card(ctx, &c)?);
        }
        CriteriaCmd::List => {
            for t in criteria::list(ctx) {
                print_task(ctx, &t);
            }
        }
        CriteriaCmd::Show { task } => print_task(ctx, &find(&task)?),
        CriteriaCmd::Add { task, text } => print_task(ctx, &criteria::add(ctx, &task, &text)?),
        CriteriaCmd::Implements {
            task,
            criterion,
            element,
        } => {
            let id = resolve(ctx, cwd, &element, false)?;
            print_task(ctx, &criteria::link_implementation(ctx, &task, &criterion, &id)?);
        }
        CriteriaCmd::Test {
            task,
            criterion,
            pattern,
        } => print_task(ctx, &criteria::link_test(ctx, &task, &criterion, &pattern)?),
        CriteriaCmd::Touch { task, criterion } => print_task(ctx, &criteria::touch(ctx, &task, &criterion)?),
        CriteriaCmd::Review { task, criterion } => print_task(ctx, &criteria::mark_reviewed(ctx, &task, &criterion)?),
        CriteriaCmd::Open { task, criterion, close } => {
            print_task(ctx, &criteria::set_open(ctx, &task, &criterion, !close)?)
        }
        CriteriaCmd::Verify => {
            let (tests, updated) = criteria::verify(ctx)?;
            println!("{tests} test result(s) read, {updated} criterion(s) updated");
            for t in criteria::list(ctx) {
                print_task(ctx, &t);
            }
        }
    }
    Ok(0)
}

fn print_question(q: &questions::Question) {
    println!("{} — {}", q.id, q.text);
    let Some(a) = &q.answer else {
        println!("  (not answered yet)");
        return;
    };
    println!(
        "  {} — examined {} of {} files",
        origin_tag(a.origin),
        a.examined,
        a.total
    );
    for i in &a.items {
        println!(
            "  {}:{}-{}  {} — {} ({})",
            i.path, i.start, i.end, i.name, i.reason, i.role
        );
    }
    for o in &a.omitted {
        println!(
            "  left out: {} — {}{}",
            o.path,
            o.reason,
            if o.deliberate { "" } else { " (not looked at)" }
        );
    }
}

pub fn ask(ctx: &Ctx, cmd: AskCmd) -> Result<u8> {
    match cmd {
        AskCmd::Q { text } => {
            let q = questions::pin(ctx, &text)?;
            print_question(&questions::answer(ctx, &q.id)?);
        }
        AskCmd::List => {
            for q in questions::list(ctx) {
                println!(
                    "{} {}",
                    q.id,
                    q.answer
                        .as_ref()
                        .map_or("(open)".to_string(), |a| format!("({} items)", a.items.len()))
                );
            }
        }
        AskCmd::Show { id } => {
            print_question(&questions::get(ctx, &id).ok_or_else(|| crate::Usage(format!("no question matches {id}")))?)
        }
        AskCmd::Again { id } => print_question(&questions::answer(ctx, &id)?),
        AskCmd::Drop { id } => questions::remove(ctx, &id)?,
    }
    Ok(0)
}

pub fn trace_cmd(
    ctx: &Ctx,
    cwd: &Path,
    element: &str,
    input: Option<String>,
    expect: Option<String>,
    card: bool,
) -> Result<u8> {
    let id = resolve(ctx, cwd, element, false)?;
    let mut t = match (&input, trace::get(ctx, &id)) {
        (None, Some(t)) if expect.is_some() || card => t,
        _ => trace::predict(ctx, &id, input.as_deref())?,
    };
    if let Some(e) = &expect {
        t = trace::set_expected(ctx, &id, e)?;
    }
    println!(
        "{} example run of {id} (a prediction from the code, not an execution)",
        origin_tag(t.origin)
    );
    println!("  Input: {}", t.input);
    for (n, st) in t.steps.iter().enumerate() {
        let at = if st.line > 0 {
            format!(" (line {})", st.line)
        } else {
            String::new()
        };
        println!(
            "  {}. {}{at}: {} -> {}\n     because {}",
            n + 1,
            st.label,
            st.before,
            st.after,
            st.why
        );
    }
    println!("  Result: {}", t.result);
    if let Some(e) = &t.expected {
        println!("  You expect: {e}");
    }
    if card {
        print_card(&trace::expectation_card(ctx, &id)?);
    }
    Ok(0)
}

pub fn relations_cmd(ctx: &Ctx, cwd: &Path, element: &str) -> Result<u8> {
    let id = resolve(ctx, cwd, element, true)?;
    let idx = relations::build(ctx)?;
    let links: Vec<relations::Link> = if ctx.root.join(&id).is_file() {
        idx.of_file(&id)
    } else {
        let mut v = idx.calls(&id);
        v.extend(idx.called_by(&id));
        v.extend(idx.tested_by(&id));
        v
    };
    println!("{id}");
    if links.is_empty() {
        println!("  (no static relations found)");
    }
    for l in links {
        println!(
            "  {:<9} {} — {}:{} {}",
            l.kind,
            l.label,
            l.path,
            l.line,
            origin_tag(l.origin)
        );
    }
    let path = id.split_once("::").map_or(id.as_str(), |(p, _)| p);
    for d in decisions::for_element(ctx, &id, path) {
        println!("  decidedBy {} — {} {}", d.id, d.title, origin_tag(d.origin));
    }
    for c in cards::for_element(ctx, &id, path) {
        println!("  changedBy {} — {} {}", c.id, c.title, origin_tag(c.origin));
    }
    Ok(0)
}
