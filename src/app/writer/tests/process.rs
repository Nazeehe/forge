//! Process-run UI side (§6): pre-flight, the read-only lock,
//! violation check, Stop/Revert, and run details. The scripted
//! agent is a file write plus tool calls: each intermediate state
//! goes through the watch poll and the real paint path.

use super::super::test_support::*;
use crate::app::test_support::*;

const DOC: &str = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";

fn started_doc() -> (
    crate::app::AppState,
    crate::session::SessionId,
    String,
    std::path::PathBuf,
) {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("n.md"), DOC).unwrap();
    assert!(open_doc(&mut state, &run, "n.md").contains(r#""ok":true"#));
    (state, id, run, dir)
}

fn start(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
) -> u64 {
    state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect("process starts")
}

fn finish(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    dir: &std::path::PathBuf,
) {
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(dir).ok();
}

fn doc_text(state: &crate::app::AppState, id: crate::session::SessionId) -> String {
    state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone()
}

fn file_text(dir: &std::path::PathBuf) -> String {
    std::fs::read_to_string(dir.join("n.md")).unwrap()
}

/// Scripted agent, step by step: write the file, poll the watch,
/// report through the real tool dispatch. Mirrors what the M5 E2E
/// does through the harness, minus the PTY.
fn agent_write_and_report(
    state: &mut crate::app::AppState,
    id: crate::session::SessionId,
    run: &str,
    dir: &std::path::PathBuf,
    rid: u64,
    body: &str,
    report_args: &str,
    now: &mut std::time::Instant,
) {
    std::fs::write(dir.join("n.md"), body).unwrap();
    *now += std::time::Duration::from_secs(2);
    state.writer_poll_files_now(*now);
    assert_eq!(doc_text(state, id), body, "watch reloads the agent write");
    let reply = comms_reply(state, run, "writer_run_report", report_args);
    assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
    let _ = rid;
}

#[test]
fn process_needs_a_document() {
    let (mut state, id, _run, dir) = writer_agent();
    let err = state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect_err("no doc, no run");
    assert!(err.contains("no document"), "err: {err}");
    finish(&mut state, id, &dir);
}

#[test]
fn process_with_no_markers_sets_info() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("p.md"), "plain text\n").unwrap();
    assert!(open_doc(&mut state, &run, "p.md").contains(r#""ok":true"#));
    let err = state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect_err("no markers, no run");
    assert!(err.contains("No markers to process"), "err: {err}");
    assert!(
        state.writers.get(&id).unwrap().process.is_none(),
        "nothing locked"
    );
    finish(&mut state, id, &dir);
}

#[test]
fn process_starts_run_and_locks_with_outside_segments() {
    let (mut state, id, _run, dir) = started_doc();
    let rid = start(&mut state, id);
    let session = state.writers.get(&id).unwrap();
    let lock = session.process.as_ref().expect("locked");
    assert_eq!(lock.run_id, rid);
    assert_eq!(lock.pre_text, DOC);
    assert_eq!(lock.outside, vec!["alpha ", "\n\n", "\n"]);
    assert!(!lock.stopped, "fresh lock unstopped");
    assert_eq!(session.queue.len(), 1, "markup queued for the agent");
    let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
    assert_eq!(run.markers.len(), 2, "both markers ride");
    finish(&mut state, id, &dir);
}

#[test]
fn process_second_start_is_refused_while_locked() {
    let (mut state, id, _run, dir) = started_doc();
    start(&mut state, id);
    let err = state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect_err("locked");
    assert!(err.contains("already processing"), "err: {err}");
    finish(&mut state, id, &dir);
}

#[test]
fn process_selection_only_takes_markers_fully_inside() {
    let (mut state, id, _run, dir) = started_doc();
    // Marker 0 spans 6..31; the selection covers it exactly.
    let rid = state
        .writer_process_start(id, Some(6..31), std::time::Instant::now())
        .expect("selection start");
    let session = state.writers.get(&id).unwrap();
    let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
    assert_eq!(run.markers.len(), 1, "only the inside marker rides");
    assert_eq!(run.markers[0].marker.index, 0);
    finish(&mut state, id, &dir);
}

#[test]
fn process_selection_clipping_a_marker_processes_nothing() {
    let (mut state, id, _run, dir) = started_doc();
    let err = state
        .writer_process_start(id, Some(0..10), std::time::Instant::now())
        .expect_err("clipped marker excluded");
    assert!(err.contains("No markers to process"), "err: {err}");
    finish(&mut state, id, &dir);
}

#[test]
fn reload_during_lock_keeps_the_undo_boundary() {
    let (mut state, id, _run, dir) = started_doc();
    start(&mut state, id);
    std::fs::write(dir.join("n.md"), "alpha DONE0\n\n@@ask capital@@Paris@@\n").unwrap();
    let later = std::time::Instant::now() + std::time::Duration::from_secs(2);
    state.writer_poll_files_now(later);
    // The buffer follows the file, but the undo stack still holds
    // the pre-run capture: one undo returns to the snapshot.
    let session = state.writers.get_mut(&id).unwrap();
    let editor = session.editor.as_mut().expect("editor live");
    editor.execute(edtui::actions::Undo);
    assert_eq!(editor.lines.to_string(), DOC, "one undo reverts the run");
    finish(&mut state, id, &dir);
}

#[test]
fn three_step_run_finishes_clean_with_details_and_unlock() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut state, id, run, dir) = started_doc();
    // A human char before the run: the snapshot holds it, and the
    // collapse must return to it, not past it.
    state.writer_feed_key(id, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let pre = format!("x{DOC}");
    assert_eq!(doc_text(&state, id), pre);
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    let step1 = "xalpha DONE0\n\n@@ask capital@@Paris@@\n";
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, step1,
        &format!(r#"{{"run":{rid},"index":0,"status":"done"}}"#), &mut now,
    );
    let step2 = "xalpha DONE0\n\nDONE1\n";
    std::fs::write(dir.join("n.md"), step2).unwrap();
    now += std::time::Duration::from_secs(2);
    state.writer_poll_files_now(now);
    let reply = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"run":{rid},"index":1,"answer":"Paris"}}"#),
    );
    assert!(reply.contains(r#""answered":true"#), "reply: {reply}");
    let done = comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"both handled"}}"#),
    );
    assert!(done.contains(r#""done":true"#), "done: {done}");
    let session = state.writers.get(&id).unwrap();
    assert!(session.process.is_none(), "finish unlocks");
    assert!(session.pending_confirm.is_none(), "clean run, no banner");
    assert_eq!(doc_text(&state, id), step2);
    let note = session.thread.last().expect("details note");
    assert!(note.answer.contains("both handled"), "summary: {}", note.answer);
    assert!(note.answer.contains("2 done"), "counts: {}", note.answer);
    assert_eq!(
        session.last_run,
        Some(crate::app::writer::process::LastRun { id: rid, done: 2, skipped: 0, failed: 0 }),
        "status counts kept"
    );
    // One undo step collapses the whole run (both agent writes).
    let session = state.writers.get_mut(&id).unwrap();
    let editor = session.editor.as_mut().expect("editor live");
    editor.execute(edtui::actions::Undo);
    assert_eq!(editor.lines.to_string(), pre, "one undo reverts the run");
    finish(&mut state, id, &dir);
}

#[test]
fn outside_edit_raises_the_violation_banner() {
    let (mut state, id, run, dir) = started_doc();
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    // Head-gap substance changed: no replacement can absorb it.
    let tampered = "beta @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, tampered,
        &format!(r#"{{"run":{rid},"index":0,"status":"done"}}"#), &mut now,
    );
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, tampered,
        &format!(r#"{{"run":{rid},"index":1,"status":"done"}}"#), &mut now,
    );
    let done = comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"s"}}"#),
    );
    assert!(done.contains(r#""done":true"#), "done: {done}");
    let session = state.writers.get(&id).unwrap();
    let confirm = session.pending_confirm.as_ref().expect("violation banner");
    assert!(
        confirm.message.contains("Agent changed text outside markers"),
        "message: {}",
        confirm.message
    );
    let labels: Vec<&str> = confirm.actions.iter().map(|a| match a {
        crate::app::writer::ConfirmAction::RevertRun(_) => "Revert run",
        crate::app::writer::ConfirmAction::Keep => "Keep",
        _ => "?",
    }).collect();
    assert_eq!(labels, vec!["Revert run", "Keep"], "actions: {labels:?}");
    finish(&mut state, id, &dir);
}

#[test]
fn revert_restores_the_exact_snapshot() {
    let (mut state, id, run, dir) = started_doc();
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    // Head-gap substance changed: no replacement can absorb it.
    let tampered = "beta @@fix typo@@this is teh@@\n\nDONE1\n";
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, tampered,
        &format!(r#"{{"run":{rid},"index":0,"status":"done"}}"#), &mut now,
    );
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, tampered,
        &format!(r#"{{"run":{rid},"index":1,"status":"done"}}"#), &mut now,
    );
    comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"s"}}"#),
    );
    assert!(state.writers.get(&id).unwrap().pending_confirm.is_some());
    state.writer_fire_confirm(id, 0);
    assert_eq!(doc_text(&state, id), DOC, "doc restored");
    assert_eq!(file_text(&dir), DOC, "file restored");
    assert!(
        state.writers.get(&id).unwrap().pending_confirm.is_none(),
        "row clears"
    );
    // The revert itself is one undo step: undo returns to the run's
    // final text, not past it.
    let session = state.writers.get_mut(&id).unwrap();
    let editor = session.editor.as_mut().expect("editor live");
    editor.execute(edtui::actions::Undo);
    assert_eq!(editor.lines.to_string(), tampered, "undo returns to final");
    finish(&mut state, id, &dir);
}

