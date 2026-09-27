//! Terminal setup and the main event loop.
//!
//! Roughly 16 ms per iteration: poll input with a timeout, drain at most
//! 100 background events so floods cannot starve input, offer pending work,
//! and repaint only when dirty. Terminal state (raw mode, alternate screen)
//! is restored on normal exit, on panic, and on drop.

pub mod dialogs;
pub mod input;
pub mod keys;
pub mod mouse;
pub mod visual;

#[cfg(test)]
mod test_support;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event;
use crossterm::tty::IsTty;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::AppState;
use crate::infra::event::AppEvent;
use crate::ui;

use self::input::InputRouter;

use dialogs::{handle_confirm_key, handle_dialog_key, handle_group_key, handle_oobe_key, handle_oobe_mouse, handle_telegram_key, handle_telegram_mouse, handle_theme_key, handle_theme_mouse, settle_quit_save};
use keys::{handle_key_at, handle_restore_key};
use mouse::forward_mouse;
#[cfg(feature = "visual")]
use visual::sync_visual_terminal;

/// Milliseconds per main-loop iteration.
pub const TICK_MS: u64 = 16;

/// Background events drained per iteration (anti-starvation bound).
pub const MAX_DRAIN: usize = 100;

/// Slowest background repaint, in milliseconds. Streaming pane output
/// (spinners, progress renders) dirties the frame up to 60 times a
/// second; repainting that often burns a core redrawing an unchanged
/// viewport, so background frames pace to at most 10fps. Human input
/// (keys, mouse, paste, resize) always paints at once.
pub const BACKGROUND_FRAME_MS: u64 = 100;

/// A dirty frame paints now when input arrived this tick, otherwise
/// only once the background budget elapsed since the last paint.
fn paint_due(last_paint: Instant, now: Instant, input_this_tick: bool) -> bool {
    input_this_tick || now.duration_since(last_paint) >= Duration::from_millis(BACKGROUND_FRAME_MS)
}

/// Grace window on quit: SIGTERM'd agents share this long to save state
/// before the state drop SIGKILLs stragglers.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// `io::Stdout` is `LineWriter`-backed with only a ~1KB internal buffer, so
/// a large per-frame cell diff can trigger several write syscalls before
/// ratatui's own end-of-draw flush ever runs. A generously sized
/// `BufWriter` batches a whole frame's escape sequences into one write
/// instead, mirroring how tmux and ghostty always flush a complete frame
/// at once rather than dribbling it out.
fn stdout_writer() -> io::BufWriter<io::Stdout> {
    io::BufWriter::with_capacity(64 * 1024, io::stdout())
}

