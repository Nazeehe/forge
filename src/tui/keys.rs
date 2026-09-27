//! TUI key handling: dispatch, prefix commands, board/tetris/tour/restore keys.

use std::time::Instant;

use crossterm::event;

use crate::app::AppState;

use super::fit_active_pane;
use super::input;
use super::input::{InputRouter, RoutedKey, UserCommand};
#[cfg(feature = "visual")]
use super::visual::handle_visual_key;
use crate::infra::ids::RunId;

pub(super) fn handle_key(state: &mut AppState, router: &mut InputRouter, key: event::KeyEvent) {
    handle_key_at(state, router, key, Instant::now());
}

pub(super) fn handle_key_at(
    state: &mut AppState,
    router: &mut InputRouter,
    key: event::KeyEvent,
    now: Instant,
) {
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
    // A pinned which-key HUD owns its keys the same way: browse the
    // map instead of typing into the pane.
    if state.whichkey.pinned() {
        handle_pinned_key(state, router, key);
        return;
    }
    // The open board owns plain keys so `j` moves the cursor instead
    // of typing into a pane; the prefix chord still escapes to the
    // router so `Ctrl-b b` (and every other command) keeps working.
    if state.board_open && !InputRouter::is_prefix(&key) && !router.is_pending() {
        handle_board_key(state, key);
        return;
    }
    // The open Tetris game owns its movement keys so arrows play the
    // game instead of typing into a pane; the prefix chord still
    // escapes to the router so `Ctrl-b r` (and every other command)
    // keeps working. Board keys win while the board is open, letters
    // always fall through so typing to agents never breaks mid-game.
    if state.tetris_open && !state.board_open && !InputRouter::is_prefix(&key) && !router.is_pending() {
        if handle_tetris_key(state, key) {
            return;
        }
    }
    match router.feed(key) {
        RoutedKey::Forward(k) => {
            state.whichkey.note_resolved();
            forward_to_pane(state, k);
        }
        RoutedKey::Help => {
            state.whichkey.show_pinned();
            state.dirty = true;
        }
        RoutedKey::Command(cmd) => {
            state.whichkey.note_resolved();
            fire_command(state, cmd);
        }
        RoutedKey::PrefixPending => {
            state.whichkey.note_pending(now);
            state.dirty = true;
        }
        RoutedKey::Cancelled => {
            state.whichkey.note_resolved();
            state.dirty = true;
        }
    }
}

/// One key while the which-key HUD is pinned open for browsing: Esc
/// closes, the prefix key keeps the map up, and every other key
/// dispatches as the second half of the prefix — the same keystrokes
/// experts type blind. Unknown keys close the HUD and fall through
/// to the pane, exactly like an unpinned prefix.
fn handle_pinned_key(
    state: &mut AppState,
    router: &mut InputRouter,
    key: event::KeyEvent,
) {
    use crossterm::event::KeyCode;
    if key.code == KeyCode::Esc && key.modifiers.is_empty() {
        state.whichkey.hide();
        state.dirty = true;
        return;
    }
    if InputRouter::is_prefix(&key) {
        // Still browsing: the prefix alone changes nothing.
        state.dirty = true;
        return;
    }
    if !router.is_pending() {
        let _ = router.feed(input::prefix_key());
    }
    match router.feed(key) {
        RoutedKey::Command(cmd) => {
            state.whichkey.hide();
            fire_command(state, cmd);
        }
        RoutedKey::Help => {
            // `?` while browsing: still browsing.
            state.dirty = true;
        }
        RoutedKey::Cancelled => {
            state.whichkey.hide();
            state.dirty = true;
        }
        RoutedKey::Forward(k) => {
            state.whichkey.hide();
            forward_to_pane(state, k);
        }
        RoutedKey::PrefixPending => {
            state.dirty = true;
        }
    }
}

/// Forward one key to the active pane, exactly as unprefixed input.
fn forward_to_pane(state: &mut AppState, key: event::KeyEvent) {
    if state.overlay_active() {
        return;
    }
    if let Some(active) = state.manager.active() {
        let app_cursor = state.manager.app_cursor(active);
        if let Some(bytes) = input::encode_key(&key, app_cursor) {
            if state.manager.pane_write(active, &bytes).is_ok() {
                state.note_human_input(active);
            }
        }
    }
}

