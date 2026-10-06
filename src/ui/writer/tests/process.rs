//! Run and margin paint tests (M5/M7): the process lock pill,
//! the M7 margin glyphs with their re-anchor, and the run status.
//! Shared fixtures (`doc_session`, `paint_doc_to`, …) live in the
//! parent `tests` module.

use super::{buffer_text, doc_session, paint_doc_to, row_text};
use super::super::layout::{toolbar_action_label, writer_layout, ToolbarButton};
use crate::app::writer::WriterSession;
use ratatui::layout::Rect;

fn run_session() -> WriterSession {
    use crate::app::writer::runs::{RunMarkerStatus, WriterRun, WriterRunState};
    use crate::app::writer::runs::RunMarker;
    use crate::writer::process::ProcessShape;
    let mut session =
        doc_session("alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n");
    session.runs.push(WriterRun {
        id: 1,
        file: "d.md".to_string(),
        rev: 0,
        started_at: std::time::Instant::now(),
        markers: vec![
            RunMarker {
                marker: crate::writer::process::ProcessMarker {
                    index: 0,
                    line: 1,
                    whole: 6..31,
                    shape: ProcessShape::Wrap,
                    verb: Some("fix".to_string()),
                    prompt: "typo".to_string(),
                    target: Some("this is teh".to_string()),
                },
                status: RunMarkerStatus::Started,
                note: None,
            },
            RunMarker {
                marker: crate::writer::process::ProcessMarker {
                    index: 1,
                    line: 3,
                    whole: 33..55,
                    shape: ProcessShape::Wrap,
                    verb: Some("ask".to_string()),
                    prompt: "capital".to_string(),
                    target: Some("Paris".to_string()),
                },
                status: RunMarkerStatus::Pending,
                note: None,
            },
        ],
        state: WriterRunState::Active,
        saw_activity: true,
        reported: true,
    });
    session.process = Some(crate::app::writer::process::ProcessLock {
        run_id: 1,
        pre_text: "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n".to_string(),
        outside: vec!["alpha ".to_string(), "\n\n".to_string(), "\n".to_string()],
        spans: vec![6..31, 33..55],
        stopped: false,
    });
    // M7 margin marks mirror the run statuses (mark-time lines
    // from the fixture doc).
    session.run_marks = vec![
        crate::app::writer::runs::RunMark {
            run_id: 1,
            doc_index: 0,
            verb: Some("fix".to_string()),
            prompt: "typo".to_string(),
            line_text: "alpha @@fix typo@@this is teh@@".to_string(),
            status: RunMarkerStatus::Started,
        },
        crate::app::writer::runs::RunMark {
            run_id: 1,
            doc_index: 1,
            verb: Some("ask".to_string()),
            prompt: "capital".to_string(),
            line_text: "@@ask capital@@Paris@@".to_string(),
            status: RunMarkerStatus::Pending,
        },
    ];
    session
}

/// Retext the session like the watch reload does: document,
/// editor buffer, and revision move together, so the paint sees
/// one coherent state.
fn retext(session: &mut WriterSession, text: &str) {
    let doc = session.doc.as_mut().expect("doc");
    doc.text = text.to_string();
    doc.revision += 1;
    session.editor.as_mut().expect("editor").lines = edtui::Lines::from(text);
    session.marker_rev = None;
    session.reset_fence_cache();
}

/// M5-fix: after the first Stop the pill offers Force stop
/// (same pill, same funnel; the rect comes from the same label
/// the paint uses).
#[test]
fn process_pill_reads_stop_then_force_stop() {
    let mut session = run_session();
    assert_eq!(
        toolbar_action_label(&session, ToolbarButton::Process),
        "Stop"
    );
    session.process.as_mut().unwrap().stopped = true;
    assert_eq!(
        toolbar_action_label(&session, ToolbarButton::Process),
        "Force stop"
    );
    session.process = None;
    assert_eq!(
        toolbar_action_label(&session, ToolbarButton::Process),
        "Process"
    );
}

/// M7 margin column: the 2-cell side padding left of the
/// text, i.e. the frame border plus one.
fn margin_x() -> u16 {
    1
}

