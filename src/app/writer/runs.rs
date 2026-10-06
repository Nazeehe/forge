//! Process runs (spec §6.1): run state in `app/writer`, the agent
//! tools, and the finish paths. A `<writer-process>` request starts
//! a run (one entry per marker); the agent edits the file directly
//! and reports per-marker progress. Runs finish on done, on
//! turn-end without done, or on [`RUN_TIMEOUT`] with no report.
//! Finish posts a single thread note; reports never touch the
//! thread. No UI here (M5 owns the panel and toolbar).

use std::time::Instant;

use crate::app::AppState;
use crate::writer::markers::parse_markers;
use crate::writer::process::{ProcessMarker, WriterProcess};
use crate::writer::{MAX_RUNS_PER_DOC, RUN_TIMEOUT};

/// Per-marker agent progress inside a run. The wire vocabulary is
/// `started` (optional: lights the M5 progress gutter), `done`,
/// `skipped`, and `failed` — the same set the run rules text names,
/// so the two cannot drift (see `tool_status_set_matches_the_rules_text`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMarkerStatus {
    /// Listed, agent silent so far.
    #[default]
    Pending,
    /// Agent started this marker (optional progress signal).
    Started,
    /// Agent handled this marker.
    Done,
    /// Agent left this marker untouched (reason in note).
    Skipped,
    /// Agent tried and failed (reason in note); marker untouched.
    Failed,
}

impl RunMarkerStatus {
    /// Wire name the agent reports.
    pub fn name(&self) -> &'static str {
        match self {
            RunMarkerStatus::Pending => "pending",
            RunMarkerStatus::Started => "started",
            RunMarkerStatus::Done => "done",
            RunMarkerStatus::Skipped => "skipped",
            RunMarkerStatus::Failed => "failed",
        }
    }

    /// Parse a reported status; anything else is refused.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "started" => Some(RunMarkerStatus::Started),
            "done" => Some(RunMarkerStatus::Done),
            "skipped" => Some(RunMarkerStatus::Skipped),
            "failed" => Some(RunMarkerStatus::Failed),
            _ => None,
        }
    }
}

/// One marker under a run: the excerpt the agent saw plus progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunMarker {
    pub marker: ProcessMarker,
    pub status: RunMarkerStatus,
    pub note: Option<String>,
}

/// A run is open until done, turn-end, or timeout closes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriterRunState {
    Active,
    Finished { summary: String },
}

/// One `<writer-process>` run: the marker list plus agent progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterRun {
    pub id: u64,
    pub file: String,
    pub rev: u64,
    pub started_at: Instant,
    pub markers: Vec<RunMarker>,
    pub state: WriterRunState,
    /// The agent worked while this run was open (turn-end needs it:
    /// idle-without-work is not a turn ending).
    pub saw_activity: bool,
    /// Any report arrived (only silent runs time out).
    pub reported: bool,
}

impl AppState {
    /// Start a run over the open document's markers: refuse empty
    /// docs, full queues, silent agents, and full run lists first (no
    /// orphans), then record the run and queue its markup for the
    /// settle flush. Returns the run id. `pub(crate)`: the Process
    /// control (M5) calls this; tests drive it directly.
    pub(crate) fn writer_start_run(
        &mut self,
        id: crate::session::SessionId,
        now: Instant,
    ) -> Result<u64, String> {
        if !self.writer_agent_live(id) {
            return Err("no live agent tab".to_string());
        }
        let queue_full = self
            .writers
            .get(&id)
            .map(|session| session.queue.len() >= super::adapter::MAX_WRITER_QUEUE)
            .unwrap_or(false);
        if queue_full {
            return Err("writer queue full (8); wait for delivery".to_string());
        }
        let (doc_text, rev, path_rel) = match self.writers.get(&id).and_then(|s| s.doc.as_ref()) {
            Some(doc) => (doc.text.clone(), doc.revision, doc.path_rel.clone()),
            None => return Err("no Writer document open for this session".to_string()),
        };
        let name = Self::writer_doc_name(&path_rel);
        let parsed = parse_markers(&doc_text);
        if parsed.markers.is_empty() {
            return Err(format!("No markers in {name}"));
        }
        let process = WriterProcess::over_markers(
            self.writers.get(&id).map(|s| s.next_run_id + 1).unwrap_or(1),
            &name,
            &doc_text,
            rev,
            &parsed,
        )
        .map_err(|e| e.to_string())?;
        let Some(session) = self.writers.get_mut(&id) else {
            return Err("no Writer document open for this session".to_string());
        };
        if session.runs.len() >= MAX_RUNS_PER_DOC {
            match session.runs.iter().position(|r| {
                matches!(r.state, WriterRunState::Finished { .. })
            }) {
                Some(oldest) => {
                    session.runs.remove(oldest);
                }
                None => {
                    return Err("run list full (8); wait for a run to finish".to_string());
                }
            }
        }
        let body = process.markup();
        session.next_run_id += 1;
        let rid = session.next_run_id;
        let run = WriterRun {
            id: rid,
            file: path_rel,
            rev,
            started_at: now,
            markers: process
                .markers
                .into_iter()
                .map(|marker| RunMarker {
                    marker,
                    status: RunMarkerStatus::Pending,
                    note: None,
                })
                .collect(),
            state: WriterRunState::Active,
            saw_activity: false,
            reported: false,
        };
        session.runs.push(run);
        session.queue.push_back(body);
        session.error = None;
        self.dirty = true;
        Ok(rid)
    }

