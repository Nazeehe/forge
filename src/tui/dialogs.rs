//! TUI dialogs: group, telegram, theme, oobe, confirm, quit-save outcomes.

use crossterm::event;

use crate::app::AppState;

use super::fit_active_pane;

/// One group-dialog key: mutations apply to the broker and stay open,
/// cancel closes, edits redraw.
pub(super) fn handle_group_key(state: &mut AppState, key: event::KeyEvent) {
    let ctx = state.group_ctx();
    let outcome = state.group_dialog.as_mut().map(|d| d.key(&key, &ctx));
    match outcome {
        Some(crate::ui::dialogs::groups::GroupOutcome::Closed) => {
            state.group_dialog = None;
            state.dirty = true;
        }
        Some(other) => {
            state.apply_group(other);
        }
        None => {}
    }
}

/// Settle one Telegram-settings outcome from keyboard or mouse:
/// submit saves and closes, test requests stay open, cancel closes,
/// edits redraw.
fn settle_telegram_outcome(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    outcome: Option<crate::ui::dialogs::telegram::TelegramOutcome>,
) {
    match outcome {
        Some(crate::ui::dialogs::telegram::TelegramOutcome::Submitted(form)) => {
            if let Err(e) = state.apply_telegram_form(loaded, home, form) {
                if let Some(dialog) = state.telegram_dialog.as_mut() {
                    dialog.set_error(e);
                }
            }
            state.dirty = true;
        }
        Some(crate::ui::dialogs::telegram::TelegramOutcome::Test { token }) => {
            state.start_telegram_test(token);
        }
        Some(crate::ui::dialogs::telegram::TelegramOutcome::Cancelled) => {
            state.telegram_dialog = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
}

/// One Telegram-settings key: Tab cycles rows, Enter fires.
pub(super) fn handle_telegram_key(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    key: event::KeyEvent,
) {
    let outcome = state.telegram_dialog.as_mut().map(|d| d.key(&key));
    settle_telegram_outcome(state, loaded, home, outcome);
}

/// One Telegram-settings click: rows take focus, buttons fire.
/// Clicks outside the modal die here so nothing behind it moves.
pub(super) fn handle_telegram_mouse(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    mev: event::MouseEvent,
) {
    if !matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
        return;
    }
    let (rows, cols) = state.term_size;
    let area = crate::ui::dialogs::telegram::telegram_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let outcome = state
        .telegram_dialog
        .as_mut()
        .and_then(|d| d.click(mev.column, mev.row, area));
    if outcome.is_some() {
        settle_telegram_outcome(state, loaded, home, outcome);
    }
}

/// Apply one theme-picker outcome: applying swaps the live theme
/// and persists `theme` in the config, cancel/close just redraws.
fn settle_theme_outcome(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    outcome: Option<crate::ui::dialogs::theme::ThemeOutcome>,
) {
    match outcome {
        Some(crate::ui::dialogs::theme::ThemeOutcome::Applied(name)) => {
            let themes = state.available_themes();
            if state.apply_theme_name(&name, &themes) {
                loaded.config.theme = name;
                if let Err(e) = loaded.save_home(home) {
                    eprintln!("warning: cannot persist theme: {e}");
                }
            }
            state.dirty = true;
        }
        Some(crate::ui::dialogs::theme::ThemeOutcome::Cancelled) => {
            state.theme_dialog = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
}

/// One theme-picker key: arrows move, Enter applies, Esc closes.
pub(super) fn handle_theme_key(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    key: event::KeyEvent,
) {
    let outcome = state.theme_dialog.as_mut().map(|d| d.key(&key));
    settle_theme_outcome(state, loaded, home, outcome);
}

/// One theme-picker click: rows take focus, Apply/Cancel fire.
/// Clicks outside the modal die here so nothing behind it moves.
pub(super) fn handle_theme_mouse(
    state: &mut AppState,
    loaded: &mut crate::infra::config::LoadedConfig,
    home: &std::path::Path,
    mev: event::MouseEvent,
) {
    if !matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
        return;
    }
    let (rows, cols) = state.term_size;
    let area = crate::ui::dialogs::theme::theme_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let outcome = state
        .theme_dialog
        .as_mut()
        .and_then(|d| d.click(mev.column, mev.row, area));
    if outcome.is_some() {
        settle_theme_outcome(state, loaded, home, outcome);
    }
}

/// Binary path the hook commands point at: this exe, like the
/// install-* subcommands in `main.rs`.
fn forge_binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "forge".to_string())
}

