//! E2b document lifecycle: the New/Open/Save-as prompt kinds,
//! Tab completion, the Overwrite and unsaved-close confirms, and
//! Close back to the empty state.

use super::super::test_support::*;
use crate::app::writer::ConfirmAction;

fn type_path(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    path: &str,
) {
    for c in path.chars() {
        state.writer_prompt_char(id, c);
    }
}

fn prompt_buffer(state: &crate::app::AppState, id: crate::session::SessionId) -> String {
    state
        .writers
        .get(&id)
        .unwrap()
        .open_prompt
        .as_ref()
        .map(|p| p.buffer.clone())
        .unwrap_or_default()
}

#[test]
fn new_submit_missing_creates_empty_and_records_opened() {
    let (mut state, id, _run, dir) = writer_agent();
    state.writer_toolbar_new(id);
    type_path(&mut state, id, "notes.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_none(), "success closes the prompt");
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "notes.md");
    assert_eq!(session.doc.as_ref().unwrap().text, "");
    assert!(session.editor.is_some(), "editor builds on open");
    assert_eq!(session.opened, vec![dir.join("notes.md")]);
    assert!(
        session.recent_cache.iter().any(|e| e.rel == "notes.md"),
        "recent shows the new doc: {:?}",
        session.recent_cache.iter().map(|e| &e.rel).collect::<Vec<_>>()
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn new_submit_existing_raises_open_instead_and_firing_opens() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("old.md"), "# old\n").unwrap();
    state.writer_toolbar_new(id);
    type_path(&mut state, id, "old.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    let confirm = session.pending_confirm.as_ref().expect("confirm raised");
    assert_eq!(confirm.message, "old.md exists:");
    assert!(
        matches!(confirm.actions.as_slice(), [ConfirmAction::OpenInstead(_), ConfirmAction::Cancel]),
        "OpenInstead + Cancel, got {:?}",
        confirm.actions
    );
    assert!(session.open_prompt.is_some(), "prompt stays open under the confirm");
    state.writer_fire_confirm(id, 0);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_none(), "fired confirm clears");
    assert!(session.open_prompt.is_none(), "open closes the prompt");
    assert_eq!(session.doc.as_ref().unwrap().text, "# old\n");
    assert_eq!(session.opened, vec![dir.join("old.md")]);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn new_submit_existing_cancel_keeps_prompt_and_buffer() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("old.md"), "# old\n").unwrap();
    state.writer_toolbar_new(id);
    type_path(&mut state, id, "old.md");
    state.writer_submit_open(id);
    state.writer_fire_confirm(id, 1);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_none(), "cancel clears the row");
    assert_eq!(prompt_buffer(&state, id), "old.md", "draft survives Cancel");
    assert!(session.doc.is_none(), "nothing opened");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_submit_missing_raises_create_instead_and_firing_creates() {
    let (mut state, id, _run, dir) = writer_agent();
    state.writer_toolbar_open(id);
    type_path(&mut state, id, "fresh.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    let confirm = session.pending_confirm.as_ref().expect("confirm raised");
    assert_eq!(confirm.message, "no file fresh.md:");
    assert!(
        matches!(confirm.actions.as_slice(), [ConfirmAction::CreateInstead(_), ConfirmAction::Cancel]),
        "CreateInstead + Cancel, got {:?}",
        confirm.actions
    );
    state.writer_fire_confirm(id, 0);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "fresh.md");
    assert_eq!(session.doc.as_ref().unwrap().text, "");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_submit_existing_opens_directly_without_confirm() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("real.md"), "real\n").unwrap();
    state.writer_toolbar_open(id);
    type_path(&mut state, id, "real.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_none(), "no confirm on a direct hit");
    assert_eq!(session.doc.as_ref().unwrap().text, "real\n");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prompt_tab_completes_from_opened_then_recent_and_stays_stable() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("alpha.md"), "a\n").unwrap();
    std::fs::write(dir.join("beta.md"), "b\n").unwrap();
    open_doc(&mut state, &run, "alpha.md");
    state.writer_toolbar_open(id);
    type_path(&mut state, id, "a");
    state.writer_prompt_complete(id);
    assert_eq!(prompt_buffer(&state, id), "alpha.md", "opened first");
    state.writer_prompt_cancel(id);
    state.writer_toolbar_open(id);
    type_path(&mut state, id, "b");
    state.writer_prompt_complete(id);
    assert_eq!(prompt_buffer(&state, id), "beta.md", "recent scan fills in");
    state.writer_prompt_complete(id);
    assert_eq!(prompt_buffer(&state, id), "beta.md", "second Tab is stable");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_as_writes_new_file_switches_path_and_updates_recent() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "hello\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    state.writer_toolbar_save_as(id);
    type_path(&mut state, id, "b.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_none(), "success closes the prompt");
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "b.md");
    assert_eq!(std::fs::read_to_string(dir.join("b.md")).unwrap(), "hello\n");
    assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "hello\n");
    assert_eq!(session.opened[0], dir.join("b.md"), "save-as heads opened");
    assert!(
        session.recent_cache.iter().any(|e| e.rel == "b.md"),
        "recent updates on save-as"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_as_existing_raises_overwrite_and_firing_replaces() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "new text\n").unwrap();
    std::fs::write(dir.join("b.md"), "stale\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    state.writer_toolbar_save_as(id);
    type_path(&mut state, id, "b.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    let confirm = session.pending_confirm.as_ref().expect("confirm raised");
    assert_eq!(confirm.message, "b.md exists:");
    assert!(
        matches!(confirm.actions.as_slice(), [ConfirmAction::Overwrite(_), ConfirmAction::Cancel]),
        "Overwrite + Cancel, got {:?}",
        confirm.actions
    );
    state.writer_fire_confirm(id, 0);
    assert_eq!(std::fs::read_to_string(dir.join("b.md")).unwrap(), "new text\n");
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "b.md");
    assert!(session.pending_confirm.is_none());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_as_overwrite_cancel_keeps_old_path_and_bytes() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "mine\n").unwrap();
    std::fs::write(dir.join("b.md"), "theirs\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    state.writer_toolbar_save_as(id);
    type_path(&mut state, id, "b.md");
    state.writer_submit_open(id);
    state.writer_fire_confirm(id, 1);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "a.md");
    assert_eq!(std::fs::read_to_string(dir.join("b.md")).unwrap(), "theirs\n");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_as_rejects_non_markdown_and_identity_target() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    state.writer_toolbar_save_as(id);
    type_path(&mut state, id, "pic.png");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(
        session.error.as_deref().unwrap_or("").contains("only .md"),
        "extension gate, got {:?}",
        session.error
    );
    assert!(session.open_prompt.is_some(), "prompt stays open on failure");
    state.writer_prompt_cancel(id);
    state.writer_toolbar_save_as(id);
    type_path(&mut state, id, "a.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(
        session.error.as_deref().unwrap_or("").contains("already saved as"),
        "identity target, got {:?}",
        session.error
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_as_without_doc_errors_instead_of_prompting() {
    let (mut state, id, _run, dir) = writer_agent();
    state.writer_toolbar_save_as(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_none(), "no prompt with nothing to write");
    assert_eq!(session.error.as_deref(), Some("no document open"));
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn close_clean_doc_returns_to_empty_but_keeps_opened() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    state.writer_save(id);
    state.writer_toolbar_close(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_none(), "clean closes at once");
    assert!(session.doc.is_none(), "back to the empty state");
    assert!(session.editor.is_none(), "editor goes with the doc");
    assert_eq!(session.opened, vec![dir.join("a.md")], "opened survives close");
    assert!(!session.recent_cache.is_empty(), "recent survives close");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn close_dirty_save_and_close_writes_then_closes() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('y'));
    assert!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().dirty);
    state.writer_toolbar_close(id);
    let session = state.writers.get(&id).unwrap();
    let confirm = session.pending_confirm.as_ref().expect("confirm raised");
    assert_eq!(confirm.message, "Unsaved changes in a.md:");
    assert_eq!(confirm.actions.len(), 3, "Save&close, Discard, Cancel");
    assert!(session.doc.is_some(), "dirty never closes outright");
    state.writer_fire_confirm(id, 0);
    assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "yx\n");
    assert!(state.writers.get(&id).unwrap().doc.is_none(), "saved, then closed");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn close_dirty_discard_closes_without_writing() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('y'));
    state.writer_toolbar_close(id);
    state.writer_fire_confirm(id, 1);
    assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "x\n");
    assert!(state.writers.get(&id).unwrap().doc.is_none());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn close_dirty_cancel_stays_open_and_dirty() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('y'));
    state.writer_toolbar_close(id);
    state.writer_fire_confirm(id, 2);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_none());
    assert!(session.doc.as_ref().unwrap().dirty, "still dirty");
    assert!(session.editor.is_some(), "still open");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fire_confirm_out_of_range_keeps_the_row() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('y'));
    state.writer_toolbar_close(id);
    assert!(state.writers.get(&id).unwrap().pending_confirm.is_some());
    state.writer_fire_confirm(id, 9);
    let session = state.writers.get(&id).unwrap();
    assert!(session.pending_confirm.is_some(), "stray pick keeps the row");
    assert!(session.doc.is_some(), "nothing closed");
    state.writer_fire_confirm(id, 2);
    assert!(state.writers.get(&id).unwrap().pending_confirm.is_none());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dirty_doc_blocks_prompt_open_of_another() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "x\n").unwrap();
    open_doc(&mut state, &run, "a.md");
    open_editor(&mut state, id);
    feed(&mut state, id, crossterm::event::KeyCode::Char('y'));
    state.writer_toolbar_new(id);
    type_path(&mut state, id, "b.md");
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.doc.as_ref().unwrap().path_rel, "a.md", "switch refused");
    assert!(
        session.error.as_deref().unwrap_or("").contains("unsaved"),
        "reason in the slot, got {:?}",
        session.error
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
