//! Broker per-session injection queues: pressure caps and due queues.

use super::*;

impl Broker {
    /// Undelivered injections waiting for one session.
    pub fn queued(&self, id: SessionId) -> usize {
        self.queue.get(&id).map(VecDeque::len).unwrap_or(0)
    }

    /// Pressure on a target: queued injections, delivered asks still
    /// awaiting a response, and armed timers (future queue entries).
    /// One ask occupies exactly one side at a time. Every admission
    /// gate (sends and schedules) reads this one budget, so timers
    /// plus sends can never stack past the cap.
    ///
    /// Completions bypass the gate by design and are not counted
    /// against it: a response resolves its ask (net zero), a fired
    /// timer converts to one queued entry (net zero), and acks,
    /// reminders, and failure notices each answer already-admitted
    /// work. Gating them would fail the very completion that drains
    /// pressure; sequential delivery paces the pane instead.
    pub fn pressure(&self, _sessions: &SessionManager, id: SessionId) -> usize {
        let asks = self
            .convs
            .values()
            .filter(|c| {
                c.state == ConvState::Open
                    && c.kind == ConvKind::Ask
                    && c.target == id
                    && c.delivered
            })
            .count();
        let bot_asks = self
            .bot_convs
            .values()
            .filter(|c| {
                c.state == BotConvState::Open
                    && c.kind == BotConvKind::Ask
                    && c.from_client
                    && c.session == id
                    && c.delivered
            })
            .count();
        let timers = self.timers.values().filter(|t| t.target == id).count();
        self.queued(id) + asks + bot_asks + timers
    }

    /// Oldest queued injection without popping: delivery peeks, writes,
    /// and only then pops, so a failed write retries on the next settle
    /// instead of losing the message with its pressure already moved.
    pub fn peek_due(&self, id: SessionId) -> Option<Injection> {
        self.queue.get(&id)?.front().cloned()
    }

    /// Pop up to `limit` queued injections, oldest first. Popping an ask
    /// marks it delivered so pressure moves with it instead of doubling.
    pub fn take_due(&mut self, id: SessionId, limit: usize) -> Vec<Injection> {
        let mut out = Vec::new();
        if let Some(q) = self.queue.get_mut(&id) {
            while out.len() < limit {
                let Some(inj) = q.pop_front() else {
                    break;
                };
                out.push(inj);
            }
        }
        for inj in &out {
            if inj.kind == InjectKind::Ask {
                if let Some(conv) = self.convs.get_mut(&inj.conv) {
                    conv.delivered = true;
                } else if let Some(conv) = self.bot_convs.get_mut(&inj.conv) {
                    conv.delivered = true;
                }
            }
        }
        out
    }

    pub(crate) fn push(&mut self, id: SessionId, inj: Injection) {
        self.queue.entry(id).or_default().push_back(inj);
    }