/// One first-run outcome: Setup installs hooks for the pick. A clean
/// sweep (every pick installed) closes at once; anything else (a skip,
/// an already-set-up entry, an error) reopens the dialog in results
/// mode so the miss is visible instead of silent. Esc closes bare.
/// `install-*` remains the repair path.
fn settle_oobe_outcome(
    state: &mut AppState,
    home: &std::path::Path,
    outcome: Option<crate::ui::dialogs::oobe::OobeOutcome>,
) {
    match outcome {
        Some(crate::ui::dialogs::oobe::OobeOutcome::Submitted(ids)) => {
            let outs = crate::ui::dialogs::oobe::install_selected(home, &ids, &forge_binary_path());
            if outs.iter().all(|o| o.installed) {
                state.oobe_dialog = None;
            } else {
                state.oobe_dialog =
                    Some(crate::ui::dialogs::oobe::OobeDialog::results(outs, state.pill_tabs));
            }
            state.dirty = true;
        }
        Some(crate::ui::dialogs::oobe::OobeOutcome::Dismissed) => {
            state.oobe_dialog = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
}

/// One first-run key: Enter installs hooks for the checked CLIs.
pub(super) fn handle_oobe_key(state: &mut AppState, home: &std::path::Path, key: event::KeyEvent) {
    let outcome = state.oobe_dialog.as_mut().map(|d| d.key(&key));
    settle_oobe_outcome(state, home, outcome);
}

/// One first-run click: a CLI row toggles, the Setup button installs.
pub(super) fn handle_oobe_mouse(state: &mut AppState, home: &std::path::Path, mev: event::MouseEvent) {
    if !matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
        return;
    }
    let (rows, cols) = state.term_size;
    let area = crate::ui::dialogs::oobe::oobe_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let outcome = state
        .oobe_dialog
        .as_mut()
        .map(|d| d.click(mev.column, mev.row, area));
    settle_oobe_outcome(state, home, outcome);
}

/// One quit-save step, after the saving modal paints: persist the live
/// sessions once, then let the loop exit. Failures warn instead of
/// failing the exit, like the post-loop save. Returns true when a
/// pending save ran.
pub(super) fn settle_quit_save(state: &mut AppState, home: &std::path::Path) -> bool {
    if !state.quit_saving {
        return false;
    }
    state.quit_saving = false;
    // Boards flush here too: the loop's per-frame flush may never run
    // between the last mutation and this exit.
    state.flush_boards(home);
    if let Err(e) = crate::session::checkpoint::save_quit_snapshot(
        &crate::infra::branding::sessions_file(home),
        state.snapshot_sessions(),
    ) {
        eprintln!("warning: cannot save sessions: {e}");
    }
    true
}

/// One confirm-modal key: Yes runs the confirmed action (quit forge
/// or kill the targeted session), anything else keeps running.
pub(super) fn handle_confirm_key(state: &mut AppState, key: event::KeyEvent) {
    let outcome = state.confirm.as_mut().map(|d| d.key(&key));
    match outcome {
        Some(crate::ui::dialogs::quit::ConfirmOutcome::Confirmed) => {
            match state.confirm.as_ref().map(|d| d.kind()) {
                Some(crate::ui::dialogs::quit::ConfirmKind::QuitForge) => {
                    state.confirm = None;
                    // Yes swaps the confirm for the saving modal: it
                    // paints this tick, then the loop persists the
                    // snapshot and exits.
                    state.quit_saving = true;
                    state.should_quit = true;
                }
                Some(crate::ui::dialogs::quit::ConfirmKind::KillSession(id)) => {
                    state.confirm = None;
                    state.terminate_session(id);
                    fit_active_pane(state);
                }
                None => {}
            }
            state.dirty = true;
        }
        Some(crate::ui::dialogs::quit::ConfirmOutcome::Dismissed) => {
            state.confirm = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
}

/// One dialog key: submit spawns and fits, cancel closes, edits redraw.
pub(super) fn handle_dialog_key(state: &mut AppState, key: event::KeyEvent) {
    let names = state.live_names();
    let outcome = state.create_dialog.as_mut().map(|d| d.key(&key, &names));
    match outcome {
        Some(crate::ui::dialogs::create::DialogOutcome::Submitted(spec)) => {
            state.create_dialog = None;
            if state.create_session(&spec).is_ok() {
                fit_active_pane(state);
            }
            state.dirty = true;
        }
        Some(crate::ui::dialogs::create::DialogOutcome::Cancelled) => {
            state.create_dialog = None;
            state.dirty = true;
        }
        _ => {
            state.dirty = true;
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::event::AppEvent;
    use crate::infra::ids::RunId;
    use crate::tui::input::InputRouter;
    use crate::tui::keys::{handle_key, spawn_shell_cmd};
    use crate::tui::mouse::forward_mouse;
    use crate::ui;

    #[test]
    fn quit_save_flushes_pending_boards() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static QUIT_HOME_COUNTER: AtomicU64 = AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "forge-quit-board-test-{}-{}",
            std::process::id(),
            QUIT_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        state.boards_dirty = true;
        state.quit_saving = true;
        assert!(settle_quit_save(&mut state, &home));
        assert!(!state.boards_dirty, "quit flushes like the frame loop");
        let mut fresh = AppState::new();
        fresh.load_boards(&home);
        assert_eq!(fresh.boards.board_names(), vec!["team".to_string()]);
    }

    #[test]
    fn quit_save_persists_snapshot_and_clears_flag() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "forge-quit-saving-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // Nothing pending: no save, no file.
        let mut idle = AppState::new();
        assert!(!settle_quit_save(&mut idle, &home), "idle saves nothing");
        assert!(
            !crate::infra::branding::sessions_file(&home).exists(),
            "idle writes nothing"
        );
        // One live agent: the pending save persists it and clears the flag.
        let mut state = AppState::new();
        let id = state
            .manager
            .spawn_agent(
                "qsave",
                &std::env::temp_dir(),
                "exec sleep 30",
                crate::infra::ids::RunId::generate(),
                "claude",
            )
            .expect("spawn agent");
        state.quit_saving = true;
        assert!(settle_quit_save(&mut state, &home), "pending save runs");
        assert!(!state.quit_saving, "flag clears after save");
        let text = std::fs::read_to_string(crate::infra::branding::sessions_file(&home))
            .expect("sessions file written");
        assert!(text.contains("qsave"), "snapshot saved: {text}");
        assert!(state.manager.remove(id), "cleanup pane");
        let _ = std::fs::remove_dir_all(&home);
    }

    use crate::tui::test_support::telegram_button_cell;

    fn telegram_test_ctx() -> (crate::infra::config::LoadedConfig, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("forge-tg-click-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let loaded = crate::infra::config::LoadedConfig::load(&home.join("config.toml"))
            .expect("missing file loads defaults");
        (loaded, home)
    }

    #[test]
    fn telegram_modal_click_cancel_closes_and_keeps_session() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        state.open_telegram_dialog();
        let (col, row) = telegram_button_cell(&mut state, "Cancel");
        let (mut loaded, home) = telegram_test_ctx();
        handle_telegram_mouse(
            &mut state, &mut loaded, &home,
            MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE },
        );
        assert!(state.telegram_dialog.is_none(), "Cancel click closes");
        assert_eq!(state.manager.active_tab_kind(id), Some(crate::session::TabKind::Agent));
        let _ = std::fs::remove_dir_all(&home);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn telegram_modal_swallows_chrome_clicks() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        state.open_telegram_dialog();
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let (mut loaded, home) = telegram_test_ctx();
        // Topbar click behind the modal: nothing switches, modal stays.
        handle_telegram_mouse(
            &mut state, &mut loaded, &home,
            MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 10, row: areas.topbar.y, modifiers: KeyModifiers::NONE },
        );
        assert!(state.telegram_dialog.is_some(), "modal stays open");
        assert_eq!(state.manager.active_tab_kind(id), Some(crate::session::TabKind::Agent));
        assert!(state.overlay_view.is_none(), "no overlay opens behind the modal");
        let _ = std::fs::remove_dir_all(&home);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn kill_confirm_no_keeps_session() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let id = state.manager.active().unwrap();
        state.open_kill_confirm();
        assert!(state.confirm.is_some(), "kill confirm opens");
        // No is default: Enter keeps the session.
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.confirm.is_none(), "confirm closed");
        assert!(state.manager.get(id).is_some(), "session kept");
        assert!(!state.should_quit, "keeping never quits");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn kill_confirm_yes_terminates_session() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let victim = state.manager.active().unwrap();
        state.broker.join(&state.manager, victim, "peers").unwrap();
        let mut router = InputRouter::new();
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('x'), none));
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Char('y'), none));
        assert!(state.confirm.is_none(), "confirm closed");
        assert!(state.manager.get(victim).is_none(), "record gone from UI");
        assert!(!state.broker.is_member(victim, "peers"), "left the group");
        assert_eq!(state.manager.order().len(), 1);
        assert_ne!(state.manager.active(), Some(victim));
        let survivor = state.manager.active().unwrap();
        assert!(state.manager.remove(survivor));
    }

    #[test]
    fn oobe_enter_installs_hooks_for_all_and_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let home = std::env::temp_dir().join(format!(
            "forge-oobe-wire-{}-enter",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // Pre-install claude's hooks so Setup yields an `unchanged` miss
        // instead of a clean sweep: the miss must reopen in results mode.
        let pre = crate::ui::dialogs::oobe::install_selected(
            &home,
            &["claude".to_string()],
            &forge_binary_path(),
        );
        assert!(pre.iter().all(|o| o.installed), "pre-installs: {pre:?}");
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        state.open_oobe_dialog();
        assert!(state.oobe_dialog.is_some(), "first run opens setup");
        // Uncheck codex and muse: claude submits from a pure
        // home-relative path, and the test never consults CODEX_HOME
        // or real config paths.
        let none = KeyModifiers::NONE;
        for _ in 0..2 {
            handle_oobe_key(&mut state, &home, KeyEvent::new(KeyCode::Down, none));
            handle_oobe_key(&mut state, &home, KeyEvent::new(KeyCode::Char(' '), none));
        }
        handle_oobe_key(
            &mut state,
            &home,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        // Already set up is a miss, so Setup lands on the results screen
        // instead of closing: the miss must be visible, not silent.
        assert!(state.oobe_dialog.is_some(), "results open");
        assert!(
            state.oobe_dialog.as_ref().is_some_and(|d| d.done()),
            "results mode"
        );
        assert!(
            !home.join(".codex/hooks.json").exists(),
            "unchecked CLI untouched"
        );
        assert!(
            !home.join(".config/muse/settings.json").exists(),
            "unchecked CLI untouched"
        );
        // Done closes the results.
        handle_oobe_key(
            &mut state,
            &home,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(state.oobe_dialog.is_none(), "results close");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oobe_all_installed_closes_without_results() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let home = std::env::temp_dir().join(format!(
            "forge-oobe-wire-{}-clean",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        state.open_oobe_dialog();
        // Uncheck everything but claude: a clean install closes at once.
        let none = KeyModifiers::NONE;
        for _ in 0..2 {
            handle_oobe_key(&mut state, &home, KeyEvent::new(KeyCode::Down, none));
            handle_oobe_key(&mut state, &home, KeyEvent::new(KeyCode::Char(' '), none));
        }
        handle_oobe_key(
            &mut state,
            &home,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(state.oobe_dialog.is_none(), "clean setup closes");
        let text =
            std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert!(text.contains("hook-relay"), "hooks installed: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oobe_esc_skips_without_touching_home() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let home = std::env::temp_dir().join(format!(
            "forge-oobe-wire-{}-esc",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        state.open_oobe_dialog();
        handle_oobe_key(
            &mut state,
            &home,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(state.oobe_dialog.is_none(), "skip closes");
        assert!(
            !home.join(".claude/settings.json").exists(),
            "no hooks installed"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oobe_swallows_clicks_behind_the_modal() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        state.open_oobe_dialog();
        // Session-bar click that would switch sessions with no modal.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 15,
                row: 23,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[0]), "click died");
        assert!(state.oobe_dialog.is_some(), "setup stays open");
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn group_dialog_mouse_checks_session_then_enter_applies() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let id = state.manager.order()[0];
        state.apply_group(crate::ui::dialogs::groups::GroupOutcome::Create("team".into()));
        state.open_group_dialog();
        handle_group_key(&mut state, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let area = crate::ui::dialogs::groups::group_area(ratatui::layout::Rect::new(0, 0, 80, 24));
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
        // at once so the submit path stays hermetic. Serialized against
        // every other CLAUDE_BIN mutator (see CLAUDE_BIN_LOCK).
        let _claude_bin = crate::app::test_support::CLAUDE_BIN_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

}
