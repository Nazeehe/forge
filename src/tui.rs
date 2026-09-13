//! Terminal setup and the main event loop.
//!
//! Roughly 16 ms per iteration: poll input with a timeout, drain at most
//! 100 background events so floods cannot starve input, offer pending work,
//! and repaint only when dirty. Terminal state (raw mode, alternate screen)
//! is restored on normal exit, on panic, and on drop.

use std::io;
use std::time::Duration;

use crossterm::event;
use crossterm::tty::IsTty;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::AppState;
use crate::event::AppEvent;
use crate::ids::RunId;
use crate::input::{self, InputRouter, RoutedKey, UserCommand};
use crate::ui;

/// Milliseconds per main-loop iteration.
pub const TICK_MS: u64 = 16;

/// Background events drained per iteration (anti-starvation bound).
pub const MAX_DRAIN: usize = 100;

/// RAII terminal setup: raw mode plus the alternate screen while alive.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    pub fn setup() -> io::Result<Self> {
        if !io::stdout().is_tty() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "forge TUI requires a terminal",
            ));
        }
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            crossterm::terminal::EnterAlternateScreen
        )?;
        Ok(TerminalGuard { active: true })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            restore_terminal();
            self.active = false;
        }
    }
}

/// Leave raw mode and the alternate screen. Safe to call repeatedly and
/// when setup never ran; errors are swallowed by design.
pub fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen
    );
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Run the TUI until quit. Returns the process exit code. Without a terminal
/// this fails cleanly instead of hanging.
pub fn run(state: &mut AppState) -> i32 {
    let _guard = match TerminalGuard::setup() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    install_panic_hook();
    let mut terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot open terminal: {e}");
            return 1;
        }
    };
    if let Err(e) = loop_until_quit(state, &mut terminal) {
        eprintln!("error: main loop failed: {e}");
        return 1;
    }
    0
}

fn loop_until_quit(
    state: &mut AppState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
    let mut router = InputRouter::new();
    let size = terminal.size()?;
    state.apply(AppEvent::Resize(size.height, size.width));
    fit_panes(state, ratatui::layout::Rect::new(0, 0, size.width, size.height));
    while !state.should_quit {
        if event::poll(Duration::from_millis(TICK_MS))? {
            match event::read()? {
                event::Event::Key(key) => handle_key(state, &mut router, key),
                event::Event::Resize(cols, rows) => {
                    state.apply(AppEvent::Resize(rows, cols));
                    fit_panes(state, ratatui::layout::Rect::new(0, 0, cols, rows));
                }
                _ => {}
            }
        }
        for (id, ev) in state.manager.drain_pty_max(MAX_DRAIN) {
            state.apply(AppEvent::from_pty(id, ev));
        }
        if state.dirty {
            let views = state.views();
            let status = state.status_text();
            terminal.draw(|f| {
                let area = f.area();
                ui::render_grid(f, area, &views, &status);
            })?;
            state.dirty = false;
        }
    }
    Ok(())
}

fn handle_key(state: &mut AppState, router: &mut InputRouter, key: event::KeyEvent) {
    match router.feed(key) {
        RoutedKey::Forward(k) => {
            if let Some(active) = state.manager.active() {
                if let Some(bytes) = input::encode_key(&k) {
                    let _ = state.manager.pane_write(active, &bytes);
                }
            }
        }
        RoutedKey::Command(cmd) => match cmd {
            UserCommand::Quit => state.should_quit = true,
            UserCommand::NextSession => {
                state.step_session(1);
                state.dirty = true;
            }
            UserCommand::PrevSession => {
                state.step_session(-1);
                state.dirty = true;
            }
            UserCommand::NewSession => {
                spawn_shell(state);
                state.dirty = true;
            }
        },
        RoutedKey::PrefixPending | RoutedKey::Cancelled => {
            state.dirty = true;
        }
    }
}

fn spawn_shell(state: &mut AppState) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    if let Ok(cwd) = std::env::current_dir() {
        let n = state.manager.len() + 1;
        // Spawn failures have no modal surface yet (Phase 3); the grid
        // simply shows no new pane.
        let _ = state.manager.spawn(
            &format!("shell-{n}"),
            &cwd,
            &format!("exec {shell} -i"),
            RunId::generate(),
        );
    }
}

fn fit_panes(state: &mut AppState, term: ratatui::layout::Rect) {
    let areas = ui::grid_areas(state.manager.len(), term);
    for (id, area) in state.manager.order().to_vec().into_iter().zip(areas.iter()) {
        let rows = area.height.saturating_sub(2).max(1);
        let cols = area.width.saturating_sub(2).max(1);
        let _ = state.manager.resize(id, rows, cols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_constants_match_blueprint() {
        assert_eq!(TICK_MS, 16);
        assert_eq!(MAX_DRAIN, 100);
    }

    #[test]
    fn setup_fails_cleanly_headless() {
        // Test harnesses run piped, never on a TTY: setup must Err, not hang.
        assert!(TerminalGuard::setup().is_err());
    }

    #[test]
    fn restore_is_idempotent_and_safe_headless() {
        restore_terminal();
        restore_terminal();
        drop(TerminalGuard { active: false });
    }
}
