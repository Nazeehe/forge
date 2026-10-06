//! Find bar through the real key path: open, incremental search,
//! next/prev with wrap, Esc, case toggle, replace and replace-all.

use super::super::test_support::*;
use crate::tui::input::InputRouter;
use crate::tui::keys::handle_key_at;
use crossterm::event;

fn press(
    state: &mut crate::app::AppState,
    router: &mut InputRouter,
    key: event::KeyEvent,
) {
    handle_key_at(state, router, key, std::time::Instant::now());
}

fn ctrl_f(state: &mut crate::app::AppState, router: &mut InputRouter) {
    press(state, router, ctrl(event::KeyCode::Char('f')));
}

fn alt_h(state: &mut crate::app::AppState, router: &mut InputRouter) {
    // Replace lives on Alt+H: 0x08 (Ctrl+H) word-deletes instead.
    press(
        state,
        router,
        event::KeyEvent::new(event::KeyCode::Char('h'), event::KeyModifiers::ALT),
    );
}

fn type_str(state: &mut crate::app::AppState, router: &mut InputRouter, text: &str) {
    for c in text.chars() {
        press(state, router, key(event::KeyCode::Char(c)));
    }
}

fn open_foo(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    dir: &std::path::Path,
) {
    std::fs::write(dir.join("d.md"), "foo bar foo").unwrap();
    open_doc(state, id, "d.md");
    state.open_writer_overlay();
}

fn find_of(
    state: &crate::app::AppState,
    id: crate::session::SessionId,
) -> crate::app::writer::WriterFind {
    state
        .writers
        .get(&id)
        .unwrap()
        .find
        .clone()
        .expect("find bar open")
}

#[test]
fn ctrl_f_seeds_the_query_from_a_live_selection() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    shift_select(&mut state, id, 3);
    ctrl_f(&mut state, &mut router);
    let find = find_of(&state, id);
    assert_eq!(find.query, "foo");
    assert_eq!(find.matches, vec![0..3, 8..11]);
    assert_eq!(find.current, 0);
    assert_eq!(cursor_offset(&state, id), 0, "cursor on the first match");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ctrl_f_without_a_selection_opens_empty() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    let find = find_of(&state, id);
    assert_eq!(find.query, "");
    assert!(find.matches.is_empty());
    assert_eq!(doc_text(&state, id), "foo bar foo", "doc untouched");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn typing_searches_incrementally() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    let find = find_of(&state, id);
    assert_eq!(find.matches, vec![0..3, 8..11]);
    assert_eq!(find.current, 0);
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        Some(0..3),
        "current match selected"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn enter_steps_forward_and_wraps() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(find_of(&state, id).current, 1);
    assert_eq!(cursor_offset(&state, id), 8);
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(find_of(&state, id).current, 0, "wraps past the last");
    assert_eq!(cursor_offset(&state, id), 0);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_enter_steps_back_and_wraps() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    press(&mut state, &mut router, shift(event::KeyCode::Enter));
    assert_eq!(find_of(&state, id).current, 1, "wraps before the first");
    assert_eq!(cursor_offset(&state, id), 8);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn f3_steps_both_ways() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    press(&mut state, &mut router, key(event::KeyCode::F(3)));
    assert_eq!(find_of(&state, id).current, 1);
    press(
        &mut state,
        &mut router,
        shift(event::KeyCode::F(3)),
    );
    assert_eq!(find_of(&state, id).current, 0);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn esc_closes_leaving_the_cursor_on_the_match() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    press(&mut state, &mut router, key(event::KeyCode::Esc));
    assert!(state.writers.get(&id).unwrap().find.is_none());
    assert_eq!(cursor_offset(&state, id), 8, "cursor stays on the match");
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        None,
        "match selection collapses"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ctrl_f_toggles_the_bar_shut() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    ctrl_f(&mut state, &mut router);
    assert!(state.writers.get(&id).unwrap().find.is_none());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_chars_route_to_the_bar_while_open() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "x");
    assert_eq!(doc_text(&state, id), "foo bar foo", "doc untouched");
    assert_eq!(find_of(&state, id).query, "x");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn case_toggle_narrows_to_exact_case() {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "Foo foo").unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    assert_eq!(find_of(&state, id).matches.len(), 2);
    // Tab moves Query → case pill; Enter toggles it.
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    let find = find_of(&state, id);
    assert!(find.case_sensitive);
    assert_eq!(find.matches, vec![4..7]);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_current_swaps_and_advances() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    // Alt+H opens straight into the replace field; the query goes
    // through the find field first via Ctrl+F.
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    alt_h(&mut state, &mut router);
    type_str(&mut state, &mut router, "qux");
    // Enter in the replace field replaces the current match.
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(doc_text(&state, id), "qux bar foo");
    assert_eq!(cursor_offset(&state, id), 8, "on the next match");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_all_is_one_undo_step() {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aa aa aa").unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "aa");
    alt_h(&mut state, &mut router);
    type_str(&mut state, &mut router, "b");
    // Tab cycles Replace field → (Replace) → (Replace all); Enter fires it.
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(doc_text(&state, id), "b b b");
    assert_eq!(
        find_of(&state, id).note.as_deref(),
        Some("3 replaced"),
        "count reported"
    );
    press(
        &mut state,
        &mut router,
        ctrl(event::KeyCode::Char('z')),
    );
    assert_eq!(doc_text(&state, id), "aa aa aa", "one undo restores all");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_all_past_the_cap_is_refused() {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "a ".repeat(10_001)).unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "a");
    let find = find_of(&state, id);
    assert!(find.overflow, "cap trips");
    alt_h(&mut state, &mut router);
    type_str(&mut state, &mut router, "b");
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(
        doc_text(&state, id),
        "a ".repeat(10_001),
        "doc untouched"
    );
    assert!(
        find_of(&state, id)
            .note
            .as_deref()
            .is_some_and(|n| n.contains("narrow")),
        "refusal says to narrow"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn preview_ctrl_f_leaves_preview_with_the_bar_open() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    press(
        &mut state,
        &mut router,
        event::KeyEvent::new(event::KeyCode::Char('p'), event::KeyModifiers::ALT),
    );
    assert!(state.writers.get(&id).unwrap().preview);
    ctrl_f(&mut state, &mut router);
    let session = state.writers.get(&id).unwrap();
    assert!(!session.preview, "source search needs the editor");
    assert!(session.find.is_some());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}