#[test]
fn pending_markers_paint_hourglass_in_the_margin_before_any_report() {
    use crate::app::writer::runs::RunMarkerStatus;
    let mut session = run_session();
    for mark in session.run_marks.iter_mut() {
        mark.status = RunMarkerStatus::Pending;
    }
    for marker in session.runs.iter_mut().flat_map(|r| r.markers.iter_mut()) {
        marker.status = RunMarkerStatus::Pending;
    }
    let buf = paint_doc_to(&mut session, 120, 30);
    for needle in ["teh", "Paris"] {
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains(needle))
            .expect("marker row");
        assert_eq!(
            buf[(margin_x(), row)].symbol(),
            "⏳",
            "queued marker waits in the margin: {needle}"
        );
    }
    assert!(
        !buffer_text(&buf).contains("⟳"),
        "no spin gutter anymore"
    );
    let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
    let status = row_text(&buf, layout.status.y, 0, 120);
    assert!(
        status.contains("run 1: 0 working · 2 queued"),
        "counts: {status:?}"
    );
}

#[test]
fn started_marker_paints_refresh_in_the_margin() {
    let mut session = run_session();
    let buf = paint_doc_to(&mut session, 120, 30);
    let row = (0..30)
        .find(|y| row_text(&buf, *y, 0, 120).contains("teh"))
        .expect("marker row");
    assert_eq!(
        buf[(margin_x(), row)].symbol(),
        "🔄",
        "started marker works in the margin"
    );
    assert!(
        !buffer_text(&buf).contains("⟳"),
        "the spin gutter is gone"
    );
}

#[test]
fn undelivered_run_waits_in_the_status() {
    let mut session = run_session();
    session.queue.push_back(
        "[forge writer \"d.md\" process 1 file rev 0, 2 markers]:\n<writer-process id=\"1\" file=\"d.md\" rev=\"0\">".to_string(),
    );
    let buf = paint_doc_to(&mut session, 120, 30);
    let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
    let status = row_text(&buf, layout.status.y, 0, 120);
    assert!(
        status.contains("Waiting for agent… (Stop)"),
        "status: {status:?}"
    );
}

#[test]
fn margin_glyph_follows_its_marker_across_a_reload() {
    use crate::app::writer::runs::RunMarkerStatus;
    let mut session = run_session();
    for mark in session.run_marks.iter_mut() {
        mark.status = RunMarkerStatus::Pending;
    }
    let buf = paint_doc_to(&mut session, 120, 30);
    let before = (0..30)
        .find(|y| buf[(margin_x(), *y)].symbol() == "⏳")
        .expect("hourglass row");
    // A reload inserts a head line, like the watch path.
    let text = "new head\n".to_string() + &session.doc.as_ref().expect("doc").text.clone();
    retext(&mut session, &text);
    let buf = paint_doc_to(&mut session, 120, 30);
    let after = (0..30)
        .find(|y| buf[(margin_x(), *y)].symbol() == "⏳")
        .expect("hourglass survives");
    assert_eq!(after, before + 1, "glyph follows the marker down");
}

#[test]
fn failure_margin_survives_until_its_line_is_edited() {
    use crate::app::writer::runs::{RunMarkerStatus, WriterRunState};
    let mut session = run_session();
    let run = session.runs.iter_mut().find(|r| r.id == 1).expect("run");
    run.state = WriterRunState::Finished { summary: "s".to_string() };
    // Only the second marker failed (the first is done and its
    // mark is gone, like the report path leaves them).
    run.markers[0].status = RunMarkerStatus::Done;
    run.markers[1].status = RunMarkerStatus::Failed;
    session.run_marks.retain(|m| m.doc_index == 1);
    session.run_marks[0].status = RunMarkerStatus::Failed;
    session.process = None;
    let buf = paint_doc_to(&mut session, 120, 30);
    let row = (0..30)
        .find(|y| row_text(&buf, *y, 0, 120).contains("Paris"))
        .expect("failed marker row");
    assert_eq!(buf[(margin_x(), row)].symbol(), "❌", "failure kept");
    // An edit elsewhere keeps it.
    let text = "head\n".to_string() + &session.doc.as_ref().expect("doc").text.clone();
    retext(&mut session, &text);
    let buf = paint_doc_to(&mut session, 120, 30);
    assert_eq!(
        buf[(margin_x(), row + 1)].symbol(),
        "❌",
        "shifts keep the failure"
    );
    // An edit on its own line clears it (the marker survives,
    // the line text changed).
    let text = session.doc.as_ref().expect("doc").text.replacen("Paris", "Pariss", 1);
    retext(&mut session, &text);
    let buf = paint_doc_to(&mut session, 120, 30);
    assert!(
        (0..30).all(|y| buf[(margin_x(), y)].symbol() != "❌"),
        "edited line clears the failure"
    );
}

