//! E3 CUA text ops through the adapter: two-space indent/outdent
//! (selection lines or current line, markers preserved), CUA word
//! kills (selection first, single undo), select-all, and the
//! clipboard ops (E5, covered here plus the TUI key/mouse suites).
//! Find/replace moved to E7: the bar, search and replace live in the
//! TUI find suite and `adapter::keys::find`.

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
fn indent_is_one_undo_step_after_e6() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa");
    feed(&mut state, id, crossterm::event::KeyCode::Tab);
    assert_eq!(doc_text(&state, id), "  aaa");
    // E6 groups the whole indent into one undo step.
    let (code, mods) = ctrl(crossterm::event::KeyCode::Char('z'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), "aaa");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cut_whole_line_keeps_the_break() {
    // M8-fix: cutting a full row must not eat its line break (same
    // EdTUI whole-row extract as the wrap path).
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "First para line.\n\nNext text here.\n");
    feed_mod(&mut state, id, crossterm::event::KeyCode::End, crossterm::event::KeyModifiers::SHIFT);
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        Some(0..16),
        "whole first row selected"
    );
    let (code, mods) = (crossterm::event::KeyCode::Char('x'), crossterm::event::KeyModifiers::CONTROL);
    feed_mod(&mut state, id, code, mods);
    assert_eq!(
        doc_text(&state, id),
        "\n\nNext text here.\n",
        "cut keeps the break"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clipboard_cut_is_one_undo_step() {
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb");
    shift_select(&mut state, id, 3);
    // E5 owns the clipboard: copy keeps the selection, cut takes it
    // in one undo step.
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
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: Backspace over a selection must delete exactly the selection.
// "aaa bbb ccc" with "bbb" selected: the two spaces survive, and
// one undo restores the word.
#[test]
fn backspace_over_a_mid_line_selection_deletes_only_the_selection() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    for _ in 0..4 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_select(&mut state, id, 3);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..7));
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), "aaa  ccc");
    let (code, mods) = ctrl(KeyCode::Char('z'));
    feed_mod(&mut state, id, code, mods);
    assert_eq!(doc_text(&state, id), "aaa bbb ccc", "one undo restores it");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: Delete over a selection deletes exactly the selection too.
#[test]
fn delete_over_a_selection_deletes_only_the_selection() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    for _ in 0..4 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_select(&mut state, id, 3);
    feed(&mut state, id, KeyCode::Delete);
    assert_eq!(doc_text(&state, id), "aaa  ccc");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: a selection at the start of the line takes nothing before it.
#[test]
fn backspace_over_a_line_start_selection_keeps_the_break() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    shift_select(&mut state, id, 3);
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), " bbb ccc");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: a selection at the end of the line takes nothing after it.
#[test]
fn backspace_over_an_end_of_line_selection_keeps_the_space() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    for _ in 0..8 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_select(&mut state, id, 3);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(8..11));
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), "aaa bbb ");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: a multi-line selection deletes exactly its span, breaks included.
#[test]
fn backspace_over_a_multiline_selection_deletes_exactly() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "ab\ncd\nef");
    shift_select(&mut state, id, 5);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..5));
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), "\nef");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: select-all plus Backspace empties the document.
#[test]
fn backspace_over_the_whole_document_empties_it() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    let (code, mods) = ctrl(KeyCode::Char('a'));
    feed_mod(&mut state, id, code, mods);
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), "");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: a single-char selection deletes just that char.
#[test]
fn backspace_over_a_single_char_selection() {
    use crossterm::event::KeyCode;
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    for _ in 0..4 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_select(&mut state, id, 1);
    feed(&mut state, id, KeyCode::Backspace);
    assert_eq!(doc_text(&state, id), "aaa bb ccc");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: Ctrl+Backspace over a whole-row selection kills the word and
// keeps the structural break (the whole-row extract quirk).
#[test]
fn ctrl_backspace_over_a_whole_row_selection_keeps_the_break() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "para\nnext\n");
    for _ in 0..4 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_left(&mut state, id, 4);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..4));
    feed_mod(&mut state, id, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!(doc_text(&state, id), "\nnext\n");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: Ctrl+Delete over a whole-row selection, same guard.
#[test]
fn ctrl_delete_over_a_whole_row_selection_keeps_the_break() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "para\nnext\n");
    shift_select(&mut state, id, 4);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..4));
    feed_mod(&mut state, id, KeyCode::Delete, KeyModifiers::CONTROL);
    assert_eq!(doc_text(&state, id), "\nnext\n");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

// B1: Alt+Backspace (the reliable word-kill twin) over a selection
// deletes exactly the selection.
#[test]
fn alt_backspace_over_a_selection_deletes_only_the_selection() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (mut state, id, run, dir) = writer_agent();
    open_text(&mut state, id, &run, &dir, "d.md", "aaa bbb ccc");
    for _ in 0..4 {
        feed(&mut state, id, KeyCode::Right);
    }
    shift_select(&mut state, id, 3);
    feed_mod(&mut state, id, KeyCode::Backspace, KeyModifiers::ALT);
    assert_eq!(doc_text(&state, id), "aaa  ccc");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