/// Fire one prefix command. Shared by the prefix path and the pinned
/// which-key browser so both stay on the same dispatch table.
fn fire_command(state: &mut AppState, cmd: UserCommand) {
    match cmd {
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
        UserCommand::FleetStep(dir) => {
            if !state.grid_mode {
                state.fleet_step(dir);
            }
            state.dirty = true;
        }
        UserCommand::FleetActivate => {
            if !state.grid_mode && state.fleet_activate() {
                fit_active_pane(state);
            }
            state.dirty = true;
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
            // Kill asks first: Yes terminates the session that was
            // active here, No keeps it.
            state.open_kill_confirm();
            fit_active_pane(state);
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
        UserCommand::ToggleBoard => {
            state.toggle_board();
            if !state.board_open {
                fit_active_pane(state);
            }
        }
        UserCommand::ToggleTetris => {
            state.toggle_tetris();
        }
        UserCommand::TelegramSettings => {
            state.open_telegram_dialog();
        }
        UserCommand::ThemePicker => {
            let themes = state.available_themes();
            state.open_theme_dialog(themes);
        }
        UserCommand::HelpManual => {
            if let Err(e) = crate::ui::help::open() {
                eprintln!("warning: cannot open help manual: {e}");
            }
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

/// One key inside the open sidebar Tetris: arrows move, Up turns,
/// Space drops, `p` pauses, `r` restarts. Ctrl/Alt chords never reach
/// here — the dispatcher keeps them for the prefix path — and every
/// other key (letters included) falls through to the agent pane, so
/// typing to agents never breaks mid-game. True when consumed.
fn handle_tetris_key(state: &mut AppState, key: event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return false;
    }
    if !key.modifiers.is_empty() {
        return false;
    }
    match key.code {
        KeyCode::Left => {
            state.tetris.try_move(-1, 0);
        }
        KeyCode::Right => {
            state.tetris.try_move(1, 0);
        }
        KeyCode::Down => {
            state.tetris.try_move(0, 1);
        }
        KeyCode::Up => {
            state.tetris.rotate_cw();
        }
        KeyCode::Char(' ') => {
            state.tetris.hard_drop();
        }
        KeyCode::Char('p') | KeyCode::Char('P') => {
            state.tetris.toggle_pause();
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            state.tetris.restart();
            state.tetris_last_drop = None;
        }
        _ => return false,
    }
    state.dirty = true;
    true
}

/// One key inside the open kanban board: the blueprint's clikan map
/// plus the `a` add draft and the `e` full-field editor modal.
/// Ctrl/Alt chords never reach here — the dispatcher keeps them for
/// the prefix path — and `q`/Esc leaves the board.
fn handle_board_key(state: &mut AppState, key: event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return;
    }
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    if !plain {
        return;
    }
    // The open card editor owns every plain key: typing edits the
    // focused row, Enter saves, Esc cancels, navigation never fires
    // mid-edit. The single-line add draft owns its keys the same way.
    if state.card_edit.is_some() {
        state.board_card_edit_key(&key);
        return;
    }
    if state.board_draft.is_some() {
        state.board_draft_key(&key);
        return;
    }
    let lower = match key.code {
        KeyCode::Char(c) if c.is_ascii_uppercase() && key.modifiers == KeyModifiers::SHIFT => {
            c.to_ascii_lowercase()
        }
        KeyCode::Char(c) => c,
        _ => '\0',
    };
    match key.code {
        KeyCode::Esc => state.toggle_board(),
        KeyCode::Enter if key.modifiers.is_empty() => state.board_enter_details(),
        KeyCode::Tab if key.modifiers.is_empty() => state.board_step_column(1),
        KeyCode::BackTab => state.board_step_column(-1),
        KeyCode::Left if key.modifiers.is_empty() => state.board_step_column(-1),
        KeyCode::Right if key.modifiers.is_empty() => state.board_step_column(1),
        KeyCode::Up if key.modifiers.is_empty() => state.board_step_card(-1),
        KeyCode::Down if key.modifiers.is_empty() => state.board_step_card(1),
        KeyCode::Home if key.modifiers.is_empty() => state.board_focus_card_edge(false),
        KeyCode::End if key.modifiers.is_empty() => state.board_focus_card_edge(true),
        KeyCode::Backspace if key.modifiers.is_empty() => state.board_shift_focused_card(-1),
        KeyCode::Char('1'..='9') => {
            state.board_focus_column_index((lower as u8 - b'1') as usize);
        }
        _ => match lower {
            'h' => state.board_step_column(-1),
            'l' => state.board_step_column(1),
            // Shift-held J/K belong to reorder below, never the cursor.
            'j' if key.modifiers.is_empty() => state.board_step_card(1),
            'k' if key.modifiers.is_empty() => state.board_step_card(-1),
            'g' => {
                if key.code == KeyCode::Char('G') {
                    state.board_focus_card_edge(true);
                } else {
                    state.board_focus_card_edge(false);
                }
            }
            ' ' | '.' => state.board_shift_focused_card(1),
            ',' => state.board_shift_focused_card(-1),
            '>' => state.board_send_focused_card(true),
            '<' => state.board_send_focused_card(false),
            'x' => state.board_complete_focused(),
            'd' => state.board_delete_focused(),
            'p' => state.board_cycle_priority_focused(),
            '+' | '=' => state.board_bump_progress_focused(10),
            '-' | '_' => state.board_bump_progress_focused(-10),
            'w' => state.board_cycle_board(),
            'r' => {
                state.ensure_board_focus();
                state.dirty = true;
            }
            'q' => state.toggle_board(),
            'a' => state.board_start_add_draft(),
            'e' => state.board_start_title_draft(),
            _ => {}
        },
    }
    // Uppercase move/reorder chords arrive lowercased above; handle the
    // shift-held originals here so `K` never reads as `k`.
    if key.modifiers == KeyModifiers::SHIFT {
        match key.code {
            KeyCode::Char('J') => state.board_reorder_focused(crate::kanban::board::Shift::Down),
            KeyCode::Char('K') => state.board_reorder_focused(crate::kanban::board::Shift::Up),
            KeyCode::Char('T') => state.board_reorder_focused(crate::kanban::board::Shift::Top),
            KeyCode::Char('B') => state.board_reorder_focused(crate::kanban::board::Shift::Bottom),
            _ => {}
        }
    }
}

/// One restore-picker key: a pick recreates the whole entry and fits
/// the panes, Esc starts fresh. Either way the picker closes.
pub(super) fn handle_restore_key(state: &mut AppState, key: event::KeyEvent) {
    let outcome = state.restore_picker.as_mut().map(|p| p.key(&key));
    match outcome {
        Some(crate::session::checkpoint::RestoreOutcome::Pick(_)) => {
            let entry = state.restore_picker.as_ref().and_then(|p| p.take_selected());
            state.restore_picker = None;
            if let Some(entry) = entry {
                state.restore_entry(&entry);
                fit_active_pane(state);
            }
            state.dirty = true;
        }
        Some(crate::session::checkpoint::RestoreOutcome::Fresh) => {
            state.restore_picker = None;
            state.dirty = true;
        }
        Some(crate::session::checkpoint::RestoreOutcome::Pending) | None => {}
    }
}

pub(super) fn spawn_shell_cmd(state: &mut AppState, cmd: &str) {
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


#[cfg(test)]
mod tests {
    use super::super::fit_grid_panes;
    use super::*;
    use crate::infra::event::AppEvent;
    use crate::tui::dialogs::{handle_confirm_key, handle_group_key, handle_theme_key};
    use crate::tui::mouse::forward_mouse;

    #[test]
    fn prefix_jk_moves_cursor_and_enter_activates() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::tui::input::{prefix_key, InputRouter};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key(&mut state, &mut router, prefix_key());
        handle_key(&mut state, &mut router, key(KeyCode::Char('j')));
        handle_key(&mut state, &mut router, prefix_key());
        handle_key(&mut state, &mut router, key(KeyCode::Char('j')));
        assert_eq!(state.fleet_cursor, Some(order[1]), "two steps reach b");
        handle_key(&mut state, &mut router, prefix_key());
        handle_key(&mut state, &mut router, key(KeyCode::Enter));
        assert_eq!(state.manager.active(), Some(order[1]), "enter activates");
        for id in order {
            assert!(state.manager.remove(id));
        }
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
        assert!(state.select_top_tab(4));
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
    fn tetris_arrows_drive_game_letters_fall_through() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        state.toggle_tetris();
        let before = state.tetris.active_cells();
        state.dirty = false;
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            std::time::Instant::now(),
        );
        assert_ne!(state.tetris.active_cells(), before, "arrow moves the piece");
        assert!(state.dirty, "game keys repaint");
        let before = state.tetris.active_cells();
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            std::time::Instant::now(),
        );
        assert_eq!(state.tetris.active_cells(), before, "typing never plays");
    }

    #[test]
    fn board_keys_drive_cursor_and_mutations() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        let id = state
            .boards
            .board_mut("team")
            .unwrap()
            .card_create(crate::kanban::board::CardDraft::new("ship"))
            .unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        // Cursor steps; x completes the focused card.
        handle_board_key(&mut state, ch('l'));
        assert_eq!(state.board_focus.column, 1);
        handle_board_key(&mut state, ch('h'));
        assert_eq!(state.board_focus.column, 0);
        handle_board_key(&mut state, ch('x'));
        let card = state.boards.board("team").unwrap().card(&id).unwrap();
        assert_eq!((card.column.as_str(), card.progress), ("Done", 100));
        assert!(state.boards_dirty, "mutations persist");
        // d deletes with footer confirmation.
        handle_board_key(&mut state, ch('d'));
        assert!(state.boards.board("team").unwrap().card(&id).is_none());
        assert!(state.board_notice.is_some());
        // Esc leaves the board; sessions are untouched.
        handle_board_key(&mut state, esc);
        assert!(!state.board_open);
    }

    #[test]
    fn board_a_types_and_enter_creates_board() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.board_open = true;
        // Reported gap: with no boards, `a` left the human stranded
        // with a notice and no way to create anything.
        handle_board_key(&mut state, ch('a'));
        assert!(state.board_draft.is_some(), "a opens a draft, not a dead end");
        for c in "team".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, enter);
        assert!(state.board_draft.is_none(), "submit closes the draft");
        assert_eq!(state.boards.board_names(), vec!["team".to_string()]);
        assert!(state.board_open, "still looking at the board");
    }

    #[test]
    fn board_a_adds_card_and_e_edits_its_title() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        handle_board_key(&mut state, ch('a'));
        for c in "first".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, enter);
        let board = state.boards.board("team").unwrap();
        assert_eq!(board.cards.len(), 1);
        assert_eq!(board.cards[0].column, "Backlog", "lands in the focused column");
        assert!(state.boards_dirty, "human adds persist");
        // `e` opens the full-field editor prefilled with the card;
        // typing on the title row appends to it, Enter saves.
        handle_board_key(&mut state, ch('e'));
        assert!(state.card_edit.is_some(), "e opens the card editor");
        for c in " v2".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, enter);
        assert!(state.card_edit.is_none(), "save closes the editor");
        assert_eq!(state.boards.board("team").unwrap().cards[0].title, "first v2");
    }

    #[test]
    fn board_e_opens_full_card_editor_prefilled() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        state
            .boards
            .board_mut("team")
            .unwrap()
            .card_create(crate::kanban::board::CardDraft::new("first"))
            .unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        handle_board_key(&mut state, ch('e'));
        assert!(state.card_edit.is_some(), "e opens the full card editor");
        assert!(state.board_draft.is_none(), "no single-line draft behind the modal");
    }

    #[test]
    fn board_editor_saves_every_field_and_moves_status() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        let id = state
            .boards
            .board_mut("team")
            .unwrap()
            .card_create(crate::kanban::board::CardDraft::new("first"))
            .unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        handle_board_key(&mut state, ch('e'));
        // Jump to the assignee row (title 0, desc 1, status 2, assignee 3)
        // and claim the card; the status cycler moves Backlog -> Todo.
        for _ in 0..3 {
            handle_board_key(&mut state, tab);
        }
        for c in "sam".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        handle_board_key(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );
        handle_board_key(&mut state, enter);
        assert!(state.card_edit.is_none(), "save closes the editor");
        let card = state.boards.board("team").unwrap().card(&id).unwrap();
        assert_eq!(card.assignee, "sam", "assignee saved");
        assert_eq!(card.column, "Todo", "status move applied");
        assert!(state.boards_dirty, "editor saves persist");
        assert!(
            state.board_notice.is_some_and(|n| n.contains("updated")),
            "save confirms in the footer"
        );
    }

    #[test]
    fn board_editor_esc_cancels_without_touching_the_card() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        let id = state
            .boards
            .board_mut("team")
            .unwrap()
            .card_create(crate::kanban::board::CardDraft::new("first"))
            .unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        handle_board_key(&mut state, ch('e'));
        for c in " v2".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, esc);
        assert!(state.card_edit.is_none(), "esc closes the editor");
        assert_eq!(
            state.boards.board("team").unwrap().card(&id).unwrap().title,
            "first",
            "cancelled typing never lands"
        );
        assert!(!state.boards_dirty, "cancel persists nothing");
    }

    #[test]
    fn board_draft_esc_cancels_and_blank_enter_keeps() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.board_open = true;
        handle_board_key(&mut state, ch('a'));
        handle_board_key(&mut state, esc);
        assert!(state.board_draft.is_none(), "esc cancels");
        assert!(state.boards.boards.is_empty(), "nothing created");
        // Blank submit stays open with guidance instead of an
        // "empty name" error that eats the draft.
        handle_board_key(&mut state, ch('a'));
        handle_board_key(&mut state, enter);
        assert!(state.board_draft.is_some(), "blank keeps the draft");
        assert!(state.board_notice.is_some(), "guidance in the footer");
        // Navigation never fires mid-draft: `j` types, it doesn't move.
        handle_board_key(&mut state, ch('j'));
        assert_eq!(state.board_draft.as_ref().map(|d| d.buffer.as_str()), Some("j"));
    }

    #[test]
    fn board_full_column_keeps_card_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ch = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        for title in ["one", "two", "three"] {
            let id = state
                .boards
                .board_mut("team")
                .unwrap()
                .card_create(crate::kanban::board::CardDraft::new(title))
                .unwrap();
            state.boards.board_mut("team").unwrap().card_move(&id, "Doing").unwrap();
        }
        state.board_open = true;
        state.ensure_board_focus();
        state.board_step_column(2);
        handle_board_key(&mut state, ch('a'));
        for c in "fourth".chars() {
            handle_board_key(&mut state, ch(c));
        }
        handle_board_key(&mut state, enter);
        // WIP blocks the create but the text survives to be fixed.
        assert!(state.board_draft.is_some(), "draft kept on failure");
        assert_eq!(
            state.board_draft.as_ref().map(|d| d.buffer.as_str()),
            Some("fourth")
        );
        assert!(state.board_notice.is_some_and(|n| n.contains("WIP")));
        assert_eq!(state.boards.board("team").unwrap().cards.len(), 3);
    }

    #[test]
    fn board_open_keeps_prefix_escapes() {
        use crate::tui::input::{prefix_key, InputRouter};
        let mut state = AppState::new();
        state.boards.board_create("team", None).unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        let mut router = InputRouter::new();
        // The prefix still reaches the router: Ctrl-b b can leave.
        handle_key_at(&mut state, &mut router, prefix_key(), std::time::Instant::now());
        assert!(router.is_pending(), "prefix escapes the board");
        handle_key_at(&mut state, &mut router, {
            use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)
        }, std::time::Instant::now());
        assert!(!state.board_open, "Ctrl-b b closes the board");
    }

    #[test]
    fn prefix_pause_shows_hud_only_after_delay() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::{Duration, Instant};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let t0 = Instant::now();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key_at(&mut state, &mut router, prefix, t0);
        assert!(router.is_pending(), "prefix waits for its second key");
        assert!(!state.whichkey.visible(), "no instant flash");
        assert!(
            !state.whichkey.poll(true, t0 + Duration::from_millis(50)),
            "fast typists stay clean"
        );
        assert!(!state.whichkey.visible());
        assert!(
            state.whichkey.poll(
                true,
                t0 + Duration::from_millis(crate::ui::whichkey::WHICHKEY_DELAY_MS)
            ),
            "the pause earns its HUD"
        );
        assert!(state.whichkey.visible());
    }

    #[test]
    fn fast_prefix_sequence_never_paints_hud() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::{Duration, Instant};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let t0 = Instant::now();
        let none = KeyModifiers::NONE;
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            t0,
        );
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('w'), none),
            t0 + Duration::from_millis(50),
        );
        assert!(!state.whichkey.visible(), "experts never see the overlay");
        assert!(!router.is_pending());
        assert!(state.grid_mode, "the command still fired");
    }

    #[test]
    fn explicit_help_pins_and_command_key_fires_and_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::Instant;
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let t0 = Instant::now();
        let none = KeyModifiers::NONE;
        let prefix = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key_at(&mut state, &mut router, prefix(), t0);
        handle_key_at(&mut state, &mut router, KeyEvent::new(KeyCode::Char('?'), none), t0);
        assert!(state.whichkey.pinned() && state.whichkey.visible(), "help pins open");
        // Browsing survives ticks with no pending prefix.
        assert!(!state.whichkey.poll(false, t0));
        assert!(state.whichkey.visible());
        // A known key fires from the browser and closes it.
        handle_key_at(&mut state, &mut router, KeyEvent::new(KeyCode::Char('w'), none), t0);
        assert!(!state.whichkey.visible() && !state.whichkey.pinned(), "fired closes");
        assert!(state.grid_mode, "ToggleGrid fired from the browser");
    }

    #[test]
    fn pinned_help_question_stays_and_escape_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::Instant;
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let t0 = Instant::now();
        let none = KeyModifiers::NONE;
        let prefix = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key_at(&mut state, &mut router, prefix(), t0);
        handle_key_at(&mut state, &mut router, KeyEvent::new(KeyCode::Char('?'), none), t0);
        handle_key_at(&mut state, &mut router, KeyEvent::new(KeyCode::Char('?'), none), t0);
        assert!(state.whichkey.pinned(), "`?` while browsing keeps browsing");
        handle_key_at(&mut state, &mut router, prefix(), t0);
        assert!(state.whichkey.pinned(), "a bare prefix keeps the map up");
        handle_key_at(&mut state, &mut router, KeyEvent::new(KeyCode::Esc, none), t0);
        assert!(!state.whichkey.visible() && !state.whichkey.pinned(), "Esc closes");
        assert!(!router.is_pending());
    }

    #[test]
    fn unknown_key_after_prefix_closes_hud_gracefully() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::{Duration, Instant};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let t0 = Instant::now();
        let none = KeyModifiers::NONE;
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            t0,
        );
        assert!(state.whichkey.poll(
            true,
            t0 + Duration::from_millis(crate::ui::whichkey::WHICHKEY_DELAY_MS)
        ));
        assert!(state.whichkey.visible());
        // No live session in a fresh state: the fallthrough is a silent
        // no-op, never a stuck prefix mode.
        handle_key_at(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('z'), none),
            t0 + Duration::from_millis(crate::ui::whichkey::WHICHKEY_DELAY_MS + 10),
        );
        assert!(!state.whichkey.visible(), "invalid key dismisses the HUD");
        assert!(!router.is_pending(), "prefix mode always resolves");
    }

    #[test]
    fn prefix_q_asks_first_and_no_stays() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix);
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(state.confirm.is_some(), "quit asks first");
        assert!(!state.should_quit, "nothing quits yet");
        // No is default: Enter stays.
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.confirm.is_none(), "confirm closed");
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
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(state.confirm.is_none(), "confirm closed");
        assert!(state.should_quit, "Yes quits");
    }

    #[test]
    fn prefix_q_then_y_shows_saving_sessions() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix);
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(state.confirm.is_none(), "confirm closed");
        assert!(state.quit_saving, "saving modal takes over");
        assert!(state.should_quit, "Yes still quits");
        assert!(state.dirty, "saving modal repaints");
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
    fn prefix_m_opens_telegram_dialog() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        let mut router = InputRouter::new();
        let prefix = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix());
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('m'), none));
        assert!(state.telegram_dialog.is_some(), "Ctrl-b m opens Telegram settings");
    }

    #[test]
    fn prefix_e_opens_theme_picker_and_enter_applies() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let home = std::env::temp_dir().join(format!(
            "forge-theme-e2e-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let dir = crate::infra::branding::themes_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("square.json"),
            r##"{"name": "square", "buttons": {"left": "[", "right": "]"}}"##,
        )
        .unwrap();
        let mut state = AppState::new();
        state.themes_dir = Some(dir.clone());
        let mut router = InputRouter::new();
        let prefix = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        handle_key(&mut state, &mut router, prefix());
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('e'), none));
        assert!(state.theme_dialog.is_some(), "Ctrl-b e opens theme picker");
        // Move to square and apply; the config persists the choice.
        handle_theme_key(
            &mut state,
            &mut crate::infra::config::LoadedConfig::load(&crate::infra::branding::config_file(&home)).unwrap(),
            &home,
            KeyEvent::new(KeyCode::Down, none),
        );
        let mut loaded =
            crate::infra::config::LoadedConfig::load(&crate::infra::branding::config_file(&home)).unwrap();
        handle_theme_key(
            &mut state,
            &mut loaded,
            &home,
            KeyEvent::new(KeyCode::Enter, none),
        );
        assert!(state.theme_dialog.is_none(), "apply closes");
        assert_eq!(crate::ui::theme::active_theme_name(), "square");
        assert_eq!(crate::ui::theme::pill_left(), '[');
        crate::ui::theme::clear_external_theme();
        assert_eq!(loaded.config.theme, "square", "choice persists");
        let _ = std::fs::remove_dir_all(&home);
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
    fn prefix_x_asks_to_kill_instead_of_terminating() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let victim = state.manager.active().unwrap();
        let mut router = InputRouter::new();
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut state, &mut router, KeyEvent::new(KeyCode::Char('x'), none));
        assert!(state.manager.get(victim).is_some(), "session survives until confirmed");
        assert!(
            matches!(
                state.confirm.as_ref().map(|d| d.kind()),
                Some(crate::ui::dialogs::quit::ConfirmKind::KillSession(id)) if id == victim
            ),
            "kill confirm targets the active session"
        );
        assert!(!state.should_quit, "killing never quits forge");
        let order = state.manager.order().to_vec();
        for id in order {
            assert!(state.manager.remove(id));
        }
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
        assert!(state.manager.get(victim).is_some(), "x asks first");
        handle_confirm_key(&mut state, KeyEvent::new(KeyCode::Char('y'), none));
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
        let entry = crate::session::checkpoint::SavedEntry {
            label: "a".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![crate::session::checkpoint::SavedSession {
                name: "a".to_string(),
                cli_tool: "codex".to_string(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                groups: vec![],
                harness_session_id: None,
            }],
        };
        let file = crate::session::checkpoint::SessionsFile {
            entries: vec![entry],
        };
        // Esc resolves fresh with no sessions and keeps the file.
        let mut state = AppState::new();
        state.restore_picker = crate::session::checkpoint::RestorePicker::new(&file);
        handle_restore_key(&mut state, KeyEvent::new(KeyCode::Esc, none));
        assert!(state.restore_picker.is_none());
        assert!(state.manager.order().is_empty());
        // Enter restores the entry.
        state.restore_picker = crate::session::checkpoint::RestorePicker::new(&file);
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
    fn restore_picker_ignores_arrows_esc_dismisses() {
        use crate::session::checkpoint::{make_entry, RestorePicker, SavedSession, SessionsFile};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        let mut file = SessionsFile::default();
        file.push(make_entry(
            vec![SavedSession {
                name: "a".to_string(),
                cli_tool: "claude".to_string(),
                cwd: "/tmp/proj".to_string(),
                groups: Vec::new(),
                harness_session_id: None,
            }],
            1_700_000_000,
        ));
        state.restore_picker = RestorePicker::new(&file);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Arrows and navigation keys must not dismiss the modal.
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
            handle_restore_key(&mut state, key(code));
            assert!(state.restore_picker.is_some(), "{code:?} dismissed the picker");
        }
        // Esc dismisses; the tested keys above left no selection damage.
        handle_restore_key(&mut state, key(KeyCode::Esc));
        assert!(state.restore_picker.is_none());
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
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Yolo);
        assert!(!state.dirty, "swallowed clicks leave no work");
        // Esc closes.
        handle_group_key(&mut state, gkey(KeyCode::Esc));
        assert!(state.group_dialog.is_none());
        assert!(state.manager.remove(order[0]));
    }

}