pub(super) type StdoutTerminal = Terminal<CrosstermBackend<io::BufWriter<io::Stdout>>>;

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
    loaded: &mut crate::infra::config::LoadedConfig,
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
    state.pill_tabs = loaded.config.pills_enabled;
    state.themes_dir = Some(crate::infra::branding::themes_dir(home));
    // Saved theme applies at startup; a missing or broken file warns
    // and keeps the builtin look instead of blocking boot.
    if loaded.config.theme != "default" {
        let themes = state.available_themes();
        if themes.iter().any(|t| t.name == loaded.config.theme) {
            let name = loaded.config.theme.clone();
            state.apply_theme_name(&name, &themes);
            state.dirty = true;
        } else {
            eprintln!(
                "warning: unknown theme {:?}; keeping default",
                loaded.config.theme
            );
        }
    }
    if let Ok(mut tg) = state.telegram_config.lock() {
        *tg = loaded.config.telegram.clone();
    }
    let mut policy = match crate::hooks::policy::Policy::new(
        loaded.config.permission.mode.clone(),
        &loaded.config.permission.allow,
        &loaded.config.permission.block,
    ) {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("warning: bad permission pattern ({e}); asking everything");
            crate::hooks::policy::Policy::new(crate::infra::config::PermissionMode::Off, &[], &[])
                .expect("empty patterns compile")
        }
    };
    let audit_path = audit_path.to_path_buf();
    // Bounded: a stalled owner must exert backpressure (handlers drop
    // to caller timeouts), never grow this queue without limit.
    let (ipc_tx, ipc_rx) = std::sync::mpsc::sync_channel(crate::ipc::listener::IPC_QUEUE_CAP);
    // The IPC listener is fail-soft: without it, hook relays simply find
    // no endpoint and exit zero. Children inherit the endpoint by env;
    // harnesses that scrub hook environments (muse) use the endpoint file.
    let _ipc = match crate::ipc::listener::spawn_all(ipc_tx.clone()) {
        Ok(spawned) => {
            std::env::set_var("FORGE_IPC_ENDPOINT", &spawned.sock_path);
            if let Err(e) = crate::hooks::relay::write_endpoint_file(
                home,
                std::process::id(),
                &spawned.sock_path,
            ) {
                eprintln!("warning: cannot publish endpoint file: {e}");
                crate::infra::logging::hook_trace_global(&format!("tui endpoint file write failed: {e}"));
            } else {
                crate::infra::logging::hook_trace_global(&format!(
                    "tui listening sock={} endpoint_file={}",
                    spawned.sock_path.display(),
                    crate::hooks::relay::endpoint_file_path(home).display()
                ));
            }
            Some(spawned)
        }
        Err(e) => {
            eprintln!("warning: ipc listener unavailable: {e}");
            crate::infra::logging::hook_trace_global(&format!("tui ipc listener unavailable: {e}"));
            None
        }
    };
    // Telegram's independent polling and sending workers start lazily in
    // the loop, so enabling from settings needs no restart. Both emit only
    // bounded AppEvents and are supervised back into service after exit.
    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout_writer())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot open terminal: {e}");
            return 1;
        }
    };
    // Reclaim placements leaked while the delete escape was malformed
    // (`a=D`): those ids are untracked, so clear them once up front.
    // Forge owns the fullscreen terminal; nothing else paints images.
    #[cfg(feature = "visual")]
    if crate::visual::kitty_supported_env() {
        use std::io::Write as _;
        let _ = write!(io::stdout(), "{}", crate::visual::kitty_delete_all());
        let _ = io::stdout().flush();
    }
    let result = loop_until_quit(
        state,
        &mut terminal,
        ipc_rx,
        &ipc_tx,
        &mut policy,
        &audit_path,
        loaded,
        home,
    );
    // Graceful shutdown: SIGTERM every live pane so agents can save
    // state; stragglers die by SIGKILL when the state drops below.
    // Bounded: the last frame sits for at most the grace window.
    let survivors = state.manager.shutdown_gracefully(SHUTDOWN_GRACE);
    if survivors > 0 {
        // The terminal still owns the screen, so the app log (not
        // stderr) carries the straggler report.
        if let Ok(mut log) = crate::infra::logging::FileLogger::open(
            &crate::infra::branding::app_log(home),
            crate::infra::logging::DEFAULT_MAX_BYTES,
        ) {
            let _ = log.append(&format!(
                "quit: {survivors} pane(s) ignored SIGTERM, SIGKILLed"
            ));
        }
    }
    // Image cleanup while the alternate screen is still up: the
    // overlay may still want its visual at quit, so take unconditionally.
    #[cfg(feature = "visual")]
    if let Some(image_id) = state.visual_take_shown() {
        use std::io::Write as _;
        let _ = write!(io::stdout(), "{}", crate::visual::kitty_delete(image_id));
        let _ = io::stdout().flush();
    }
    if let Err(e) = result {
        eprintln!("error: main loop failed: {e}");
        crate::hooks::relay::clear_endpoint_file(home);
        return 1;
    }
    crate::hooks::relay::clear_endpoint_file(home);
    0
}

