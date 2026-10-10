//! Tests of the insight layer: lenses, inspector tabs, history, pins, input, templates.

use ratatui::crossterm::event::KeyCode;
use rekon_core::decisions::{self, Decision};
use rekon_core::{cards, criteria, element, questions};

use super::app::{Detail, Focus, View};
use super::editor::{EditRequest, Followup};
use super::tests::{Fixture, fixture};

fn id_of(fx: &Fixture, path: &str, name: &str) -> String {
    let text = std::fs::read_to_string(fx.dir.path().join(path)).unwrap();
    element::elements(path, &text)
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no element {name} in {path}"))
        .id
}

fn type_text(fx: &mut Fixture, text: &str) {
    for c in text.chars() {
        fx.key(KeyCode::Char(c));
    }
    fx.key(KeyCode::Enter);
}

#[test]
fn lens_tabs_are_visible_and_switch_with_number_keys() {
    let mut fx = fixture(160, 30);
    let screen = fx.draw().join("\n");
    for tab in ["1 Tree", "2 Changes", "3 Why", "4 Cards", "5 Criteria", "6 Ask"] {
        assert!(screen.contains(tab), "{tab}: {screen}");
    }
    for (key, view, hint) in [
        ('3', View::Decisions, "No decisions yet"),
        ('4', View::Cards, "No cards yet"),
        ('5', View::Criteria, "No tasks yet"),
        ('6', View::Ask, "No questions yet"),
    ] {
        fx.key(KeyCode::Char(key));
        let screen = fx.draw().join("\n");
        assert_eq!(fx.app.view, view);
        assert!(screen.contains(hint), "{hint}: {screen}");
    }
    fx.key(KeyCode::Char('1'));
    assert_eq!(fx.app.view, View::Tree);
}

#[test]
fn decisions_lens_shows_origin_and_the_reason_in_the_footer() {
    let mut fx = fixture(160, 30);
    let ctx = fx.app.ctx.clone();
    let mut d = Decision::new("Queue the mail", "Send mail through a queue");
    d.cost = "Delay".into();
    d.revisit_if = "Instant answers are required".into();
    d.elements = vec!["src/main.rs".into()];
    decisions::add(&ctx, d).unwrap();
    decisions::reconstruct(&ctx, &id_of(&fx, "src/main.rs", "main")).unwrap();
    fx.key(KeyCode::Char('3'));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("Queue the mail"), "{screen}");
    assert!(screen.contains("[human]") && screen.contains("[AI]"), "{screen}");
    assert!(screen.contains("unconfirmed decision"), "header badge: {screen}");
    let desc = fx.app.selected_description();
    assert!(
        desc.contains("costs: Delay") && desc.contains("Instant answers"),
        "{desc}"
    );
    // The code the decision explains is shown next to it.
    assert!(fx.app.evidence.is_some());
}

#[test]
fn inspector_tabs_show_relations_why_contract_and_example_of_the_element() {
    let mut fx = fixture(170, 36);
    let ctx = fx.app.ctx.clone();
    let main = id_of(&fx, "src/main.rs", "main");
    fx.app.goto_element(&main);
    fx.draw();
    assert_eq!(fx.app.ins.subject.as_ref().unwrap().id, main);

    fx.key(KeyCode::Char('v'));
    assert_eq!(fx.app.ins.detail, Detail::Relations);
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("Relations") && screen.contains("Called by") && screen.contains("Tested by"),
        "{screen}"
    );

    fx.key(KeyCode::Char('v'));
    let screen = fx.draw().join("\n");
    assert!(screen.contains("No decision explains this yet"), "{screen}");
    let mut d = Decision::new("Entry point", "main only starts the app");
    d.elements = vec![main.clone()];
    decisions::add(&ctx, d).unwrap();
    let screen = fx.draw().join("\n");
    assert!(screen.contains("Entry point") && screen.contains("[human]"), "{screen}");

    // Contract: describe (model), then wish; the two layers show apart.
    fx.key(KeyCode::Char('v'));
    fx.app.focus = Focus::Code;
    fx.key(KeyCode::Char('d'));
    fx.wait_jobs();
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("Now (what the code does)") && screen.contains("[AI]"),
        "{screen}"
    );
    assert!(screen.contains("Nothing wanted yet"), "{screen}");

    // Example run: a prediction, labelled as such, with no recorded evidence.
    fx.key(KeyCode::Char('v'));
    fx.key(KeyCode::Char('p'));
    fx.wait_jobs();
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("[predicted]") && screen.contains("not executed"),
        "{screen}"
    );
    assert!(screen.contains("none: rekon does not run code"), "{screen}");
    assert_eq!(fx.app.errors, 0, "{:?}", fx.app.last_error);
}

#[test]
fn history_and_pins_compare_elements() {
    let mut fx = fixture(170, 36);
    let (main, run) = (id_of(&fx, "src/main.rs", "main"), id_of(&fx, "src/main.rs", "run"));
    fx.app.goto_element(&main);
    fx.draw();
    fx.key(KeyCode::Char('P'));
    fx.app.goto_element(&run);
    fx.draw();
    fx.key(KeyCode::Char('P'));
    assert_eq!(fx.app.ins.pins, [main.clone(), run.clone()]);
    fx.key(KeyCode::Char('<'));
    fx.draw();
    assert_eq!(
        fx.app.ins.subject.as_ref().unwrap().id,
        main,
        "back goes to the previous element"
    );
    fx.key(KeyCode::Char('>'));
    fx.draw();
    assert_eq!(fx.app.ins.subject.as_ref().unwrap().id, run);

    fx.app.set_detail(Detail::Compare);
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("main") && screen.contains("run") && screen.contains("called by"),
        "{screen}"
    );
}

