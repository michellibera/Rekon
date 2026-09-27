//! Drawing: header, tree and code panels, footer, overview and help windows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use rekon_core::text;

use super::app::{App, Focus, Popup, View};
use super::graph;
use super::rows::{self, DIM, Row, RowRef};

const TREE_PERCENT: u16 = 45;
/// The graph needs more room than the tree.
const GRAPH_PERCENT: u16 = 55;
const FOCUS_BORDER: Style = Style::new().fg(Color::Cyan);
const SEL_FOCUSED: Style = Style::new().bg(Color::Rgb(26, 50, 74));
const SEL_UNFOCUSED: Style = Style::new().bg(Color::Rgb(46, 48, 54));
/// Lines of the evidence shown for a graph element.
const EVIDENCE: Style = Style::new().bg(Color::Rgb(66, 56, 18));
const TAB_ACTIVE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(5)]).areas(f.area());

    let (tree_area, code_area) = if app.wide {
        (body, Rect::default())
    } else {
        let percent = if app.view == View::Ontology {
            GRAPH_PERCENT
        } else {
            TREE_PERCENT
        };
        let [t, c] = Layout::horizontal([Constraint::Percentage(percent), Constraint::Min(10)]).areas(body);
        (t, c)
    };
    let (tree_block, tabs) = left_block(app, tree_area);
    app.tabs = tabs;
    let code_title = code_title(app);
    let code_block = panel_block(code_title, app.focus == Focus::Code);
    match app.view {
        View::Tree => app.tree_panel.area = tree_block.inner(tree_area),
        View::Ontology => {
            app.tree_panel.area = Rect::default();
            app.explorer.area = tree_block.inner(tree_area);
        }
    }
    app.code_panel.area = if app.wide {
        Rect::default()
    } else {
        code_block.inner(code_area)
    };

    app.rebuild(app.tree_panel.area.width as usize, app.code_panel.area.width as usize);
    let (tree_len, code_len) = (app.tree_rows.len(), app.code_rows.len());
    app.tree_panel.scroll_to_selection(tree_len);
    app.code_panel.scroll_to_selection(code_len);

    draw_header(f, app, header);
    f.render_widget(tree_block, tree_area);
    match app.view {
        View::Tree => draw_rows(
            f,
            &app.tree_rows,
            app.tree_panel.area,
            app.tree_panel.offset,
            app.tree_panel.sel,
            app.focus == Focus::Tree,
        ),
        View::Ontology => {
            let message = app.explorer.error.clone().unwrap_or_else(|| {
                if app.ontology.is_some() {
                    text::ONTOLOGY_RUNNING.to_string()
                } else {
                    text::NO_ONTOLOGY.to_string()
                }
            });
            graph::draw(f, &mut app.explorer, app.focus == Focus::Tree, Some(&message));
        }
    }
    if !app.wide {
        f.render_widget(code_block, code_area);
        draw_code(f, app);
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
        Some(Popup::Help) => {
            let key = |(k, d): &(&str, &'static str)| {
                Line::from(vec![Span::styled(format!("{k:<14}"), FOCUS_BORDER), Span::raw(*d)])
            };
            let mut lines: Vec<Line> = text::HELP.iter().map(key).collect();
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                text::HELP_ONTOLOGY_TITLE,
                Style::new().add_modifier(Modifier::BOLD),
            )));
            lines.extend(text::HELP_ONTOLOGY.iter().map(key));
            popup(f, text::HELP_TITLE, lines, 0);
        }
        None => {}
    }
}

fn panel_block(title: String, focused: bool) -> Block<'static> {
    Block::bordered()
        .title(format!(" {title} "))
        .border_style(if focused { FOCUS_BORDER } else { Style::new() })
}

/// Border of the left panel: tabs `1 Tree │ 2 Ontology` (with their areas for
/// clicks) and, over the graph, its size.
fn left_block(app: &App, area: Rect) -> (Block<'static>, Vec<(Rect, View)>) {
    let tab = |key: &str, name: &str, view: View| {
        let style = if app.view == view { TAB_ACTIVE } else { DIM };
        Span::styled(format!("{key} {name}"), style)
    };
    let spans = vec![
        Span::raw(" "),
        tab("1", text::TAB_TREE, View::Tree),
        Span::styled(" │ ", DIM),
        tab("2", text::TAB_ONTOLOGY, View::Ontology),
        Span::raw(" "),
    ];
    let mut tabs = Vec::new();
    let mut x = area.x + 1;
    for (i, s) in spans.iter().enumerate() {
        let w = s.width() as u16;
        match i {
            1 => tabs.push((Rect::new(x, area.y, w, 1), View::Tree)),
            3 => tabs.push((Rect::new(x, area.y, w, 1), View::Ontology)),
            _ => {}
        }
        x += w;
    }
    let focused = app.focus == Focus::Tree;
    let mut block =
        Block::bordered()
            .title(Line::from(spans))
            .border_style(if focused { FOCUS_BORDER } else { Style::new() });
    if app.view == View::Ontology
        && let Some(g) = &app.explorer.graph
    {
        let size = format!(" {} nodes · {} edges ", g.graph.nodes.len(), g.graph.edges.len());
        block = block.title_top(Line::from(Span::styled(size, DIM)).right_aligned());
    }
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
    if let Some(p) = &app.ontology {
        line = format!("{} · {line}", p.line());
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