fn loop_until_quit(
    state: &mut AppState,
    terminal: &mut StdoutTerminal,
    ipc: std::sync::mpsc::Receiver<AppEvent>,
    ipc_tx: &std::sync::mpsc::SyncSender<AppEvent>,
    policy: &mut crate::hooks::policy::Policy,
    audit_path: &std::path::Path,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
) -> io::Result<()> {
    let mut router = InputRouter::new();
    let size = terminal.size()?;
    state.apply(AppEvent::Resize(size.height, size.width));
    // Saved snapshots offer themselves before anything else: a pick
    // restores the whole topology, Esc starts fresh.
    let file = crate::session::checkpoint::SessionsFile::load(&crate::infra::branding::sessions_file(home));
    if let Some(picker) = crate::session::checkpoint::RestorePicker::new(&file) {
        state.restore_picker = Some(picker);
        state.dirty = true;
    }
    // Workspace boards load once; a corrupt save quarantines with a
    // footer notice instead of blocking startup.
    state.load_boards(home);
    fit_active_pane(state);
    let mut cursor_shown = true;
    // Frame pacer: the first paint is immediate, background repaints
    // wait out BACKGROUND_FRAME_MS, input repaints skip the wait.
    let mut last_paint = Instant::now()
        .checked_sub(Duration::from_millis(BACKGROUND_FRAME_MS))
        .unwrap_or_else(Instant::now);
    // OS theme watcher: a switch repaints with the new map next frame,
    // no restart. Missing state (non-Omarchy, SSH) polls false forever.
    let mut theme_watcher = crate::ui::theme::ThemeWatcher::omarchy();
    while !state.should_quit {
        state.dirty |= theme_watcher.poll();
        // Live mode switches (sidebar buttons, `y` key) rebuild policy and
        // persist the config; a failed save keeps the live mode and warns.
        if state.permission_mode != policy.mode() {
            let patterns = loaded.config.permission.clone();
            match crate::hooks::policy::Policy::new(
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
        let mut input_this_tick = false;
        if event::poll(Duration::from_millis(TICK_MS))? {
            match event::read()? {
                event::Event::Key(key) => {
                    input_this_tick = true;
                    if state.oobe_dialog.is_some() {
                        handle_oobe_key(state, home, key);
                    } else if state.restore_picker.is_some() {
                        handle_restore_key(state, key);
                    } else if state.create_dialog.is_some() {
                        handle_dialog_key(state, key);
                    } else if state.group_dialog.is_some() {
                        handle_group_key(state, key);
                    } else if state.telegram_dialog.is_some() {
                        handle_telegram_key(state, loaded, home, key);
                    } else if state.theme_dialog.is_some() {
                        handle_theme_key(state, loaded, home, key);
                    } else if state.confirm.is_some() {
                        handle_confirm_key(state, key);
                    } else {
                        handle_key_at(state, &mut router, key, Instant::now());
                    }
                }
                event::Event::Mouse(mev) => {
                    input_this_tick = true;
                    if state.oobe_dialog.is_some() {
                        // The first-run dialog owns the mouse like every
                        // other modal: clicks inside hit rows/buttons,
                        // everything else dies here.
                        handle_oobe_mouse(state, home, mev);
                    } else if state.telegram_dialog.is_some() {
                        // The settings modal owns the mouse like every
                        // other modal: clicks inside hit rows/buttons,
                        // everything else dies here.
                        handle_telegram_mouse(state, loaded, home, mev);
                    } else if state.theme_dialog.is_some() {
                        handle_theme_mouse(state, loaded, home, mev);
                    } else {
                        forward_mouse(state, mev);
                    }
                }
                event::Event::Paste(text) => {
                    input_this_tick = true;
                    if let Some(dialog) = state.telegram_dialog.as_mut() {
                        // The settings form takes the paste like typing;
                        // every other modal still swallows pastes.
                        dialog.paste(&text);
                        state.dirty = true;
                    } else if state.oobe_dialog.is_none()
                        && state.restore_picker.is_none()
                        && state.create_dialog.is_none()
                        && state.group_dialog.is_none()
                        && state.theme_dialog.is_none()
                        && state.confirm.is_none()
                    {
                        // A tour draft takes the paste single-line, like
                        // typed input; the pane path below stays untouched.
                        if state
                            .walkthrough_overlay_mut()
                            .is_some_and(|tour| tour.input.is_some())
                        {
                            if let Some(tour) = state.walkthrough_overlay_mut() {
                                tour.push_paste(&text);
                                state.dirty = true;
                            }
                        } else if state.card_edit.is_some() {
                            // The open card editor takes the paste like
                            // typing; the panes behind it never see it.
                            state.board_card_edit_paste(&text);
                        } else if state.board_draft.is_some() {
                            // An open board draft takes the paste
                            // single-line like typing; the panes behind
                            // the board never see it.
                            state.board_push_paste(&text);
                        } else if let Some(active) = state.manager.active() {
                            let bracketed = state.manager.bracketed_paste(active);
                            let bytes = input::paste_bytes(&text, bracketed);
                            if state.manager.pane_write(active, &bytes).is_ok() {
                                state.note_human_input(active);
                            }
                        }
                    }
                }
                event::Event::Resize(cols, rows) => {
                    input_this_tick = true;
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
        state.drain_visual();
        state.drain_telegram_test();
        // Sidebar Tetris gravity: steps at most once per interval and
        // only while open, so idle agents never pay for the game.
        state.tetris_tick(Instant::now());
        // Which-key clock: a prefix held past the delay earns its HUD;
        // a sequence finished fast never paints.
        state.dirty |= state.whichkey.poll(router.is_pending(), Instant::now());
        // Presence rides the loop's own input flag (keys, mouse,
        // paste): twenty silent minutes announce away to every live
        // session, the next input announces the return.
        state.settle_presence(input_this_tick, Instant::now());
        // Lazy poller start: the first pass that sees Telegram enabled
        // launches the thread exactly once, so the settings modal can
        // enable delivery without a restart.
        if state.telegram_poller_wanted() {
            let tg_cfg = state.telegram_config.clone();
            let tg_tx = ipc_tx.clone();
            let done_tx = ipc_tx.clone();
            match std::thread::Builder::new().name("telegram-poller".to_string()).spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::telegram::poll_forever(tg_cfg, tg_tx)
                }));
                let _ = done_tx.send(AppEvent::TelegramWorkerStopped(crate::telegram::WorkerKind::Poller));
            }) {
                Ok(_) => state.telegram_poller = true,
                Err(_) => state.telegram_last_poll_failed = true,
            }
        }
        if state.telegram_sender_wanted() {
            let tg_cfg = state.telegram_config.clone();
            let tg_tx = ipc_tx.clone();
            let done_tx = ipc_tx.clone();
            let tg_outbox = state.telegram_outbox.clone();
            let tg_wake = state.telegram_outbox_wake.clone();
            match std::thread::Builder::new().name("telegram-sender".to_string()).spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::telegram::sender_forever(tg_cfg, tg_outbox, tg_wake, tg_tx)
                }));
                let _ = done_tx.send(AppEvent::TelegramWorkerStopped(crate::telegram::WorkerKind::Sender));
            }) {
                Ok(_) => state.telegram_sender = true,
                Err(_) => state.telegram_last_poll_failed = true,
            }
        }
        // Streaming pane output paces to BACKGROUND_FRAME_MS; input
        // paints at once. A skipped tick keeps dirty set, so no frame
        // is lost, only delayed past the budget.
        if state.dirty && paint_due(last_paint, Instant::now(), input_this_tick) {
            last_paint = Instant::now();
            if state.grid_mode {
                fit_grid_panes(state);
            }
            let views = state.views();
            let info = state.sidebar_info();
            let chrome = ui::Chrome {
                tabs: state.tabs(),
                topbar: state.topbar(),
                detail: info.session,
                sessions: info.sessions,
                active: info.active,
                fleet_cursor: info.fleet_cursor,
                fleet_scroll: info.fleet_scroll,
                other_timers: info.other_timers,
                pending: state.pending_hooks.len(),
                mode: policy.mode().as_str(),
                telegram: state
                    .telegram_config
                    .lock()
                    .map(|cfg| if cfg.enabled { "on" } else { "off" })
                    .unwrap_or("off"),
                telegram_badge: info.telegram_badge,
                board_open: state.board_open,
                board: state.board_open.then(|| state.board_view()),
                tetris_open: state.tetris_open,
                tetris: state.tetris_open.then(|| state.tetris.clone()),
                grid: state.grid_mode,
                pills: state.pill_tabs,
            };
            let cursor_visible = views.iter().any(|v| v.focused && v.cursor.is_some());
            // Board mutations flush within a frame; the dirty flag keeps
            // clean frames free of filesystem work.
            state.flush_boards(home);
            terminal.draw(|f| {
                let area = f.area();
                ui::render(f, area, &views, &chrome);
                // The which-key HUD floats above the chrome but below
                // every modal: it only ever opens off the prefix path,
                // which modals bypass, so both can never want input.
                if state.whichkey.visible() {
                    let hud = crate::ui::whichkey::whichkey_area(area);
                    crate::ui::whichkey::render_whichkey(f, area, hud);
                }
                if let Some(dialog) = state.create_dialog.as_mut() {
                    dialog.view(f, crate::ui::dialogs::create::create_area(area));
                }
                if state.group_dialog.is_some() {
                    let ctx = state.group_ctx();
                    let garea = crate::ui::dialogs::groups::group_area(area);
                    if let Some(dialog) = state.group_dialog.as_ref() {
                        dialog.view(f, garea, &ctx);
                    }
                }
                if let Some(dialog) = state.telegram_dialog.as_mut() {
                    dialog.view(f, crate::ui::dialogs::telegram::telegram_area(area));
                }
                if let Some(dialog) = state.card_edit.as_mut() {
                    dialog.view(f, crate::ui::dialogs::card_edit::card_edit_area(area));
                }
                if let Some(dialog) = state.theme_dialog.as_ref() {
                    dialog.view(f, crate::ui::dialogs::theme::theme_area(area));
                }
                if let Some(dialog) = state.confirm.as_ref() {
                    dialog.view(f, crate::ui::dialogs::quit::confirm_area(area));
                }
                if state.quit_saving {
                    crate::ui::dialogs::quit::view_saving(f, crate::ui::dialogs::quit::saving_area(area));
                }
                if let Some(picker) = state.restore_picker.as_ref() {
                    picker.view(f, crate::session::checkpoint::RestorePicker::picker_area(area));
                }
                // First run sits above every other modal (below the tour):
                // it owns input while present, so it paints on top.
                if let Some(dialog) = state.oobe_dialog.as_ref() {
                    dialog.view(f, crate::ui::dialogs::oobe::oobe_area(area));
                }
                // The tour takes over the main area above every dialog:
                // it is opaque and owns input while open.
                if let Some(tour) = state.walkthrough_overlay() {
                    tour.view(f, crate::walkthrough::walk_area(area));
                }
            })?;
            #[cfg(feature = "visual")]
            sync_visual_terminal(state, &mut *terminal)?;
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
        // Yes confirmed quit above: the saving modal just painted, so
        // persist the snapshot now and let the loop exit.
        settle_quit_save(state, home);
    }
    Ok(())
}

