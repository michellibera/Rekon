//! Rows and texts of the "changes since your review" view.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rekon_core::model::Stamped;
use rekon_core::review::{Change, Status, Update};
use unicode_width::UnicodeWidthStr;

use super::rows::{DIM, Row, RowKind, RowRef, WARN, fit};

fn glyph(status: &Status) -> (&'static str, Style) {
    match status {
        Status::Changed => ("~", WARN),
        Status::CallerChanged => ("↑", Style::new().fg(Color::Magenta)),
        Status::Renamed(_) => ("→", Style::new().fg(Color::Cyan)),
        Status::Added => ("+", Style::new().fg(Color::Green)),
        Status::Removed => ("-", Style::new().fg(Color::Red)),
        Status::FormatOnly | Status::Unchanged => ("·", DIM),
    }
}

/// `~ name  path:line  status`.
pub fn row(c: &Change, width: usize) -> Row {
    let (mark, style) = glyph(&c.status);
    let name = fit(&c.name, width.saturating_sub(2));
    let rest = width.saturating_sub(name.width() + 4);
    let place = if c.start > 0 {
        format!("{}:{}  {}", c.path, c.start, c.status.label())
    } else {
        format!("{}  {}", c.path, c.status.label())
    };
    let mut spans = vec![Span::styled(mark, style), Span::raw(" "), Span::raw(name)];
    if rest > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(fit(&place, rest), DIM));
    }
    Row {
        depth: 0,
        kind: RowKind::File,
        target: RowRef::Node(c.id.clone()),
        line: Line::from(spans),
    }
}

/// A row that only carries a message (nothing to compare yet).
pub fn message_row(text: &str) -> Row {
    Row {
        depth: 0,
        kind: RowKind::Spacer,
        target: RowRef::Line(0),
        line: Line::from(Span::styled(text.to_string(), DIM.add_modifier(Modifier::ITALIC))),
    }
}

/// Footer text of a change: what it is, what it means, and the model's update if there is one.
pub fn description(c: &Change, update: Option<&Stamped<Update>>) -> String {
    let mut out = format!("{} · {}", c.status.label(), c.id);
    match &c.status {
        Status::Renamed(to) => out.push_str(&format!(" → {to}")),
        Status::CallerChanged => out.push_str(&format!(" — changed callers: {}", c.callers.join(", "))),
        Status::FormatOnly => out.push_str(" — whitespace or comments only, a skim is enough"),
        Status::Changed if update.is_none() => out.push_str(" — u: ask the model what changed for you"),
        _ => {}
    }
    if let Some(u) = update {
        let v = &u.value;
        out.push_str(&format!(
            " — [{}, {}] Until now: {} Now: {}",
            u.origin.label(),
            v.effort,
            v.assumed,
            v.now
        ));
        for p in &v.points {
            out.push_str(&format!(" · L{}: {}", p.line, p.text));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: Status) -> Change {
        Change {
            id: "a.rs::a::run".into(),
            path: "a.rs".into(),
            name: "run".into(),
            kind: "fn".into(),
            status,
            start: 3,
            end: 9,
            snapshot: None,
            new_hash: None,
            callers: vec!["a.rs::a::go".into()],
        }
    }

    #[test]
    fn row_points_at_the_change() {
        let r = row(&change(Status::Changed), 40);
        assert_eq!(r.target, RowRef::Node("a.rs::a::run".into()));
        let text: String = r.line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("~ run"), "{text}");
        assert!(text.contains("a.rs:3"), "{text}");
    }

    #[test]
    fn description_names_callers_and_marks_origin() {
        assert!(description(&change(Status::CallerChanged), None).contains("a.rs::a::go"));
        let u = Stamped {
            hash: "h".into(),
            origin: rekon_core::model::Origin::Ai,
            value: Update {
                assumed: "x".into(),
                now: "y".into(),
                points: vec![],
                effort: "review".into(),
            },
        };
        assert!(description(&change(Status::Changed), Some(&u)).contains("[AI, review]"));
    }
}