/// The walk itself, unit by unit: sound on clean runs, tripped
/// by substance changes and verbatim deletions, blind (by design)
/// to insertions absorbed into a neighboring wildcard.
#[test]
fn run_intact_walk_pins_substance_not_junctions() {
    use crate::writer::markers::parse_markers;
    let intact = crate::app::AppState::process_run_intact;
    let pre = DOC;
    let spans: Vec<std::ops::Range<usize>> =
        parse_markers(pre).markers.iter().map(|m| m.whole.clone()).collect();
    // Clean replacements pass.
    assert!(intact(
        pre, &spans, &[false, false],
        "alpha FIXED\n\nANSWER\n",
    ));
    // Head substance fails.
    assert!(!intact(
        pre, &spans, &[false, false],
        "beta @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n",
    ));
    // Middle-gap substance fails.
    assert!(!intact(
        pre, &spans, &[false, false],
        "alpha @@fix typo@@this is teh@@\nX\n@@ask capital@@Paris@@\n",
    ));
    // A deleted skipped marker fails (verbatim anchor gone).
    assert!(!intact(
        pre, &spans, &[true, false],
        "alpha \n\nANSWER\n",
    ));
    // The skipped marker kept verbatim passes.
    assert!(intact(
        pre, &spans, &[true, false],
        "alpha @@fix typo@@this is teh@@\n\nANSWER\n",
    ));
    // Junction insertions next to a done marker are absorbed into
    // its free-form replacement: provably undetectable, Revert is
    // the remedy.
    assert!(intact(
        pre, &spans, &[false, false],
        "alpha CHANGED @@fix typo@@this is teh@@\n\nANSWER\n",
    ));
}

