use super::super::test_support::*;


#[test]
fn rephrase_leaves_the_chat_draft_alone() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
    let doc = crate::writer::document::Document::open(&dir, "d.md").unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
    shift_select(&mut state, id, 3);
    state.writers.get_mut(&id).unwrap().chat_input = "abc".to_string();
    state.writer_rephrase(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.chat_input, "abc", "draft survives Rephrase");
    assert!(
        session.queue.back().expect("queued").contains("<instruction></instruction>"),
        "no instruction sent"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rephrase_uses_the_live_selection_range() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    // Cursor to 4, then select 4..7.
    for _ in 0..4 {
        feed(&mut state, id, crossterm::event::KeyCode::Right);
    }
    shift_select(&mut state, id, 3);
    assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..7));
    state.writer_rephrase(id);
    let session = state.writers.get(&id).unwrap();
    let record = session.requests.last().expect("request recorded");
    assert_eq!(record.range, 4..7);
    assert_eq!(record.action, crate::writer::request::WriterAction::Rephrase);
    let body = session.queue.back().expect("request queued");
    assert!(body.contains("action=rephrase"), "body: {body}");
    assert!(body.contains("chars 4-7"), "body: {body}");
    assert!(body.contains("<selection>\nbbb\n</selection>"), "body: {body}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rephrase_without_selection_takes_the_paragraph() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "head\n\nfirst para\nsecond line\n\ntail").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    // Cursor into "second line" (char 20), no selection.
    let editor = state.writers.get_mut(&id).unwrap().editor.as_mut().unwrap();
    editor.cursor = crate::app::writer::adapter::offset_to_index2("head\n\nfirst para\nsecond line\n\ntail", 20);
    state.writer_rephrase(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.requests.last().expect("request").range, 6..28);
    let body = session.queue.back().expect("queued");
    assert!(body.contains("first para\nsecond line"), "body: {body}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn busy_agent_holds_requests_without_pane_writes() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Thinking;
    state.writer_rephrase(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.queue.len(), 1, "held, not dropped");
    state.settle_writer_queues(std::time::Instant::now());
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.queue.len(), 1, "still held while busy");
    assert!(!state.pending_enter.contains_key(&id), "no staged Enter while busy");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn idle_agent_gets_inject_plus_staged_enter() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    state.writer_rephrase(id);
    assert_eq!(state.writers.get(&id).unwrap().queue.len(), 1);
    state.settle_writer_queues(std::time::Instant::now());
    let session = state.writers.get(&id).unwrap();
    assert!(session.queue.is_empty(), "delivered");
    assert!(state.pending_enter.contains_key(&id), "staged Enter follows the inject");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ninth_queued_request_is_refused_without_dropping() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    // Hold the agent so nothing drains while filling the queue.
    state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Thinking;
    for _ in 0..8 {
        state.writer_rephrase(id);
    }
    assert_eq!(state.writers.get(&id).unwrap().queue.len(), 8);
    let records = state.writers.get(&id).unwrap().requests.len();
    state.writer_rephrase(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.queue.len(), 8, "nothing dropped, nothing added");
    assert_eq!(session.requests.len(), records, "no orphan record");
    assert_eq!(
        session.error.as_deref(),
        Some("request queue full (8); wait for delivery")
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn chat_sends_with_and_without_selection() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    // With a selection: the request covers it.
    shift_select(&mut state, id, 3);
    state.writers.get_mut(&id).unwrap().chat_input = "why?".to_string();
    state.writer_chat_send(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.requests.last().expect("request").range, 0..3);
    let body = session.queue.back().expect("queued");
    assert!(body.contains("action=chat"), "body: {body}");
    assert!(body.contains("<instruction>why?</instruction>"), "body: {body}");
    assert!(session.chat_input.is_empty(), "chat box clears on send");
    // Without a selection: the whole document goes.
    state.writers.get_mut(&id).unwrap().editor.as_mut().unwrap().selection = None;
    state.writers.get_mut(&id).unwrap().chat_input = "sum up".to_string();
    state.writer_chat_send(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.requests.last().expect("request").range, 0..7);
    assert!(session.queue.back().expect("queued").contains("aaa bbb"));
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn chat_box_input_is_bounded_with_a_movable_cursor() {
    let (mut state, id, _run, dir) = writer_agent();
    state.writers.entry(id).or_default();
    for _ in 0..crate::writer::MAX_INPUT_CHARS + 10 {
        state.writer_chat_char(id, 'z');
    }
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.chat_input.chars().count(), crate::writer::MAX_INPUT_CHARS);
    assert_eq!(session.chat_cursor, crate::writer::MAX_INPUT_CHARS);
    // Control chars never enter the box.
    state.writer_chat_char(id, '\n');
    assert_eq!(
        state.writers.get(&id).unwrap().chat_input.chars().count(),
        crate::writer::MAX_INPUT_CHARS
    );
    state.writer_chat_move(id, -4_000_000);
    assert_eq!(state.writers.get(&id).unwrap().chat_cursor, 0);
    state.writer_chat_backspace(id);
    assert_eq!(state.writers.get(&id).unwrap().chat_cursor, 0, "nothing to delete");
    state.writer_chat_move(id, 4_000_000);
    state.writer_chat_backspace(id);
    assert_eq!(
        state.writers.get(&id).unwrap().chat_input.chars().count(),
        crate::writer::MAX_INPUT_CHARS - 1
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn paste_lands_in_the_editor_not_the_pane() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    state.writer_paste(id, "XY");
    assert_eq!(
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
        "XYaaa bbb"
    );
    // Prompt paste fills the bounded buffer instead.
    state.writers.get_mut(&id).unwrap().doc = None;
    state.writer_prompt_open(id, false);
    state.writer_paste(id, "d.md");
    assert_eq!(
        state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer,
        "d.md"
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn detach_chip_clears_the_editor_selection() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    open_editor(&mut state, id);
    shift_select(&mut state, id, 3);
    assert!(state.writers.get(&id).unwrap().selection.is_some());
    state.writer_clear_selection(id);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.selection, None);
    assert!(session.editor.as_ref().unwrap().selection.is_none());
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prompt_submit_opens_and_failures_keep_the_prompt() {
    let (mut state, id, _run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    // Empty submit refuses without opening.
    state.writer_prompt_open(id, false);
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_some(), "prompt stays");
    assert_eq!(session.error.as_deref(), Some("type a path first"));
    // A non-markdown path keeps the prompt with the reason slotted
    // (missing .md paths open as empty documents, by design).
    for c in "notes.json".chars() {
        state.writer_prompt_char(id, c);
    }
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_some(), "prompt stays");
    assert!(session.error.is_some(), "reason slotted");
    assert!(session.doc.is_none(), "nothing opened");
    // Control chars never enter the path.
    state.writer_prompt_char(id, '\n');
    assert_eq!(
        state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer,
        "notes.json"
    );
    // Bound: typing past MAX_PATH_CHARS stops.
    for _ in 0..crate::app::writer::MAX_PATH_CHARS {
        state.writer_prompt_char(id, 'x');
    }
    assert_eq!(
        state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer.chars().count(),
        crate::app::writer::MAX_PATH_CHARS,
    );
    // Backspace, cancel, then a good submit.
    state.writer_prompt_cancel(id);
    assert!(state.writers.get(&id).unwrap().open_prompt.is_none());
    state.writer_prompt_open(id, false);
    for c in "d.md".chars() {
        state.writer_prompt_char(id, c);
    }
    state.writer_submit_open(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.open_prompt.is_none(), "prompt closes");
    assert_eq!(session.doc.as_ref().unwrap().text, "hello");
    assert!(session.editor.is_some(), "editor builds");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rephrase_without_a_live_agent_tab_errors_without_writing() {
    let (mut state, _id, _run, dir) = writer_agent();
    // A writer entry with no session record behind it: no agent tab,
    // so the request is refused before any record or queue write.
    let ghost = crate::session::SessionId::fresh();
    state.writers.entry(ghost).or_default();
    state.writer_rephrase(ghost);
    let session = state.writers.get(&ghost).unwrap();
    assert_eq!(session.requests.len(), 0, "no record without a live tab");
    assert!(session.queue.is_empty(), "no write without a live tab");
    assert_eq!(session.error.as_deref(), Some("no live agent tab"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn writer_open_keeps_whatever_view_is_showing() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    let slot = state.writer_slot(id).expect("writer slot exists");
    // Human on the Writer tab: stays there (the switch is a no-op).
    state.overlay_view = Some((id, slot));
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    assert_eq!(state.overlay_view, Some((id, slot)));
    // Human elsewhere: the open stays silent.
    state.overlay_view = None;
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    assert_eq!(state.overlay_view, None);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}
