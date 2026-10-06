//! E3 CUA text ops through the adapter: two-space indent/outdent
//! (selection lines or current line, markers preserved), CUA word
//! kills (selection first, single undo), select-all, and the
//! reserved-for-later no-ops (find is E7; clipboard moved to E5 and
//! is covered here plus the TUI key/mouse suites).

use super::super::test_support::*;

fn feed_mod(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) {
    state.writer_feed_key(
        id,
        crossterm::event::KeyEvent::new(code, mods),
    );
}

fn ctrl(code: crossterm::event::KeyCode) -> (crossterm::event::KeyCode, crossterm::event::KeyModifiers) {
    (code, crossterm::event::KeyModifiers::CONTROL)
}

fn doc_text(state: &crate::app::AppState, id: crate::session::SessionId) -> String {
    state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone()
}

fn open_text(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    run: &str,
    dir: &std::path::Path,
    name: &str,
    text: &str,
) {
    std::fs::write(dir.join(name), text).unwrap();
    assert!(open_doc(state, run, name).contains(r#""ok":true"#));
    open_editor(state, id);
}

#[test]
fn tab_inserts_two_spaces_at_the_cursor() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa");
    feed(&mut state, id, crossterm::event::KeyCode::Tab);
    assert_eq!(doc_text(&state, id), "  aaa");
    assert!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().dirty);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_tab_strips_two_leading_spaces() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "  aaa");
    feed_mod(
        &mut state,
        id,
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::SHIFT,
    );
    assert_eq!(doc_text(&state, id), "aaa");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn indent_selection_indents_each_touched_line() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aa\nbb");
    shift_select(&mut state, id, 4);
    feed(&mut state, id, crossterm::event::KeyCode::Tab);
    assert_eq!(doc_text(&state, id), "  aa\n  bb");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn indent_list_item_keeps_its_marker() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "- item\n1. two");
    feed(&mut state, id, crossterm::event::KeyCode::Tab);
    assert_eq!(doc_text(&state, id), "  - item\n1. two");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn outdent_selection_strips_each_line() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "  aa\n  bb");
    shift_select(&mut state, id, 8);
    feed_mod(
        &mut state,
        id,
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::SHIFT,
    );
    assert_eq!(doc_text(&state, id), "aa\nbb");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn outdent_plain_line_without_indent_is_a_noop() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa");
    let rev = state.writers.get(&id).unwrap().doc.as_ref().unwrap().revision;
    feed_mod(
        &mut state,
        id,
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::SHIFT,
    );
    assert_eq!(doc_text(&state, id), "aaa");
    assert_eq!(
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().revision,
        rev,
        "no edit, no revision bump"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn word_kill_with_live_selection_deletes_the_selection() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb");
    shift_select(&mut state, id, 3);
    let (code, mods) = ctrl(crossterm::event::KeyCode::Backspace);
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), " bbb");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn word_kill_at_doc_start_is_a_noop() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa");
    let rev = state.writers.get(&id).unwrap().doc.as_ref().unwrap().revision;
    let (code, mods) = ctrl(crossterm::event::KeyCode::Backspace);
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), "aaa");
    assert_eq!(
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().revision,
        rev
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn select_all_empty_doc_selects_nothing() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "");
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('a'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(state.writers.get(&id).unwrap().selection, None);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn indent_takes_two_undos_until_e6_groups() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa");
    feed(&mut state, id, crossterm::event::KeyCode::Tab);
    assert_eq!(doc_text(&state, id), "  aaa");
    // Each inserted space captures its own undo step today; E6
    // coalesces typing (and indent) into word/pause groups.
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('z'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), " aaa");
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), "aaa");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clipboard_and_find_keys_are_reserved_noops() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb");
    shift_select(&mut state, id, 3);
    // E5 owns the clipboard: copy keeps the selection, cut takes it
    // in one undo step. Find/replace stays reserved until E7.
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('c'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        Some(0..3),
        "copy keeps the selection"
    );
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('x'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), " bbb", "cut takes the range");
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('z'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), "aaa bbb", "one undo restores the cut");
    for code in [
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyCode::Char('h'),
    ] {
        let (code, mods) = ctrl(code);
        feed_mod(&mut state, id, code, mods);
    }
    assert_eq!(doc_text(&state, id), "aaa bbb");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
