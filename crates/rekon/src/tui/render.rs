//! Drawing: header, tree and code panels, footer, overview and help windows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use rekon_core::text;

use super::app::{App, Detail, Focus, Popup, View};
use super::rows::{self, DIM, Row, RowRef, WARN};

const TREE_PERCENT: u16 = 45;
const FOCUS_BORDER: Style = Style::new().fg(Color::Cyan);
const SEL_FOCUSED: Style = Style::new().bg(Color::Rgb(26, 50, 74));
const SEL_UNFOCUSED: Style = Style::new().bg(Color::Rgb(46, 48, 54));
/// Lines of the evidence shown for a change.
const EVIDENCE: Style = Style::new().bg(Color::Rgb(66, 56, 18));
const TAB_ACTIVE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(5)]).areas(f.area());

    let (tree_area, code_area) = if app.wide {
        (body, Rect::default())
    } else {
        let [t, c] = Layout::horizontal([Constraint::Percentage(TREE_PERCENT), Constraint::Min(10)]).areas(body);
        (t, c)
    };
    let (tree_block, tabs) = left_block(app, tree_area);
    app.tabs = tabs;
    let code_title = code_title(app);
    let (code_block, detail_tabs) = right_block(app, code_title, code_area);
    app.ins.tabs = detail_tabs;
    app.tree_panel.area = tree_block.inner(tree_area);
    app.code_panel.area = if app.wide {
        Rect::default()
    } else {
        code_block.inner(code_area)
    };
    app.ins.area = app.code_panel.area;

    app.rebuild(app.tree_panel.area.width as usize, app.code_panel.area.width as usize);
    let (tree_len, code_len) = (app.tree_rows.len(), app.code_rows.len());
    app.tree_panel.scroll_to_selection(tree_len, app.tree_panel.sel + 1);
    let code_end = super::rows::selection_end(&app.code_rows, app.code_panel.sel);
    app.code_panel.scroll_to_selection(code_len, code_end);

    draw_header(f, app, header);
    f.render_widget(tree_block, tree_area);
    draw_rows(
        f,
        &app.tree_rows,
        app.tree_panel.area,
        app.tree_panel.offset,
        app.tree_panel.sel,
        app.focus == Focus::Tree,
    );
    if !app.wide {
        f.render_widget(code_block, code_area);
        if app.ins.detail == Detail::Code {
            draw_code(f, app);
        } else {
            draw_inspector(f, app);
        }
    }
    draw_footer(f, app, footer);

    match app.popup {
        Some(Popup::Overview(scroll)) => {
            let text = app.project_overview().unwrap_or_else(|| text::NO_OVERVIEW.to_string());
            let summary = app.project_summary();
            let mut lines = Vec::new();
            if let Some(s) = summary {
                lines.push(Line::from(Span::styled(s, Style::new().add_modifier(Modifier::BOLD))));
                lines.push(Line::default());
            }
            lines.extend(text.lines().map(|l| Line::from(l.to_string())));
            popup(f, text::OVERVIEW_TITLE, lines, scroll);
        }
        Some(Popup::Text(scroll)) => {
            let lines: Vec<Line> = app.ins.text_body.lines().map(|l| Line::from(l.to_string())).collect();
            let title = app.ins.text_title.clone();
            popup(f, &title, lines, scroll);
        }
        Some(Popup::Input) => {
            if let Some(input) = &app.ins.input {
                input_popup(f, input.kind.title(), &input.text);
            }
        }
        Some(Popup::Help) => {
            let key = |(k, d): &(&str, &'static str)| {
                Line::from(vec![Span::styled(format!("{k:<14}"), FOCUS_BORDER), Span::raw(*d)])
            };
            let lines: Vec<Line> = text::HELP.iter().map(key).collect();
            popup(f, text::HELP_TITLE, lines, 0);
        }
        None => {}
    }
}

