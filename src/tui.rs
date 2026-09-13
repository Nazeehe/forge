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
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableBracketedPaste,
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
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
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
/// this fails cleanly instead of hanging. Invalid permission patterns fall
/// back to Off (ask everything) rather than blocking startup.
pub fn run(
    state: &mut AppState,
    permission: &crate::config::PermissionConfig,
    audit_path: &std::path::Path,
) -> i32 {
    let _guard = match TerminalGuard::setup() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    install_panic_hook();
    let mut policy = match crate::policy::Policy::new(
        permission.mode.clone(),
        &permission.allow,
        &permission.block,
    ) {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("warning: bad permission pattern ({e}); asking everything");
            crate::policy::Policy::new(crate::config::PermissionMode::Off, &[], &[])
                .expect("empty patterns compile")
        }
    };
    let audit_path = audit_path.to_path_buf();
    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel();
    // The IPC listener is fail-soft: without it, hook relays simply find
    // no endpoint and exit zero. Children inherit the endpoint by env.
    let _ipc = match crate::listener::spawn_all(ipc_tx) {
        Ok(spawned) => {
            std::env::set_var("FORGE_IPC_ENDPOINT", &spawned.sock_path);
            Some(spawned)
        }
        Err(e) => {
            eprintln!("warning: ipc listener unavailable: {e}");
            None
        }
    };
    let mut terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot open terminal: {e}");
            return 1;
        }
    };
    if let Err(e) = loop_until_quit(state, &mut terminal, ipc_rx, &mut policy, &audit_path) {
        eprintln!("error: main loop failed: {e}");
        return 1;
    }
    0
}

fn loop_until_quit(
    state: &mut AppState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ipc: std::sync::mpsc::Receiver<AppEvent>,
    policy: &mut crate::policy::Policy,
    audit_path: &std::path::Path,
) -> io::Result<()> {
    let mut router = InputRouter::new();
    let size = terminal.size()?;
    state.apply(AppEvent::Resize(size.height, size.width));
    fit_active_pane(state);
    let mut cursor_shown = true;
    while !state.should_quit {
        if event::poll(Duration::from_millis(TICK_MS))? {
            match event::read()? {
                event::Event::Key(key) => {
                    if state.modal.is_some() {
                        let outcome = state.modal.as_mut().map(|m| m.modal.key(&key));
                        match outcome {
                            Some(crate::modal::ModalOutcome::Decided { allow }) => {
                                state.decide_modal(allow, policy, audit_path);
                            }
                            _ => {
                                state.dirty = true;
                            }
                        }
                    } else {
                        handle_key(state, &mut router, key);
                    }
                }
                event::Event::Mouse(mev) => forward_mouse(state, mev, policy, audit_path),
                event::Event::Paste(text) => {
                    if state.modal.is_none() {
                        if let Some(active) = state.manager.active() {
                            let bracketed = state.manager.bracketed_paste(active);
                            let bytes = input::paste_bytes(&text, bracketed);
                            let _ = state.manager.pane_write(active, &bytes);
                        }
                    }
                }
                event::Event::Resize(cols, rows) => {
                    state.apply(AppEvent::Resize(rows, cols));
                    fit_active_pane(state);
                }
                _ => {}
            }
        }
        for (id, ev) in state.manager.drain_pty_max(MAX_DRAIN) {
            state.apply(AppEvent::from_pty(id, ev));
        }
        for ev in ipc.try_iter().take(MAX_DRAIN) {
            state.apply(ev);
        }
        state.settle_hooks(policy, audit_path);
        state.open_modal_if_needed();
        if state.dirty {
            let views = state.views();
            let status = state.status_text();
            let chrome = ui::Chrome {
                tabs: state.tabs(),
                pending: state.pending_hooks.len(),
                mode: policy.mode().as_str(),
                status,
            };
            let cursor_visible =
                state.modal.is_none() && views.iter().any(|v| v.focused && v.cursor.is_some());
            terminal.draw(|f| {
                let area = f.area();
                ui::render(f, area, &views, &chrome);
                if let Some(active) = state.modal.as_mut() {
                    active.modal.view(f, crate::modal::modal_area(area));
                }
            })?;
            if cursor_visible != cursor_shown {
                if cursor_visible {
                    terminal.show_cursor()?;
                } else {
                    terminal.hide_cursor()?;
                }
                cursor_shown = cursor_visible;
            }
            state.dirty = false;
        }
    }
    Ok(())
}