#[test]
fn deleted_skipped_marker_raises_the_banner() {
    let (mut state, id, run, dir) = started_doc();
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    // Marker 0 skipped, then deleted from the file anyway.
    let scrubbed = "alpha \n\n@@ask capital@@Paris@@\n";
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, scrubbed,
        &format!(r#"{{"run":{rid},"index":0,"status":"skipped","note":"too risky"}}"#),
        &mut now,
    );
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, scrubbed,
        &format!(r#"{{"run":{rid},"index":1,"status":"done"}}"#), &mut now,
    );
    comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"s"}}"#),
    );
    assert!(
        state.writers.get(&id).unwrap().pending_confirm.is_some(),
        "verbatim deletion trips the walk"
    );
    finish(&mut state, id, &dir);
}

#[test]
fn save_conflict_aborts_the_start_with_the_banner() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut state, id, _run, dir) = started_doc();
    // Dirty buffer plus disk movement: the pre-flight save conflicts.
    state.writer_feed_key(id, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    std::fs::write(dir.join("n.md"), "external\n").unwrap();
    let err = state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect_err("conflict aborts");
    assert!(err.contains("save failed"), "err: {err}");
    let session = state.writers.get(&id).unwrap();
    assert!(session.process.is_none(), "nothing locked");
    assert!(session.pending_confirm.is_some(), "banner offers reload/keep");
    finish(&mut state, id, &dir);
}

