use super::*;
use crate::app::test_support::*;

fn writer_agent() -> (AppState, crate::session::SessionId, String, std::path::PathBuf) {
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

fn open_doc(state: &mut AppState, run: &str, path: &str) -> String {
    comms_reply(state, run, "writer_open", &format!(r#"{{"path":{}}}"#, crate::ipc::mcp::escape_json(path)))
}

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

fn open_editor(state: &mut AppState, id: crate::session::SessionId) {
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

fn feed(state: &mut AppState, id: crate::session::SessionId, code: crossterm::event::KeyCode) {
    state.writer_feed_key(
        id,
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    );
}

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
    use adapter::editor_selection_to_range;
    use edtui::{actions::SwitchMode, EditorMode, EditorState, Index2, Lines};
    // Empty document: no selection, no range.
    let empty = EditorState::new(Lines::from(""));
    assert_eq!(editor_selection_to_range(&empty), None);
    // Zero-width visual selection is a cursor, not a range.
    let mut plain = EditorState::new(Lines::from("hello"));
    plain.execute(SwitchMode(EditorMode::Visual));
    assert_eq!(editor_selection_to_range(&plain), None);
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
    // Shift+Ctrl+Right runs to the next word start (EdTUI word motion),
    // taking the separating space: standard Ctrl+Shift+Right behavior.
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

fn propose_for_accept(state: &mut AppState, id: crate::session::SessionId, run: &str, rid: u64, text: &str) -> u64 {
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

fn shift_select(state: &mut AppState, id: crate::session::SessionId, count: usize) {
    let shift = crossterm::event::KeyModifiers::SHIFT;
    for _ in 0..count {
        state.writer_feed_key(
            id,
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Right, shift),
        );
    }
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
    editor.cursor = adapter::offset_to_index2("head\n\nfirst para\nsecond line\n\ntail", 20);
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
