//! Writer test fixtures shared across the topic test modules.

use super::*;
use crate::app::test_support::*;

pub(super) fn writer_agent() -> (AppState, crate::session::SessionId, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "forge-writer-tool-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut state = AppState::new();
    let id = state
        .manager
        .spawn_agent(
            "agent",
            &dir,
            "exec cat",
            crate::infra::ids::RunId::generate(),
            "codex",
        )
        .unwrap();
    let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    (state, id, run, dir)
}
pub(super) fn open_doc(state: &mut AppState, run: &str, path: &str) -> String {
    comms_reply(state, run, "writer_open", &format!(r#"{{"path":{}}}"#, crate::ipc::mcp::escape_json(path)))
}
pub(super) fn open_editor(state: &mut AppState, id: crate::session::SessionId) {
    state.writer_open_editor(id);
    assert!(
        state.writers.get(&id).unwrap().editor.is_some(),
        "editor opens with Insert mode and the Forge clipboard"
    );
    assert_eq!(
        state.writers.get(&id).unwrap().editor.as_ref().unwrap().mode,
        edtui::EditorMode::Insert
    );
}

pub(super) fn feed(state: &mut AppState, id: crate::session::SessionId, code: crossterm::event::KeyCode) {
    state.writer_feed_key(
        id,
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    );
}
pub(super) fn propose_for_accept(state: &mut AppState, id: crate::session::SessionId, run: &str, rid: u64, text: &str) -> u64 {
    let reply = comms_reply(
        state, run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"{text}"}}"#),
    );
    assert!(reply.contains(r#""proposed":true"#), "proposed: {reply}");
    // The tool reports the id the same way an agent learns it.
    let _ = id;
    reply
        .split("\"proposal\":")
        .nth(1)
        .and_then(|tail| tail.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<u64>().ok())
        .expect("reply carries the proposal id")
}
pub(super) fn shift_select(state: &mut AppState, id: crate::session::SessionId, count: usize) {
    let shift = crossterm::event::KeyModifiers::SHIFT;
    for _ in 0..count {
        state.writer_feed_key(
            id,
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Right, shift),
        );
    }
}

pub(super) fn shift_left(state: &mut AppState, id: crate::session::SessionId, count: usize) {
    let shift = crossterm::event::KeyModifiers::SHIFT;
    for _ in 0..count {
        state.writer_feed_key(
            id,
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Left, shift),
        );
    }
}