fn handle_key(state: &mut AppState, router: &mut InputRouter, key: event::KeyEvent) {
    match router.feed(key) {
        RoutedKey::Forward(k) => {
            if let Some(active) = state.manager.active() {
                let app_cursor = state.manager.app_cursor(active);
                if let Some(bytes) = input::encode_key(&k, app_cursor) {
                    let _ = state.manager.pane_write(active, &bytes);
                }
            }
        }
        RoutedKey::Command(cmd) => match cmd {
            UserCommand::Quit => state.should_quit = true,
            UserCommand::NextSession => {
                state.step_session(1);
                fit_active_pane(state);
                state.dirty = true;
            }
            UserCommand::PrevSession => {
                state.step_session(-1);
                fit_active_pane(state);
                state.dirty = true;
            }
            UserCommand::NewSession => {
                spawn_shell(state);
                state.dirty = true;
            }
            UserCommand::SelectSession(index) => {
                if state.select_session(index) {
                    fit_active_pane(state);
                }
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
    spawn_shell_cmd(state, &format!("exec {shell} -i"));
}

fn spawn_shell_cmd(state: &mut AppState, cmd: &str) {
    if let Ok(cwd) = std::env::current_dir() {
        let n = state.manager.len() + 1;
        // Spawn failures have no modal surface yet (Phase 3); the grid
        // simply shows no new pane.
        let _ = state.manager.spawn(
            &format!("shell-{n}"),
            &cwd,
            cmd,
            RunId::generate(),
        );
        // A first/new active pane takes the main area at once instead of
        // keeping the hardcoded 24x80 until the next outer resize.
        // Background panes keep their size until focused.
        fit_active_pane(state);
    }
}

/// Route an outer mouse event: session-bar clicks switch sessions, the
/// main area forwards to the active pane when it wants mouse reporting,
/// and everything else (sidebar, status bar, borders) is chrome-owned.
fn forward_mouse(
    state: &mut AppState,
    mev: event::MouseEvent,
    policy: &mut crate::policy::Policy,
    audit_path: &std::path::Path,
) {
    // An open modal swallows all mouse input; clicks on its choice rows
    // decide it, everything else is ignored.
    if state.modal.is_some() {
        if matches!(mev.kind, event::MouseEventKind::Down(_)) {
            let (rows, cols) = state.term_size;
            let marea = crate::modal::modal_area(ratatui::layout::Rect::new(0, 0, cols, rows));
            if let Some((list_y, list_h)) = crate::modal::list_rows(marea) {
                if mev.row >= list_y
                    && mev.row < list_y + list_h
                    && mev.column >= marea.x
                    && mev.column < marea.x + marea.width
                {
                    let outcome = state
                        .modal
                        .as_mut()
                        .map(|m| m.modal.click((mev.row - list_y) as usize));
                    if let Some(crate::modal::ModalOutcome::Decided { allow }) = outcome {
                        state.decide_modal(allow, policy, audit_path);
                    } else {
                        state.dirty = true;
                    }
                }
            }
        }
        return;
    }
    let (rows, cols) = state.term_size;
    let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    if mev.row >= areas.session_bar.y
        && mev.row < areas.session_bar.y + areas.session_bar.height
        && areas.session_bar.height > 0
    {
        let titles: Vec<String> = state.tabs().into_iter().map(|t| t.title).collect();
        let buttons = ui::layout_session_bar(areas.session_bar, &titles);
        if let Some(index) = ui::session_at(&buttons, mev.column) {
            if state.select_session(index) {
                fit_active_pane(state);
            }
            state.dirty = true;
        }
        return;
    }
    let Some(active) = state.manager.active() else {
        return;
    };
    let mode = state.manager.mouse_mode(active);
    if mode == vt100::MouseProtocolMode::None {
        return;
    }
    let Some((col, row)) = ui::translate_mouse(areas.main, mev.column, mev.row) else {
        return;
    };
    let pev = event::MouseEvent {
        column: col,
        row,
        ..mev
    };
    if let Some(bytes) = input::encode_mouse(&pev, mode, state.manager.mouse_encoding(active)) {
        let _ = state.manager.pane_write(active, &bytes);
    }
}

/// Fit the active pane to the main area. Background panes keep their size
/// until focused, when they are fitted in turn.
fn fit_active_pane(state: &mut AppState) {
    let Some(active) = state.manager.active() else {
        return;
    };
    let (rows, cols) = state.term_size;
    let main = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows)).main;
    let pane_rows = main.height.saturating_sub(2).max(1);
    let pane_cols = main.width.saturating_sub(2).max(1);
    let _ = state.manager.resize(active, pane_rows, pane_cols);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_fits_main_pane() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 1);
        // 80x24 less session bar, status row, main-pane borders.
        assert_eq!(state.manager.pane_size(order[0]), Some((20, 62)));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 2);
        // Background panes keep spawn size until focused...
        assert_eq!(state.manager.pane_size(order[1]), Some((24, 80)));
        // ...then take the main area on selection.
        assert!(state.select_session(1));
        fit_active_pane(&mut state);
        assert_eq!(state.manager.pane_size(order[1]), Some((20, 62)));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn session_bar_click_switches_sessions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        // Session bar is row 22; second button starts at column 11.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 22,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let mut policy =
            crate::policy::Policy::new(crate::config::PermissionMode::Off, &[], &[]).unwrap();
        forward_mouse(&mut state, click, &mut policy, std::path::Path::new(""));
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[1]));
        assert_eq!(state.manager.pane_size(order[1]), Some((20, 62)));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn modal_click_decides_and_outside_click_ignored() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let audit = std::env::temp_dir().join(format!(
            "forge-modal-click-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy =
            crate::policy::Policy::new(crate::config::PermissionMode::Off, &[], &[]).unwrap();
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"doom"}}"#.to_string(),
            sync: true,
            reply: reply_tx,
        }));
        state.settle_hooks(&mut policy, &audit);
        assert!(state.open_modal_if_needed());
        let click_at = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        // Click far from the modal: swallowed, modal stays open.
        forward_mouse(&mut state, click_at(0, 0), &mut policy, &audit);
        assert!(state.modal.is_some());
        assert!(reply_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        // Click the Deny choice row: modal decides deny and closes.
        let marea = crate::modal::modal_area(ratatui::layout::Rect::new(0, 0, 80, 24));
        let (list_y, _) = crate::modal::list_rows(marea).unwrap();
        forward_mouse(
            &mut state,
            click_at(marea.x + 2, list_y + 1),
            &mut policy,
            &audit,
        );
        assert!(state.modal.is_none());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""decision":"deny""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1, "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

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