#[test]
fn failed_markers_land_in_the_details_with_reasons() {
    let (mut state, id, run, dir) = started_doc();
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, "alpha DONE0\n\n@@ask capital@@Paris@@\n",
        &format!(r#"{{"run":{rid},"index":0,"status":"done"}}"#), &mut now,
    );
    agent_write_and_report(
        &mut state, id, &run, &dir, rid, "alpha DONE0\n\n@@ask capital@@Paris@@\n",
        &format!(r#"{{"run":{rid},"index":1,"status":"failed","note":"no capital in file"}}"#),
        &mut now,
    );
    comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"half"}}"#),
    );
    let session = state.writers.get(&id).unwrap();
    let note = session.thread.last().expect("details note");
    assert!(note.answer.contains("1 done · 0 skipped · 1 failed"), "counts: {}", note.answer);
    assert!(note.answer.contains("no capital in file"), "reason: {}", note.answer);
    assert_eq!(
        session.last_run,
        Some(crate::app::writer::process::LastRun { id: rid, done: 1, skipped: 0, failed: 1 }),
    );
    finish(&mut state, id, &dir);
}

#[test]
fn turn_end_unlocks_and_keeps_details() {
    let (mut state, id, _run, dir) = started_doc();
    let t0 = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, t0).expect("starts");
    state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Thinking;
    state.settle_writer_runs(t0);
    state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Idle;
    state.settle_writer_runs(t0 + std::time::Duration::from_secs(10));
    let session = state.writers.get(&id).unwrap();
    assert!(session.process.is_none(), "turn-end unlocks");
    assert!(session.last_run.is_some_and(|l| l.id == rid), "counts kept");
    assert!(session.thread.iter().any(|e| e.run == Some(rid)), "details note");
    finish(&mut state, id, &dir);
}

