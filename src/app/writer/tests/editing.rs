use super::super::test_support::*;
use crate::app::test_support::*;

#[test]
fn typing_edits_the_document_and_bumps_revision() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('X'));
    feed(&mut state, id, crossterm::event::KeyCode::Char('Y'));
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().text, "XYhello");
    assert_eq!(session.doc.as_ref().unwrap().revision, 2);
    assert!(session.doc.as_ref().unwrap().dirty);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn typing_stales_intersecting_proposals() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello world").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(crate::writer::request::WriterAction::Rephrase, "hello world", 6..11, 0).unwrap();
    let proposed = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"Forge"}}"#),
    );
    assert!(proposed.contains(r#""proposed":true"#), "proposed: {proposed}");
    // Typing at the start shifts nothing in the slice: the edit
    // precedes the range, so the proposal goes stale through the
    // real typing path (diff + on_edit, no test-only hooks).
    feed(&mut state, id, crossterm::event::KeyCode::Char('!'));
    let session = state.writers.get(&id).unwrap();
    use crate::writer::proposal::ProposalState;
    assert_eq!(session.proposals.get(1).unwrap().state, ProposalState::Stale);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn selection_conversion_boundaries() {
    use crate::app::writer::adapter::editor_selection_to_range;
    use edtui::{actions::SwitchMode, EditorMode, EditorState, Index2, Lines};
    // Empty document: no selection, no range.
    let empty = EditorState::new(Lines::from(""));
    assert_eq!(editor_selection_to_range(&empty), None);
    // An inclusive point selection is one char: EdTUI addresses both
    // ends inclusively, so start == end still covers that char. A
    // true cursor is `selection = None`, never a zero-width value.
    let mut plain = EditorState::new(Lines::from("hello"));
    plain.execute(SwitchMode(EditorMode::Visual));
    assert_eq!(editor_selection_to_range(&plain), Some(0..1));
    // ASCII range: inclusive end becomes exclusive.
    let mut ascii = EditorState::new(Lines::from("hello"));
    ascii.execute(SwitchMode(EditorMode::Visual));
    ascii.execute(edtui::actions::MoveForward(2));
    assert_eq!(editor_selection_to_range(&ascii), Some(0..3));
    // Multibyte: offsets count chars, not bytes.
    let mut wide = EditorState::new(Lines::from("aéc"));
    wide.execute(SwitchMode(EditorMode::Visual));
    wide.execute(edtui::actions::MoveForward(2));
    assert_eq!(editor_selection_to_range(&wide), Some(0..3));
    // Multi-line across a newline (down extends past the line end;
    // plain forward motion stops there).
    let mut multi = EditorState::new(Lines::from("ab\ncd"));
    multi.cursor = Index2::new(0, 1);
    multi.execute(SwitchMode(EditorMode::Visual));
    multi.execute(edtui::actions::MoveDown(1));
    assert_eq!(editor_selection_to_range(&multi), Some(1..5));
    // End of line: selecting down to the next line's start takes the
    // newline with it (chars 1..3 are "b\n").
    let mut eol = EditorState::new(Lines::from("ab\ncd"));
    eol.cursor = Index2::new(0, 1);
    eol.execute(SwitchMode(EditorMode::Visual));
    eol.execute(edtui::actions::MoveDown(1));
    eol.execute(edtui::actions::MoveBackward(1));
    assert_eq!(editor_selection_to_range(&eol), Some(1..4));
}

#[test]
fn shift_arrows_select_without_reaching_edtui() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    let shift = crossterm::event::KeyModifiers::SHIFT;
    for _ in 0..2 {
        state.writer_feed_key(
            id,
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Right, shift),
        );
    }
    // Text-field counts: two presses select two chars, not three.
    // (EdTUI visual counts the anchor char; the adapter compensates.)
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..2));
    // Shift+Ctrl+Right runs to the next word start (adapter word
    // motion), taking the separating space: standard behavior.
    let word = crossterm::event::KeyModifiers::SHIFT | crossterm::event::KeyModifiers::CONTROL;
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("e.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "e.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    state.writer_feed_key(
        id,
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Right, word),
    );
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..4));
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn plain_arrow_collapses_the_selection() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    shift_select(&mut state, id, 3);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..3));
    // A plain arrow drops the selection instead of leaving a stale
    // mirror behind for the next Rephrase to act on.
    feed(&mut state, id, crossterm::event::KeyCode::Right);
    assert_eq!(state.writers.get(&id).unwrap().selection, None);
    // A new gesture anchors fresh from the cursor.
    shift_select(&mut state, id, 2);
    let selection = state.writers.get(&id).unwrap().selection.clone();
    assert!(selection.is_some(), "selects again, got {selection:?}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
#[test]
fn accept_replaces_exactly_with_one_undo_and_clipboard_intact() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello world, hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    // The user copied something before the accept arrived.
    state.writers.get(&id).unwrap().clip.0.borrow_mut().push_str("USER");
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(crate::writer::request::WriterAction::Rephrase, "hello world, hello", 13..18, 0).unwrap();
    let pid = propose_for_accept(&mut state, id, &run, rid, "bye");
    state.writer_accept(id, pid).unwrap();
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().text, "hello world, bye");
    assert_eq!(session.doc.as_ref().unwrap().revision, 1);
    assert_eq!(
        session.editor.as_ref().unwrap().lines.to_string(),
        "hello world, bye",
        "editor buffer matches the document"
    );
    // The clipboard survived the DeleteSelection inside Accept.
    assert_eq!(session.clip.0.borrow().as_str(), "USER");
    // One editor undo restores the pre-accept text.
    let editor = state.writers.get_mut(&id).unwrap().editor.as_mut().unwrap();
    editor.undo();
    assert_eq!(editor.lines.to_string(), "hello world, hello");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn accept_multiline_proposal_then_typing_keeps_doc_exact() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc.clone());
    state.writer_open_editor(id);
    let pid = state
        .writers
        .get_mut(&id)
        .unwrap()
        .proposals
        .propose(&doc, None, 0..5, "X\nY".to_string(), None)
        .unwrap();
    state.writer_accept(id, pid).unwrap();
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().text, "X\nY\nl3");
    assert_eq!(
        session.editor.as_ref().unwrap().lines.to_string(),
        "X\nY\nl3",
        "buffer matches the document right after accept"
    );
    // The next keystroke must diff a correct buffer, not corrupt.
    feed(&mut state, id, crossterm::event::KeyCode::Char('!'));
    assert_eq!(
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
        "X\nY!\nl3"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn accept_proposal_changing_line_count() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc.clone());
    state.writer_open_editor(id);
    let pid = state
        .writers
        .get_mut(&id)
        .unwrap()
        .proposals
        .propose(&doc, None, 0..2, "longer\nlines\nhere".to_string(), None)
        .unwrap();
    state.writer_accept(id, pid).unwrap();
    let session = state.writers.get(&id).unwrap();
    assert_eq!(
        session.doc.as_ref().unwrap().text,
        "longer\nlines\nhere\nl2\nl3"
    );
    assert_eq!(
        session.editor.as_ref().unwrap().lines.to_string(),
        "longer\nlines\nhere\nl2\nl3",
        "buffer matches"
    );
    feed(&mut state, id, crossterm::event::KeyCode::Char('?'));
    assert_eq!(
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
        "longer\nlines\nhere?\nl2\nl3"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_right_once_selects_one_char() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
    shift_select(&mut state, id, 1);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..1));
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_selection_crosses_line_boundaries() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "ab\ncd").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
    // Forward across the newline: three presses take "ab\n".
    shift_select(&mut state, id, 3);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..3));
    // Back across it from a fresh gesture: plain arrows collapse and
    // clear the anchor, then three Shift+Left take it all back.
    feed(&mut state, id, crossterm::event::KeyCode::Right);
    feed(&mut state, id, crossterm::event::KeyCode::Left);
    assert_eq!(state.writers.get(&id).unwrap().selection, None);
    let shift = crossterm::event::KeyModifiers::SHIFT;
    for _ in 0..3 {
        state.writer_feed_key(
            id,
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Left, shift),
        );
    }
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..3));
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_selection_reextends_after_collapse() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
    shift_select(&mut state, id, 1);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..1));
    shift_left(&mut state, id, 1);
    assert_eq!(state.writers.get(&id).unwrap().selection, None);
    shift_select(&mut state, id, 1);
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        Some(0..1),
        "re-extend after collapse"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shift_left_past_anchor_after_collapse_selects_before() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
    feed(&mut state, id, crossterm::event::KeyCode::Right);
    feed(&mut state, id, crossterm::event::KeyCode::Right);
    shift_select(&mut state, id, 2);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(2..4));
    shift_left(&mut state, id, 2);
    assert_eq!(state.writers.get(&id).unwrap().selection, None);
    shift_left(&mut state, id, 1);
    assert_eq!(
        state.writers.get(&id).unwrap().selection,
        Some(1..2),
        "the char before the anchor"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn accept_insert_range_is_one_undo_step() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "ac").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(crate::writer::request::WriterAction::Rephrase, "ac", 1..1, 0).unwrap();
    let pid = propose_for_accept(&mut state, id, &run, rid, "b");
    state.writer_accept(id, pid).unwrap();
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().text, "abc");
    assert_eq!(session.editor.as_ref().unwrap().lines.to_string(), "abc");
    let editor = state.writers.get_mut(&id).unwrap().editor.as_mut().unwrap();
    editor.undo();
    assert_eq!(editor.lines.to_string(), "ac");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn accept_refuses_stale_and_reject_settles() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaaa bbbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(crate::writer::request::WriterAction::Rephrase, "aaaa bbbb", 5..9, 0).unwrap();
    let pid = propose_for_accept(&mut state, id, &run, rid, "B");
    // Change the text under the range without on_edit (reload path): the
    // S2 snapshot inside accept refuses even though the record reads Pending.
    state.writers.get_mut(&id).unwrap().doc.as_mut().unwrap()
        .apply_edit(5..6, "X").unwrap();
    let err = state.writer_accept(id, pid).unwrap_err();
    assert!(err.contains("stale"), "accept stale: {err}");
    assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "aaaa Xbbb");
    // Reject works on stale and settles it.
    state.writer_reject(id, pid).unwrap();
    use crate::writer::proposal::ProposalState;
    assert_eq!(
        state.writers.get(&id).unwrap().proposals.get(pid).unwrap().state,
        ProposalState::Rejected
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_writes_and_conflicts_land_in_the_slot() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('!'));
    state.writer_save(id);
    assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!hello");
    assert_eq!(state.writers.get(&id).unwrap().error, None);
    // External change, then save: conflict goes to the fixed slot.
    std::fs::write(dir.join("d.md"), "theirs").unwrap();
    feed(&mut state, id, crossterm::event::KeyCode::Char('?'));
    state.writer_save(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.error.as_deref(), Some("save conflict: file changed on disk"));
    assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "theirs");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
