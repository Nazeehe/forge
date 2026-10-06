//! AppState settling: comms delivery, enters, ticks, and hook verdicts.

use super::*;

pub mod hooks;
mod inbox;
#[cfg(test)]
mod trace;

impl AppState {
    pub fn settle_comms(&mut self) {
        use crate::session::Activity;
        let now = std::time::Instant::now();
        // Operator texts route first so fresh injections can deliver on
        // this same tick when their target is idle.
        self.drain_telegram();
        // Second-scale sweep on a 16ms loop: skip inside the window.
        // Everything below (debounce gates, queue delivery, staged
        // Enters) stays per-tick, so injections never wait on this.
        // Hold logging rides the sweep flag: a stuck message logs its
        // reason at most once a second per session, never per tick.
        let swept = Self::tick_due(self.last_broker_tick, now);
        if swept {
            self.broker.tick(now);
            self.last_broker_tick = Some(now);
        }
        let order = self.manager.order().to_vec();
        for id in order {
            if self.broker.queued(id) == 0 {
                continue;
            }
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            let settled = self.injection_settled_for(id, now) && self.turn_registered(id, now);
            let enter_pending = self.pending_enter.contains_key(&id);
            if !idle || !settled || enter_pending {
                if swept {
                    if let Some(head) = self.broker.peek_due(id) {
                        // Log a hold when its reason changes, else only as
                        // a periodic reminder: a stuck message must not
                        // flood the trace and rotate the evidence away.
                        let reason = format!(
                            "{}|{:?}|{}|{}|{}",
                            head.conv,
                            self.manager.get(id).map(|rec| rec.activity),
                            self.broker.queued(id),
                            enter_pending,
                            settled,
                        );
                        let quiet = self.hold_logged.get(&id).is_some_and(|(last, at)| {
                            *last == reason
                                && now.saturating_duration_since(*at)
                                    < crate::comms::HOLD_LOG_REMINDER
                        });
                        if quiet {
                            continue;
                        }
                        self.hold_logged.insert(id, (reason, now));
                        let to = self
                            .manager
                            .get(id)
                            .map(|rec| rec.name.clone())
                            .unwrap_or_default();
                        let activity = self
                            .manager
                            .get(id)
                            .map(|rec| crate::comms::Broker::activity_label(rec.activity))
                            .unwrap_or("gone");
                        let summary = crate::comms::hold_summary(
                            activity,
                            self.broker.queued(id),
                            enter_pending,
                            Self::ms_ago(now, self.last_human_input.get(&id)),
                            Self::ms_ago(now, self.last_hook_activity.get(&id)),
                        );
                        self.trace_comms(&format!(
                            "hold to={} conv={} kind={} from={} {summary}",
                            crate::comms::log_quote(&to),
                            crate::comms::log_quote(&head.conv),
                            head.kind.trace_label(),
                            crate::comms::log_quote(&head.from),
                        ));
                    }
                }
                continue;
            }
            // Peek before pop: the message leaves the queue only after
            // its bytes reach the pane, so a failed write retries next
            // settle with pressure untouched. Single owner, so the head
            // cannot change between peek and pop.
            let Some(head) = self.broker.peek_due(id) else {
                continue;
            };
            // Panes that opted into paste mode (DECSET 2004) take the
            // whole payload as one bracketed-paste transaction; the rest
            // take it raw, exactly as before.
            let bracketed = self.manager.bracketed_paste(id);
            let bytes = head.render_framed(bracketed);
            match self.manager.inject_write(id, &bytes) {
                Ok(()) => {
                    self.hold_logged.remove(&id);
                    let taken = self.broker.take_due(id, 1);
                    debug_assert!(taken.first().map(|t| &t.conv) == Some(&head.conv));
                    // Arm the staged Enter: the CR goes out on a later tick,
                    // never in the same burst as the text. Remember what
                    // went out, so human input that kills the submit can
                    // tell the sender.
                    self.pending_enter.insert(
                        id,
                        (now, Some((head.conv.clone(), head.kind))),
                    );
                    let to = self
                        .manager
                        .get(id)
                        .map(|rec| rec.name.clone())
                        .unwrap_or_default();
                    let left = self.broker.queued(id);
                    self.trace_comms(&format!(
                        "deliver to={} conv={} kind={} from={} bytes={} queued_left={left}",
                        crate::comms::log_quote(&to),
                        crate::comms::log_quote(&head.conv),
                        head.kind.trace_label(),
                        crate::comms::log_quote(&head.from),
                        bytes.len(),
                    ));
                    self.dirty = true;
                }
                Err(e) => {
                    let to = self
                        .manager
                        .get(id)
                        .map(|rec| rec.name.clone())
                        .unwrap_or_default();
                    self.trace_comms(&format!(
                        "deliver-fail to={} conv={} kind={} from={} err={}",
                        crate::comms::log_quote(&to),
                        crate::comms::log_quote(&head.conv),
                        head.kind.trace_label(),
                        crate::comms::log_quote(&head.from),
                        crate::comms::log_quote(&crate::infra::logging::truncate(
                            &e.to_string(),
                            200
                        )),
                    ));
                }
            }
        }
        self.settle_enters(now);
        // Writer request bodies ride the same gates through their own
        // per-session queues (cap 8, never drops).
        self.settle_writer_queues(now);
        // Process runs finish on turn-end or silence on the same tick.
        self.settle_writer_runs(now);
    }