#[test]
fn closing_the_doc_drops_the_bar() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    assert!(state.writers.get(&id).unwrap().find.is_some());
    state.writer_toolbar_close(id);
    assert!(state.writers.get(&id).unwrap().find.is_none(), "bar gone with the doc");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opening_another_doc_drops_the_bar() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    std::fs::write(dir.join("e.md"), "nothing here").unwrap();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    state.writer_open_rel(id, "e.md");
    let session = state.writers.get(&id).unwrap();
    assert!(session.find.is_none(), "no stale matches");
    assert_eq!(doc_text(&state, id), "nothing here");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn overflow_counter_reads_capped() {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "a ".repeat(10_001)).unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "a");
    let find = find_of(&state, id);
    assert_eq!(crate::ui::writer::layout::find_counter(&find), "1/10000+");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn up_and_down_step_through_matches() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "foo");
    press(&mut state, &mut router, key(event::KeyCode::Down));
    assert_eq!(find_of(&state, id).current, 1);
    press(&mut state, &mut router, key(event::KeyCode::Up));
    assert_eq!(find_of(&state, id).current, 0);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seeded_query_types_to_replace() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    shift_select(&mut state, id, 3);
    ctrl_f(&mut state, &mut router);
    assert_eq!(find_of(&state, id).query, "foo");
    type_str(&mut state, &mut router, "b");
    assert_eq!(find_of(&state, id).query, "b", "seed replaced, not appended");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_toggle_opens_and_collapses_via_tab() {
    let (mut state, id, dir) = writer_agent();
    open_foo(&mut state, id, &dir);
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    // Closed cycle: Query -> case -> toggle -> Query.
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    let find = find_of(&state, id);
    assert!(find.replace_open, "toggle opened");
    assert_eq!(find.focus, crate::app::writer::FindFocus::Replace);
    // Open cycle reaches the toggle five Tabs past Replace.
    for _ in 0..5 {
        press(&mut state, &mut router, key(event::KeyCode::Tab));
    }
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    let find = find_of(&state, id);
    assert!(!find.replace_open, "toggle collapsed");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_all_redo_restores_replaced_text() {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aa aa aa").unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let mut router = InputRouter::new();
    ctrl_f(&mut state, &mut router);
    type_str(&mut state, &mut router, "aa");
    alt_h(&mut state, &mut router);
    type_str(&mut state, &mut router, "b");
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Tab));
    press(&mut state, &mut router, key(event::KeyCode::Enter));
    assert_eq!(doc_text(&state, id), "b b b");
    press(&mut state, &mut router, ctrl(event::KeyCode::Char('z')));
    assert_eq!(doc_text(&state, id), "aa aa aa", "one undo restores all");
    press(&mut state, &mut router, ctrl(event::KeyCode::Char('y')));
    assert_eq!(doc_text(&state, id), "b b b", "redo returns the replaced text");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
