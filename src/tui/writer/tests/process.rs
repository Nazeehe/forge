//! Process-run lock (§6) through the real key path: Alt+Enter
//! starts, typing is refused with the notice, navigation and
//! selection still work.

use super::super::test_support::*;
use crate::tui::input::InputRouter;
use crate::tui::keys::handle_key_at;
use crossterm::event;

type State = crate::app::AppState;
type Id = crate::session::SessionId;

const DOC: &str = "alpha @@fix typo@@this is teh@@\n";

fn alt_enter() -> event::KeyEvent {
    event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::ALT)
}

fn setup_locked() -> (State, Id, std::path::PathBuf, InputRouter, std::time::Instant) {
    let (mut state, id, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), DOC).unwrap();
    open_doc(&mut state, id, "d.md");
    state.open_writer_overlay();
    let router = InputRouter::new();
    let now = std::time::Instant::now();
    (state, id, dir, router, now)
}

fn teardown(state: &mut State, id: Id, dir: &std::path::PathBuf) {
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn alt_enter_starts_the_run_and_locks() {
    let (mut state, id, dir, mut router, now) = setup_locked();
    handle_key_at(&mut state, &mut router, alt_enter(), now);
    assert!(
        state.writers.get(&id).unwrap().process.is_some(),
        "Alt+Enter locks"
    );
    teardown(&mut state, id, &dir);
}

#[test]
fn typing_is_refused_with_the_processing_notice() {
    let (mut state, id, dir, mut router, now) = setup_locked();
    handle_key_at(&mut state, &mut router, alt_enter(), now);
    handle_key_at(
        &mut state,
        &mut router,
        key(event::KeyCode::Char('x')),
        now,
    );
    assert_eq!(doc_text(&state, id), DOC, "typing never lands");
    assert_eq!(
        state.writers.get(&id).unwrap().banner.as_deref(),
        Some("Processing… (Stop)"),
        "notice shows"
    );
    teardown(&mut state, id, &dir);
}

#[test]
fn esc_while_locked_stops_the_run() {
    let (mut state, id, dir, mut router, now) = setup_locked();
    handle_key_at(&mut state, &mut router, alt_enter(), now);
    handle_key_at(
        &mut state,
        &mut router,
        event::KeyEvent::new(event::KeyCode::Esc, event::KeyModifiers::NONE),
        now,
    );
    let session = state.writers.get(&id).unwrap();
    assert!(
        session.process.as_ref().is_some_and(|l| l.stopped),
        "Esc stops"
    );
    assert_eq!(
        session.banner.as_deref(),
        Some("Stopping…"),
        "banner: {:?}",
        session.banner
    );
    teardown(&mut state, id, &dir);
}

#[test]
fn navigation_and_selection_still_work_while_locked() {
    let (mut state, id, dir, mut router, now) = setup_locked();
    handle_key_at(&mut state, &mut router, alt_enter(), now);
    handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
    handle_key_at(
        &mut state,
        &mut router,
        event::KeyEvent::new(event::KeyCode::Right, event::KeyModifiers::SHIFT),
        now,
    );
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.selection, Some(1..2), "shift selects");
    assert_eq!(doc_text(&state, id), DOC, "doc untouched");
    assert!(
        session.banner.is_none(),
        "no notice for allowed keys: {:?}",
        session.banner
    );
    teardown(&mut state, id, &dir);
}