#[test]
fn text_column_x_is_stable_across_a_run() {
    use crate::app::writer::runs::RunMarkerStatus;
    let column = |session: &mut WriterSession| -> u16 {
        let buf = paint_doc_to(session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("alpha"))
            .expect("text row");
        (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("alpha"))
            .expect("text cell")
    };
    let mut plain = doc_session("alpha @@fix typo@@this is teh@@\n");
    let before = column(&mut plain);
    let mut session = run_session();
    for mark in session.run_marks.iter_mut() {
        mark.status = RunMarkerStatus::Pending;
    }
    let during = column(&mut session);
    session.process = None;
    session.run_marks.clear();
    let after = column(&mut session);
    assert_eq!((before, during, after), (before, before, before));
}

#[test]
fn wrapped_marker_marks_only_its_first_screen_row() {
    use crate::app::writer::runs::RunMarkerStatus;
    let target = "w".repeat(200);
    let text = format!("@@fix long@@{target}@@\n");
    let mut session = doc_session(&text);
    session.run_marks.push(crate::app::writer::runs::RunMark {
        run_id: 1,
        doc_index: 0,
        verb: Some("fix".to_string()),
        prompt: "long".to_string(),
        line_text: text.trim_end().to_string(),
        status: RunMarkerStatus::Pending,
    });
    let buf = paint_doc_to(&mut session, 120, 30);
    let first = (0..30)
        .find(|y| buf[(margin_x(), *y)].symbol() == "⏳")
        .expect("hourglass on the first row");
    assert_eq!(
        buf[(margin_x(), first + 1)].symbol(),
        " ",
        "wrapped continuation stays unmarked"
    );
}

#[test]
fn run_margin_wins_over_the_error_gutter_on_a_shared_row() {
    use crate::app::writer::runs::RunMarkerStatus;
    let text = "@@fix typo@@this is teh@@ @@oops\n";
    let mut session = doc_session(text);
    session.run_marks.push(crate::app::writer::runs::RunMark {
        run_id: 1,
        doc_index: 0,
        verb: Some("fix".to_string()),
        prompt: "typo".to_string(),
        line_text: "@@fix typo@@this is teh@@ @@oops".to_string(),
        status: RunMarkerStatus::Pending,
    });
    session.process = Some(crate::app::writer::process::ProcessLock {
        run_id: 1,
        pre_text: text.to_string(),
        outside: vec![String::new(), String::new()],
        spans: vec![0..26],
        stopped: false,
    });
    let buf = paint_doc_to(&mut session, 120, 30);
    let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
    let row = (0..30)
        .find(|y| row_text(&buf, *y, 0, 120).contains("teh"))
        .expect("shared row");
    assert_eq!(buf[(margin_x(), row)].symbol(), "⏳", "margin wins");
    assert_eq!(
        buf[(layout.gutter.x, row)].symbol(),
        " ",
        "gutter stands down on the shared row"
    );
}

#[test]
fn run_status_shows_counts_and_details() {
    let mut session = run_session();
    session.process = None;
    session.last_run = Some(crate::app::writer::process::LastRun {
        id: 3,
        done: 3,
        skipped: 1,
        failed: 0,
    });
    let buf = paint_doc_to(&mut session, 120, 30);
    let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
    let status = row_text(&buf, layout.status.y, 0, 120);
    assert!(
        status.contains("Run: 3 done · 1 skipped (Details)"),
        "status: {status:?}"
    );
}

#[test]
fn violation_banner_paints_revert_and_keep_pills() {
    let mut session = run_session();
    session.pending_confirm = Some(crate::app::writer::PendingConfirm {
        message: "Agent changed text outside markers".to_string(),
        actions: vec![
            crate::app::writer::ConfirmAction::RevertRun("pre".to_string()),
            crate::app::writer::ConfirmAction::Keep,
        ],
    });
    let buf = paint_doc_to(&mut session, 120, 30);
    let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
    let slot = row_text(&buf, layout.error.y, 0, 120);
    assert!(slot.contains("Agent changed text outside markers"), "slot: {slot:?}");
    assert!(slot.contains("Revert run"), "slot: {slot:?}");
    assert!(slot.contains("Keep"), "slot: {slot:?}");
}