/// Border of the right panel: the tabs of the inspector (with their areas for clicks), the
/// title of what is shown and, at the bottom, the keys of the current tab.
fn right_block(app: &App, title: String, area: Rect) -> (Block<'static>, Vec<(Rect, Detail)>) {
    let mut spans = vec![Span::raw(" ")];
    let mut tabs = Vec::new();
    let y = area.y + area.height.saturating_sub(1);
    let mut x = area.x + 2;
    for (i, d) in Detail::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", DIM));
            x += 3;
        }
        let style = if app.ins.detail == d { TAB_ACTIVE } else { DIM };
        let span = Span::styled(d.label().to_string(), style);
        let w = span.width() as u16;
        tabs.push((Rect::new(x, y, w, 1), d));
        x += w;
        spans.push(span);
    }
    let hint = format!(" · {} ", app.detail_hint());
    if x as usize + hint.chars().count() < (area.x + area.width) as usize {
        spans.push(Span::styled(hint, DIM));
    }
    let block = Block::bordered()
        .title(format!(" {title} "))
        .title_bottom(Line::from(spans))
        .border_style(if app.focus == Focus::Code { FOCUS_BORDER } else { Style::new() });
    (block, tabs)
}

/// The inspector: relations, why, contract, example run or comparison of the subject.
fn draw_inspector(f: &mut Frame, app: &App) {
    let area = app.ins.area;
    let visible: Vec<Line> = app
        .ins
        .items
        .iter()
        .skip(app.ins.scroll)
        .take(area.height as usize)
        .map(|i| i.line.clone())
        .collect();
    f.render_widget(Paragraph::new(visible), area);
    if let Some(item) = app.ins.items.get(app.ins.cursor)
        && item.nav.is_some()
        && app.ins.cursor >= app.ins.scroll
        && app.ins.cursor < app.ins.scroll + area.height as usize
    {
        let rect = Rect::new(area.x, area.y + (app.ins.cursor - app.ins.scroll) as u16, area.width, 1);
        f.buffer_mut()
            .set_style(rect, if app.focus == Focus::Code { SEL_FOCUSED } else { SEL_UNFOCUSED });
    }
}

/// One-line input in the lower part of the screen.
fn input_popup(f: &mut Frame, title: &str, text: &str) {
    let area = f.area();
    let w = (area.width * 7 / 10).max(30).min(area.width);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + area.height * 2 / 3,
        width: w,
        height: 3.min(area.height),
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered().title(format!(" {title} · Enter saves, Esc cancels ")).border_style(FOCUS_BORDER);
    let shown: String = {
        let keep = (w as usize).saturating_sub(4);
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len().saturating_sub(keep)..].iter().collect()
    };
    f.render_widget(Paragraph::new(format!("{shown}█")).block(block), rect);
}

/// Border of the left panel: tabs `1 Tree │ 2 Changes │ ...` (with their areas for
/// clicks).
fn left_block(app: &App, area: Rect) -> (Block<'static>, Vec<(Rect, View)>) {
    let entries = [
        ("1", text::TAB_TREE, View::Tree),
        ("2", text::TAB_CHANGES, View::Changes),
        ("3", text::TAB_WHY, View::Decisions),
        ("4", text::TAB_CARDS, View::Cards),
        ("5", text::TAB_CRITERIA, View::Criteria),
        ("6", text::TAB_ASK, View::Ask),
    ];
    let mut spans = vec![Span::raw(" ")];
    let mut tabs = Vec::new();
    let mut x = area.x + 1 + 1;
    for (i, (key, name, view)) in entries.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", DIM));
            x += 3;
        }
        let style = if app.view == view { TAB_ACTIVE } else { DIM };
        let span = Span::styled(format!("{key} {name}"), style);
        let w = span.width() as u16;
        tabs.push((Rect::new(x, area.y, w, 1), view));
        x += w;
        spans.push(span);
    }
    spans.push(Span::raw(" "));
    let focused = app.focus == Focus::Tree;
    let block = Block::bordered()
        .title(Line::from(spans))
        .border_style(if focused { FOCUS_BORDER } else { Style::new() });
    (block, tabs)
}