    /// Report one run marker as started, done, skipped, or failed.
    pub(super) fn writer_run_report(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let rid = Self::tool_run_id(args, "writer_run_report")?;
        let index = match crate::ipc::mcp::top_raw(args, "index") {
            Some(_) => Self::tool_u32(args, "index")
                .ok_or_else(|| "index must be a non-negative integer".to_string())?,
            None => return Err("writer_run_report needs run, index, and status".to_string()),
        };
        let status = Self::tool_arg(args, "status")
            .ok_or_else(|| "writer_run_report needs run, index, and status".to_string())?;
        let status = RunMarkerStatus::parse(&status)
            .ok_or_else(|| {
                format!("bad status '{status}': started, done, skipped, or failed")
            })?;
        let note = Self::tool_arg(args, "note");
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        let run = session
            .runs
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| format!("unknown run {rid}"))?;
        if matches!(run.state, WriterRunState::Finished { .. }) {
            return Err(format!("run {rid} already finished"));
        }
        let marker = run
            .markers
            .iter_mut()
            .find(|m| m.marker.index as u32 == index)
            .ok_or_else(|| format!("unknown marker index {index} for run {rid}"))?;
        marker.status = status;
        marker.note = note;
        run.reported = true;
        self.dirty = true;
        Ok(format!(
            r#"{{"reported":true,"run":{rid},"index":{index},"status":"{}"}}"#,
            status.name(),
        ))
    }

    /// Close a run with a one-line summary: finish state plus the
    /// single thread note. Unknown and finished runs are refused.
    pub(super) fn writer_run_done(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let rid = Self::tool_run_id(args, "writer_run_done")?;
        let summary = Self::tool_arg(args, "summary")
            .ok_or_else(|| "writer_run_done needs run and summary".to_string())?;
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        if !session.runs.iter().any(|r| r.id == rid) {
            return Err(format!("unknown run {rid}"));
        }
        if session
            .runs
            .iter()
            .any(|r| r.id == rid && matches!(r.state, WriterRunState::Finished { .. }))
        {
            return Err(format!("run {rid} already finished"));
        }
        self.finish_writer_run(id, rid, summary);
        Ok(format!(r#"{{"done":true,"run":{rid}}}"#))
    }

    /// Answer a run's question marker: post the answer to the thread
    /// and close that marker as done, in one step. Unknown and
    /// finished runs, and bad marker indexes, are refused.
    /// `pub(super)`: the answer tool shares it with the run tools.
    pub(super) fn writer_answer_run(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
        answer: String,
    ) -> Result<String, String> {
        let rid = Self::tool_run_id(args, "writer_answer")?;
        let index = match crate::ipc::mcp::top_raw(args, "index") {
            Some(_) => Self::tool_u32(args, "index")
                .ok_or_else(|| "index must be a non-negative integer".to_string())?,
            None => return Err("writer_answer needs run, index, and answer".to_string()),
        };
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        let run = session
            .runs
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| format!("unknown run {rid}"))?;
        if matches!(run.state, WriterRunState::Finished { .. }) {
            return Err(format!("run {rid} already finished"));
        }
        let marker = run
            .markers
            .iter_mut()
            .find(|m| m.marker.index as u32 == index)
            .ok_or_else(|| format!("unknown marker index {index} for run {rid}"))?;
        marker.status = RunMarkerStatus::Done;
        marker.note = Some(answer.clone());
        run.reported = true;
        session.push_thread_note(crate::app::writer::WriterThreadEntry {
            request_id: 0,
            answer,
            run: Some(rid),
        });
        self.dirty = true;
        Ok(format!(r#"{{"answered":true,"run":{rid},"index":{index}}}"#))
    }

    /// A `run` tool arg: present, numeric, and positive.
    /// `pub(super)`: the answer tool shares it with the run tools.
    pub(super) fn tool_run_id(args: &str, tool: &str) -> Result<u64, String> {
        match crate::ipc::mcp::top_raw(args, "run") {
            Some(_) => Self::tool_u32(args, "run")
                .filter(|n| *n > 0)
                .map(|n| n as u64)
                .ok_or_else(|| "run must be a positive integer".to_string()),
            None => Err(format!("{tool} needs run")),
        }
    }

    /// Finish a run with `summary`: finished state plus the single
    /// thread note. Reports never post notes; done, turn-end, and
    /// timeout all land here.
    fn finish_writer_run(
        &mut self,
        id: crate::session::SessionId,
        run_id: u64,
        summary: String,
    ) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(run) = session.runs.iter_mut().find(|r| r.id == run_id) else {
            return;
        };
        run.state = WriterRunState::Finished {
            summary: summary.clone(),
        };
        session.push_thread_note(crate::app::writer::WriterThreadEntry {
            request_id: 0,
            answer: summary,
            run: Some(run_id),
        });
        self.dirty = true;
    }

    /// Finish runs whose agent turn ended or whose silence outran the
    /// timer. Every tick, next to the queue flush: a run whose agent
    /// worked and went quiet finishes ("turn ended without done");
    /// idle-without-work is not a turn, and only report-less runs
    /// time out. `pub(crate)`: the settle path calls this every tick.
    pub(crate) fn settle_writer_runs(&mut self, now: Instant) {
        use crate::session::Activity;
        let ids: Vec<crate::session::SessionId> = self
            .writers
            .iter()
            .filter(|(_, session)| {
                session
                    .runs
                    .iter()
                    .any(|r| matches!(r.state, WriterRunState::Active))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let activity = self.manager.get(id).map(|rec| rec.activity);
            let pending: Vec<u64> = self
                .writers
                .get(&id)
                .map(|session| {
                    session
                        .runs
                        .iter()
                        .filter(|r| matches!(r.state, WriterRunState::Active))
                        .map(|r| r.id)
                        .collect()
                })
                .unwrap_or_default();
            for rid in pending {
                if self.finish_run_on_timeout(id, rid, now) {
                    continue;
                }
                match activity {
                    Some(
                        Activity::Thinking | Activity::ToolUse | Activity::Waiting,
                    ) => {
                        if let Some(session) = self.writers.get_mut(&id) {
                            if let Some(run) =
                                session.runs.iter_mut().find(|r| r.id == rid)
                            {
                                run.saw_activity = true;
                            }
                        }
                    }
                    Some(Activity::Idle) | Some(Activity::Stopped) => {
                        let worked = self
                            .writers
                            .get(&id)
                            .and_then(|session| {
                                session.runs.iter().find(|r| r.id == rid)
                            })
                            .is_some_and(|run| run.saw_activity);
                        if worked {
                            let (done, total) = self
                                .writers
                                .get(&id)
                                .and_then(|session| {
                                    session.runs.iter().find(|r| r.id == rid)
                                })
                                .map(|run| {
                                    (
                                        run.markers
                                            .iter()
                                            .filter(|m| {
                                                m.status == RunMarkerStatus::Done
                                            })
                                            .count(),
                                        run.markers.len(),
                                    )
                                })
                                .unwrap_or((0, 0));
                            self.finish_writer_run(
                                id,
                                rid,
                                format!("turn ended without done ({done}/{total} markers done)"),
                            );
                        }
                    }
                    None => {}
                }
            }
        }
    }

    /// Force-finish a silent run past [`RUN_TIMEOUT`]. Returns true
    /// when it finished (the caller moves on).
    fn finish_run_on_timeout(
        &mut self,
        id: crate::session::SessionId,
        run_id: u64,
        now: Instant,
    ) -> bool {
        let silent = self
            .writers
            .get(&id)
            .and_then(|session| session.runs.iter().find(|r| r.id == run_id))
            .is_some_and(|run| {
                !run.reported && now.duration_since(run.started_at) > RUN_TIMEOUT
            });
        if silent {
            self.finish_writer_run(
                id,
                run_id,
                format!(
                    "run timed out after {} seconds with no report",
                    RUN_TIMEOUT.as_secs()
                ),
            );
        }
        silent
    }
}
