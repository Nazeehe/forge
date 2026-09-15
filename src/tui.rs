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

/// Grace window on quit: SIGTERM'd agents share this long to save state
/// before the state drop SIGKILLs stragglers.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

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
    state.pill_tabs = loaded.config.pills_enabled;
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
    // no endpoint and exit zero. Children inherit the endpoint by env;
    // harnesses that scrub hook environments (muse) use the endpoint file.
    let _ipc = match crate::listener::spawn_all(ipc_tx) {
        Ok(spawned) => {
            std::env::set_var("FORGE_IPC_ENDPOINT", &spawned.sock_path);
            if let Err(e) = crate::relay::write_endpoint_file(
                home,
                std::process::id(),
                &spawned.sock_path,
            ) {
                eprintln!("warning: cannot publish endpoint file: {e}");
            }
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
        if let Ok(mut log) = crate::logging::FileLogger::open(
            &crate::branding::app_log(home),
            crate::logging::DEFAULT_MAX_BYTES,
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
        crate::relay::clear_endpoint_file(home);
        return 1;
    }
    crate::relay::clear_endpoint_file(home);
    0
}

/// Kitty image sync, after the ratatui draw flushed: hide first (a
/// replacement emits both sides), then position the cursor at the
/// overlay origin and transmit `c` cell columns wide. Best effort —
/// a dead terminal means we are quitting anyway.
/// Focused Visual tab viewport: session, tab image region, and
/// text-cell size. The region is the same chrome the fallback art
/// and the button hit test use, so all three backends agree. The
/// cell size comes from the terminal pixel report, falling back to
/// the 8x16 the half-block art draws with exactly.
#[cfg(feature = "visual")]
fn visual_viewport(
    state: &AppState,
) -> Option<(crate::session::SessionId, crate::ui::VisualChrome, (f64, f64))> {
    let (id, _) = state.visual_focused_frame()?;
    let (rows, cols) = state.term_size;
    let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    let chrome = ui::visual_chrome(ui::pane_content_area(&areas), state.pill_tabs);
    let cell = match crossterm::terminal::window_size() {
        Ok(ws) => crate::visual::cell_px(cols, rows, ws.width as u32, ws.height as u32),
        Err(_) => crate::visual::FALLBACK_CELL_PX,
    };
    Some((id, chrome, cell))
}

#[cfg(feature = "visual")]
fn sync_visual_terminal(
    state: &mut AppState,
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
    use std::io::Write as _;
    if let Some(image_id) = state.visual_take_hide() {
        write!(io::stdout(), "{}", crate::visual::kitty_delete(image_id))?;
    }
    if !crate::visual::kitty_supported_env() {
        return Ok(());
    }
    // Peek before bytes: an unchanged frame costs comparisons, not
    // a clone or crop+encode the gate would drop. Bytes still come
    // before the claim, so the gate records only paints the terminal
    // actually receives and a failed encode retries next frame.
    if let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) {
        let (_, generation) = state.visual_focused_frame().expect("viewport implies frame");
        if let Some(paint) = state.visual_paint(id, chrome.image, cell_w, cell_h) {
            if state.visual_current_paint() != Some(paint) {
                if let Some(png) = state.visual_frame_png(id, generation, paint) {
                    if let Some(spec) = state.visual_take_show(true, paint) {
                        // No delete before a same-id re-display: the
                        // fixed placement id swaps it in place, while
                        // delete-then-show flashes the bare pane.
                        let placed = spec.paint;
                        if placed.out_cols > 0 && placed.out_rows > 0 {
                            crossterm::execute!(
                                io::stdout(),
                                crossterm::cursor::MoveTo(placed.cursor_x, placed.cursor_y)
                            )?;
                            write!(
                                io::stdout(),
                                "{}",
                                crate::visual::kitty_transmit(
                                    &png,
                                    spec.image_id,
                                    placed.out_cols,
                                    placed.out_rows
                                )
                            )?;
                        }
                    }
                }
            }
        }
    }
    io::stdout().flush()?;
    Ok(())
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
    // Saved snapshots offer themselves before anything else: a pick
    // restores the whole topology, Esc starts fresh.
    let file = crate::checkpoint::SessionsFile::load(&crate::branding::sessions_file(home));
    if let Some(picker) = crate::checkpoint::RestorePicker::new(&file) {
        state.restore_picker = Some(picker);
        state.dirty = true;
    }
    fit_active_pane(state);
    let mut cursor_shown = true;
    // OS theme watcher: a switch repaints with the new map next frame,
    // no restart. Missing state (non-Omarchy, SSH) polls false forever.
    let mut theme_watcher = crate::theme::ThemeWatcher::omarchy();
    while !state.should_quit {
        state.dirty |= theme_watcher.poll();
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
                    if state.restore_picker.is_some() {
                        handle_restore_key(state, key);
                    } else if state.create_dialog.is_some() {
                        handle_dialog_key(state, key);
                    } else if state.group_dialog.is_some() {
                        handle_group_key(state, key);
                    } else if state.quit_confirm.is_some() {
                        handle_quit_key(state, key);
                    } else {
                        handle_key(state, &mut router, key);
                    }
                }
                event::Event::Mouse(mev) => forward_mouse(state, mev),
                event::Event::Paste(text) => {
                    if state.restore_picker.is_none()
                        && state.create_dialog.is_none()
                        && state.group_dialog.is_none()
                        && state.quit_confirm.is_none()
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
        if state.dirty {
            if state.grid_mode {
                fit_grid_panes(state);
            }
            let views = state.views();
            let info = state.sidebar_info();
            let chrome = ui::Chrome {
                tabs: state.tabs(),
                topbar: state.topbar(),
                detail: info.session,
                pending: state.pending_hooks.len(),
                mode: policy.mode().as_str(),
                grid: state.grid_mode,
                pills: state.pill_tabs,
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
                if let Some(dialog) = state.quit_confirm.as_ref() {
                    dialog.view(f, crate::quit::quit_area(area));
                }
                if let Some(picker) = state.restore_picker.as_ref() {
                    picker.view(f, crate::checkpoint::RestorePicker::picker_area(area));
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
    }
    Ok(())
}

/// Visual tab viewport keys: arrows pan, `+`/`-` zoom, Esc leaves
/// the tab. Returns true when the key belonged to the viewport.
/// Plain keys only: chords with Ctrl/Alt still reach the router, so
/// prefixes keep working with a diagram open.
#[cfg(feature = "visual")]
fn handle_visual_key(state: &mut AppState, key: event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) else {
        return true;
    };
    let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
    if key.modifiers != KeyModifiers::NONE {
        return false;
    }
    match key.code {
        KeyCode::Left => {
            state.visual_scroll(id, -1, 0, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Right => {
            state.visual_scroll(id, 1, 0, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Up => {
            state.visual_scroll(id, 0, -1, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Down => {
            state.visual_scroll(id, 0, 1, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            state.visual_zoom(id, crate::ui::VisualButton::ZoomOut, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Esc => {
            state.overlay_view = None;
            state.dirty = true;
        }
        _ => {}
    }
    true
}

fn handle_key(state: &mut AppState, router: &mut InputRouter, key: event::KeyEvent) {
    // An open tour captures every key, including prefix chords: the
    // overlay owns input while open and Esc leaves it.
    if state.walkthrough_overlay_active() {
        handle_walkthrough_key(state, key);
        return;
    }
    // A focused Visual tab owns its viewport keys the same way, so
    // arrows pan the diagram instead of reaching the agent pane.
    #[cfg(feature = "visual")]
    if state.visual_tab_focused() && handle_visual_key(state, key) {
        return;
    }
    match router.feed(key) {
        RoutedKey::Forward(k) => {
            if state.overlay_active() { return; }
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
            UserCommand::Quit => state.open_quit_confirm(),
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
                    state.overlay_view = None;
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
            UserCommand::TerminateSession => {
                if let Some(active) = state.manager.active() {
                    state.terminate_session(active);
                    fit_active_pane(state);
                }
                state.dirty = true;
            }
            UserCommand::ToggleGrid => {
                state.toggle_grid();
                // Leaving grid restores the focused pane to full size;
                // entering syncs every pane on the next dirty frame.
                if !state.grid_mode {
                    fit_active_pane(state);
                }
            }
        },
        RoutedKey::PrefixPending | RoutedKey::Cancelled => {
            state.dirty = true;
        }
    }
}

/// One key inside an open tour: step/scroll chords repaint, a
/// submitted draft injects into the agent pane, browse-mode Esc leaves
/// the overlay. Everything else stays swallowed.
fn handle_walkthrough_key(state: &mut AppState, key: event::KeyEvent) {
    use crate::walkthrough::WalkKey;
    let Some(active) = state.manager.active() else {
        return;
    };
    let outcome = match state.walkthrough_overlay_mut() {
        Some(tour) => tour.key(&key),
        None => return,
    };
    match outcome {
        WalkKey::Moved | WalkKey::Edited | WalkKey::CancelledInput => {
            state.dirty = true;
        }
        WalkKey::Submitted => {
            if !state.submit_walkthrough_question(active) {
                if let Some(tour) = state.walkthrough_overlay_mut() {
                    tour.status = Some("session is not live".to_string());
                }
                state.dirty = true;
            }
        }
        WalkKey::Closed => {
            state.overlay_view = None;
            state.dirty = true;
        }
        WalkKey::Ignored => {}
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

/// One restore-picker key: a pick recreates the whole entry and fits
/// the panes, Esc starts fresh. Either way the picker closes.
fn handle_restore_key(state: &mut AppState, key: event::KeyEvent) {
    let outcome = state.restore_picker.as_mut().map(|p| p.key(&key));
    match outcome {
        Some(crate::checkpoint::RestoreOutcome::Pick(_)) => {
            let entry = state.restore_picker.as_ref().and_then(|p| p.take_selected());
            state.restore_picker = None;
            if let Some(entry) = entry {
                state.restore_entry(&entry);
                fit_active_pane(state);
            }
            state.dirty = true;
        }
        Some(_) => {
            state.restore_picker = None;
            state.dirty = true;
        }
        None => {}
    }
}

/// One quit-confirm key: Yes quits, anything else keeps running.
fn handle_quit_key(state: &mut AppState, key: event::KeyEvent) {
    let outcome = state.quit_confirm.as_mut().map(|d| d.key(&key));
    match outcome {
        Some(crate::quit::QuitOutcome::Confirmed) => {
            state.quit_confirm = None;
            state.should_quit = true;
        }
        Some(crate::quit::QuitOutcome::Dismissed) => {
            state.quit_confirm = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
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
    // Modals own mouse input. Only a left press inside the group dialog
    // reaches its List/Checkbox rows; clicks behind it do nothing.
    // The restore picker is keyboard-only: every click dies here.
    if state.restore_picker.is_some() {
        return;
    }
    if state.group_dialog.is_some() {
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let (rows, cols) = state.term_size;
            let area = crate::groups::group_area(ratatui::layout::Rect::new(0, 0, cols, rows));
            if mev.column >= area.x && mev.column < area.right()
                && mev.row >= area.y && mev.row < area.bottom()
            {
                let ctx = state.group_ctx();
                if let Some(dialog) = state.group_dialog.as_mut() {
                    if dialog.click(mev.column, mev.row, area, &ctx) {
                        state.dirty = true;
                    }
                }
            }
        }
        return;
    }
    if state.create_dialog.is_some() {
        return;
    }
    if state.quit_confirm.is_some() {
        return;
    }
    let (rows, cols) = state.term_size;
    let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    // Tab strip: click-to-activate like the sessions bar, hover ignored.
    // Grid mode draws no tab strip, so row 0 belongs to the tiles there.
    if !state.grid_mode
        && areas.topbar.height > 0
        && mev.row >= areas.topbar.y
        && mev.row < areas.topbar.y + areas.topbar.height
    {
        let topbar = state.topbar();
        let buttons = ui::layout_topbar(areas.topbar, &topbar.tabs, state.pill_tabs);
        if let Some(index) = ui::topbar_at(&buttons, mev.column) {
            let button = &buttons[index];
            let area = ratatui::layout::Rect::new(button.start, areas.topbar.y, button.end - button.start, 1);
            if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
                && ui::ChromeButton::new("", ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
            {
                if state.manager.active().is_some() {
                    // Refit whenever a live pane tab wins: lazy tabs are
                    // born 24x80 and would otherwise render cornered.
                    if state.select_top_tab(index) && !state.overlay_active() {
                        fit_active_pane(state);
                    }
                    state.dirty = true;
                }
            }
        }
        return;
    }
    // Sidebar settings row: click-to-switch Off/Yolo like the bars; hover
    // and drags must never flip the live permission mode. Grid mode hides
    // the sidebar, so this whole region belongs to the tiles instead.
    if !state.grid_mode
        && areas.sidebar.width > 0
        && areas.sidebar.height > 0
        && mev.column >= areas.sidebar.x
        && mev.column < areas.sidebar.x + areas.sidebar.width
        && mev.row >= areas.sidebar.y
        && mev.row < areas.sidebar.y + areas.sidebar.height
    {
        // Armed-timer Cancel buttons: same rects the render paints,
        // recomputed live, so a repaint can never desync them.
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let info = state.sidebar_info();
            for (timer_id, area) in ui::timer_cancel_rects(areas.sidebar, &info, state.pill_tabs) {
                if ui::ChromeButton::new("[Cancel]", ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
                {
                    state.cancel_timer(&timer_id);
                    state.dirty = true;
                    return;
                }
            }
        }
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            // Same rich/compact split the render uses: compact buttons
            // follow the content-built row.
            let buttons = if areas.sidebar.width >= 40 && areas.sidebar.height >= 30 {
                ui::mode_button_areas(areas.sidebar, state.pill_tabs)
            } else {
                ui::compact_mode_buttons(areas.sidebar, &state.sidebar_info(), state.pill_tabs)
            };
            match ui::mode_at(&buttons, mev.column, mev.row) {
                Some("yolo") => {
                    if ui::ChromeButton::new("[Yolo]", ratatui::style::Style::default())
                        .click(mev.column, mev.row, buttons.yolo) {
                        state.set_permission_mode(crate::config::PermissionMode::Yolo);
                    }
                }
                Some(_) => {
                    if ui::ChromeButton::new("[Off]", ratatui::style::Style::default())
                        .click(mev.column, mev.row, buttons.off) {
                        state.set_permission_mode(crate::config::PermissionMode::Off);
                    }
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
        let segments = ui::session_bar_segments_for_area(&state.tabs(), areas.session_bar, state.pill_tabs);
        let buttons = ui::layout_session_bar(areas.session_bar, &segments);
        // Click-to-activate only: hover (Moved) and drags must never steal
        // the session; pane mouse protocols still get every event below.
        if let Some(index) = ui::session_at(&buttons, mev.column) {
            let button = buttons.iter().find(|b| b.index == Some(index)).unwrap();
            let area = ratatui::layout::Rect::new(button.start, areas.session_bar.y, button.end - button.start, 1);
            if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
                && ui::ChromeButton::new(&button.label, ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
            {
                if state.select_session(index) {
                    fit_active_pane(state);
                }
                state.dirty = true;
            }
        }
        return;
    }
    // An open tour owns the main area: the wheel scrolls its code
    // window and every other main-area event dies here, so clicks
    // never reach the agent pane hiding behind the tour. Chrome above
    // (tabs, sidebar, session bar) already returned, so those clicks
    // keep working.
    if state.walkthrough_overlay_active() {
        match mev.kind {
            event::MouseEventKind::ScrollUp => {
                if let Some(tour) = state.walkthrough_overlay_mut() {
                    tour.scroll_code(crate::walkthrough::WHEEL_SCROLL_LINES);
                    state.dirty = true;
                }
            }
            event::MouseEventKind::ScrollDown => {
                if let Some(tour) = state.walkthrough_overlay_mut() {
                    tour.scroll_code(-crate::walkthrough::WHEEL_SCROLL_LINES);
                    state.dirty = true;
                }
            }
            _ => {}
        }
        return;
    }
    // Grid mode owns the main area: a left-click focuses the clicked
    // tile and the wheel scrolls the focused pane's scrollback. App
    // mouse protocols stay quiet here: tile coordinates don't map onto
    // full-size panes, so forwarding them would mis-deliver.
    if state.grid_mode {
        let full = ratatui::layout::Rect::new(0, 0, cols, rows);
        let grid = ui::grid_area(full);
        let in_grid = mev.column >= grid.x
            && mev.column < grid.right()
            && mev.row >= grid.y
            && mev.row < grid.bottom();
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let order = state.manager.order().to_vec();
            let cells = ui::grid_cells(grid, order.len());
            if let Some(index) = ui::grid_cell_at(&cells, mev.column, mev.row) {
                if let Some(&id) = order.get(index) {
                    state.manager.switch(id);
                    state.dirty = true;
                }
            }
        } else if in_grid {
            match mev.kind {
                event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown => {
                    if let Some(active) = state.manager.active() {
                        let step = crate::pty::PtyPane::SCROLL_LINES_PER_NOTCH;
                        let delta = if matches!(mev.kind, event::MouseEventKind::ScrollUp) {
                            step
                        } else {
                            -step
                        };
                        state.manager.scroll_view(active, delta);
                        state.dirty = true;
                    }
                }
                _ => {}
            }
        }
        return;
    }
    let Some(active) = state.manager.active() else {
        return;
    };
    // A focused Visual tab owns the wheel (vertical pan) and its
    // zoom buttons; every other main-area event dies here so clicks
    // never reach the agent pane behind the diagram.
    #[cfg(feature = "visual")]
    if state.visual_tab_focused() {
        if let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) {
            let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
            match mev.kind {
                event::MouseEventKind::ScrollUp => {
                    state.visual_scroll(id, 0, -3, area_cols, area_rows, cell_w, cell_h);
                }
                event::MouseEventKind::ScrollDown => {
                    state.visual_scroll(id, 0, 3, area_cols, area_rows, cell_w, cell_h);
                }
                event::MouseEventKind::Down(event::MouseButton::Left) => {
                    if let Some(button) = ui::visual_button_at(&chrome, mev.column, mev.row) {
                        state.visual_zoom(id, button, area_cols, area_rows, cell_w, cell_h);
                    }
                }
                _ => {}
            }
        }
        return;
    }
    if state.overlay_active() { return; }
    let mode = state.manager.mouse_mode(active);
    if mode == vt100::MouseProtocolMode::None {
        // No mouse protocol: the wheel scrolls this pane instead of dying
        // silently, so every session scrolls whether or not its app
        // reports mouse. Normal screen moves the scrollback viewport;
        // the alternate screen has none, so scroll_view sends Up/Down
        // arrows for fullscreen apps that never take the mouse (codex,
        // claude). Other buttons still do nothing.
        if ui::translate_mouse(ui::pane_grid_area(&areas), mev.column, mev.row).is_some() {
            let step = crate::pty::PtyPane::SCROLL_LINES_PER_NOTCH;
            match mev.kind {
                event::MouseEventKind::ScrollUp => {
                    state.manager.scroll_view(active, step);
                    state.dirty = true;
                }
                event::MouseEventKind::ScrollDown => {
                    state.manager.scroll_view(active, -step);
                    state.dirty = true;
                }
                _ => {}
            }
        }
        return;
    }
    let Some((col, row)) = ui::translate_mouse(ui::pane_grid_area(&areas), mev.column, mev.row) else {
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
    let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    let main = ui::pane_grid_area(&areas);
    let pane_rows = main.height.saturating_sub(2).max(1);
    let pane_cols = main.width.saturating_sub(2).max(1);
    let _ = state.manager.resize(active, pane_rows, pane_cols);
}

/// Fit every session to its grid cell inner area, skipping cells with no
/// room and panes already at size (exited panes report none). Runs on
/// every dirty frame in grid mode, so spawns, exits, resizes, and the
/// toggle itself can never leave a tile showing an un-resized pane.
fn fit_grid_panes(state: &mut AppState) {
    let (rows, cols) = state.term_size;
    let grid = ui::grid_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let order = state.manager.order().to_vec();
    let cells = ui::grid_cells(grid, order.len());
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
    fn session_bar_click_switches_sessions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        // Session bar owns the last row; caps plus centering pads push
        // the second button to column 15, so this click lands on its
        // left cap.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 23,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        forward_mouse(&mut state, click);
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[1]));
        assert_eq!(state.manager.pane_size(order[1]), Some((20, 62)));
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
        assert_eq!(bar.tabs.len(), 3);
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

    #[cfg(feature = "visual")]
    fn open_visual_overlay(state: &mut AppState) -> crate::session::SessionId {
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: vec![1, 2, 3],
            rgba: Vec::new(),
            width: 1000,
            height: 1000,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
        });
        // Agent tabs: CLI, terminal, SCM, then Events/Tasks/Visual.
        assert!(state.select_top_tab(5));
        assert!(state.visual_tab_focused());
        id
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_arrows_pan_plus_minus_zoom_esc_leaves() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Contained at zoom 1, nothing to pan: keys are swallowed and
        // the viewport does not move.
        handle_key(&mut state, &mut router, key(KeyCode::Right));
        handle_key(&mut state, &mut router, key(KeyCode::Down));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (0, 0));
        assert!(state.visual_tab_focused(), "arrows stay in the tab");
        // Zoom in, then pan down and back up.
        handle_key(&mut state, &mut router, key(KeyCode::Char('+')));
        assert!(state.visual_slots.get(&id).unwrap().zoom > 1.0);
        handle_key(&mut state, &mut router, key(KeyCode::Down));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 1);
        handle_key(&mut state, &mut router, key(KeyCode::Up));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 0);
        handle_key(&mut state, &mut router, key(KeyCode::Char('-')));
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, 1.0);
        // Esc leaves the tab; other keys never reach the pane.
        handle_key(&mut state, &mut router, key(KeyCode::Char('x')));
        assert!(state.visual_tab_focused(), "plain keys are swallowed");
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        assert!(!state.visual_tab_focused());
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_wheel_pans_and_button_clicks_zoom() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let chrome = ui::visual_chrome(ui::pane_content_area(&areas), state.pill_tabs);
        let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
        for _ in 0..3 {
            assert!(state.visual_zoom(id, ui::VisualButton::ZoomIn, area_cols, area_rows, 8.0, 16.0));
        }
        let zoomed = state.visual_slots.get(&id).unwrap().zoom;
        let wheel = |kind| MouseEvent {
            kind, column: chrome.image.x + 2, row: chrome.image.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollDown));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 3);
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollUp));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 0);
        // Zoom-in button click; a click on the image itself is dead.
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row,
            modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, click(chrome.zoom_in.x, chrome.zoom_in.y));
        assert!(state.visual_slots.get(&id).unwrap().zoom > zoomed);
        let zoomed = state.visual_slots.get(&id).unwrap().zoom;
        forward_mouse(&mut state, click(chrome.image.x + 2, chrome.image.y + 2));
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, zoomed);
        forward_mouse(&mut state, click(chrome.zoom_out.x, chrome.zoom_out.y));
        assert!(state.visual_slots.get(&id).unwrap().zoom < zoomed);
        assert!(state.visual_tab_focused(), "clicks stay in the tab");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn topbar_click_opens_events_and_returns_to_agent() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        let click = |column| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row: areas.topbar.y,
            modifiers: KeyModifiers::NONE,
        };
        // Index 2 is the live SCM tab; Events overlay sits at 3.
        forward_mouse(&mut state, click(buttons[3].start));
        assert!(state.overlay_active());
        assert!(state.topbar().tabs[3].active);
        forward_mouse(&mut state, click(buttons[0].start));
        assert!(!state.overlay_active());
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_keys_drive_input_submit_and_leave() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let content = (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let tour = crate::walkthrough::Walkthrough::start(
            "Tour".to_string(),
            "f.rs".to_string(),
            &content,
            vec![crate::walkthrough::Step {
                start: 1,
                end: 3,
                explanation: "first".to_string(),
            }],
        ).unwrap();
        state.walkthroughs.insert(id, tour);
        // Walkthrough is the last overlay slot past the three PTY tabs.
        assert!(state.select_top_tab(6));
        assert!(state.walkthrough_overlay_active());
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        // Step chord on a single step clamps; the tour stays open.
        handle_walkthrough_key(&mut state, ch('j'));
        assert!(state.walkthrough_overlay_active());
        // Enter opens the ask prompt; typing fills the draft.
        handle_walkthrough_key(&mut state, enter);
        assert!(state.walkthrough_overlay().unwrap().input.is_some());
        handle_walkthrough_key(&mut state, ch('h'));
        handle_walkthrough_key(&mut state, ch('i'));
        assert_eq!(state.walkthrough_overlay().unwrap().input.as_deref(), Some("hi"));
        // Enter submits into the live pane; Esc in browse mode leaves.
        handle_walkthrough_key(&mut state, enter);
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.questions.len(), 1);
        assert_eq!(tour.questions[0].question, "hi");
        assert!(state.pending_enter.contains_key(&id));
        handle_walkthrough_key(&mut state, esc);
        assert!(state.walkthrough_overlay().is_none());
        assert!(!state.walkthrough_overlay_active());
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wheel_scrolls_tour_code_while_tabs_stay_clickable() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let content = (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let tour = crate::walkthrough::Walkthrough::start(
            "Tour".to_string(),
            "f.rs".to_string(),
            &content,
            vec![crate::walkthrough::Step {
                start: 1,
                end: 3,
                explanation: "first".to_string(),
            }],
        ).unwrap();
        state.walkthroughs.insert(id, tour);
        assert!(state.select_top_tab(6));
        let wheel = |kind| MouseEvent {
            kind, column: 90, row: 20, modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollDown));
        assert_eq!(state.walkthrough_overlay().unwrap().code_scroll, 3);
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollUp));
        assert_eq!(state.walkthrough_overlay().unwrap().code_scroll, 0);
        // Clicks in the tour pane never reach the agent, but the tab
        // strip above it still switches back to the CLI tab.
        let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        forward_mouse(&mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: buttons[0].start, row: areas.topbar.y, modifiers: KeyModifiers::NONE,
        });
        assert!(!state.walkthrough_overlay_active());
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn sidebar_cancel_click_drops_armed_timer() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        std::env::set_var("CODEX_BIN", "cat");
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let now = std::time::Instant::now();
        state.broker.call(&state.manager, &run, "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#, now).expect("arms");
        let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let rects = ui::timer_cancel_rects(areas.sidebar, &state.sidebar_info(), state.pill_tabs);
        assert_eq!(rects.len(), 1, "one armed timer, one button");
        let (_, area) = &rects[0];
        forward_mouse(&mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1, row: area.y, modifiers: KeyModifiers::NONE,
        });
        assert!(state.sidebar_info().session.unwrap().timers.is_empty(), "click cancels");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn topbar_click_fits_lazy_scm_pane_to_main() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let areas = ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        // Click the SCM tab: the pane is born 24x80 and must take the
        // main area at once (180x40 less top strip, session bar, and
        // main-pane borders), not render cornered.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: buttons[2].start,
                row: areas.topbar.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(state.manager.active_tab_kind(id), Some(crate::session::TabKind::Scm));
        assert_eq!(state.manager.pane_size(id), Some((36, 133)));
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
                    row: 23,
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
    fn wheel_scrolls_pane_scrollback_when_mouse_is_off() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(
            &mut state,
            "for i in $(seq 1 40); do echo line-$i; done; exec cat",
        );
        let id = state.manager.active().unwrap();
        assert_eq!(
            state.manager.mouse_mode(id),
            vt100::MouseProtocolMode::None
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let tail = state
                .manager
                .styled_rows(id)
                .last()
                .map(|r| r.iter().map(|c| c.text.clone()).collect::<String>())
                .unwrap_or_default();
            if tail.contains("line-40") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "history never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let areas = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 80, 24));
        let grid = crate::ui::pane_grid_area(&areas);
        let at = |kind| MouseEvent {
            kind,
            column: grid.x + grid.width / 2,
            row: grid.y + grid.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let screen_text = |state: &AppState| {
            state
                .manager
                .styled_rows(id)
                .iter()
                .flat_map(|r| r.iter().map(|c| c.text.clone()))
                .collect::<String>()
        };
        state.dirty = false;
        forward_mouse(&mut state, at(MouseEventKind::ScrollUp));
        assert!(state.dirty, "wheel marks redraw");
        assert!(
            !screen_text(&state).contains("line-40"),
            "wheel up leaves the live tail"
        );
        forward_mouse(&mut state, at(MouseEventKind::ScrollDown));
        assert!(
            screen_text(&state).contains("line-40"),
            "wheel down returns to live"
        );
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wheel_on_alt_screen_without_mouse_sends_arrows() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        // cat never takes the mouse; the escapes put its screen on the
        // alternate buffer like codex/claude, cursor parked at (9, 19).
        // Raw mode first: every real fullscreen app takes it, and without
        // it the line discipline echoes ESC back as `^[`, which the parser
        // prints instead of driving the cursor.
        spawn_shell_cmd(
            &mut state,
            "stty raw -echo; printf '\\033[?1049h\\033[10;20H'; exec cat",
        );
        let id = state.manager.active().unwrap();
        assert_eq!(
            state.manager.mouse_mode(id),
            vt100::MouseProtocolMode::None
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if state.manager.cursor(id) == Some((9, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "alt screen never engaged");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(state.manager.alternate_screen(id));
        let areas = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 80, 24));
        let grid = crate::ui::pane_grid_area(&areas);
        let at = |kind| MouseEvent {
            kind,
            column: grid.x + grid.width / 2,
            row: grid.y + grid.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        // Wheel up arrives as 3 Up arrows echoed back through cat and
        // lifts the cursor 3 rows; wheel down returns it.
        forward_mouse(&mut state, at(MouseEventKind::ScrollUp));
        loop {
            if state.manager.cursor(id) == Some((6, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "wheel up never moved the cursor");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        forward_mouse(&mut state, at(MouseEventKind::ScrollDown));
        loop {
            if state.manager.cursor(id) == Some((9, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "wheel down never moved the cursor");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(state.manager.remove(id));
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
        // Click Yolo to return (pill starts at x=74).
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 75,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::config::PermissionMode::Yolo);
    }

    #[test]
    fn prefix_q_asks_first_and_no_stays() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix);
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(state.quit_confirm.is_some(), "quit asks first");
        assert!(!state.should_quit, "nothing quits yet");
        // No is default: Enter stays.
        handle_quit_key(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.quit_confirm.is_none(), "confirm closed");
        assert!(!state.should_quit, "No keeps running");
    }

    #[test]
    fn prefix_q_then_y_quits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix);
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        handle_quit_key(&mut state, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(state.quit_confirm.is_none(), "confirm closed");
        assert!(state.should_quit, "Yes quits");
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
    fn prefix_w_toggles_grid() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix());
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('w'), none));
        assert!(state.grid_mode);
        handle_key(&mut state, &mut router, prefix());
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('w'), none));
        assert!(!state.grid_mode);
    }

    #[test]
    fn grid_click_focuses_cell() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 2);
        state.toggle_grid();
        // Click the middle of the first tile (cells tile 40x23 from y 0).
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 12,
                modifiers: none,
            },
        );
        assert_eq!(state.manager.active(), Some(order[0]));
        // Click the second tile.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 60,
                row: 12,
                modifiers: none,
            },
        );
        assert_eq!(state.manager.active(), Some(order[1]));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn grid_enter_fits_panes_to_cells_and_exit_restores() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 2);
        let active = state.manager.active().unwrap();
        assert_eq!(state.manager.pane_size(active), Some((20, 62)), "spawn fits main");
        // Grid enter fits every pane to its 40x23 tile inner.
        state.toggle_grid();
        fit_grid_panes(&mut state);
        assert_eq!(state.manager.pane_size(order[0]), Some((21, 38)));
        assert_eq!(state.manager.pane_size(order[1]), Some((21, 38)));
        // Idempotent and resize-aware: terminal growth refits all tiles.
        fit_grid_panes(&mut state);
        assert_eq!(state.manager.pane_size(order[1]), Some((21, 38)), "same size skips");
        state.apply(AppEvent::Resize(30, 100));
        fit_grid_panes(&mut state);
        assert_eq!(state.manager.pane_size(order[0]), Some((27, 48)));
        assert_eq!(state.manager.pane_size(order[1]), Some((27, 48)));
        // Grid exit restores the focused pane to full size.
        state.apply(AppEvent::Resize(24, 80));
        state.toggle_grid();
        fit_active_pane(&mut state);
        let active = state.manager.active().unwrap();
        assert_eq!(state.manager.pane_size(active), Some((20, 62)));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn prefix_x_terminates_active_session() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        assert_eq!(state.manager.order().len(), 2);
        let victim = state.manager.active().unwrap();
        state.broker.join(&state.manager, victim, "peers").unwrap();
        let mut router = InputRouter::new();
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('x'), none));
        assert!(state.manager.get(victim).is_none(), "record gone from UI");
        assert!(!state.broker.is_member(victim, "peers"), "left the group");
        assert_eq!(state.manager.order().len(), 1);
        assert_ne!(state.manager.active(), Some(victim));
        let survivor = state.manager.active().unwrap();
        assert!(state.manager.remove(survivor));
    }

    #[test]
    fn restore_picker_pick_spawns_and_esc_goes_fresh() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let entry = crate::checkpoint::SavedEntry {
            label: "a".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![crate::checkpoint::SavedSession {
                name: "a".to_string(),
                cli_tool: "codex".to_string(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                groups: vec![],
                harness_session_id: None,
            }],
        };
        let file = crate::checkpoint::SessionsFile {
            entries: vec![entry],
        };
        // Esc resolves fresh with no sessions and keeps the file.
        let mut state = AppState::new();
        state.restore_picker = crate::checkpoint::RestorePicker::new(&file);
        handle_restore_key(&mut state, KeyEvent::new(KeyCode::Esc, none));
        assert!(state.restore_picker.is_none());
        assert!(state.manager.order().is_empty());
        // Enter restores the entry.
        state.restore_picker = crate::checkpoint::RestorePicker::new(&file);
        handle_restore_key(&mut state, KeyEvent::new(KeyCode::Enter, none));
        assert!(state.restore_picker.is_none());
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 1);
        assert_eq!(state.manager.get(order[0]).unwrap().name, "a");
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
        assert!(state.manager.remove(order[0]));
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
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('g'), none));
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
    fn group_dialog_mouse_checks_session_then_enter_applies() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let id = state.manager.order()[0];
        state.apply_group(crate::groups::GroupOutcome::Create("team".into()));
        state.open_group_dialog();
        handle_group_key(&mut state, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let area = crate::groups::group_area(ratatui::layout::Rect::new(0, 0, 80, 24));
        // Padded content: header sits one row down, first session on
        // the row after it.
        forward_mouse(&mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 3,
            row: area.y + 3,
            modifiers: KeyModifiers::NONE,
        });
        handle_group_key(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.broker.is_member(id, "team"));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn dialog_submit_spawns_and_cancel_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        // The dialog only offers agents now; stand in a binary that exits
        // at once so the submit path stays hermetic.
        let saved = std::env::var("CLAUDE_BIN").ok();
        std::env::set_var("CLAUDE_BIN", "/bin/true");
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        state.open_create_dialog();
        // Prefilled claude-1 submits a real (stand-in) agent session.
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
        match saved {
            Some(v) => std::env::set_var("CLAUDE_BIN", v),
            None => std::env::remove_var("CLAUDE_BIN"),
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