fn code_title(app: &mut App) -> String {
    if app.evidence_lines().is_some()
        && let Some(ev) = &app.evidence
    {
        let e = ev.current();
        return text::evidence_title(&ev.title, ev.index + 1, ev.items.len(), &e.location(), &e.reason);
    }
    let status = app.blocks_status();
    match (&app.open, status) {
        (None, _) => text::CODE_TITLE_EMPTY.to_string(),
        (Some(open), None) => open.path.clone(),
        (Some(open), Some(s)) => format!("{} — {s}", open.path),
    }
}

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let summary = app.project_summary();
    let mut spans = vec![
        Span::styled("rekon", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" · "),
        Span::styled(app.repo_name(), Style::new().add_modifier(Modifier::BOLD)),
    ];
    if let Some(s) = summary {
        spans.push(Span::raw(" — "));
        spans.push(Span::styled(s, DIM));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    // Right side: what needs attention, always visible.
    let mut badges: Vec<Span> = Vec::new();
    let changed = app
        .changes
        .iter()
        .filter(|c| !matches!(c.status, rekon_core::review::Status::FormatOnly))
        .count();
    if changed > 0 {
        badges.push(Span::styled(format!("Δ{changed} since review"), WARN));
    }
    let open = app.ins.decisions.iter().filter(|d| d.status == rekon_core::decisions::Status::Proposed).count();
    if open > 0 {
        badges.push(Span::styled(format!("{open} unconfirmed decision(s)"), WARN));
    }
    if !app.ins.pins.is_empty() {
        badges.push(Span::styled(format!("pinned {}", app.ins.pins.len()), DIM));
    }
    if !badges.is_empty() {
        let mut line: Vec<Span> = Vec::new();
        for (i, b) in badges.into_iter().enumerate() {
            if i > 0 {
                line.push(Span::styled("  ·  ", DIM));
            }
            line.push(b);
        }
        f.render_widget(Paragraph::new(Line::from(line)).right_aligned(), area);
    }
}

/// Draws the visible slice of rows and highlights the selected one, together with
/// everything a selected block contains.
pub fn draw_rows(f: &mut Frame, rows: &[Row], area: Rect, offset: usize, sel: usize, focused: bool) {
    let visible: Vec<Line> = rows
        .iter()
        .skip(offset)
        .take(area.height as usize)
        .map(|r| r.line.clone())
        .collect();
    f.render_widget(Paragraph::new(visible), area);
    if sel >= rows.len() {
        return;
    }
    let end = rows::selection_end(rows, sel);
    let first = sel.max(offset);
    let last = end.min(offset + area.height as usize);
    if first < last {
        let rect = Rect {
            x: area.x,
            y: area.y + (first - offset) as u16,
            width: area.width,
            height: (last - first) as u16,
        };
        f.buffer_mut()
            .set_style(rect, if focused { SEL_FOCUSED } else { SEL_UNFOCUSED });
    }
}

fn draw_code(f: &mut Frame, app: &App) {
    let area = app.code_panel.area;
    let message = match &app.open {
        None => Some(text::SELECT_FILE.to_string()),
        Some(open) => open.problem.clone(),
    };
    if let Some(m) = message {
        f.render_widget(Paragraph::new(Span::styled(m, DIM)).wrap(Wrap { trim: true }), area);
        return;
    }
    let focused = app.focus == Focus::Code;
    let evidence = app.evidence_lines();
    // Evidence lines stand out on their own; the selection shows only when focused.
    let sel = if evidence.is_some() && !focused {
        usize::MAX
    } else {
        app.code_panel.sel
    };
    draw_rows(f, &app.code_rows, area, app.code_panel.offset, sel, focused);
    if let Some((a, b)) = evidence {
        let visible = app.code_rows.iter().enumerate().skip(app.code_panel.offset);
        for (k, row) in visible.take(usize::from(area.height)) {
            if let RowRef::Line(nr) = row.target
                && nr >= a
                && nr <= b
                && !(k == sel && focused)
            {
                let y = area.y + (k - app.code_panel.offset) as u16;
                f.buffer_mut().set_style(Rect::new(area.x, y, area.width, 1), EVIDENCE);
            }
        }
    }
}

fn draw_footer(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [desc, status] = Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).areas(inner);
    let description = app.selected_description();
    f.render_widget(Paragraph::new(description).wrap(Wrap { trim: true }), desc);
    let mut line = text::status_line(
        app.jobs_running(),
        app.errors,
        app.last_error.as_deref(),
        app.ctx.cost.usd(),
    );
    if let Some(p) = &app.init {
        line = format!("init: {} · {line}", p.line());
    }
    let hint = app.context_hint();
    if !hint.is_empty() {
        line = format!("{hint} · {line}");
    }
    if let Some(n) = &app.notice {
        line = format!("{n} · {line}");
    }
    f.render_widget(Paragraph::new(Span::styled(line, DIM)), status);
}

fn popup(f: &mut Frame, title: &str, lines: Vec<Line<'static>>, scroll: u16) {
    let area = f.area();
    let w = (area.width * 4 / 5).max(20).min(area.width);
    let h = (area.height * 4 / 5).max(5).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered().title(format!(" {title} ")).border_style(FOCUS_BORDER);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        rect,
    );
}