    /// Send staged Enters whose beat has elapsed. Same gates as bodies —
    /// settled debounce plus an idle target — so a CR never lands mid-turn
    /// or into a human draft (human input clears the entry outright).
    /// Sessions that exited meanwhile are pruned.
    fn settle_enters(&mut self, now: std::time::Instant) {
        use crate::session::Activity;
        let due: Vec<crate::session::SessionId> = self
            .pending_enter
            .iter()
            .filter(|(_, (at, _))| now.duration_since(*at) >= crate::comms::INJECT_ENTER_DELAY)
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !idle || !self.injection_settled_for(id, now) {
                continue;
            }
            // A CR that never reaches the pane stays staged for retry:
            // removing it would strand an unsubmitted body. Exited
            // sessions are pruned below, so a dead pane stops here.
            // Either outcome is traced: a missing Enter is the classic
            // "body arrived but never submitted" mystery.
            let staged = self.pending_enter.get(&id).and_then(|(_, what)| what.clone());
            match self.manager.inject_write(id, &[crate::comms::INJECT_ENTER_CR]) {
                Ok(()) => {
                    self.pending_enter.remove(&id);
                    self.enter_sent.insert(id, now);
                    let to = self
                        .manager
                        .get(id)
                        .map(|rec| rec.name.clone())
                        .unwrap_or_default();
                    match staged {
                        Some((conv, kind)) => self.trace_comms(&format!(
                            "enter to={} conv={} kind={}",
                            crate::comms::log_quote(&to),
                            crate::comms::log_quote(&conv),
                            kind.trace_label(),
                        )),
                        None => self.trace_comms(&format!(
                            "enter to={} conv=none kind=command",
                            crate::comms::log_quote(&to),
                        )),
                    }
                    self.dirty = true;
                }
                Err(e) => {
                    let to = self
                        .manager
                        .get(id)
                        .map(|rec| rec.name.clone())
                        .unwrap_or_default();
                    self.trace_comms(&format!(
                        "enter-fail to={} err={}",
                        crate::comms::log_quote(&to),
                        crate::comms::log_quote(&crate::infra::logging::truncate(&e.to_string(), 200)),
                    ));
                }
            }
        }
        // Records outlive their sessions (a natural exit marks the
        // record, it does not remove it), so mere presence prunes
        // nothing: only live sessions keep a staged Enter.
        self.pending_enter.retain(|id, _| {
            self.manager.get(*id).is_some_and(|rec| rec.state.is_live())
        });
        self.enter_sent.retain(|id, _| {
            self.manager.get(*id).is_some_and(|rec| rec.state.is_live())
        });
        self.hold_logged.retain(|id, _| {
            self.manager.get(*id).is_some_and(|rec| rec.state.is_live())
        });
    }

    /// Whether the turn the last Enter started has registered: a hook
    /// newer than that Enter, or [`crate::comms::INJECT_TURN_GRACE`]
    /// passed. Until then the pane only looks idle. Panes that never
    /// hooked (plain shells) report no turns, so they never wait.
    /// Saturating, since Enters may be stamped with a supplied `now`.
    pub(crate) fn turn_registered(
        &self,
        id: crate::session::SessionId,
        now: std::time::Instant,
    ) -> bool {
        let Some(&sent) = self.enter_sent.get(&id) else {
            return true;
        };
        let Some(&hooked_at) = self.last_hook_activity.get(&id) else {
            return true;
        };
        hooked_at > sent || now.saturating_duration_since(sent) >= crate::comms::INJECT_TURN_GRACE
    }

    /// Deliver due injections into idle, non-recently-typed panes. Targets
    /// whose hook activity is Thinking/ToolUse/Waiting keep waiting, as do
    /// panes the human just typed into.
    /// Delivery gate for bodies and staged Enters alike, per target:
    /// quiet human hands plus quiet hooks for that session only. Either
    /// recent activity holds that target, never its neighbors.
    pub(crate) fn injection_settled_for(
        &self,
        id: crate::session::SessionId,
        now: std::time::Instant,
    ) -> bool {
        let hands_off = self.last_human_input.get(&id).is_none_or(|t| {
            now.duration_since(*t) >= crate::comms::INJECT_DEBOUNCE
        });
        let hooks_quiet = self.last_hook_activity.get(&id).is_none_or(|t| {
            now.duration_since(*t) >= crate::comms::INJECT_HOOK_DEBOUNCE
        });
        hands_off && hooks_quiet
    }

    /// Whether the courtesy/timer sweep is due: always on a fresh
    /// window (`None`), otherwise once the interval lapses. Pure so
    /// the throttle itself is unit-testable; the sweep stays live.
    fn tick_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
        last.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::BROKER_TICK_INTERVAL
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::ids::RunId;

    #[test]
    fn broker_tick_throttles_but_delivery_stays_per_tick() {
        // The courtesy/timer sweep is second-scale work on a 16ms loop:
        // rapid settles must skip it, while queue delivery below stays
        // per-tick. A delay-0 timer scheduled inside the window proves
        // the skip (still pending), and one after a lapsed window
        // proves the sweep resumes.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        assert!(s.manager.set_activity(a, crate::session::Activity::Idle));
        let schedule = |s: &mut AppState, prompt: &str| {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "schedule_prompt".to_string(),
                args: format!("{{\"prompt\":{prompt:?},\"delay_seconds\":0}}"),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
            }));
            let line = reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("schedule answers at once");
            assert!(line.contains("\"ok\":true"), "schedule accepted: {line}");
        };
        // First settle sweeps: the due timer fires.
        schedule(&mut s, "tick-one");
        s.settle_comms();
        assert!(s.broker.timers_for(a).is_empty(), "first settle sweeps");
        // A timer scheduled inside the throttle window waits out the
        // next sweep instead of firing on the very next settle.
        schedule(&mut s, "tick-two");
        s.settle_comms();
        assert_eq!(
            s.broker.timers_for(a).len(),
            1,
            "rapid settle skips the sweep"
        );
        // A lapsed window sweeps again: delivery never waited, only
        // the sweep did.
        s.last_broker_tick = None;
        s.settle_comms();
        assert!(s.broker.timers_for(a).is_empty(), "lapsed window sweeps");
        assert!(s.manager.remove(a));
    }

    #[test]
    fn broker_tick_window_opens_once_per_interval() {
        use std::time::{Duration, Instant};
        let window = crate::comms::BROKER_TICK_INTERVAL;
        let start = Instant::now();
        // Fresh windows (boot, tests) always sweep at once.
        assert!(AppState::tick_due(None, start));
        // Inside the window the sweep waits...
        assert!(!AppState::tick_due(Some(start), start));
        assert!(!AppState::tick_due(Some(start), start + window - Duration::from_millis(1)));
        // ...then opens again once it lapses.
        assert!(AppState::tick_due(Some(start), start + window));
    }

    #[test]
    fn comms_ask_flows_through_apply_and_replies() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":true"#), "line: {line:?}");
        assert!(line.contains(r#""conversation":""#), "line: {line:?}");
        assert!(line.ends_with('\n'));
        assert_eq!(s.broker.queued(b), 1);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn comms_rejections_are_single_line_json() {
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: "f".repeat(32),
            tool: "ask_session".to_string(),
            args: "{}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(!line.trim_end().contains('\n'), "one line: {line:?}");
    }

    #[test]
    fn session_exit_fails_open_conversations() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"q?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        assert!(s.manager.kill(b));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(b).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source learns the failure");
        assert!(matches!(
            due[0].kind,
            crate::comms::InjectKind::Failed
        ));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_waits_for_the_turn_an_enter_starts() {
        // The harness reports the turn an Enter starts ~100-300ms later.
        // Until then the pane still reads idle, and the next body used
        // to land in a busy pane, its own Enter then held for the whole
        // turn (comms.log: mu_2, 96s stuck, then wiped by typing).
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        for text in ["first", "second"] {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: format!("{{\"target\":\"b\",\"text\":\"{text}\"}}"),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                    crate::ipc::listener::CLAIM_PENDING,
                )),
            }));
        }
        // b is a hooking harness: an earlier, settled hook is on record.
        s.last_hook_activity
            .insert(b, std::time::Instant::now() - std::time::Duration::from_secs(5));
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "first body out");
        s.settle_enters(std::time::Instant::now() + crate::comms::INJECT_ENTER_DELAY);
        assert!(!s.pending_enter.contains_key(&b), "first Enter out");
        // No hook has reported the new turn yet: the pane only looks idle.
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "second body waits for the turn to register");
        // A hook newer than the Enter registers the turn (its activity
        // gates from here); once its debounce settles the body goes.
        let now = std::time::Instant::now();
        s.enter_sent.insert(b, now - std::time::Duration::from_millis(1000));
        s.last_hook_activity.insert(b, now - std::time::Duration::from_millis(600));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "registered turn releases the body");
        // A turn whose hook never lands resumes after the grace bound.
        s.pending_enter.remove(&b);
        s.last_hook_activity
            .insert(b, std::time::Instant::now() - std::time::Duration::from_secs(5));
        s.enter_sent.insert(b, std::time::Instant::now());
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"text\":\"third\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::ipc::listener::CLAIM_PENDING,
            )),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "inside the grace: still waiting");
        s.enter_sent
            .insert(b, std::time::Instant::now() - crate::comms::INJECT_TURN_GRACE);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "grace elapsed: body goes");
        // A pane that never hooked (plain shell) never waits at all.
        s.last_hook_activity.remove(&b);
        s.enter_sent.insert(b, std::time::Instant::now());
        assert!(s.turn_registered(b, std::time::Instant::now()));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_to_idle_panes_only() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Busy target: the injection waits.
        assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target waits");
        // Idle target: delivered into the pane.
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(b).is_some_and(|t| t.contains("hello-b")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "injection never reached the pane"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn reply_round_trip_needs_stop_to_reach_first_session() {
        // Live shape: both agents have fired hooks, so neither is Idle.
        // The ask goes out, the target answers, and the reply waits until
        // the first session's turn ends (Stop parks Stopped). Before the
        // Stop edge was installed, that wait never ended.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        let comms = |s: &mut AppState, run_id: String, tool: &str, args: &str| -> String {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id,
                tool: tool.to_string(),
                args: args.to_string(),
                reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("broker answers at once")
        };
        // Both sessions mid-turn, like live agents that called tools.
        hook(&mut s, "PreToolUse", run_a.to_string());
        hook(&mut s, "PreToolUse", run_b.to_string());
        // A's ask waits while B is busy, then lands when B's turn ends.
        let ask_line = comms(&mut s, run_a.to_string(), "ask_session", "{\"target\":\"b\",\"message\":\"ready?\"}");
        assert!(ask_line.contains("\"ok\":true"), "ask accepted: {ask_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target holds the ask");
        hook(&mut s, "Stop", run_b.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity.insert(
            b,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "turn end delivers the ask");
        // B answers; A is still mid-turn so the reply waits.
        let conv = crate::hooks::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("ask returns a conversation");
        let resp_line = comms(
            &mut s,
            run_b.to_string(),
            "send_response",
            &format!("{{\"conversation_id\":\"{conv}\",\"message\":\"got-it\"}}"),
        );
        assert!(resp_line.contains("\"ok\":true"), "reply accepted: {resp_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "reply waits while A works");
        // A's turn ends: the reply lands in A's pane.
        hook(&mut s, "Stop", run_a.to_string());
        s.last_hook_activity.insert(
            a,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers the reply");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(a).is_some_and(|t| t.contains("got-it")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reply never reached the first session"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn prompt_submit_holds_injections_until_stop() {
        // A freshly prompted session looks settled but its agent is
        // generating: UserPromptSubmit must hold injections (where Enter
        // would be eaten and the text left as a draft) until Stop.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        // A was parked (Stopped) but just got prompted: generating.
        hook(&mut s, "Stop", run_a.to_string());
        hook(&mut s, "UserPromptSubmit", run_a.to_string());
        assert_eq!(
            s.manager.get(a).unwrap().activity,
            crate::session::Activity::Thinking
        );
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_b.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"a\",\"text\":\"hold\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "generating target waits");
        hook(&mut s, "Stop", run_a.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity.insert(
            a,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_stages_enter_beat_after_body() {
        // Byte-level human parity through a raw-mode slave (no CR/LF
        // translation anywhere, no echo): the body arrives whole with no
        // Enter bundled in, and the CR follows as its own input event
        // after the beat — type text, press Enter, never one burst.
        use crate::session::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Wait out the spawn: anything the starting shell echoes (including
        // canonical-mode translations) predates raw mode and must not
        // pollute the byte assertions below.
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(8).any(|w| w == b"STTYFAIL") {
                panic!("stty raw failed in the probe session");
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"message\":\"ping-body\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "raw session is idle: body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Drain raw output until cat echoes the full body back.
        let body = b"[forge tell_session from a]: ping-body";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(body.len()).any(|w| w == body) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "body never echoed, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!raw.contains(&b'\r'), "no Enter bundled with the body");
        // After the beat the CR goes out as its own event, trailing the body.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter sent");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.contains(&b'\r') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "staged enter never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pos = raw
            .windows(body.len())
            .position(|w| w == body)
            .expect("body present");
        assert!(
            raw[pos + body.len()..].contains(&b'\r'),
            "enter trails the body: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_one_body_per_enter() {
        // Two queued tells must not merge into one submission: the first
        // settle writes only the head body and stages its Enter; the
        // second body waits for its own Enter after the first CR lands.
        use crate::session::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let tell = |s: &mut AppState, text: &str| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: format!("{{\"target\":\"b\",\"text\":{text:?}}}"),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
            }));
        };
        tell(&mut s, "first-one");
        tell(&mut s, "second-two");
        // Only the head body goes out; the second waits in queue.
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "second body waits for its own enter");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Past the beat the first CR lands while the second body is
        // still queued — never in the same burst.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "first enter sent");
        assert_eq!(s.broker.queued(b), 1, "second body still queued");
        // The next settle writes the second body and stages its Enter.
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "second body delivered");
        assert!(s.pending_enter.contains_key(&b), "second enter staged");
        // Byte order on the wire: body1, CR, body2, CR — the second
        // body never appears before the first Enter.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(b"second-two".len()).any(|w| w == b"second-two") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second body never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let first_cr = raw.iter().position(|&byte| byte == b'\r').expect("CR sent");
        let second_at = raw
            .windows(b"second-two".len())
            .position(|w| w == b"second-two")
            .expect("second body present");
        assert!(
            second_at > first_cr,
            "second body follows the first enter: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn failed_body_write_keeps_message_queued() {
        // Popping is not delivery: if the bytes never reach the pane,
        // the message must stay queued for the next settle instead of
        // vanishing with its pressure already moved.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","text":"held"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        // Kill the writer while the record stays idle (exit undrained):
        // every write now fails deterministically.
        s.manager.active_pane_mut(b).expect("live pane").close();
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "failed write retries, never drops");
        assert!(
            !s.pending_enter.contains_key(&b),
            "no enter staged without a body"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn failed_enter_write_stays_staged() {
        // A CR that never reaches the pane must be retried, not
        // forgotten: removing it strands an unsubmitted body.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","text":"held"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert!(s.pending_enter.contains_key(&b), "enter staged");
        // Kill the writer with the CR still staged, then age past the beat.
        s.manager.active_pane_mut(b).expect("live pane").close();
        s.pending_enter.insert(
            b,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(
            s.pending_enter.contains_key(&b),
            "failed enter stays staged for retry"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_during_human_typing() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        // Typing debounces the typed-in pane only (never its neighbors).
        s.note_human_input(b);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "fresh typing debounces delivery");
        // Human input also drops a staged Enter for that session: our CR
        // must never submit the human's draft.
        s.pending_enter.insert(b, (std::time::Instant::now(), None));
        s.note_human_input(b);
        assert!(!s.pending_enter.contains_key(&b), "human owns the prompt");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn typed_over_tell_notifies_the_source() {
        // The tell body reached B's prompt with its Enter staged;
        // B types first, so the staged submit dies to protect the
        // draft. The body may have mixed or been discarded, so A is
        // told loudly instead of assuming it landed cleanly.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::ipc::listener::CLAIM_PENDING,
            )),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged");
        s.note_human_input(b);
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source hears the clobber");
        assert!(matches!(due[0].kind, crate::comms::InjectKind::Failed));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn typed_over_response_notifies_the_answerer() {
        // B's answer sits in A's prompt awaiting its staged Enter;
        // A types first. The answerer (not the asker) is told.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let mut converse = |run: &str, tool: &str, args: String| {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run.to_string(),
                tool: tool.to_string(),
                args,
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                    crate::ipc::listener::CLAIM_PENDING,
                )),
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("verdict arrives")
        };
        let ask_line = converse(
            &run_a.to_string(),
            "ask_session",
            r#"{"target":"b","message":"q?"}"#.to_string(),
        );
        assert!(ask_line.contains(r#""ok":true"#), "ask accepted: {ask_line}");
        let conv = crate::hooks::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("conversation id");
        let resp_line = converse(
            &run_b.to_string(),
            "send_response",
            format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        );
        assert!(resp_line.contains(r#""ok":true"#), "answer accepted: {resp_line}");
        s.settle_comms();
        assert!(s.pending_enter.contains_key(&a), "enter staged");
        s.note_human_input(a);
        let due = s.broker.take_due(b, 10);
        assert_eq!(due.len(), 1, "answerer hears the clobber");
        assert!(matches!(due[0].kind, crate::comms::InjectKind::Failed));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn staged_enter_prunes_exited_records() {
        // A naturally exited session keeps its record (marked not
        // live); its staged Enter must still go, or the map — and
        // futile pane retries — survive forever.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        assert!(s.manager.kill(a));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(a).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.get(a).is_some(), "record retained");
        assert!(!s.manager.get(a).unwrap().state.is_live(), "marked exited");
        s.pending_enter.insert(
            a,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&a), "exited entry pruned");
    }

    #[test]
    fn activity_in_one_pane_never_holds_another() {
        // Typing in A and hook traffic from A must not starve idle B:
        // debounce gates delivery per target, never app-wide.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(a, crate::session::Activity::Idle));
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let tell = |s: &mut AppState| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: r#"{"target":"b","message":"wait"}"#.to_string(),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
            }));
        };
        // Human typing in A leaves B's delivery alone.
        tell(&mut s);
        s.note_human_input(a);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "A typing holds only A");
        // Flush the staged Enter so the second half tests the hook
        // gate, not one-body-per-Enter staging.
        s.pending_enter.insert(
            b,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter flushed");
        // Hook traffic attributed to A leaves B's delivery alone.
        tell(&mut s);
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: run_a.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "A hooks hold only A");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_after_hook_activity() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
        // Fresh hook activity holds delivery even to an idle target.
        s.last_hook_activity.insert(b, std::time::Instant::now());
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "hook debounce holds delivery");
        // Past the beat the same settle delivers.
        s.last_hook_activity.insert(
            b,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }
}
