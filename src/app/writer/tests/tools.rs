use super::super::test_support::*;
use crate::app::test_support::*;
use crate::writer::request::WriterAction;
use super::super::WriterRequestState;

#[test]
fn open_creates_and_opens_markdown() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("notes.md"), "# hi\nbody\n").unwrap();
    let opened = open_doc(&mut state, &run, "notes.md");
    assert!(opened.contains(r#""ok":true"#), "opened: {opened}");
    assert!(opened.contains(r#""revision":0"#), "opened: {opened}");
    let session = state.writers.get(&id).expect("writer state");
    assert_eq!(session.doc.as_ref().unwrap().text, "# hi\nbody\n");
    // A missing file opens as an empty document, written on first save.
    let created = open_doc(&mut state, &run, "new.md");
    assert!(created.contains(r#""ok":true"#), "created: {created}");
    assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_refuses_over_unsaved_document() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("a.md"), "aaa").unwrap();
    assert!(open_doc(&mut state, &run, "a.md").contains(r#""ok":true"#));
    state.writers.get_mut(&id).unwrap().doc.as_mut().unwrap()
        .apply_edit(0..3, "bbb").unwrap();
    let refused = open_doc(&mut state, &run, "b.md");
    assert!(refused.contains(r#""ok":false"#), "refused: {refused}");
    assert!(refused.contains("unsaved document open"), "refused: {refused}");
    // The dirty document is untouched.
    assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "bbb");
    // Reopening the same path is fine.
    let same = open_doc(&mut state, &run, "a.md");
    assert!(same.contains(r#""ok":true"#), "same: {same}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_never_switches_the_view_in_the_slice() {
    let (mut state, _id, run, dir) = writer_agent();
    assert!(state.overlay_view.is_none());
    open_doc(&mut state, &run, "q.md");
    // No Writer tab exists in S3: the open stays silent (Q1).
    assert!(state.overlay_view.is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn foreign_run_id_is_refused_for_all_writer_tools() {
    let (mut state, _id, _run, dir) = writer_agent();
    for (tool, args) in [
        ("writer_open", r#"{"path":"x.md"}"#),
        ("writer_read", "{}"),
        ("writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#),
        ("writer_answer", r#"{"request_id":1,"answer":"x"}"#),
        (
            "writer_run_report",
            r#"{"run":1,"index":0,"status":"done"}"#,
        ),
        ("writer_run_done", r#"{"run":1,"summary":"x"}"#),
    ] {
        let reply = comms_reply(&mut state, "bogus-run", tool, args);
        assert!(reply.contains("unknown or stale run ID"), "{tool}: {reply}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_by_request_id_uses_the_exact_range() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello world").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Rephrase, "hello world", 6..11, 0).unwrap();
    let reply = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"Forge","note":"tight"}}"#),
    );
    assert!(reply.contains(r#""proposed":true"#), "reply: {reply}");
    assert!(reply.contains(r#""proposal":1"#), "reply: {reply}");
    let session = state.writers.get(&id).unwrap();
    let proposal = session.proposals.get(1).unwrap();
    assert_eq!(proposal.range, 6..11, "exact request range, no rounding");
    assert_eq!(proposal.original, "world");
    assert_eq!(proposal.request_id, Some(rid));
    // Proposing records; only Accept (S4) applies.
    assert_eq!(session.doc.as_ref().unwrap().text, "hello world");
    assert_eq!(session.requests.iter().find(|r| r.id == rid).unwrap().state, WriterRequestState::Proposed);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_rejects_unknown_and_cancelled_requests() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello world").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let unknown = comms_reply(&mut state, &run, "writer_propose", r#"{"request_id":99,"text":"x"}"#);
    assert!(unknown.contains("unknown request 99"), "unknown: {unknown}");
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Ask, "hello world", 0..5, 0).unwrap();
    assert!(state.writers.get_mut(&id).unwrap().cancel_request(rid));
    let cancelled = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"x"}}"#),
    );
    assert!(cancelled.contains("cancelled"), "cancelled: {cancelled}");
    let cancelled_answer = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"request_id":{rid},"answer":"x"}}"#),
    );
    assert!(cancelled_answer.contains("cancelled"), "cancelled answer: {cancelled_answer}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_after_answer_and_answer_after_propose() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello world").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    // Propose first, then answer: both land.
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Ask, "hello world", 0..5, 0).unwrap();
    let proposed = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"hi"}}"#),
    );
    assert!(proposed.contains(r#""proposed":true"#), "proposed: {proposed}");
    let answered = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"request_id":{rid},"answer":"why"}}"#),
    );
    assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
    // Answer first, then propose: both land the other way round.
    let rid2 = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Chat, "hello world", 0..5, 0).unwrap();
    let answered2 = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"request_id":{rid2},"answer":"done"}}"#),
    );
    assert!(answered2.contains(r#""answered":true"#), "answered2: {answered2}");
    let proposed2 = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid2},"text":"yo"}}"#),
    );
    assert!(proposed2.contains(r#""proposed":true"#), "proposed2: {proposed2}");
    let session = state.writers.get(&id).unwrap();
    assert_eq!(
        session.requests.iter().find(|r| r.id == rid).unwrap().state,
        WriterRequestState::Both
    );
    assert_eq!(
        session.requests.iter().find(|r| r.id == rid2).unwrap().state,
        WriterRequestState::Both
    );
    // Each kind arrives once: repeats are refused.
    let propose_again = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"z"}}"#),
    );
    assert!(propose_again.contains("already proposed"), "repeat propose: {propose_again}");
    let answer_again = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"request_id":{rid},"answer":"z"}}"#),
    );
    assert!(answer_again.contains("already answered"), "repeat answer: {answer_again}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn malformed_request_id_errors_plainly() {
    let (mut state, _id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    for args in [
        r#"{"request_id":-1,"text":"x"}"#,
        r#"{"request_id":"x","text":"x"}"#,
        r#"{"request_id":-1,"text":"x","start_line":1,"end_line":1}"#,
    ] {
        let reply = comms_reply(&mut state, &run, "writer_propose", args);
        assert!(
            reply.contains("request_id must be a positive integer"),
            "args {args}: {reply}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_arriving_after_drift_is_stale() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Rephrase, "aaa bbb ccc", 4..7, 0).unwrap();
    // Human types above the range; the request-time text moved.
    state.writers.get_mut(&id).unwrap().doc.as_mut().unwrap()
        .apply_edit(0..0, "XX").unwrap();
    let reply = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"B"}}"#),
    );
    assert!(reply.contains(r#""proposed":true"#), "reply: {reply}");
    assert!(reply.contains(r#""stale":true"#), "stale flag: {reply}");
    assert!(reply.contains("text changed since request"), "reason: {reply}");
    let session = state.writers.get(&id).unwrap();
    let proposal = session.proposals.get(1).unwrap();
    assert_eq!(proposal.original, "bbb", "request-time snapshot kept");
    use crate::writer::proposal::ProposalState;
    use crate::writer::WriterError;
    assert_eq!(proposal.state, ProposalState::Stale);
    // A stale arrival can never be accepted (S2 snapshot check).
    let session = state.writers.get_mut(&id).unwrap();
    let doc = session.doc.as_mut().unwrap();
    assert_eq!(
        session.proposals.accept(doc, 1).unwrap_err(),
        WriterError::StaleProposal(1)
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_without_drift_stays_pending() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Rephrase, "aaa bbb ccc", 4..7, 0).unwrap();
    let reply = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{rid},"text":"B"}}"#),
    );
    assert!(reply.contains(r#""stale":false"#), "fresh: {reply}");
    use crate::writer::proposal::ProposalState;
    assert_eq!(
        state.writers.get(&id).unwrap().proposals.get(1).unwrap().state,
        ProposalState::Pending
    );
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn thread_and_finished_requests_are_bounded() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    for _ in 0..70 {
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Chat, "hello", 0..5, 0).unwrap();
        let answered = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid},"answer":"a"}}"#),
        );
        assert!(answered.contains(r#""answered":true"#));
    }
    // Three requests stay open; they are never evicted.
    let mut keepers = Vec::new();
    for _ in 0..3 {
        keepers.push(state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Ask, "hello", 0..5, 0).unwrap());
    }
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.thread.len(), crate::writer::MAX_THREAD_ENTRIES);
    assert_eq!(session.thread[0].request_id, 7, "oldest answers evicted first");
    assert_eq!(session.requests.len(), 64 + 3);
    for rid in &keepers {
        assert_eq!(
            session.requests.iter().find(|r| r.id == *rid).unwrap().state,
            WriterRequestState::Open,
            "open {rid} evicted"
        );
    }
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finished_requests_evict_both_and_settled_proposed() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    // 70 rephrase-style requests: propose, then accept or reject.
    // Every tenth also gets an answer (Both). Proposal ids run 1..=70
    // in order: exactly one proposal is recorded per iteration.
    for i in 0..70u64 {
        let text = state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone();
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Rephrase, &text, 0..0, 0).unwrap();
        let proposed = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"v{i}"}}"#),
        );
        assert!(proposed.contains(r#""proposed":true"#), "iter {i}: {proposed}");
        if i % 10 == 0 {
            let answered = comms_reply(
                &mut state, &run, "writer_answer",
                &format!(r#"{{"request_id":{rid},"answer":"a"}}"#),
            );
            assert!(answered.contains(r#""answered":true"#));
        }
        let pid = (i + 1) as u64;
        let session = state.writers.get_mut(&id).unwrap();
        if i % 2 == 0 {
            let doc = session.doc.as_mut().unwrap();
            session.proposals.accept(doc, pid).unwrap();
        } else {
            session.proposals.reject(pid).unwrap();
        }
        session.evict_finished();
    }
    // One more request stays Proposed with a pending proposal.
    let text = state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone();
    let keeper = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Ask, &text, 0..0, 0).unwrap();
    let kept = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"request_id":{keeper},"text":"k"}}"#),
    );
    assert!(kept.contains(r#""proposed":true"#), "keeper: {kept}");
    state.writers.get_mut(&id).unwrap().evict_finished();
    // 64 finished (Accepted/Rejected/Both mixed) + the pending keeper.
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.requests.len(), 65, "bounded");
    let kept_rec = session.requests.iter().find(|r| r.id == keeper).unwrap();
    assert_eq!(kept_rec.state, WriterRequestState::Proposed);
    assert!(session.requests.iter().all(|r| r.id > 6 || r.id == keeper || {
        !matches!(
            r.state,
            WriterRequestState::Answered
                | WriterRequestState::Cancelled
                | WriterRequestState::Both
        )
    }), "oldest finished evicted first");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn line_inserts_and_appends_stay_line_granular() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    // Insert mid-doc gains its newline.
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"New","start_line":2,"end_line":1}"#);
    // Insert at line 1 the same.
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"Top","start_line":1,"end_line":0}"#);
    // Append without a trailing newline is separated.
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"End","start_line":4,"end_line":4}"#);
    // A line replace stays verbatim.
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"R","start_line":2,"end_line":2}"#);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.proposals.get(1).unwrap().range, 3..3);
    assert_eq!(session.proposals.get(1).unwrap().text, "New\n");
    assert_eq!(session.proposals.get(2).unwrap().range, 0..0);
    assert_eq!(session.proposals.get(2).unwrap().text, "Top\n");
    assert_eq!(session.proposals.get(3).unwrap().range, 8..8);
    assert_eq!(session.proposals.get(3).unwrap().text, "\nEnd");
    assert_eq!(session.proposals.get(4).unwrap().range, 3..5);
    assert_eq!(session.proposals.get(4).unwrap().text, "R");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn append_with_trailing_newline_and_empty_doc() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("t.md"), "a\n").unwrap();
    assert!(open_doc(&mut state, &run, "t.md").contains(r#""ok":true"#));
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"B","start_line":3,"end_line":3}"#);
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.proposals.get(1).unwrap().range, 2..2);
    assert_eq!(session.proposals.get(1).unwrap().text, "B\n");
    // Empty document: append addresses the start.
    std::fs::write(dir.join("e.md"), "").unwrap();
    assert!(open_doc(&mut state, &run, "e.md").contains(r#""ok":true"#));
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":2,"end_line":2}"#);
    // Opening the new document reset the proposal store: ids restart.
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.proposals.get(1).unwrap().range, 0..0);
    assert_eq!(session.proposals.get(1).unwrap().text, "A\n");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_by_line_range_insert_and_append() {
    let (mut state, _id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    // Whole line 2.
    let r2 = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"L2","start_line":2,"end_line":2}"#);
    assert!(r2.contains(r#""proposal":1"#), "line range: {r2}");
    // Insert before line 2 (end = start - 1).
    let ins = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"I","start_line":2,"end_line":1}"#);
    assert!(ins.contains(r#""proposal":2"#), "insert: {ins}");
    // Append past the last line (start = total + 1).
    let app = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":4,"end_line":4}"#);
    assert!(app.contains(r#""proposal":3"#), "append: {app}");
    for bad in [
        r#"{"text":"x","start_line":0,"end_line":1}"#,
        r#"{"text":"x","start_line":3,"end_line":1}"#,
        r#"{"text":"x","start_line":9,"end_line":9}"#,
        r#"{"text":"x"}"#,
    ] {
        let reply = comms_reply(&mut state, &run, "writer_propose", bad);
        assert!(reply.contains(r#""ok":false"#), "bad {bad}: {reply}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_line_ranges_map_to_exact_char_ranges() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"L2","start_line":2,"end_line":2}"#);
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"I","start_line":2,"end_line":1}"#);
    comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":4,"end_line":4}"#);
    let session = state.writers.get(&id).unwrap();
    // "l1\nl2\nl3": line 2 is chars 3..5; insert/append are empty at 3 and 8.
    assert_eq!(session.proposals.get(1).unwrap().range, 3..5);
    assert_eq!(session.proposals.get(1).unwrap().original, "l2");
    assert_eq!(session.proposals.get(2).unwrap().range, 3..3);
    assert_eq!(session.proposals.get(3).unwrap().range, 8..8);
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn propose_refuses_oversize_and_over_cap() {
    let (mut state, _id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "abc").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let big = "x".repeat(crate::writer::MAX_PROPOSAL_CHARS + 1);
    let too_big = comms_reply(
        &mut state, &run, "writer_propose",
        &format!(r#"{{"text":"{big}","start_line":1,"end_line":1}}"#),
    );
    assert!(too_big.contains("too large"), "oversize: {too_big}");
    for _ in 0..crate::writer::MAX_PENDING_PROPOSALS {
        let reply = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
        assert!(reply.contains(r#""proposed":true"#), "fill: {reply}");
    }
    let capped = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
    assert!(capped.contains("too many pending"), "cap: {capped}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn answer_stores_thread_entry_and_rejects_repeats() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "hello").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Chat, "hello", 0..5, 0).unwrap();
    let reply = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r##"{{"request_id":{rid},"answer":"# hi"}}"##),
    );
    assert!(reply.contains(r#""answered":true"#), "reply: {reply}");
    let session = state.writers.get(&id).unwrap();
    assert_eq!(session.thread.len(), 1);
    assert_eq!(session.thread[0].request_id, rid);
    assert_eq!(session.thread[0].answer, "# hi");
    let double = comms_reply(
        &mut state, &run, "writer_answer",
        &format!(r#"{{"request_id":{rid},"answer":"again"}}"#),
    );
    assert!(double.contains("already answered"), "double: {double}");
    let unknown = comms_reply(&mut state, &run, "writer_answer", r#"{"request_id":77,"answer":"x"}"#);
    assert!(unknown.contains("unknown request 77"), "unknown: {unknown}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_returns_metadata_selection_and_lines() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    let rid = state.writers.get_mut(&id).unwrap()
        .new_request(WriterAction::Ask, "l1\nl2\nl3", 0..2, 0).unwrap();
    state.writers.get_mut(&id).unwrap().selection = Some(3..5);
    let reply = comms_reply(&mut state, &run, "writer_read", "{}");
    assert!(reply.contains(r#""path":"d.md""#), "reply: {reply}");
    assert!(reply.contains(r#""revision":0"#), "reply: {reply}");
    assert!(reply.contains(r#""total_lines":3"#), "reply: {reply}");
    assert!(reply.contains(&format!(r#""pending_requests":[{rid}]"#)), "reply: {reply}");
    assert!(reply.contains(r#""selection":{"start":3,"end":5"#), "reply: {reply}");
    assert!(reply.contains("l1") && reply.contains("l3"), "reply: {reply}");
    // A line slice narrows the payload.
    let slice = comms_reply(&mut state, &run, "writer_read", r#"{"start_line":2,"end_line":2}"#);
    assert!(slice.contains("l2"), "slice: {slice}");
    assert!(!slice.contains("l1"), "slice: {slice}");
    assert!(state.manager.remove(id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_truncates_past_the_cap_with_a_note() {
    let (mut state, _id, run, dir) = writer_agent();
    let big = "y".repeat(crate::writer::MAX_READ_CHARS + 100);
    std::fs::write(dir.join("big.md"), &big).unwrap();
    assert!(open_doc(&mut state, &run, "big.md").contains(r#""ok":true"#));
    let reply = comms_reply(&mut state, &run, "writer_read", "{}");
    assert!(reply.contains(r#""truncated":true"#), "reply truncated");
    assert!(reply.contains("start_line"), "reread hint: {reply}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_without_an_open_document_fails() {
    let (mut state, _id, run, dir) = writer_agent();
    let reply = comms_reply(&mut state, &run, "writer_read", "{}");
    assert!(reply.contains(r#""ok":false"#), "reply: {reply}");
    assert!(reply.contains("no Writer document open"), "reply: {reply}");
    let propose = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
    assert!(propose.contains("no Writer document open"), "propose: {propose}");
    let answer = comms_reply(&mut state, &run, "writer_answer", r#"{"request_id":1,"answer":"x"}"#);
    assert!(answer.contains("no Writer document open"), "answer: {answer}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn terminate_drops_writer_state() {
    let (mut state, id, run, dir) = writer_agent();
    std::fs::write(dir.join("d.md"), "x").unwrap();
    assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
    assert!(state.writers.contains_key(&id));
    assert!(state.terminate_session(id));
    assert!(!state.writers.contains_key(&id), "writer state drops with the session");
    std::fs::remove_dir_all(&dir).ok();
}
