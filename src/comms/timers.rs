//! Broker timers and the expiry sweep: schedule, cancel, and record eviction.

use super::*;

impl Broker {
    /// Queue `/compact` for a session: self is always allowed, another
    /// target needs a shared group like any peer write. Delivery waits
    /// for the idle path, so the command never lands mid-turn.
    pub(super) fn compact(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
    ) -> Result<String, String> {
        let target = match Self::arg(args, "target").filter(|s| !s.is_empty()) {
            None => caller,
            Some(name) => {
                let target = self.resolve_target(sessions, &name)?;
                if target != caller && !self.shared_group(caller, target) {
                    return Err("no shared group with target".to_string());
                }
                target
            }
        };
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err("pressure cap reached".to_string());
        }
        self.push(
            target,
            Injection {
                conv: crate::infra::ids::ConversationId::generate().to_string(),
                kind: InjectKind::Command,
                from: self.names(sessions, caller),
                text: "/compact".to_string(),
            },
        );
        Ok(r#"{"queued":true}"#.to_string())
    }

    /// Arm a self-injection timer: the prompt fires into the caller's
    /// own queue after the delay and delivers when idle. `clear_context`
    /// prefixes `/clear` so the prompt starts a fresh context.
    pub(super) fn schedule(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let prompt = Self::arg(args, "prompt").filter(|s| !s.is_empty())
            .ok_or_else(|| "schedule_prompt needs a prompt".to_string())?;
        if crate::ipc::mcp::top_raw(args, "delay_seconds").is_some()
            && Self::arg_num(args, "delay_seconds").is_none()
        {
            return Err("delay_seconds must be a number of seconds".to_string());
        }
        let delay = Self::arg_num(args, "delay_seconds").unwrap_or(0.0);
        if !(0.0..=MAX_SCHEDULE_DELAY_SECS).contains(&delay) {
            return Err("delay_seconds must sit between 0 and 86400".to_string());
        }
        if crate::ipc::mcp::top_raw(args, "clear_context").is_some()
            && Self::arg_bool(args, "clear_context").is_none()
        {
            return Err("clear_context must be true or false".to_string());
        }
        let clear = Self::arg_bool(args, "clear_context").unwrap_or(false);
        // Same budget as sends: timers are future queue entries, so a
        // loaded target refuses new ones instead of stacking past the cap.
        if self.pressure(sessions, caller) >= PRESSURE_CAP {
            return Err("pressure cap reached".to_string());
        }
        let text = if clear {
            format!("/clear\n{prompt}")
        } else {
            prompt
        };
        let timer = crate::infra::ids::ConversationId::generate().to_string();
        self.timers.insert(
            timer.clone(),
            Timer {
                target: caller,
                from: self.names(sessions, caller),
                text,
                due: now + Duration::from_secs_f64(delay),
            },
        );
        Ok(format!(r#"{{"timer_id":"{timer}"}}"#))
    }

    /// Armed timers for one session, soonest first: (timer ID, due).
    /// The sidebar countdowns and cancel buttons read this.
    pub fn timers_for(&self, id: SessionId) -> Vec<(String, Instant)> {
        let mut out: Vec<(String, Instant)> = self
            .timers
            .iter()
            .filter(|(_, timer)| timer.target == id)
            .map(|(timer_id, timer)| (timer_id.clone(), timer.due))
            .collect();
        out.sort_by_key(|(_, due)| *due);
        out
    }

    /// Human cancel from the sidebar: any armed timer drops. The UI
    /// only offers the focused session's timers; the tool path keeps
    /// its owner check in `cancel_scheduled`.
    pub fn cancel_timer(&mut self, timer_id: &str) -> bool {
        self.timers.remove(timer_id).is_some()
    }

    /// Cancel an armed timer. Fired or unknown IDs fail rather than
    /// confirming thin air; only the owning session cancels.
    pub(super) fn cancel_scheduled(&mut self, caller: SessionId, args: &str) -> Result<String, String> {
        let timer = Self::arg(args, "timer_id").filter(|s| !s.is_empty())
            .ok_or_else(|| "cancel_scheduled_prompt needs a timer_id".to_string())?;
        match self.timers.get(&timer) {
            Some(pending) if pending.target == caller => {
                self.timers.remove(&timer);
                Ok(r#"{"cancelled":true}"#.to_string())
            }
            _ => Err("unknown timer".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

    #[test]
    fn overdue_timers_fire_earliest_first() {
        // Five armed out of order must still inject earliest-due
        // first: hash order is not an ordering.
        let mut p = live_pair().grouped();
        for (prompt, delay) in [("p1", 5), ("p2", 4), ("p3", 3), ("p4", 2), ("p5", 1)] {
            p.call(
                &p.run_a.clone(),
                "schedule_prompt",
                &format!(r#"{{"prompt":"{prompt}","delay_seconds":{delay}}}"#),
            )
            .expect("schedule validates");
        }
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(10));
        let due = p.state.broker.take_due(p.a, 10);
        let texts: Vec<&str> = due.iter().map(|inj| inj.text.as_str()).collect();
        assert_eq!(texts, vec!["p5", "p4", "p3", "p2", "p1"]);
    }

    #[test]
    fn compact_self_needs_no_group_peer_needs_one() {
        let mut p = live_pair();
        let out = p
            .call(&p.run_a.clone(), "compact_session", "{}")
            .expect("self compact queues");
        assert!(out.contains(r#""queued":true"#), "out: {out}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, InjectKind::Command);
        assert_eq!(due[0].text, "/compact");
        let body = String::from_utf8(due[0].render_body()).unwrap();
        assert!(body.contains("/compact"), "body: {body}");
        assert!(!body.contains("send_response"), "commands carry no reply guidance: {body}");
        let err = p
            .call(&p.run_a.clone(), "compact_session", r#"{"target":"b"}"#)
            .expect_err("groupless peer compact fails");
        assert!(err.contains("no shared group"), "err: {err}");
        let mut grouped = p.grouped();
        grouped
            .call(&grouped.run_a.clone(), "compact_session", r#"{"target":"b"}"#)
            .expect("grouped peer compact queues");
        assert_eq!(grouped.state.broker.queued(grouped.b), 1);
    }

    #[test]
    fn schedule_fires_clear_text_then_cancel_drops() {
        let mut p = live_pair();
        let out = p
            .call(&p.run_a.clone(), "schedule_prompt", r#"{"prompt":"nudge","delay_seconds":0}"#)
            .expect("schedules");
        assert!(out.contains("timer_id"), "out: {out}");
        assert_eq!(p.state.broker.queued(p.a), 0, "timers wait for the tick");
        p.state.broker.tick(std::time::Instant::now());
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].kind, due[0].text.as_str()), (InjectKind::Command, "nudge"));
        p.call(&p.run_a.clone(), "schedule_prompt",
            r#"{"prompt":"fresh","delay_seconds":0,"clear_context":true}"#)
            .expect("clear-context schedules");
        p.state.broker.tick(std::time::Instant::now());
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "/clear\nfresh", "clear prefixes the prompt");
        let out = p
            .call(&p.run_a.clone(), "schedule_prompt", r#"{"prompt":"later","delay_seconds":60}"#)
            .expect("future schedules");
        let timer = crate::hooks::policy::json_string_field(out.as_bytes(), &["timer_id"]).unwrap();
        let cancelled = p
            .call(&p.run_a.clone(), "cancel_scheduled_prompt", &format!(r#"{{"timer_id":"{timer}"}}"#))
            .expect("cancel works");
        assert!(cancelled.contains("cancelled"), "cancelled: {cancelled}");
        p.state.broker.tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert_eq!(p.state.broker.queued(p.a), 0, "cancelled timer never fires");
        let again = p
            .call(&p.run_a.clone(), "cancel_scheduled_prompt", &format!(r#"{{"timer_id":"{timer}"}}"#))
            .expect_err("fired timers stay unknown");
        assert!(again.contains("unknown timer"), "again: {again}");
    }

    #[test]
    fn timers_for_lists_soonest_first_per_session() {
        let mut p = live_pair();
        let run_a = p.run_a.clone();
        p.call(&run_a, "schedule_prompt", r#"{"prompt":"slow","delay_seconds":60}"#)
            .expect("slow arms");
        let _ = p
            .call(&run_a, "schedule_prompt", r#"{"prompt":"fast","delay_seconds":0}"#)
            .expect("fast arms");
        let listed = p.state.broker.timers_for(p.a);
        assert_eq!(listed.len(), 2);
        assert!(listed[0].1 <= listed[1].1, "soonest first");
        assert!(p.state.broker.timers_for(p.b).is_empty(), "per-session");
        assert!(p.state.broker.cancel_timer(&listed[0].0), "human cancel drops");
        assert!(!p.state.broker.cancel_timer("nope"), "unknown stays false");
        assert_eq!(p.state.broker.timers_for(p.a).len(), 1);
    }

    #[test]
    fn schedule_rejects_blank_prompt_and_bad_delays() {
        let mut p = live_pair();
        let blank = p
            .call(&p.run_a.clone(), "schedule_prompt", "{}")
            .expect_err("blank prompt fails");
        assert!(blank.contains("needs a prompt"), "blank: {blank}");
        for args in [
            r#"{"prompt":"x","delay_seconds":-1}"#,
            r#"{"prompt":"x","delay_seconds":86401}"#,
            r#"{"prompt":"x","delay_seconds":"soon"}"#,
        ] {
            let err = p
                .call(&p.run_a.clone(), "schedule_prompt", args)
                .expect_err("bad delay fails");
            assert!(err.contains("delay_seconds"), "args {args}: {err}");
        }
    }

    #[test]
    fn terminal_records_cap_oldest_first() {
        // Past CONV_CAP the sweep evicts the oldest terminal
        // records even within their TTL; Open work never counts.
        let mut p = live_pair().grouped();
        let now = std::time::Instant::now();
        let mut oldest = String::new();
        for i in 0..(crate::comms::CONV_CAP + 1) {
            let id = format!("cap-{i}");
            if i == 0 {
                oldest = id.clone();
            }
            p.state.broker.convs.insert(
                id,
                Conv {
                    kind: ConvKind::Ask,
                    source: p.a,
                    target: p.b,
                    source_name: "a".to_string(),
                    target_name: "b".to_string(),
                    state: ConvState::Done,
                    acked: false,
                    last_update: now - std::time::Duration::from_secs(1000 - (i as u64).min(999)),
                    reminded: false,
                    target_updated: false,
                    delivered: false,
                },
            );
        }
        p.state.broker.convs.insert(
            "live-open".to_string(),
            Conv {
                kind: ConvKind::Ask,
                source: p.a,
                target: p.b,
                source_name: "a".to_string(),
                target_name: "b".to_string(),
                state: ConvState::Open,
                acked: false,
                last_update: now - std::time::Duration::from_secs(5000),
                reminded: false,
                target_updated: false,
                delivered: false,
            },
        );
        p.state.broker.tick(now);
        let terminal = p
            .state
            .broker
            .convs
            .values()
            .filter(|c| c.state != ConvState::Open)
            .count();
        assert_eq!(terminal, crate::comms::CONV_CAP, "cap enforced");
        assert!(!p.state.broker.convs.contains_key(&oldest), "oldest evicted");
        assert!(p.state.broker.convs.contains_key("live-open"), "open spared");
    }

    #[test]
    fn terminal_conversations_evict_past_ttl() {
        // Done and Failed records must not pile up forever: past the
        // TTL the sweep evicts them and late arrivals read as unknown
        // (still rejected, like a closed conversation).
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let answer = format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#);
        p.call(&p.run_b.clone(), "send_response", &answer)
            .expect("target answers");
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("closed rejects dups");
        assert_eq!(err, "conversation is closed");
        // The Done record survives while its response sits queued...
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("evicted reads unknown");
        assert_eq!(err, "unknown conversation");
        // ...and a Failed record evicts the same way.
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q2"}"#)
            .expect("ask validates");
        let conv2 = json_field(&res, "conversation").expect("conversation id");
        p.state.broker.target_exited(&p.state.manager, p.b);
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv2}","message":"late"}}"#),
            )
            .expect_err("failed record evicted");
        assert_eq!(err, "unknown conversation");
    }

    #[test]
    fn terminal_bot_conversations_evict_past_ttl() {
        use crate::comms::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .bcall(
                "skippy",
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"again"}}"#),
            )
            .expect_err("evicted bot conv reads unknown");
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn quiet_tells_quiesce_past_ttl() {
        // An Open tell quiet for a day is a dead thread: evict it
        // (resume with a fresh tell). Open asks await answers and
        // never quiesce; queued work pins its record.
        let mut p = live_pair().grouped();
        let now = std::time::Instant::now();
        let old = now - crate::comms::QUIESCE_TTL - std::time::Duration::from_secs(60);
        let (a, b) = (p.a, p.b);
        let mk = move |kind: ConvKind| Conv {
            kind,
            source: a,
            target: b,
            source_name: "a".to_string(),
            target_name: "b".to_string(),
            state: ConvState::Open,
            acked: true,
            last_update: old,
            reminded: true,
            target_updated: false,
            delivered: false,
        };
        p.state.broker.convs.insert("quiet-tell".to_string(), mk(ConvKind::Tell));
        p.state.broker.convs.insert("quiet-ask".to_string(), mk(ConvKind::Ask));
        let mut fresh = mk(ConvKind::Tell);
        fresh.last_update = now;
        p.state.broker.convs.insert("fresh-tell".to_string(), fresh);
        p.state.broker.convs.insert("pinned-tell".to_string(), mk(ConvKind::Tell));
        p.state.broker.push(
            b,
            Injection {
                conv: "pinned-tell".to_string(),
                kind: InjectKind::Tell,
                from: "a".to_string(),
                text: "waiting".to_string(),
            },
        );
        p.state.broker.tick(now);
        assert!(!p.state.broker.convs.contains_key("quiet-tell"), "quiet tell evicted");
        assert!(p.state.broker.convs.contains_key("quiet-ask"), "open ask spared");
        assert!(p.state.broker.convs.contains_key("fresh-tell"), "fresh tell spared");
        assert!(p.state.broker.convs.contains_key("pinned-tell"), "queued tell pinned");
    }

    #[test]
    fn quiet_bot_tells_quiesce_past_ttl() {
        // Same quiescence rule, bot table: a day-quiet Open tell
        // evicts, an Open ask never does.
        use crate::comms::bot::{BotConv, BotConvKind, BotConvState};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let now = std::time::Instant::now();
        let old = now - crate::comms::QUIESCE_TTL - std::time::Duration::from_secs(60);
        let b = p.b;
        let mk = move |kind: BotConvKind| BotConv {
            kind,
            session: b,
            session_name: "b".to_string(),
            client: "skippy".to_string(),
            from_client: true,
            state: BotConvState::Open,
            acked: true,
            delivered: false,
            last_update: old,
            reminded: true,
            target_updated: false,
        };
        p.state.broker.bot_convs.insert("quiet-tell".to_string(), mk(BotConvKind::Tell));
        p.state.broker.bot_convs.insert("quiet-ask".to_string(), mk(BotConvKind::Ask));
        p.state.broker.tick(now);
        assert!(!p.state.broker.bot_convs.contains_key("quiet-tell"), "quiet tell evicted");
        assert!(p.state.broker.bot_convs.contains_key("quiet-ask"), "open ask spared");
    }
}
