//! Broker lifecycle: session exit notices and the periodic tick.

use super::*;

impl Broker {
    /// A staged submit died to human input with its body already in
    /// the pane: the message may have mixed with the draft or been
    /// discarded. Tell the sender loudly (one notice, never chained).
    /// Asks and tells notify their source, responses and acks their
    /// target; every other kind stays best-effort as before (timer
    /// commands name no conversation at all, and follow-ups ride
    /// threads their sources already watch). Bot senders hear it
    /// through their inbox, parking on a full one like any exit
    /// notice. Nobody is told about their own typing.
    pub fn clobber_notice(
        &mut self,
        typer: SessionId,
        typer_name: &str,
        conv_id: &str,
        kind: InjectKind,
    ) {
        const TEXT: &str =
            "typed over your message before it submitted; confirm they saw it";
        if let Some(sender) = self.convs.get(conv_id).and_then(|conv| match kind {
            InjectKind::Ask | InjectKind::Tell => Some(conv.source),
            InjectKind::Response | InjectKind::Ack => Some(conv.target),
            _ => None,
        }) {
            if sender == typer {
                return;
            }
            self.push(
                sender,
                Injection {
                    conv: conv_id.to_string(),
                    kind: InjectKind::Failed,
                    from: typer_name.to_string(),
                    text: TEXT.to_string(),
                },
            );
            return;
        }
        // Same four kinds: follow-up senders are ambiguous on both
        // tables, so they stay best-effort.
        let clobbered = matches!(
            kind,
            InjectKind::Ask | InjectKind::Tell | InjectKind::Response | InjectKind::Ack
        );
        if clobbered {
            if let Some(client) = self
                .bot_convs
                .get(conv_id)
                .map(|conv| conv.client.clone())
            {
                let deposited = match self.clients.get_mut(&client) {
                    Some(c) => c
                        .deposit(
                            BotKind::Failed,
                            conv_id,
                            &typer.to_string(),
                            typer_name,
                            TEXT,
                            crate::comms::bot::now_unix_ms(),
                        )
                        .is_ok(),
                    // No inbox exists: nothing to retry toward.
                    None => true,
                };
                if !deposited {
                    self.pending_failures.push((
                        client,
                        conv_id.to_string(),
                        typer.to_string(),
                        typer_name.to_string(),
                    ));
                }
            }
        }
        // Non-clobbered kinds, timer commands, and unknown IDs have
        // nobody to tell.
    }

