//! Drawing: header, tree and code panels, footer, overview and help windows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use rekon_core::text;

use super::app::{App, Focus, Popup};
use super::rows::{DIM, Row};

const TREE_PERCENT: u16 = 45;
const FOCUS_BORDER: Style = Style::new().fg(Color::Cyan);
const SEL_FOCUSED: Style = Style::new().bg(Color::Indexed(24));
const SEL_UNFOCUSED: Style = Style::new().bg(Color::Indexed(237));

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(5)]).areas(f.area());

    let (tree_area, code_area) = if app.wide {
        (body, Rect::default())
    } else {
        let [t, c] = Layout::horizontal([Constraint::Percentage(TREE_PERCENT), Constraint::Min(10)]).areas(body);
        (t, c)
    };
    let tree_block = panel_block(text::TREE_TITLE.to_string(), app.focus == Focus::Tree);
    let code_title = code_title(app);
    let code_block = panel_block(code_title, app.focus == Focus::Code);
    app.tree_panel.area = tree_block.inner(tree_area);
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
            let lines = text::HELP
                .iter()
                .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<12}"), FOCUS_BORDER), Span::raw(*d)]))
                .collect();
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

fn code_title(app: &mut App) -> String {
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

/// Draws the visible slice of rows and highlights the selected one.
pub fn draw_rows(f: &mut Frame, rows: &[Row], area: Rect, offset: usize, sel: usize, focused: bool) {
    let visible: Vec<Line> = rows
        .iter()
        .skip(offset)
        .take(area.height as usize)
        .map(|r| r.line.clone())
        .collect();
    f.render_widget(Paragraph::new(visible), area);
    if sel >= offset && sel < offset + area.height as usize && sel < rows.len() {
        let y = area.y + (sel - offset) as u16;
        let rect = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
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
    draw_rows(
        f,
        &app.code_rows,
        area,
        app.code_panel.offset,
        app.code_panel.sel,
        app.focus == Focus::Code,
    );
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
