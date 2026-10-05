//! Writer test fixtures shared across the topic test modules.

use super::*;
use crate::app::test_support::*;

/// Process-wide scratch-dir sequence: parallel tests can spawn in
/// the same nanosecond, and time-only names then share one dir (and
/// one `d.md`), flaking exact-bytes assertions. The counter makes
/// every dir unique by construction.
static WRITER_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[test]
fn tmp_dirs_stay_unique_under_bursts() {
    // Pin for the burst-collision guard: 500 rapid names must all
    // differ. (Time-only names collide when parallel test spawns
    // share one nanosecond; the counter closes it by construction.)
    let mut dirs = std::collections::HashSet::new();
    for _ in 0..500 {
        assert!(dirs.insert(writer_tmp_dir()), "scratch dir repeated");
    }
}

pub(super) fn writer_tmp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "forge-writer-tool-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
        WRITER_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ))
}

pub(super) fn writer_agent() -> (AppState, crate::session::SessionId, String, std::path::PathBuf) {
    let dir = writer_tmp_dir();
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
