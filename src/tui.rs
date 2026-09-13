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
/// back to Off (every ask goes back to the harness) rather than blocking
/// startup.
pub fn run(
    state: &mut AppState,
    loaded: &mut crate::config::LoadedConfig,
    home: &std::path::Path,
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
    state.permission_mode = loaded.config.permission.mode.clone();
    let mut policy = match crate::policy::Policy::new(
        loaded.config.permission.mode.clone(),
        &loaded.config.permission.allow,
        &loaded.config.permission.block,
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
    if let Err(e) = loop_until_quit(
        state,
        &mut terminal,
        ipc_rx,
        &mut policy,
        &audit_path,
        loaded,
        home,
    ) {
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
    loaded: &mut crate::config::LoadedConfig,
    home: &std::path::Path,
) -> io::Result<()> {
    let mut router = InputRouter::new();
    let size = terminal.size()?;
    state.apply(AppEvent::Resize(size.height, size.width));
    fit_active_pane(state);
    let mut cursor_shown = true;
    while !state.should_quit {
        // Live mode switches (sidebar buttons, `y` key) rebuild policy and
        // persist the config; a failed save keeps the live mode and warns.
        if state.permission_mode != policy.mode() {
            let patterns = loaded.config.permission.clone();
            match crate::policy::Policy::new(
                state.permission_mode.clone(),
                &patterns.allow,
                &patterns.block,
            ) {
                Ok(next) => {
                    *policy = next;
                    loaded.config.permission.mode = state.permission_mode.clone();
                    if let Err(e) = loaded.save_home(home) {
                        eprintln!("warning: cannot persist permission mode: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("warning: bad permission pattern ({e}); reverting mode");
                    state.permission_mode = policy.mode().clone();
                }
            }
        }
        if event::poll(Duration::from_millis(TICK_MS))? {
            match event::read()? {
                event::Event::Key(key) => {
                    if state.create_dialog.is_some() {
                        handle_dialog_key(state, key);
                    } else if state.group_dialog.is_some() {
                        handle_group_key(state, key);
                    } else {
                        handle_key(state, &mut router, key);
                    }
                }
                event::Event::Mouse(mev) => forward_mouse(state, mev),
                event::Event::Paste(text) => {
                    if state.create_dialog.is_none() && state.group_dialog.is_none() {
                        if let Some(active) = state.manager.active() {
                            let bracketed = state.manager.bracketed_paste(active);
                            let bytes = input::paste_bytes(&text, bracketed);
                            if state.manager.pane_write(active, &bytes).is_ok() {
                                state.note_human_input(active);
                            }
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
        for (id, _tab, ev) in state.manager.drain_pty_max(MAX_DRAIN) {
            state.apply(AppEvent::from_pty(id, ev));
        }
        for ev in ipc.try_iter().take(MAX_DRAIN) {
            state.apply(ev);
        }
        state.settle_hooks(policy, audit_path);
        state.settle_comms();
        if state.dirty {
            let views = state.views();
            let status = state.status_text();
            let info = state.sidebar_info();
            let chrome = ui::Chrome {
                tabs: state.tabs(),
                topbar: state.topbar(),
                detail: info.session,
                pending: state.pending_hooks.len(),
                mode: policy.mode().as_str(),
                status,
            };
            let cursor_visible = views.iter().any(|v| v.focused && v.cursor.is_some());
            terminal.draw(|f| {
                let area = f.area();
                ui::render(f, area, &views, &chrome);
                if let Some(dialog) = state.create_dialog.as_mut() {
                    dialog.view(f, crate::create::create_area(area));
                }
                if state.group_dialog.is_some() {
                    let ctx = state.group_ctx();
                    let garea = crate::groups::group_area(area);
                    if let Some(dialog) = state.group_dialog.as_ref() {
                        dialog.view(f, garea, &ctx);
                    }
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
                    if state.manager.pane_write(active, &bytes).is_ok() {
                        state.note_human_input(active);
                    }
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
            UserCommand::CreateSession => {
                state.open_create_dialog();
            }
            UserCommand::ManageGroups => {
                state.open_group_dialog();
            }
            UserCommand::SelectSession(index) => {
                if state.select_session(index) {
                    fit_active_pane(state);
                }
                state.dirty = true;
            }
            UserCommand::TogglePeerGroup => {
                if let Some(active) = state.manager.active() {
                    if state.broker.is_member(active, "peers") {
                        state.broker.leave(active, "peers");
                    } else {
                        let _ = state.broker.join(&state.manager, active, "peers");
                    }
                    state.dirty = true;
                }
            }
            UserCommand::SwitchTab => {
                if let Some(active) = state.manager.active() {
                    if state.manager.switch_tab(active) {
                        // A lazily spawned terminal tab takes the main
                        // area at once instead of keeping 24x80.
                        fit_active_pane(state);
                    }
                    state.dirty = true;
                }
            }
            UserCommand::TogglePermissionMode => {
                state.toggle_permission_mode();
            }
        },
        RoutedKey::PrefixPending | RoutedKey::Cancelled => {
            state.dirty = true;
        }
    }
}

/// One group-dialog key: mutations apply to the broker and stay open,
/// cancel closes, edits redraw.
fn handle_group_key(state: &mut AppState, key: event::KeyEvent) {
    let ctx = state.group_ctx();
    let outcome = state.group_dialog.as_mut().map(|d| d.key(&key, &ctx));
    match outcome {
        Some(crate::groups::GroupOutcome::Closed) => {
            state.group_dialog = None;
            state.dirty = true;
        }
        Some(other) => {
            state.apply_group(other);
        }
        None => {}
    }
}

/// One dialog key: submit spawns and fits, cancel closes, edits redraw.
fn handle_dialog_key(state: &mut AppState, key: event::KeyEvent) {
    let names = state.live_names();
    let outcome = state.create_dialog.as_mut().map(|d| d.key(&key, &names));
    match outcome {
        Some(crate::create::DialogOutcome::Submitted(spec)) => {
            state.create_dialog = None;
            if state.create_session(&spec).is_ok() {
                fit_active_pane(state);
            }
            state.dirty = true;
        }
        Some(crate::create::DialogOutcome::Cancelled) => {
            state.create_dialog = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
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
            "shell",
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
fn forward_mouse(state: &mut AppState, mev: event::MouseEvent) {
    // An open dialog swallows all mouse input: keyboard-first by design,
    // clicks behind it must not refocus sessions mid-form.
    if state.create_dialog.is_some() || state.group_dialog.is_some() {
        return;
    }
    let (rows, cols) = state.term_size;
    let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    // Tab strip: click-to-activate like the sessions bar, hover ignored.
    if areas.topbar.height > 0
        && mev.row >= areas.topbar.y
        && mev.row < areas.topbar.y + areas.topbar.height
    {
        let topbar = state.topbar();
        let buttons = ui::layout_topbar(areas.topbar, &topbar.tabs);
        if let Some(index) = ui::topbar_at(&buttons, mev.column) {
            if matches!(mev.kind, event::MouseEventKind::Down(_)) {
                if let Some(active) = state.manager.active() {
                    if state.manager.select_tab(active, index) {
                        fit_active_pane(state);
                    }
                    state.dirty = true;
                }
            }
        }
        return;
    }
    // Sidebar settings row: click-to-switch Off/Yolo like the bars; hover
    // and drags must never flip the live permission mode.
    if areas.sidebar.width > 0
        && areas.sidebar.height > 0
        && mev.column >= areas.sidebar.x
        && mev.column < areas.sidebar.x + areas.sidebar.width
        && mev.row >= areas.sidebar.y
        && mev.row < areas.sidebar.y + areas.sidebar.height
    {
        if matches!(mev.kind, event::MouseEventKind::Down(_)) {
            let buttons = ui::mode_button_areas(areas.sidebar);
            match ui::mode_at(&buttons, mev.column, mev.row) {
                Some("yolo") => {
                    state.set_permission_mode(crate::config::PermissionMode::Yolo);
                }
                Some(_) => {
                    state.set_permission_mode(crate::config::PermissionMode::Off);
                }
                None => {}
            }
        }
        return;
    }
    if mev.row >= areas.session_bar.y
        && mev.row < areas.session_bar.y + areas.session_bar.height
        && areas.session_bar.height > 0
    {
        let segments = ui::session_bar_segments(&state.tabs());
        let buttons = ui::layout_session_bar(areas.session_bar, &segments);
        // Click-to-activate only: hover (Moved) and drags must never steal
        // the session; pane mouse protocols still get every event below.
        if let Some(index) = ui::session_at(&buttons, mev.column) {
            if matches!(mev.kind, event::MouseEventKind::Down(_)) {
                if state.select_session(index) {
                    fit_active_pane(state);
                }
                state.dirty = true;
            }
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
        // 80x24 less top strip, session bar, status row, main-pane borders.
        assert_eq!(state.manager.pane_size(order[0]), Some((19, 62)));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 2);
        // Background panes keep spawn size until focused...
        assert_eq!(state.manager.pane_size(order[1]), Some((24, 80)));
        // ...then take the main area on selection.
        assert!(state.select_session(1));
        fit_active_pane(&mut state);
        assert_eq!(state.manager.pane_size(order[1]), Some((19, 62)));
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
        forward_mouse(&mut state, click);
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[1]));
        assert_eq!(state.manager.pane_size(order[1]), Some((19, 62)));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn topbar_click_selects_terminal_tab() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        let id = state
            .manager
            .spawn_agent(
                "a",
                &std::env::temp_dir(),
                "exec cat",
                crate::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        // Top strip is row 0: "[Codex]" then "[Terminal]" from column 10.
        let bar = state.topbar();
        assert_eq!(bar.tabs.len(), 2);
        assert!(bar.tabs[0].active);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 12,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        let bar = state.topbar();
        assert!(bar.tabs[1].active);
        // Hover on the strip never switches back.
        state.dirty = false;
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 3,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(state.topbar().tabs[1].active);
        assert!(!state.dirty);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn session_bar_hover_never_switches_sessions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[0]));
        state.dirty = false;
        // Same coordinates as the click test, but a hover and a drag.
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            forward_mouse(
                &mut state,
                MouseEvent {
                    kind,
                    column: 12,
                    row: 22,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            );
        }
        assert_eq!(state.manager.active(), Some(order[0]));
        assert!(!state.dirty, "hover leaves no work");
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn sidebar_click_switches_mode_hover_ignored() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        // Sidebar is x=64..80; settings row is y=11, Off at x=66..71.
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Yolo);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Off);
        // Hover over Yolo (x=72..78) must not flip it back.
        state.dirty = false;
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 73,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Off);
        assert!(!state.dirty, "hover leaves no work");
        // Click Yolo to return.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 73,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Yolo);
    }

    #[test]
    fn prefix_c_opens_create_dialog() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix);
        handle_key(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        assert!(state.create_dialog.is_some());
    }

    #[test]
    fn prefix_o_manages_groups_end_to_end() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        // Prefix o opens the group dialog.
        let mut router = InputRouter::new();
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('o'), none));
        assert!(state.group_dialog.is_some());
        // n + typed name + Enter creates the group and stays open.
        let gkey = |code| KeyEvent::new(code, none);
        handle_group_key(&mut state, gkey(KeyCode::Char('n')));
        for ch in "team".chars() {
            handle_group_key(&mut state, gkey(KeyCode::Char(ch)));
        }
        handle_group_key(&mut state, gkey(KeyCode::Enter));
        assert!(state.group_dialog.is_some(), "stays open for more");
        assert!(state.broker.group_names().contains(&"team".to_string()));
        // a + Space + Enter checks the lone session into the group.
        handle_group_key(&mut state, gkey(KeyCode::Char('a')));
        handle_group_key(&mut state, gkey(KeyCode::Char(' ')));
        handle_group_key(&mut state, gkey(KeyCode::Enter));
        assert!(state.broker.is_member(order[0], "team"));
        assert_eq!(state.tabs()[0].group.as_deref(), Some("team"), "bar reflects it");
        // Mouse is swallowed while open: sidebar click flips nothing.
        state.dirty = false;
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Yolo);
        assert!(!state.dirty, "swallowed clicks leave no work");
        // Esc closes.
        handle_group_key(&mut state, gkey(KeyCode::Esc));
        assert!(state.group_dialog.is_none());
        assert!(state.manager.remove(order[0]));
    }

    #[test]
    fn dialog_submit_spawns_and_cancel_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        state.open_create_dialog();
        // Prefilled shell-1 submits a real shell.
        handle_dialog_key(&mut state, KeyEvent::new(KeyCode::Enter, none));
        assert!(state.create_dialog.is_none());
        assert_eq!(state.manager.len(), 1);
        // Escape closes without spawning.
        state.open_create_dialog();
        handle_dialog_key(&mut state, KeyEvent::new(KeyCode::Esc, none));
        assert!(state.create_dialog.is_none());
        assert_eq!(state.manager.len(), 1);
        let order = state.manager.order().to_vec();
        for id in order {
            assert!(state.manager.remove(id));
        }
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
