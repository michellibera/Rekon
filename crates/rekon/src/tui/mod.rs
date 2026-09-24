//! Terminal UI: tree of the repository on the left, code split into blocks on the right.

mod app;
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
    let mut terminal = enter()?;
    let result = event_loop(&mut terminal, &mut app);
    leave();
    result
}

/// Raw mode, alternate screen and mouse capture; restored on panic too.
fn enter() -> Result<Term> {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        original(info);
    }));
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
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