    /// Fail every open conversation touching an exited session. Targets fail
    /// loudly (their sources are told); sources fail silently.
    pub fn target_exited(&mut self, _sessions: &SessionManager, id: SessionId) {
        // A dead caller can never retry (its run is unbound and a
        // restart mints a fresh session), so its idempotency records
        // go with it instead of occupying another caller's capacity.
        self.session_idem.remove(&id.to_string());
        // A dying queue can strand answers: a queued response or ack
        // means its sender already got success, so the other party must
        // hear the loss loudly instead of assuming it was read.
        let dropped = self.queue.remove(&id).unwrap_or_default();
        let mut bot_dropped = Vec::new();
        for inj in &dropped {
            let (other, from) = match inj.kind {
                InjectKind::Response | InjectKind::Ack => match self.convs.get(&inj.conv) {
                    Some(conv) => (conv.target, conv.source_name.clone()),
                    None => {
                        // Same stranded-answer shape, bot table: the
                        // other party is a client, told via its inbox
                        // below instead of a session queue.
                        if let Some(conv) = self.bot_convs.get(&inj.conv) {
                            bot_dropped.push((
                                conv.client.clone(),
                                inj.conv.clone(),
                                conv.session.to_string(),
                                conv.session_name.clone(),
                            ));
                        }
                        continue;
                    }
                },
                _ => continue,
            };
            if other == id {
                continue;
            }
            self.push(
                other,
                Injection {
                    conv: inj.conv.clone(),
                    kind: InjectKind::Failed,
                    from,
                    text: "source exited before delivery".to_string(),
                },
            );
        }
        for (client, conv_id, session_id, session_name) in bot_dropped {
            // Deposit first: only a recorded failure closes an Open
            // record. A full inbox parks the notice for the tick
            // retry; Done records were already terminal either way.
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &session_id,
                        &session_name,
                        "target exited",
                        crate::comms::bot::now_unix_ms(),
                    )
                    .is_ok(),
                // No inbox exists: nothing to retry toward.
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    if conv.state == BotConvState::Open {
                        conv.state = BotConvState::Failed;
                    }
                }
            } else {
                self.pending_failures
                    .push((client, conv_id, session_id, session_name));
            }
        }
        self.timers.retain(|_, timer| timer.target != id);
        let mut notify = Vec::new();
        for (conv_id, conv) in self.convs.iter_mut() {
            if conv.state != ConvState::Open {
                continue;
            }
            if conv.target == id {
                conv.state = ConvState::Failed;
                notify.push((conv.source, conv_id.clone(), conv.target_name.clone()));
            } else if conv.source == id {
                conv.state = ConvState::Failed;
            }
        }
        for (source, conv_id, target_name) in notify {
            self.push(
                source,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Failed,
                    from: target_name,
                    text: "target exited".to_string(),
                },
            );
        }
        // Bot conversations touching the exited session fail too. A
        // client that asked or told the session is told loudly through
        // its inbox; a client the session asked keeps its inbox event
        // but the conversation is over (late answers close).
        let mut bot_notify = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.state != BotConvState::Open || conv.session != id {
                continue;
            }
            if conv.from_client {
                bot_notify.push((
                    conv.client.clone(),
                    conv_id.clone(),
                    conv.session_name.clone(),
                ));
            } else {
                conv.state = BotConvState::Failed;
            }
        }
        for (client, conv_id, session_name) in bot_notify {
            // Deposit first: only a recorded failure closes the
            // conversation. A full inbox leaves it Open and parks the
            // session for retry on later sweeps (see tick).
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &id.to_string(),
                        &session_name,
                        "target exited",
                        crate::comms::bot::now_unix_ms(),
                    )
                    .is_ok(),
                // No inbox exists (revocation fails convs itself, so
                // this is belt-and-braces): nothing to retry toward.
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    conv.state = BotConvState::Failed;
                }
            } else {
                self.dead_sessions.insert(id);
            }
        }
    }

    /// Emit due courtesy reminders: one per acked tell whose target went
    /// quiet past the grace period. Never repeats, never nudges otherwise.
    pub fn tick(&mut self, now: Instant) {
        let mut due = Vec::new();
        for (conv_id, conv) in self.convs.iter_mut() {
            if conv.kind != ConvKind::Tell
                || conv.state != ConvState::Open
                || !conv.acked
                || conv.reminded
                || conv.target_updated
            {
                continue;
            }
            if now.duration_since(conv.last_update) >= COURTESY_GRACE {
                conv.reminded = true;
                due.push((conv.target, conv_id.clone(), conv.source_name.clone()));
            }
        }
        for (target, conv_id, source_name) in due {
            self.push(
                target,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Reminder,
                    from: source_name,
                    text: "no update since your ack; the source is still waiting".to_string(),
                },
            );
        }
        // Fire due self-injection timers into their queues, earliest
        // due first: hash order is not an ordering, and a stalled
        // loop can owe several deadlines at once.
        let mut fired = Vec::new();
        for (timer_id, timer) in self.timers.iter() {
            if now >= timer.due {
                fired.push((timer.due, timer_id.clone()));
            }
        }
        fired.sort();
        let fired: Vec<String> = fired.into_iter().map(|(_, id)| id).collect();
        for timer_id in fired {
            if let Some(timer) = self.timers.remove(&timer_id) {
                self.push(
                    timer.target,
                    Injection {
                        conv: timer_id,
                        kind: InjectKind::Command,
                        from: timer.from,
                        text: timer.text,
                    },
                );
            }
        }
        // Bot courtesy reminders follow the same rule; the target side
        // picks the delivery path (inbox event for clients, pane write
        // for sessions).
        let mut bot_due = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.kind != BotConvKind::Tell
                || conv.state != BotConvState::Open
                || !conv.acked
                || conv.reminded
                || conv.target_updated
                // A dead session's queue is gone: reminding it only
                // recreates a queue nobody drains (its failure notice
                // is already parked for retry).
                || self.dead_sessions.contains(&conv.session)
            {
                continue;
            }
            if now.duration_since(conv.last_update) >= COURTESY_GRACE {
                let source = if conv.from_client {
                    conv.client.clone()
                } else {
                    conv.session_name.clone()
                };
                bot_due.push((
                    conv.session,
                    conv.client.clone(),
                    conv_id.clone(),
                    source,
                    conv.from_client,
                ));
            }
        }
        for (session, client, conv_id, source, to_session) in bot_due {
            if to_session {
                // Session queues are unbounded: the push cannot fail.
                self.push(
                    session,
                    Injection {
                        conv: conv_id.clone(),
                        kind: InjectKind::Reminder,
                        from: source,
                        text: "no update since your ack; the source is still waiting".to_string(),
                    },
                );
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    conv.reminded = true;
                }
            } else {
                // The inbox is capped: only a deposited reminder counts
                // as sent. A full inbox leaves the conversation
                // un-reminded and the next sweep retries.
                let deposited = self
                    .clients
                    .get_mut(&client)
                    .map(|c| {
                        c.deposit(
                            BotKind::Reminder,
                            &conv_id,
                            &session.to_string(),
                            &source,
                            "no update since your ack; the source is still waiting",
                            crate::comms::bot::now_unix_ms(),
                        )
                        .is_ok()
                    })
                    .unwrap_or(false);
                if deposited {
                    if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                        conv.reminded = true;
                    }
                }
            }
        }
        // Retry exit-failure notices whose inbox was full: each success
        // closes its conversation, and ids with nothing Open drop out.
        let dead: Vec<SessionId> = self.dead_sessions.iter().copied().collect();
        for id in dead {
            let mut pending = Vec::new();
            for (conv_id, conv) in self.bot_convs.iter() {
                if conv.state == BotConvState::Open
                    && conv.session == id
                    && conv.from_client
                {
                    pending.push((
                        conv_id.clone(),
                        conv.client.clone(),
                        conv.session_name.clone(),
                    ));
                }
            }
            for (conv_id, client, session_name) in pending {
                let deposited = self
                    .clients
                    .get_mut(&client)
                    .map(|c| {
                        c.deposit(
                            BotKind::Failed,
                            &conv_id,
                            &id.to_string(),
                            &session_name,
                            "target exited",
                            crate::comms::bot::now_unix_ms(),
                        )
                        .is_ok()
                    })
                    .unwrap_or(false);
                if deposited {
                    if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                        conv.state = BotConvState::Failed;
                    }
                }
            }
            let live = self
                .bot_convs
                .values()
                .any(|c| c.state == BotConvState::Open && c.session == id);
            if !live {
                self.dead_sessions.remove(&id);
                // Nothing legitimate queues to a dead session (sends
                // resolve live targets only), so anything here is
                // sweep debris for nobody: drop it with the entry.
                self.queue.remove(&id);
            }
        }
        // Retry stranded-answer notices parked at exit time: each
        // success closes an Open record (Done ones stay Done) and
        // drops its entry; a still-full inbox keeps its entry for
        // the next sweep. A vanished client drops its entries.
        let mut still = Vec::new();
        for (client, conv_id, session_id, session_name) in
            std::mem::take(&mut self.pending_failures)
        {
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &session_id,
                        &session_name,
                        "target exited",
                        crate::comms::bot::now_unix_ms(),
                    )
                    .is_ok(),
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    if conv.state == BotConvState::Open {
                        conv.state = BotConvState::Failed;
                    }
                }
            } else {
                still.push((client, conv_id, session_id, session_name));
            }
        }
        self.pending_failures = still;
        // Evict terminal records past their TTL so the maps stay
        // bounded by live work, not history. A record survives while
        // a queued response or ack still names it: evicting under one
        // would strand the exit path's loud-failure lookup (None
        // reads as "no party to tell"). Other kinds never look the
        // record up after terminal state, so they pin nothing.
        // Quiescence pins harder: any queued work keeps an Open tell,
        // since its body may still deliver.
        let mut pinned = std::collections::HashSet::new();
        let mut referenced = std::collections::HashSet::new();
        for q in self.queue.values() {
            for inj in q {
                referenced.insert(inj.conv.clone());
                if matches!(
                    inj.kind,
                    InjectKind::Response | InjectKind::Ack
                ) {
                    pinned.insert(inj.conv.clone());
                }
            }
        }
        self.convs.retain(|id, conv| {
            if conv.state == ConvState::Open {
                // Only tells go quiet: an ask awaits its answer,
                // which can land at any time. A tell silent for a day
                // is a dead thread (resume with a fresh tell).
                if conv.kind == ConvKind::Tell
                    && now.duration_since(conv.last_update) >= QUIESCE_TTL
                    && !referenced.contains(id)
                {
                    return false;
                }
                return true;
            }
            now.duration_since(conv.last_update) < CONV_TTL || pinned.contains(id)
        });
        // Terminal count cap, oldest first, even within TTL. Open
        // work never counts.
        let over = self
            .convs
            .values()
            .filter(|c| c.state != ConvState::Open)
            .count()
            .saturating_sub(CONV_CAP);
        if over > 0 {
            let mut oldest: Vec<(String, Instant)> = self
                .convs
                .iter()
                .filter(|(_, c)| c.state != ConvState::Open)
                .map(|(id, c)| (id.clone(), c.last_update))
                .collect();
            oldest.sort_by_key(|(_, at)| *at);
            for (id, _) in oldest.into_iter().take(over) {
                self.convs.remove(&id);
            }
        }
        self.bot_convs.retain(|id, conv| {
            if conv.state == BotConvState::Open {
                if conv.kind == BotConvKind::Tell
                    && now.duration_since(conv.last_update) >= QUIESCE_TTL
                    && !referenced.contains(id)
                {
                    return false;
                }
                return true;
            }
            now.duration_since(conv.last_update) < CONV_TTL
        });
        let bot_over = self
            .bot_convs
            .values()
            .filter(|c| c.state != BotConvState::Open)
            .count()
            .saturating_sub(CONV_CAP);
        if bot_over > 0 {
            let mut oldest: Vec<(String, Instant)> = self
                .bot_convs
                .iter()
                .filter(|(_, c)| c.state != BotConvState::Open)
                .map(|(id, c)| (id.clone(), c.last_update))
                .collect();
            oldest.sort_by_key(|(_, at)| *at);
            for (id, _) in oldest.into_iter().take(bot_over) {
                self.bot_convs.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;
    use crate::infra::event::AppEvent;

    #[test]
    fn exit_failure_notice_retries_a_full_inbox() {
        // Same ordering for exit notices: a full inbox must hold the
        // conversation Open (retry on later sweeps), never mark Failed
        // while the notice is lost.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("client tells");
        let conv = json_field(&res, "conversation").expect("conversation id");
        fill_inbox(&mut p, "skippy");
        p.state.broker.target_exited(&p.state.manager, p.b);
        assert_eq!(
            p.state.broker.bot_convs.get(&conv).expect("conv").state,
            crate::comms::bot::BotConvState::Open,
            "unsent failure notice holds the conv open"
        );
        // Room opens: the next sweep delivers the failure and closes.
        drain_inbox(&mut p, "skippy");
        p.state.broker.tick(std::time::Instant::now());
        assert_eq!(
            p.state.broker.bot_convs.get(&conv).expect("conv").state,
            crate::comms::bot::BotConvState::Failed
        );
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert_eq!(poll.matches(r#""kind":"failed""#).count(), 1, "poll: {poll}");
    }

    #[test]
    fn full_inbox_parks_bot_exit_notice_for_retry() {
        // The inbox is full when the session exits: the failure
        // notice parks instead of dropping, and the next sweep
        // delivers it once space frees.
        use crate::comms::bot::{BotKind, INBOX_CAP};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"skippy","message":"are you there?"}"#,
            )
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("client answers");
        // Receive and ack the ask so the pads below start past it;
        // the epoch rides along for the later cursor polls and acks.
        let poll1 = p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        let epoch = crate::ipc::mcp::top_raw(&poll1, "epoch").expect("epoch echoed");
        p.bcall("skippy", "bot_ack", &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#))
            .expect("ack advances");
        // Fill the inbox to the cap: the exit notice must park.
        {
            let client = p
                .state
                .broker
                .clients
                .get_mut("skippy")
                .expect("client registered");
            for n in 0..INBOX_CAP {
                let _ = client.deposit(
                    BotKind::Tell,
                    "pad",
                    "s",
                    "a",
                    &n.to_string(),
                    1,
                );
            }
        }
        p.state.broker.target_exited(&p.state.manager, p.a);
        assert_eq!(
            p.state.broker.pending_failures.len(),
            1,
            "full inbox parks the notice"
        );
        // Free one page: receive 2..21, ack through it.
        p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":21,"epoch":{epoch}}}"#),
        )
        .expect("ack frees space");
        p.state.broker.tick(std::time::Instant::now());
        assert!(
            p.state.broker.pending_failures.is_empty(),
            "retry delivers"
        );
        let poll = p
            .bcall(
                "skippy",
                "bot_poll",
                &format!(r#"{{"cursor":240,"epoch":{epoch}}}"#),
            )
            .expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn target_exit_fails_the_conversation() {
        let mut p = live_pair().grouped();
        p.call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q?"}"#)
            .unwrap();
        p.state.broker.take_due(p.b, 10);
        p.state.broker.target_exited(&p.state.manager, p.b);
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Failed));
        // The dead target keeps no letters.
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn stale_run_id_after_exit_is_rejected() {
        let mut p = live_pair().grouped();
        assert!(p.state.manager.kill(p.b));
        // The reader thread reports the exit asynchronously; poll for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in p.state.manager.drain_pty_max(100) {
                p.state.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = p
                .state
                .manager
                .get(p.b)
                .is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            p.state.manager.get(p.b).is_none_or(|rec| !rec.state.is_live()),
            "b exited"
        );
        p.state.broker.target_exited(&p.state.manager, p.b);
        let err = p
            .call(&p.run_b.clone(), "list_sessions", "{}")
            .expect_err("exited run ID is stale");
        assert!(err.contains("run ID"), "err: {err}");
    }
}
