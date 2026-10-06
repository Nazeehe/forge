    //! Process-run agent side (§6.1): start/report/done dispatch,
    //! markup delivery, and finish paths (done, turn-end, timeout).

    use super::super::test_support::*;
    use crate::app::test_support::*;
    use crate::writer::process::ProcessShape;

    const DOC: &str = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";

    fn seeded() -> (
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

    fn finish(state: &mut crate::app::AppState, id: crate::session::SessionId, dir: &std::path::PathBuf) {
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn start_run_enqueues_markup_and_tracks_state() {
        let (mut state, id, _run, dir) = seeded();
        let now = std::time::Instant::now();
        let rid = state.writer_start_run(id, now).expect("run starts");
        let session = state.writers.get(&id).unwrap();
        assert_eq!(rid, 1);
        let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
        assert!(matches!(
            run.state,
            crate::app::writer::runs::WriterRunState::Active
        ));
        assert_eq!(run.markers.len(), 2);
        assert_eq!(run.markers[0].marker.shape, ProcessShape::Wrap);
        assert_eq!(run.markers[0].marker.verb.as_deref(), Some("fix"));
        assert_eq!(run.markers[0].marker.prompt, "typo");
        assert_eq!(run.markers[1].marker.line, 3);
        let body = session.queue.back().expect("markup queued");
        assert!(body.contains("<writer-process id=\"1\""), "body: {body}");
        assert!(body.contains("writer_run_report"), "rules: {body}");
        assert!(body.contains("writer_run_done"), "rules: {body}");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn start_run_refuses_without_markers() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("p.md"), "plain text\n").unwrap();
        assert!(open_doc(&mut state, &run, "p.md").contains(r#""ok":true"#));
        let err = state
            .writer_start_run(id, std::time::Instant::now())
            .unwrap_err();
        assert!(err.contains("No markers in"), "err: {err}");
        assert!(state.writers.get(&id).unwrap().runs.is_empty(), "no orphan run");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn start_run_refuses_a_full_queue() {
        let (mut state, id, _run, dir) = seeded();
        state.writers.get_mut(&id).unwrap().queue.resize(8, String::new());
        let err = state
            .writer_start_run(id, std::time::Instant::now())
            .unwrap_err();
        assert!(err.contains("queue full"), "err: {err}");
        assert!(state.writers.get(&id).unwrap().runs.is_empty(), "run held, not dropped");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn start_run_refuses_without_a_live_agent() {
        let (mut state, id, _run, dir) = seeded();
        state
            .manager
            .get_mut(id)
            .unwrap()
            .tabs
            .retain(|tab| tab.kind != crate::session::TabKind::Agent);
        let err = state
            .writer_start_run(id, std::time::Instant::now())
            .unwrap_err();
        assert!(err.contains("no live agent"), "err: {err}");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn report_updates_status_and_done_closes_with_a_note() {
        let (mut state, id, run, dir) = seeded();
        let now = std::time::Instant::now();
        let rid = state.writer_start_run(id, now).expect("run starts");
        let reply = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            &format!(r#"{{"run":{rid},"index":0,"status":"started"}}"#),
        );
        assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
        // Reports leave the thread alone: finish posts the one note.
        assert!(state.writers.get(&id).unwrap().thread.is_empty());
        let reply = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            &format!(r#"{{"run":{rid},"index":0,"status":"done","note":"tightened"}}"#),
        );
        assert!(reply.contains(r#""reported":true"#), "reply: {reply}");
        let reply = comms_reply(
            &mut state,
            &run,
            "writer_run_done",
            &format!(r#"{{"run":{rid},"summary":"both markers applied"}}"#),
        );
        assert!(reply.contains(r#""done":true"#), "reply: {reply}");
        let session = state.writers.get(&id).unwrap();
        assert!(matches!(
            session.runs.iter().find(|r| r.id == rid).expect("run kept").state,
            crate::app::writer::runs::WriterRunState::Finished { .. }
        ));
        let note = session.thread.last().expect("finish note");
        assert!(note.answer.contains("both markers applied"), "note: {}", note.answer);
        // Done twice is refused.
        let again = comms_reply(
            &mut state,
            &run,
            "writer_run_done",
            &format!(r#"{{"run":{rid},"summary":"x"}}"#),
        );
        assert!(again.contains("already finished"), "again: {again}");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn report_refuses_bad_run_index_and_status() {
        let (mut state, id, run, dir) = seeded();
        let now = std::time::Instant::now();
        let rid = state.writer_start_run(id, now).expect("run starts");
        let unknown = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            r#"{"run":99,"index":0,"status":"started"}"#,
        );
        assert!(unknown.contains("unknown run 99"), "unknown: {unknown}");
        let bad_index = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            &format!(r#"{{"run":{rid},"index":7,"status":"started"}}"#),
        );
        assert!(bad_index.contains("unknown marker index 7"), "index: {bad_index}");
        let bad_status = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            &format!(r#"{{"run":{rid},"index":0,"status":"bogus"}}"#),
        );
        assert!(bad_status.contains("bad status"), "status: {bad_status}");
        let bad_run = comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            r#"{"run":"x","index":0,"status":"started"}"#,
        );
        assert!(bad_run.contains("must be a positive integer"), "run: {bad_run}");
        let foreign = comms_reply(
            &mut state,
            "bogus-run",
            "writer_run_report",
            &format!(r#"{{"run":{rid},"index":0,"status":"started"}}"#),
        );
        assert!(foreign.contains("unknown or stale run ID"), "foreign: {foreign}");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn turn_end_without_done_finishes_the_run() {
        let (mut state, id, _run, dir) = seeded();
        let t0 = std::time::Instant::now();
        let rid = state.writer_start_run(id, t0).expect("run starts");
        // Idle from the start: no turn happened, the run survives.
        state.settle_writer_runs(t0);
        assert!(
            state.writers.get(&id).unwrap().runs.iter().any(|r| r.id == rid
                && matches!(r.state, crate::app::writer::runs::WriterRunState::Active)),
            "idle without work is not a turn-end"
        );
        // The agent works a turn, then goes quiet: that end finishes.
        state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Thinking;
        state.settle_writer_runs(t0);
        state.manager.get_mut(id).unwrap().activity = crate::session::Activity::Idle;
        state.settle_writer_runs(t0 + std::time::Duration::from_secs(10));
        let session = state.writers.get(&id).unwrap();
        let run = session.runs.iter().find(|r| r.id == rid).expect("run kept");
        match &run.state {
            crate::app::writer::runs::WriterRunState::Finished { summary } => {
                assert!(summary.contains("turn ended"), "summary: {summary}")
            }
            other => panic!("run finished on turn-end: {other:?}"),
        }
        assert_eq!(session.thread.len(), 1, "one finish note");
        finish(&mut state, id, &dir);
    }

    #[test]
    fn timeout_finishes_a_silent_run_but_not_a_reporting_one() {
        let (mut state, id, run, dir) = seeded();
        let t0 = std::time::Instant::now();
        let silent = state.writer_start_run(id, t0).expect("silent run");
        let late = t0 + std::time::Duration::from_secs(121);
        state.settle_writer_runs(late);
        let session = state.writers.get(&id).unwrap();
        match &session.runs.iter().find(|r| r.id == silent).expect("run kept").state {
            crate::app::writer::runs::WriterRunState::Finished { summary } => {
                assert!(summary.contains("timed out"), "summary: {summary}")
            }
            other => panic!("silent run timed out: {other:?}"),
        }
        // A run that reported is the turn's problem, not the timer's.
        let t1 = late + std::time::Duration::from_secs(1);
        let talking = state.writer_start_run(id, t1).expect("talking run");
        comms_reply(
            &mut state,
            &run,
            "writer_run_report",
            &format!(r#"{{"run":{talking},"index":1,"status":"blocked","note":"needs context"}}"#),
        );
        state.settle_writer_runs(t1 + std::time::Duration::from_secs(600));
        assert!(
            state.writers.get(&id).unwrap().runs.iter().any(|r| r.id == talking
                && matches!(r.state, crate::app::writer::runs::WriterRunState::Active)),
            "reporting run survives the timer"
        );
        finish(&mut state, id, &dir);
    }

    #[test]
    fn run_list_evicts_oldest_finished_first() {
        let (mut state, id, run, dir) = seeded();
        let mut now = std::time::Instant::now();
        for _ in 0..8 {
            now += std::time::Duration::from_secs(1);
            let rid = state.writer_start_run(id, now).expect("run starts");
            // The settle path delivers the queued markup; drain it here.
            state.writers.get_mut(&id).unwrap().queue.pop_front();
            comms_reply(
                &mut state,
                &run,
                "writer_run_done",
                &format!(r#"{{"run":{rid},"summary":"s"}}"#),
            );
        }
        now += std::time::Duration::from_secs(1);
        let ninth = state.writer_start_run(id, now).expect("evicts finished");
        let ids: Vec<u64> = state.writers.get(&id).unwrap().runs.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 8);
        assert!(!ids.contains(&1), "oldest finished evicted");
        assert!(ids.contains(&ninth));
        finish(&mut state, id, &dir);
    }