/// PRD §7 answers: over-long prompts never ride a run. Valid
/// markers proceed; the start info names the excluded lines.
#[test]
fn overlong_prompt_is_excluded_with_lines_named() {
    let (mut state, id, _run, dir) = writer_agent_with("codex");
    let long = "p".repeat(2001);
    let text = format!("@@fix typo@@fine@@\n\n@@{long}@@end\n");
    std::fs::write(dir.join("n.md"), &text).unwrap();
    // open_doc needs the run id: read it back off the record.
    let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    assert!(open_doc(&mut state, &run, "n.md").contains(r#""ok":true"#));
    let rid = state.writer_process_start(id, None, std::time::Instant::now()).expect("starts");
    let session = state.writers.get(&id).unwrap();
    let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
    assert_eq!(run.markers.len(), 1, "only the valid marker rides");
    assert!(
        session.banner.as_deref().unwrap_or("").contains("excluded"),
        "banner: {:?}",
        session.banner
    );
    assert!(
        session.banner.as_deref().unwrap_or("").contains('3'),
        "line named: {:?}",
        session.banner
    );
    finish(&mut state, id, &dir);
}

#[test]
fn all_excluded_is_no_markers_with_count() {
    let (mut state, id, _run, dir) = writer_agent_with("codex");
    let long = "p".repeat(2001);
    let text = format!("@@{long}@@end\n");
    std::fs::write(dir.join("n.md"), &text).unwrap();
    let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    assert!(open_doc(&mut state, &run, "n.md").contains(r#""ok":true"#));
    let err = state
        .writer_process_start(id, None, std::time::Instant::now())
        .expect_err("nothing valid, no run");
    assert_eq!(err, "No markers to process (1 excluded)", "err: {err}");
    assert!(
        state.writers.get(&id).unwrap().process.is_none(),
        "nothing locked"
    );
    finish(&mut state, id, &dir);
}

#[test]
fn stop_with_interrupt_bytes_stops_and_shows_stopping() {
    let (mut state, id, _run, dir) = started_doc();
    start(&mut state, id);
    state.writer_process_stop(id);
    let session = state.writers.get(&id).unwrap();
    let lock = session.process.as_ref().expect("still locked until done");
    assert!(lock.stopped, "stop flagged");
    assert_eq!(
        session.banner.as_deref(),
        Some("Stopping…"),
        "banner: {:?}",
        session.banner
    );
    finish(&mut state, id, &dir);
}

#[test]
fn stop_without_bytes_requests_and_waits_for_the_marker() {
    let (mut state, id, _run, dir) = writer_agent_with("muse");
    std::fs::write(dir.join("n.md"), DOC).unwrap();
    let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    assert!(open_doc(&mut state, &run, "n.md").contains(r#""ok":true"#));
    state.writer_process_start(id, None, std::time::Instant::now()).expect("starts");
    state.writer_process_stop(id);
    let session = state.writers.get(&id).unwrap();
    let lock = session.process.as_ref().expect("still locked until done");
    assert!(lock.stopped, "stop flagged");
    assert_eq!(
        session.banner.as_deref(),
        Some("Stop requested: the agent will finish its current marker"),
        "banner: {:?}",
        session.banner
    );
    finish(&mut state, id, &dir);
}

/// M5-fix: a second Stop force-finishes at once (the agent may
/// never yield). What landed collapses to one undo, the violation
/// check runs on it, and late tool calls bounce as finished.
#[test]
fn second_stop_force_finishes_with_collapse_and_check() {
    let (mut state, id, run, dir) = writer_agent_with("muse");
    std::fs::write(dir.join("n.md"), DOC).unwrap();
    let run_id = state.manager.get(id).unwrap().run_id.as_str().to_string();
    assert!(open_doc(&mut state, &run_id, "n.md").contains(r#""ok":true"#));
    let mut now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    // The agent lands one marker plus an outside edit, then goes silent.
    let landed = "beta DONE0\n\n@@ask capital@@Paris@@\n";
    std::fs::write(dir.join("n.md"), landed).unwrap();
    now += std::time::Duration::from_secs(2);
    state.writer_poll_files_now(now);
    state.writer_process_stop(id);
    assert!(
        state.writers.get(&id).unwrap().process.is_some(),
        "first stop waits"
    );
    state.writer_process_stop(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.process.is_none(), "force unlocks at once");
    let note = session.thread.last().expect("details note");
    assert!(note.answer.contains("stopped by user"), "status: {}", note.answer);
    assert!(
        session.pending_confirm.is_some(),
        "violation checked on what landed"
    );
    // Late tool calls bounce as finished.
    let late = comms_reply(
        &mut state, &run, "writer_run_report",
        &format!(r#"{{"run":{rid},"index":0,"status":"done"}}"#),
    );
    assert!(late.contains("already finished"), "late report: {late}");
    let late_done = comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"x"}}"#),
    );
    assert!(late_done.contains("already finished"), "late done: {late_done}");
    // What landed collapses to one undo step.
    let session = state.writers.get_mut(&id).unwrap();
    let editor = session.editor.as_mut().expect("editor live");
    editor.execute(edtui::actions::Undo);
    assert_eq!(editor.lines.to_string(), DOC, "one undo reverts the landing");
    // A later agent write rides the normal watch (clean reload).
    std::fs::write(dir.join("n.md"), "late\n").unwrap();
    now += std::time::Duration::from_secs(2);
    state.writer_poll_files_now(now);
    assert_eq!(doc_text(&state, id), "late\n", "normal reload after force");
    finish(&mut state, id, &dir);
}

#[test]
fn stop_without_a_lock_is_a_noop() {
    let (mut state, id, _run, dir) = started_doc();
    state.writer_process_stop(id);
    let session = state.writers.get(&id).unwrap();
    assert!(session.process.is_none(), "nothing to stop");
    assert!(session.banner.is_none(), "no banner: {:?}", session.banner);
    finish(&mut state, id, &dir);
}

#[test]
fn answer_auto_opens_the_panel() {
    let (mut state, id, run, dir) = started_doc();
    assert!(!state.writers.get(&id).unwrap().panel_visible);
    let now = std::time::Instant::now();
    let rid = state.writer_process_start(id, None, now).expect("starts");
    comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"run":{rid},"index":1,"answer":"Paris"}}"#),
    );
    assert!(
        state.writers.get(&id).unwrap().panel_visible,
        "answer opens the panel"
    );
    finish(&mut state, id, &dir);
}

fn margin_marks(
    state: &crate::app::AppState,
    id: crate::session::SessionId,
) -> Vec<crate::app::writer::runs::RunMark> {
    state.writers.get(&id).unwrap().run_marks.clone()
}

fn report(
    state: &mut crate::app::AppState,
    run: &str,
    rid: u64,
    args_tail: &str,
) -> String {
    comms_reply(
        state, run, "writer_run_report",
        &format!(r#"{{"run":{rid},{args_tail}}}"#),
    )
}

#[test]
fn process_start_marks_every_riding_marker_queued() {
    use crate::app::writer::runs::RunMarkerStatus;
    let (mut state, id, _run, dir) = started_doc();
    let rid = start(&mut state, id);
    let marks = margin_marks(&state, id);
    assert_eq!(marks.len(), 2, "every riding marker: {marks:?}");
    assert!(
        marks.iter().all(|m| m.run_id == rid && m.status == RunMarkerStatus::Pending),
        "all queued before any report: {marks:?}"
    );
    assert_eq!(marks[0].doc_index, 0);
    assert_eq!(marks[0].verb.as_deref(), Some("fix"));
    assert_eq!(marks[0].prompt, "typo");
    assert_eq!(
        marks[0].line_text, "alpha @@fix typo@@this is teh@@",
        "first buffer line at mark time"
    );
    assert_eq!(marks[1].doc_index, 1);
    assert_eq!(marks[1].prompt, "capital");
    finish(&mut state, id, &dir);
}

#[test]
fn report_moves_marks_started_then_done_removes() {
    use crate::app::writer::runs::RunMarkerStatus;
    let (mut state, id, run, dir) = started_doc();
    let rid = start(&mut state, id);
    let reply = report(&mut state, &run, rid, r#""index":0,"status":"started""#);
    assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
    assert_eq!(
        margin_marks(&state, id)[0].status,
        RunMarkerStatus::Started,
        "started lights working"
    );
    let reply = report(&mut state, &run, rid, r#""index":0,"status":"done""#);
    assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
    let marks = margin_marks(&state, id);
    assert_eq!(marks.len(), 1, "done removes the mark: {marks:?}");
    assert_eq!(marks[0].doc_index, 1);
    let reply = report(
        &mut state, &run, rid,
        r#""index":1,"status":"failed","note":"boom""#,
    );
    assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
    assert_eq!(
        margin_marks(&state, id)[0].status,
        RunMarkerStatus::Failed,
        "failed stays"
    );
    finish(&mut state, id, &dir);
}

#[test]
fn finish_keeps_failures_and_a_new_run_clears_them() {
    use crate::app::writer::runs::RunMarkerStatus;
    let (mut state, id, run, dir) = started_doc();
    let rid = start(&mut state, id);
    report(&mut state, &run, rid, r#""index":0,"status":"failed""#);
    let done = comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"s"}}"#),
    );
    assert!(done.contains(r#""done":true"#), "done: {done}");
    let marks = margin_marks(&state, id);
    assert_eq!(marks.len(), 1, "only the failure survives: {marks:?}");
    assert_eq!(marks[0].status, RunMarkerStatus::Failed);
    let rid2 = start(&mut state, id);
    assert_ne!(rid2, rid, "second run");
    let marks = margin_marks(&state, id);
    assert_eq!(marks.len(), 2, "fresh marks: {marks:?}");
    assert!(
        marks.iter().all(|m| m.run_id == rid2 && m.status == RunMarkerStatus::Pending),
        "old failures retired: {marks:?}"
    );
    finish(&mut state, id, &dir);
}

#[test]
fn revert_clears_margin_marks() {
    let (mut state, id, run, dir) = started_doc();
    let rid = start(&mut state, id);
    report(&mut state, &run, rid, r#""index":0,"status":"failed""#);
    let done = comms_reply(
        &mut state, &run, "writer_run_done",
        &format!(r#"{{"run":{rid},"summary":"s"}}"#),
    );
    assert!(done.contains(r#""done":true"#), "done: {done}");
    assert_eq!(margin_marks(&state, id).len(), 1);
    let pre = doc_text(&state, id);
    state.writer_process_revert(id, pre);
    assert!(margin_marks(&state, id).is_empty(), "revert starts over");
    finish(&mut state, id, &dir);
}

#[test]
fn process_caps_at_64_markers_with_a_process_again_note() {
    let (mut state, id, run, dir) = writer_agent();
    let mut text = String::new();
    for i in 0..70 {
        text.push_str(&format!("@@note prompt{i} @@end\n"));
    }
    std::fs::write(dir.join("many.md"), &text).unwrap();
    assert!(open_doc(&mut state, &run, "many.md").contains(r#""ok":true"#));
    let rid = start(&mut state, id);
    let session = state.writers.get(&id).unwrap();
    let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
    assert_eq!(run.markers.len(), 64, "extras wait");
    assert!(
        session.banner.as_deref().unwrap_or("").contains("process again"),
        "banner: {:?}",
        session.banner
    );
    finish(&mut state, id, &dir);
}