    /// Drop every queued injection for `id`, returning the count. The
    /// operator `/clear` unsticks a session; delivered asks and
    /// completions are untouched.
    pub(crate) fn clear_queue(&mut self, id: SessionId) -> usize {
        self.queue.remove(&id).map(|q| q.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

        #[test]
        fn sixth_message_hits_the_pressure_cap() {
            let mut p = live_pair().grouped();
            for i in 0..5 {
                p.call(
                    &p.run_a.clone(),
                    "ask_session",
                    &format!(r#"{{"target":"b","message":"q{i}"}}"#),
                )
                .expect("first five fit");
            }
            let err = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q5"}"#)
                .expect_err("sixth must fail");
            assert!(err.contains("pressure"), "err: {err}");
            // Draining the queue does not help while five asks await response.
            p.state.broker.take_due(p.b, 10);
            let err = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q6"}"#)
                .expect_err("delivered asks still count");
            assert!(err.contains("pressure"), "err: {err}");
        }

        #[test]
        fn answered_ask_releases_pressure() {
            let mut p = live_pair().grouped();
            let res = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q0"}"#)
                .unwrap();
            let conv = json_field(&res, "conversation").unwrap();
            // b reads the question, then answers; the response goes back to a
            // and the ask stops counting.
            assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
            let r = p
                .call(
                    &p.run_b.clone(),
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
                )
                .expect("target answers");
            assert!(r.contains(&conv), "res: {r}");
            let due = p.state.broker.take_due(p.a, 10);
            assert_eq!(due.len(), 1);
            assert!(matches!(due[0].kind, InjectKind::Response));
            assert_eq!(p.state.broker.pressure(&p.state.manager, p.b), 0);
        }

        #[test]
        fn armed_timers_count_against_send_pressure() {
            // Five armed timers saturate the target: the sixth unit of work
            // (a new ask) must wait, or timers plus sends stack past the cap.
            let mut p = live_pair().grouped();
            for i in 0..PRESSURE_CAP {
                p.call(
                    &p.run_a.clone(),
                    "schedule_prompt",
                    &format!(r#"{{"prompt":"timer-{i}","delay_seconds":3600}}"#),
                )
                .expect("timers arm");
            }
            let err = p
                .call(&p.run_b.clone(), "ask_session", r#"{"target":"a","message":"q"}"#)
                .expect_err("timers hold the pressure budget");
            assert!(err.contains("pressure cap"), "err: {err}");
        }

        #[test]
        fn delivered_asks_count_against_scheduling() {
            // Scheduling reads the same budget as sending: five delivered
            // asks awaiting answers leave no room for a new timer.
            let mut p = live_pair().grouped();
            for i in 0..PRESSURE_CAP {
                p.call(
                    &p.run_b.clone(),
                    "ask_session",
                    &format!(r#"{{"target":"a","message":"q{i}"}}"#),
                )
                .expect("asks queue");
            }
            assert_eq!(p.state.broker.take_due(p.a, 10).len(), PRESSURE_CAP);
            let err = p
                .call(
                    &p.run_a.clone(),
                    "schedule_prompt",
                    r#"{"prompt":"later","delay_seconds":3600}"#,
                )
                .expect_err("asks hold the pressure budget");
            assert!(err.contains("pressure cap"), "err: {err}");
        }

        #[test]
        fn caller_backlog_gates_new_sends() {
            // A answers nothing while asking on: each answer piles a
            // response behind busy A. Past the queue cap A's new sends
            // refuse with backpressure instead of growing the queue
            // without limit; draining unblocks. Completions themselves
            // still bypass (B keeps answering throughout).
            let mut p = live_pair().grouped();
            for _ in 0..crate::comms::QUEUE_CAP {
                let res = p
                    .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
                    .expect("ask admitted");
                let conv = json_field(&res, "conversation").expect("conversation id");
                assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
                p.call(
                    &p.run_b.clone(),
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
                )
                .expect("B answers");
            }
            assert_eq!(p.state.broker.queued(p.a), crate::comms::QUEUE_CAP);
            let err = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"one more"}"#)
                .expect_err("backlogged caller waits");
            assert!(err.contains("caller queue full"), "err: {err}");
            let err = p
                .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"one more"}"#)
                .expect_err("tells gate the same way");
            assert!(err.contains("caller queue full"), "err: {err}");
            // Draining unblocks: backpressure, not deadlock.
            assert_eq!(p.state.broker.take_due(p.a, 200).len(), crate::comms::QUEUE_CAP);
            p.call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"again"}"#)
                .expect("drained caller sends");
        }

        #[test]
        fn queued_references_pin_terminal_conversations() {
            // Eviction must not strand a queued response: while the
            // injection still waits, the Done record stays so an exit
            // still notifies the responder loudly (Major 5's guarantee).
            let mut p = live_pair().grouped();
            let res = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
                .expect("ask validates");
            let conv = json_field(&res, "conversation").expect("conversation id");
            let answer = format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#);
            p.call(&p.run_b.clone(), "send_response", &answer)
                .expect("target answers");
            p.state
                .broker
                .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
            let err = p
                .call(&p.run_b.clone(), "send_response", &answer)
                .expect_err("pinned record still names closed");
            assert_eq!(err, "conversation is closed");
            // Once the queue drains the pin releases and it evicts.
            assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
            p.state
                .broker
                .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
            let err = p
                .call(&p.run_b.clone(), "send_response", &answer)
                .expect_err("unpinned record evicts");
            assert_eq!(err, "unknown conversation");
        }
}
