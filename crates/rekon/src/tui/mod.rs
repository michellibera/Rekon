//! Terminal UI: tree of the repository on the left, code split into blocks on the right.

mod app;
mod editor;
mod highlight;
mod render;
mod rows;
#[cfg(test)]
mod tests;

use std::io::{Stdout, stdout};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use rekon_core::Ctx;

use app::App;

/// Input poll interval; background messages are handled between polls.
const POLL: Duration = Duration::from_millis(100);

type Term = Terminal<CrosstermBackend<Stdout>>;

pub fn run(ctx: Arc<Ctx>) -> Result<()> {
    let mut app = App::new(ctx)?;
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        original(info);
    }));
    enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let result = event_loop(&mut terminal, &mut app);
    leave();
    result
}

/// Raw mode, alternate screen and mouse capture (restored on panic by `run`).
fn enter() -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(())
}

fn leave() {
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn event_loop(terminal: &mut Term, app: &mut App) -> Result<()> {
    loop {
        app.poll();
        if app.dirty {
            app.dirty = false;
            terminal.draw(|f| render::draw(f, app))?;
        }
        if app.quit {
            return Ok(());
        }
        if let Some(req) = app.editor_request.take() {
            // The editor gets the real terminal; the TUI is redrawn from scratch after it.
            leave();
            let result = editor::run(app.ctx.config.editor_cmd.as_deref(), &req);
            enter()?;
            terminal.clear()?;
            app.after_edit(req, result);
            continue;
        }
        if event::poll(POLL)? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m),
                Event::Resize(..) => app.dirty = true,
                _ => {}
            }
        }
    }
}