#[test]
fn asking_a_question_lists_the_answer_and_what_was_left_out() {
    let mut fx = fixture(170, 36);
    fx.key(KeyCode::Char('6'));
    fx.key(KeyCode::Char('n'));
    type_text(&mut fx, "How does the program start?");
    fx.wait_jobs();
    let screen = fx.draw().join("\n");
    assert!(screen.contains("How does the program start?"), "{screen}");
    assert!(screen.contains("looked at 2/"), "{screen}");
    assert!(screen.contains("left out:"), "{screen}");
    assert_eq!(questions::list(&fx.app.ctx).len(), 1);
    assert_eq!(fx.app.errors, 0, "{:?}", fx.app.last_error);
}

#[test]
fn templates_edited_outside_are_saved_when_the_editor_returns() {
    let mut fx = fixture(120, 30);
    fx.key(KeyCode::Char('3'));
    fx.key(KeyCode::Char('n'));
    let req = fx.app.editor_request.take().expect("n asks for an editor");
    assert!(matches!(req.followup, Some(Followup::Decision { supersede: None })));
    let abs = req.abs.clone();
    std::fs::write(&abs, "title: Use a queue\nstatus: accepted\nelements: src/lib.rs\n\n--- decision\nSend through a queue\n\n--- cost\nDelay\n").unwrap();
    fx.app.after_edit(req, Ok(()));
    assert_eq!(decisions::list(&fx.app.ctx)[0].title, "Use a queue");
    assert!(!abs.exists(), "the temporary file is removed");
    let screen = fx.draw().join("\n");
    assert!(screen.contains("Use a queue"), "{screen}");

    // A broken template saves nothing and says why.
    fx.key(KeyCode::Char('n'));
    let req = fx.app.editor_request.take().unwrap();
    std::fs::write(&req.abs, "title:\n").unwrap();
    fx.app.after_edit(req, Ok(()));
    assert_eq!(decisions::list(&fx.app.ctx).len(), 1);
    assert!(fx.app.last_error.as_deref().unwrap_or("").contains("not saved"));
}

#[test]
fn cards_and_criteria_lenses_track_three_separate_answers() {
    let mut fx = fixture(170, 36);
    let ctx = fx.app.ctx.clone();
    let mut card = cards::Card::spec("Retry payments", "Fails at once", "Retries after a short failure");
    card.consequences = vec!["Show a separate notification status".into()];
    let card = cards::add(&ctx, card).unwrap();
    fx.key(KeyCode::Char('4'));
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("Retry payments") && screen.contains("[human]") && screen.contains("proposed"),
        "{screen}"
    );
    fx.key(KeyCode::Char('c'));
    let task = criteria::list(&ctx).remove(0);
    assert_eq!(task.items.len(), 2);
    let main = id_of(&fx, "src/main.rs", "main");
    criteria::link_implementation(&ctx, &task.id, "c1", &main).unwrap();

    fx.key(KeyCode::Char('5'));
    let screen = fx.draw().join("\n");
    assert!(
        screen.contains("Retry payments")
            && screen.contains("I:ok")
            && screen.contains("T:-")
            && screen.contains("R:-"),
        "{screen}"
    );
    assert!(screen.contains("0/2 met"), "{screen}");

    // Editing the code makes the earlier claim stale.
    std::fs::write(
        fx.dir.path().join("src/main.rs"),
        "fn main() {\n    run();\n    more();\n}\n\nfn run() {}\n",
    )
    .unwrap();
    let screen = fx.draw().join("\n");
    assert!(screen.contains("I:stale"), "{screen}");
    let _ = card;
}

#[test]
fn header_shows_what_changed_since_the_review() {
    let mut fx = fixture(170, 30);
    let ctx = fx.app.ctx.clone();
    rekon_core::review::mark_file(&ctx, "src/main.rs").unwrap();
    std::fs::write(
        fx.dir.path().join("src/main.rs"),
        "fn main() {\n    run();\n    more();\n}\n\nfn run() {}\n",
    )
    .unwrap();
    fx.app.refresh_changes();
    let screen = fx.draw().join("\n");
    assert!(screen.contains("Δ1 since review"), "{screen}");
    let _ = EditRequest {
        path: String::new(),
        abs: Default::default(),
        line: 0,
        hash_before: None,
        followup: None,
    };
}

/// Prints the screens of the insight layer: `cargo test -p rekon print_insight_screens -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_insight_screens() {
    let mut fx = fixture(150, 34);
    let ctx = fx.app.ctx.clone();
    let main = id_of(&fx, "src/main.rs", "main");
    let mut d = Decision::new("Entry point stays tiny", "main only wires things together");
    d.context = "The team wants the startup path readable in one screen".into();
    d.cost = "Logic lives in other modules, one more hop".into();
    d.revisit_if = "Startup needs branching per platform".into();
    d.assumptions = vec!["Only one binary".into()];
    d.elements = vec![main.clone()];
    decisions::add(&ctx, d).unwrap();
    decisions::reconstruct(&ctx, &main).unwrap();
    fx.app.goto_element(&main);
    fx.draw();
    fx.app.set_detail(Detail::Why);
    println!("{}", fx.draw().join("\n"));
    fx.app.set_detail(Detail::Relations);
    println!("{}", fx.draw().join("\n"));
    fx.app.focus = Focus::Code;
    fx.app.set_detail(Detail::Contract);
    fx.key(KeyCode::Char('d'));
    fx.wait_jobs();
    println!("{}", fx.draw().join("\n"));
    fx.key(KeyCode::Char('4'));
    fx.key(KeyCode::Char('5'));
    fx.key(KeyCode::Char('6'));
    println!("{}", fx.draw().join("\n"));
}