/// Fit the active pane to the main area. Background panes keep their size
/// until focused, when they are fitted in turn.
pub(super) fn fit_active_pane(state: &mut AppState) {
    let Some(active) = state.manager.active() else {
        return;
    };
    let (rows, cols) = state.term_size;
    let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    let main = ui::layout::pane_grid_area(&areas);
    let pane_rows = main.height.saturating_sub(2).max(1);
    let pane_cols = main.width.saturating_sub(2).max(1);
    let _ = state.manager.resize(active, pane_rows, pane_cols);
}

/// Fit every session to its grid cell inner area, skipping cells with no
/// room and panes already at size (exited panes report none). Runs on
/// every dirty frame in grid mode, so spawns, exits, resizes, and the
/// toggle itself can never leave a tile showing an un-resized pane.
pub(super) fn fit_grid_panes(state: &mut AppState) {
    let (rows, cols) = state.term_size;
    let grid = ui::layout::grid_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let order = state.manager.order().to_vec();
    let cells = ui::layout::grid_cells(grid, order.len());
    for (id, cell) in order.iter().zip(cells.iter()) {
        if cell.width <= 2 || cell.height <= 2 {
            continue;
        }
        let target = (cell.height - 2, cell.width - 2);
        if state.manager.pane_size(*id) != Some(target) {
            let _ = state.manager.resize(*id, target.0, target.1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::spawn_shell_cmd;

    #[test]
    fn stdout_writer_batches_a_frame_into_one_buffered_flush() {
        // `io::Stdout` is `LineWriter`-backed with a small (~1KB) internal
        // buffer, so a large per-frame cell diff can trigger several write
        // syscalls before ratatui's own end-of-draw flush. Wrapping it in a
        // generously sized `BufWriter` (tmux/ghostty both batch a full
        // frame into one write) means the draw's many small `queue!`
        // writes coalesce into a single flush instead.
        let w = stdout_writer();
        assert!(
            w.capacity() >= 64 * 1024,
            "buffer must hold a full frame's worth of escape sequences without flushing early"
        );
    }

    #[test]
    fn new_session_fits_main_pane() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 1);
        // 80x24 less top strip, session bar, and main-pane borders.
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
    fn loop_constants_match_blueprint() {
        assert_eq!(TICK_MS, 16);
        assert_eq!(MAX_DRAIN, 100);
    }

    #[test]
    fn background_frames_pace_input_paints_at_once() {
        let start = Instant::now();
        // Input always paints, even right after a paint.
        assert!(paint_due(start, start, true));
        // Background waits out the budget...
        assert!(!paint_due(start, start, false));
        assert!(!paint_due(
            start,
            start + Duration::from_millis(BACKGROUND_FRAME_MS - 1),
            false
        ));
        // ...then paints once it elapsed.
        assert!(paint_due(
            start,
            start + Duration::from_millis(BACKGROUND_FRAME_MS),
            false
        ));
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
