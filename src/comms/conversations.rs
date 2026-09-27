//! Broker conversations: ask, tell, responses, acks, and the bot bridges.

use super::*;

impl Broker {
    pub(super) fn ask(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let target_name = Self::arg(args, "target").filter(|s| !s.is_empty())
            .ok_or_else(|| "ask needs a target".to_string())?;
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "ask needs text".to_string())?;
        let target = match self.resolve_target(sessions, &target_name) {
            Ok(id) => {
                if !self.shared_group(caller, id) {
                    return Err("no shared group with target".to_string());
                }
                if self.pressure(sessions, id) >= PRESSURE_CAP {
                    return Err(format!("pressure cap reached for {target_name:?}"));
                }
                id
            }
            Err(e) if e.starts_with("ambiguous") => return Err(e),
            Err(e) => {
                if !self.clients.contains_key(&target_name) {
                    return Err(e);
                }
                return self.send_client(sessions, caller, &target_name, &text, now, BotConvKind::Ask);
            }
        };
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let from = self.names(sessions, caller);
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Ask,
                from: from.clone(),
                text,
            },
        );
        self.convs.insert(
            conv.clone(),
            Conv {
                kind: ConvKind::Ask,
                source: caller,
                target,
                source_name: from,
                target_name: target_name_live,
                state: ConvState::Open,
                acked: false,
                last_update: now,
                reminded: false,
                target_updated: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    /// Session sends to a bot peer (ask or new tell): same
    /// shared-group and inbox-cap rules as any send, delivered as a
    /// structured inbox event — never a pane write, so polling cannot
    /// race terminal delivery.
    fn send_client(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        target_name: &str,
        text: &str,
        now: Instant,
        kind: BotConvKind,
    ) -> Result<String, String> {
        let groups = self
            .clients
            .get(target_name)
            .map(|c| c.groups.clone())
            .expect("caller checked registration");
        if !self.shares_with_client(caller, &groups) {
            return Err("no shared group with target".to_string());
        }
        if self
            .clients
            .get(target_name)
            .expect("caller checked registration")
            .unacked()
            >= crate::comms::bot::INBOX_CAP
        {
            return Err(format!("pressure cap reached for {target_name:?}"));
        }
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let event = match kind {
            BotConvKind::Ask => BotKind::Ask,
            BotConvKind::Tell => BotKind::Tell,
        };
        self.clients
            .get_mut(target_name)
            .expect("caller checked registration")
            .deposit(
                event,
                &conv,
                &from_id,
                &from_name,
                text,
                crate::comms::bot::now_unix_ms(),
            )
            .map_err(|_| format!("pressure cap reached for {target_name:?}"))?;
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind,
                session: caller,
                session_name: from_name,
                client: target_name.to_string(),
                from_client: false,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    pub(super) fn send_response(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let id = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "a conversation ID is required".to_string())?;
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "send_response needs text".to_string())?;
        if let Some(r) = self.respond_bot(sessions, caller, &id, &text, now) {
            return r;
        }
        let source = {
            let conv = self
                .convs
                .get(&id)
                .ok_or_else(|| "unknown conversation".to_string())?;
            if conv.state != ConvState::Open {
                return Err("conversation is closed".to_string());
            }
            if conv.kind != ConvKind::Ask {
                return Err("only an ask takes a response".to_string());
            }
            if caller != conv.target {
                return Err("only the target answers".to_string());
            }
            if !self.shared_group(caller, conv.source) {
                return Err("no shared group with target".to_string());
            }
            conv.source
        };
        let from = self.names(sessions, caller);
        self.push(
            source,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Response,
                from,
                text,
            },
        );
        if let Some(conv) = self.convs.get_mut(&id) {
            conv.state = ConvState::Done;
            conv.last_update = now;
        }
        Ok(format!(r#"{{"conversation":"{id}","completed":true}}"#))
    }

    pub(super) fn tell(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let target_name = Self::arg(args, "target").filter(|s| !s.is_empty())
            .ok_or_else(|| "tell needs a target".to_string())?;
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "tell needs text".to_string())?;
        if let Some(id) = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty()) {
            if let Some(r) = self.tell_bot_followup(sessions, caller, &id, &target_name, &text, now) {
                return r;
            }
            // Informational follow-up on an existing conversation: delivered
            // like a tell, but no new ack is expected. Either party can
            // follow up — this is the receiver's way back — delivering to
            // the other side.
            let peer = {
                let conv = self
                    .convs
                    .get(&id)
                    .ok_or_else(|| "unknown conversation".to_string())?;
                if conv.kind != ConvKind::Tell {
                    return Err("only a tell takes follow-ups".to_string());
                }
                if conv.state != ConvState::Open {
                    return Err("conversation is closed".to_string());
                }
                if caller == conv.source {
                    conv.target
                } else if caller == conv.target {
                    conv.source
                } else {
                    return Err("only conversation parties can follow up".to_string());
                }
            };
            let live = self.resolve_target(sessions, &target_name)?;
            if live != peer {
                return Err("follow-up target mismatch".to_string());
            }
            if !self.shared_group(caller, peer) {
                return Err("no shared group with target".to_string());
            }
            if self.pressure(sessions, peer) >= PRESSURE_CAP {
                return Err(format!("pressure cap reached for {target_name:?}"));
            }
            let from = self.names(sessions, caller);
            self.push(
                peer,
                Injection {
                    conv: id.clone(),
                    kind: InjectKind::FollowUp,
                    from,
                    text,
                },
            );
            if let Some(conv) = self.convs.get_mut(&id) {
                // Only the owing party's update satisfies courtesy: a
                // target follow-up suppresses the reminder for good,
                // while the source's own follow-up merely restarts grace.
                if caller == conv.target {
                    conv.target_updated = true;
                }
                conv.last_update = now;
            }
            return Ok(format!(r#"{{"conversation":"{id}","followup":true}}"#));
        }
        let target = match self.resolve_target(sessions, &target_name) {
            Ok(id) => {
                if !self.shared_group(caller, id) {
                    return Err("no shared group with target".to_string());
                }
                if self.pressure(sessions, id) >= PRESSURE_CAP {
                    return Err(format!("pressure cap reached for {target_name:?}"));
                }
                id
            }
            Err(e) if e.starts_with("ambiguous") => return Err(e),
            Err(e) => {
                if !self.clients.contains_key(&target_name) {
                    return Err(e);
                }
                return self.send_client(sessions, caller, &target_name, &text, now, BotConvKind::Tell);
            }
        };
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let from = self.names(sessions, caller);
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Tell,
                from: from.clone(),
                text,
            },
        );
        self.convs.insert(
            conv.clone(),
            Conv {
                kind: ConvKind::Tell,
                source: caller,
                target,
                source_name: from,
                target_name: target_name_live,
                state: ConvState::Open,
                acked: false,
                last_update: now,
                reminded: false,
                target_updated: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    /// Session follow-up on a bot conversation: the peer is the client,
    /// so the update lands in its inbox, never a pane. `None` when the
    /// ID is not a bot conversation (the session path decides).
    fn tell_bot_followup(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        target_name: &str,
        text: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Tell {
            return Some(Err("only a tell takes follow-ups".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only conversation parties can follow up".to_string()));
        }
        if target_name != client {
            return Some(Err("follow-up target mismatch".to_string()));
        }
        let groups = self
            .clients
            .get(&client)
            .map(|c| c.groups.clone())
            .unwrap_or_default();
        if !self.shares_with_client(caller, &groups) {
            return Some(Err("no shared group with target".to_string()));
        }
        if self
            .clients
            .get(&client)
            .is_some_and(|c| c.unacked() >= crate::comms::bot::INBOX_CAP)
        {
            return Some(Err(format!("pressure cap reached for {target_name:?}")));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::FollowUp,
                id,
                &from_id,
                &from_name,
                text,
                crate::comms::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {target_name:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            // The session owes the update exactly when the client told
            // it (client-originated tell): then this follow-up satisfies
            // courtesy for good.
            if conv.from_client {
                conv.target_updated = true;
            }
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","followup":true}}"#)))
    }

    /// Session answers a bot's ask: the answer lands in the client
    /// inbox. `None` when the ID is not a bot conversation.
    fn respond_bot(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        text: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Ask {
            return Some(Err("only an ask takes a response".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only the target answers".to_string()));
        }
        // The client grant behind this ask is live: leaving the shared
        // group since revokes the session's answer channel.
        let granted = self
            .clients
            .get(&client)
            .is_some_and(|c| self.shares_with_client(caller, &c.groups));
        if !granted {
            return Some(Err("no shared group with target".to_string()));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::Response,
                id,
                &from_id,
                &from_name,
                text,
                crate::comms::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {client:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            conv.state = BotConvState::Done;
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","completed":true}}"#)))
    }

    /// Session acknowledges a bot's tell. `None` when the ID is not a
    /// bot conversation.
    fn ack_bot(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Tell {
            return Some(Err("nothing to acknowledge".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only the target acknowledges".to_string()));
        }
        // The client grant behind this tell is live: leaving the shared
        // group since revokes the session's ack channel.
        let granted = self
            .clients
            .get(&client)
            .is_some_and(|c| self.shares_with_client(caller, &c.groups));
        if !granted {
            return Some(Err("no shared group with target".to_string()));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::Ack,
                id,
                &from_id,
                &from_name,
                "ack_message",
                crate::comms::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {client:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            conv.acked = true;
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#)))
    }

    pub(super) fn ack(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let id = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "a conversation ID is required".to_string())?;
        if let Some(r) = self.ack_bot(sessions, caller, &id, now) {
            return r;
        }
        let (source, acked) = {
            let conv = self
                .convs
                .get(&id)
                .ok_or_else(|| "unknown conversation".to_string())?;
            if conv.kind != ConvKind::Tell {
                return Err("nothing to acknowledge".to_string());
            }
            if conv.state != ConvState::Open {
                return Err("conversation is closed".to_string());
            }
            if caller == conv.source && self.shared_group(caller, conv.target) {
                // The source is acking a follow-up it received. Those
                // expect no ack, so succeed quietly: nothing reaches
                // the target, and courtesy stays the target's own.
                return Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#));
            }
            if caller != conv.target {
                return Err("only the target acknowledges".to_string());
            }
            if !self.shared_group(caller, conv.source) {
                return Err("no shared group with target".to_string());
            }
            (conv.source, conv.acked)
        };
        if acked {
            // Idempotent replay: the Ack already went out, so a
            // retried acknowledgement succeeds without queueing a
            // duplicate or nudging the courtesy clock.
            return Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#));
        }
        let from = self.names(sessions, caller);
        self.push(
            source,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Ack,
                from,
                text: "ack_message".to_string(),
            },
        );
        if let Some(conv) = self.convs.get_mut(&id) {
            conv.acked = true;
            conv.last_update = now;
        }
        Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

    #[test]
    fn source_acking_a_follow_up_is_a_quiet_success() {
        // a tells b, b follows up back to a. a acking that follow-up used
        // to fail "only the target acknowledges". Follow-ups expect no
        // ack, so the source's ack succeeds without pinging b, and it
        // never stands in for b's own ack (courtesy stays b's).
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"hi"}"#)
            .expect("tell");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.state.broker.take_due(p.b, 10);
        p.call(
            &p.run_b.clone(),
            "tell_session",
            &format!(r#"{{"target":"a","message":"back at you","conversation_id":"{conv}"}}"#),
        )
        .expect("follow-up");
        p.state.broker.take_due(p.a, 10);
        let ack = p
            .call(&p.run_a.clone(), "ack_message", &format!(r#"{{"conversation_id":"{conv}"}}"#))
            .expect("source ack of a follow-up succeeds");
        assert!(ack.contains("\"acknowledged\":true"), "{ack}");
        assert_eq!(p.state.broker.queued(p.b), 0, "no ack pinged back to b");
        // b's own ack still goes to a exactly as before.
        p.call(&p.run_b.clone(), "ack_message", &format!(r#"{{"conversation_id":"{conv}"}}"#))
            .expect("target ack");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, InjectKind::Ack);
    }

    #[test]
    fn bot_ask_response_roundtrip_never_touches_a_pane() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(res.contains("\"epoch\""), "res: {res}");
        // The question reaches the session pane, never the client inbox.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        assert!(p
            .bcall("skippy", "bot_poll", "{}")
            .unwrap()
            .contains("\"events\":[]"));
        // The session answers through its own tool; the answer lands in
        // the inbox and nowhere else.
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"response""#), "poll: {poll}");
        let epoch = crate::ipc::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#),
        )
        .expect("ack validates");
        assert!(p
            .bcall("skippy", "bot_poll", "{}")
            .unwrap()
            .contains("\"events\":[]"));
    }

    #[test]
    fn session_ask_client_roundtrip_flows_through_the_inbox() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"skippy","message":"are you there?"}"#)
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        // No pane anywhere holds the question.
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"ask""#), "poll: {poll}");
        let epoch = crate::ipc::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#),
        )
        .unwrap();
        p.bcall(
            "skippy",
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("client answers");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Response));
    }

    #[test]
    fn bot_tell_ack_roundtrip_confirms_receipt_not_work() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","message":"deploy at dawn"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Tell));
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("target acks");
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"ack""#), "poll: {poll}");
    }

    #[test]
    fn tell_target_can_follow_up_back_to_source() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        // The receiver talks back on the same conversation by naming the
        // source as target; the injection lands at the source, from b.
        let r = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                &format!(r#"{{"target":"a","message":"back","conversation_id":"{conv}"}}"#),
            )
            .expect("target follows up");
        assert!(r.contains("followup"), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::FollowUp));
        assert_eq!(due[0].from, "b");
        assert_eq!(due[0].conv, conv);
        // Rendered guidance points back at b with the same conversation.
        let bytes = due[0].render();
        assert!(
            bytes.windows(9).any(|w| w == b"target b "),
            "guidance: {bytes:?}"
        );
        assert!(bytes.ends_with(b"\r"), "enter: {bytes:?}");
        // Outsiders still cannot ride the conversation, even in-group.
        let c = p
            .state
            .manager
            .spawn(
                "c",
                &std::env::temp_dir(),
                "exec sleep 30",
                crate::infra::ids::RunId::generate(),
                "shell",
            )
            .unwrap();
        p.state.broker.join(&p.state.manager, c, "peers").unwrap();
        let run_c = p.state.manager.get(c).unwrap().run_id.to_string();
        let err = p
            .call(
                &run_c,
                "tell_session",
                &format!(r#"{{"target":"a","message":"hijack","conversation_id":"{conv}"}}"#),
            )
            .expect_err("outsider follow-up rejected");
        assert!(err.contains("parties"), "err: {err}");
    }

    #[test]
    fn tell_followed_by_ack_notifies_source() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").unwrap();
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Tell));
        // Delivered tells no longer count toward pressure.
        assert_eq!(p.state.broker.pressure(&p.state.manager, p.b), 0);
        let r = p
            .call(
                &p.run_b.clone(),
                "ack_message",
                &format!(r#"{{"conversation_id":"{conv}"}}"#),
            )
            .expect("target acks");
        assert!(r.contains(&conv), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Ack));
    }

    #[test]
    fn tell_with_existing_conversation_needs_no_new_ack() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        // b reads the tell, then acks; the ack goes back to a.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .unwrap();
        p.state.broker.take_due(p.a, 10);
        // Follow-up on the same conversation: delivered, no ack expected.
        p.call(
            &p.run_a.clone(),
            "tell_session",
            &format!(r#"{{"target":"b","message":"more","conversation_id":"{conv}"}}"#),
        )
        .expect("follow-up validates");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::FollowUp));
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
    }

    #[test]
    fn dropped_response_on_source_exit_notifies_responder() {
        // B answers, gets success, and A's queue still holds the
        // response when A exits: deleting it silently would leave B
        // believing it was read. B must hear the loss loudly.
        let mut p = live_pair().grouped();
        let ask = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("ask validates");
        let conv = json_field(&ask, "conversation").expect("conversation id");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the ask");
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        assert_eq!(p.state.broker.queued(p.a), 1);
        p.state.broker.target_exited(&p.state.manager, p.a);
        assert_eq!(p.state.broker.queued(p.a), 0, "exited queue is gone");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1, "responder hears the loss");
        assert!(matches!(due[0].kind, InjectKind::Failed));
        assert_eq!(due[0].conv, conv);
    }

    #[test]
    fn dropped_ack_on_source_exit_notifies_acker() {
        // Same window for acks: the teller exits before reading the
        // ack, so the acker must hear it instead of assuming receipt.
        let mut p = live_pair().grouped();
        let tell = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&tell, "conversation").expect("conversation id");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the tell");
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("target acks");
        assert_eq!(p.state.broker.queued(p.a), 1);
        p.state.broker.target_exited(&p.state.manager, p.a);
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1, "acker hears the loss");
        assert!(matches!(due[0].kind, InjectKind::Failed));
        assert_eq!(due[0].conv, conv);
    }

    #[test]
    fn dropped_bot_response_on_source_exit_notifies_client() {
        // The session asked the client, the client answered (got
        // completed:true), and the session exits with the response
        // still queued: the client must hear the loss loudly through
        // its inbox, not lose it silently.
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
        p.state.broker.target_exited(&p.state.manager, p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn dropped_bot_ack_on_source_exit_notifies_client() {
        // The client acked a session tell (got acknowledged:true) and
        // the session exits with the Ack still queued: the client is
        // told, and the record closes instead of lingering Open.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","message":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall("skippy", "ack_message", &format!(r#"{{"conversation_id":"{conv}"}}"#))
            .expect("client acks");
        p.state.broker.target_exited(&p.state.manager, p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn typed_over_bot_ack_notifies_the_inbox() {
        // The client's ack reached A's prompt with its Enter staged;
        // A types first, so the submit dies and the client is told
        // through its inbox instead of assuming clean delivery.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","message":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        p.state.settle_comms();
        assert!(p.state.pending_enter.contains_key(&p.a), "enter staged");
        p.state.note_human_input(p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn courtesy_reminder_fires_once_after_grace() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .unwrap();
        p.state.broker.take_due(p.a, 10);
        p.state.broker.take_due(p.b, 10);
        // Inside the grace period: silence.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(10));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        // Past it: exactly one reminder to the target, never repeated.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Reminder));
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn bot_courtesy_reminder_retries_a_full_inbox() {
        // A reminder that cannot be deposited must not count as sent:
        // the conversation stays un-reminded and the next sweep retries
        // once the client makes room. Session-originated tell, so the
        // reminder travels client-ward into the (possibly full) inbox.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                r#"{"target":"skippy","text":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the ack");
        fill_inbox(&mut p, "skippy");
        // Past grace with nowhere to put the reminder: still pending.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        assert!(
            !p.state.broker.bot_convs.get(&conv).expect("conv").reminded,
            "undelivered reminder stays un-reminded"
        );
        // Room opens: the next sweep delivers exactly one reminder.
        drain_inbox(&mut p, "skippy");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.bot_convs.get(&conv).expect("conv").reminded);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert_eq!(poll.matches(r#""kind":"reminder""#).count(), 1, "poll: {poll}");
        assert!(poll.contains(&conv), "poll: {poll}");
    }

    #[test]
    fn target_followup_suppresses_the_courtesy_reminder() {
        // The courtesy obligation ends when the owing party updates:
        // b acked and then sent the update itself, so no reminder may
        // fire — not now, not after more silence. The source's own
        // follow-ups merely restart grace (see the back-to-source test).
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .unwrap();
        p.call(
            &p.run_b.clone(),
            "tell_session",
            &format!(r#"{{"target":"a","message":"on it","conversation_id":"{conv}"}}"#),
        )
        .expect("target follows up");
        p.state.broker.take_due(p.a, 10);
        p.state.broker.take_due(p.b, 10);
        // Past grace and far past it: silence, forever.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn client_followup_suppresses_the_courtesy_reminder() {
        // Same rule client-ward: the session told the client, the
        // client acked and followed up, so the inbox stays quiet.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                r#"{"target":"skippy","text":"fyi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        p.bcall(
            "skippy",
            "tell_session",
            &format!(r#"{{"target":"b","text":"on it","conversation_id":"{conv}"}}"#),
        )
        .expect("client follows up");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(!poll.contains(r#""kind":"reminder""#), "poll: {poll}");
    }

    #[test]
    fn courtesy_skips_dead_sessions() {
        // A bot tell was acked, the session exits with a full inbox
        // (failure retry pending): the courtesy sweep must not push
        // a Reminder to the removed queue — nobody will drain it.
        use crate::comms::bot::{BotKind, INBOX_CAP};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","message":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("session acks");
        {
            let client = p
                .state
                .broker
                .clients
                .get_mut("skippy")
                .expect("client registered");
            for n in 0..INBOX_CAP {
                let _ = client.deposit(BotKind::Tell, "pad", "s", "a", &n.to_string(), 1);
            }
        }
        p.state.broker.target_exited(&p.state.manager, p.b);
        assert!(p.state.broker.dead_sessions.contains(&p.b), "retry pending");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert_eq!(
            p.state.broker.queued(p.b),
            0,
            "no reminder to a dead session"
        );
        assert!(
            p.state.broker.dead_sessions.contains(&p.b),
            "failure retry still pending"
        );
    }

    #[test]
    fn repeat_ack_replays_without_dup() {
        // A retried acknowledgement (lost verdict, impatient
        // harness) returns success again but queues no second Ack
        // and stops touching the courtesy clock.
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let ack = format!(r#"{{"conversation_id":"{conv}"}}"#);
        p.call(&p.run_b.clone(), "ack_message", &ack)
            .expect("ack validates");
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        let again = p
            .call(&p.run_b.clone(), "ack_message", &ack)
            .expect("retry still acknowledges");
        assert!(again.contains(r#""acknowledged":true"#), "again: {again}");
        assert_eq!(
            p.state.broker.take_due(p.a, 10).len(),
            0,
            "no duplicate Ack queued"
        );
    }

    #[test]
    fn epochless_nonzero_ack_conflicts() {
        // Like polls, a nonzero ack without the epoch may be a stale
        // pre-restart retry: accepting it would advance (and delete)
        // a fresh epoch's received prefix.
        use crate::comms::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        for n in 0..3 {
            p.call(
                &p.run_a.clone(),
                "tell_session",
                &format!(r#"{{"target":"skippy","message":"m{n}"}}"#),
            )
            .expect("tell validates");
        }
        p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        let err = p
            .bcall("skippy", "bot_ack", r#"{"cursor":2}"#)
            .expect_err("epochless nonzero ack conflicts");
        assert_eq!(err.code, ErrorCode::Conflict);
        // Nothing was deleted: the fresh prefix still polls.
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains("m0"), "fresh events intact: {poll}");
    }
}
