//! Broker bot clients: registry, authentication, and the `bot_*` handlers.

use super::*;

impl Broker {
    /// Operator registration of one bot peer: validated name, at least
    /// one group grant, operator-minted credential, optional tool
    /// grants (messaging is the default; nothing else is implied).
    /// Rejects duplicates and collisions with live session names so
    /// routing can never be ambiguous. No MCP path calls this.
    pub fn register_client(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        groups: Vec<String>,
        token: &str,
        grants: Vec<String>,
    ) -> Result<(), BotError> {
        crate::comms::bot::validate_name(name)?;
        if groups.is_empty() {
            return Err(BotError::new(
                ErrorCode::InvalidArguments,
                "client needs at least one group",
            ));
        }
        for g in &groups {
            crate::comms::bot::validate_group(g)?;
        }
        crate::comms::bot::validate_token(token)?;
        if self.clients.contains_key(name) {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "client already registered",
            ));
        }
        for &id in sessions.order() {
            let live =
                sessions.get(id).is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "name is taken by a live session",
                ));
            }
        }
        self.clients.insert(
            name.to_string(),
            BotClient::new(name, token, groups, grants),
        );
        Ok(())
    }

    /// Operator file-bound registration: validates name and grants
    /// like [`Self::register_client`] but takes no inline secret — the
    /// credential arrives exclusively through the bound token file
    /// (empty never authenticates, so an unread file fails closed).
    pub fn register_client_file(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        groups: Vec<String>,
        grants: Vec<String>,
        token_file: std::path::PathBuf,
    ) -> Result<(), BotError> {
        crate::comms::bot::validate_name(name)?;
        if groups.is_empty() {
            return Err(BotError::new(
                ErrorCode::InvalidArguments,
                "client needs at least one group",
            ));
        }
        for g in &groups {
            crate::comms::bot::validate_group(g)?;
        }
        if self.clients.contains_key(name) {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "client already registered",
            ));
        }
        for &id in sessions.order() {
            let live =
                sessions.get(id).is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "name is taken by a live session",
                ));
            }
        }
        let mut client = BotClient::new(name, "", groups, grants);
        client.set_token_file(token_file);
        client.refresh_token();
        self.clients.insert(name.to_string(), client);
        Ok(())
    }

    /// Revoke one client: drops the inbox and fails its open
    /// conversations, telling session peers loudly through their panes.
    /// The credential stops working on the very next call.
    pub fn revoke_client(&mut self, name: &str) -> bool {
        if self.clients.remove(name).is_none() {
            return false;
        }
        let mut notify = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.client != name || conv.state != BotConvState::Open {
                continue;
            }
            conv.state = BotConvState::Failed;
            notify.push((conv.session, conv_id.clone()));
        }
        for (session, conv_id) in notify {
            self.push(
                session,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Failed,
                    from: name.to_string(),
                    text: "client revoked".to_string(),
                },
            );
        }
        true
    }

    /// Authenticate one bot call. Unknown names and wrong credentials
    /// share one `unauthorized` answer (no oracle); the stored secret is
    /// only ever constant-time compared, never logged or returned.
    /// Called on every bot call, so rotation and revocation bite at once.
    fn check_client(&self, name: &str, token: &str) -> Result<(), BotError> {
        const DUMMY: &str = "0123456789abcdef0123456789abcdef";
        match self.clients.get(name) {
            Some(c) if c.check_token(token) => Ok(()),
            Some(_) => Err(BotError::new(
                ErrorCode::Unauthorized,
                "unknown or revoked client",
            )),
            None => {
                let _ = crate::comms::bot::token_eq(DUMMY, token);
                Err(BotError::new(
                    ErrorCode::Unauthorized,
                    "unknown or revoked client",
                ))
            }
        }
    }

    /// Whether a session and a client grant list share a group.
    pub(super) fn shares_with_client(&self, id: SessionId, grants: &[String]) -> bool {
        self.membership
            .get(&id)
            .is_some_and(|mine| mine.iter().any(|g| grants.iter().any(|h| h == g)))
    }

    /// Open client-originated conversations: asks plus unacked tells.
    /// Mirrors pressure semantics at the client level (delivered tells
    /// no longer count).
    fn client_outstanding(&self, name: &str) -> usize {
        self.bot_convs
            .values()
            .filter(|c| {
                c.client == name
                    && c.from_client
                    && c.state == BotConvState::Open
                    && (c.kind == BotConvKind::Ask || !c.acked)
            })
            .count()
    }

    /// Execute one bot tool under an authenticated client identity.
    pub fn bot_call(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        // Bound token files re-read on every call: rotation and
        // deletion bite at once, with no grant cache anywhere.
        if let Some(client) = self.clients.get_mut(name) {
            client.refresh_token();
        }
        self.check_client(name, token)?;
        match tool {
            "list_sessions" => Ok(self.bot_list_sessions(sessions, name)),
            "ask_session" => self.bot_ask(sessions, name, args, now),
            "tell_session" => self.bot_tell(sessions, name, args, now),
            "send_response" => self.bot_respond(sessions, name, args, now),
            "ack_message" => self.bot_ack_msg(sessions, name, args, now),
            "bot_poll" => self.bot_poll(name, args),
            "bot_ack" => self.bot_ack_cursor(name, args),
            "start_session" => Err(BotError::new(
                ErrorCode::Unauthorized,
                "start_session is not enabled for external bots in this release",
            )),
            _ => Err(BotError::new(ErrorCode::NotFound, "unknown tool")),
        }
    }

    /// Bot-visible discovery: only sessions sharing a grant group,
    /// with epoch-scoped session IDs. Never the unscoped agent list.
    fn bot_list_sessions(&self, sessions: &SessionManager, client: &str) -> String {
        let grants = self
            .clients
            .get(client)
            .map(|c| c.groups.clone())
            .unwrap_or_default();
        let mut out = format!(
            r#"{{"you":{},"epoch":{},"sessions":["#,
            crate::ipc::mcp::escape_json(client),
            self.epoch,
        );
        let mut first = true;
        for &id in sessions.order() {
            let Some(rec) = sessions.get(id) else {
                continue;
            };
            if !rec.state.is_live() {
                continue;
            }
            if !self.shares_with_client(id, &grants) {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let activity = Self::activity_label(rec.activity);
            let group = self
                .primary_group(id)
                .map(crate::ipc::mcp::escape_json)
                .unwrap_or_else(|| "null".to_string());
            out.push_str(&format!(
                r#"{{"id":"{id}","name":{},"activity":"{activity}","group":{group}}}"#,
                crate::ipc::mcp::escape_json(&rec.name),
            ));
        }
        out.push_str("]}");
        out
    }

    fn bot_ask(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let target_name = Self::arg(args, "target")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "ask needs a target"))?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "ask needs text"))?;
        let key = Self::arg(args, "idempotency_key").filter(|s| !s.is_empty());
        let fp = crate::comms::bot::fingerprint("ask_session", &[&target_name, &text, ""]);
        if let Some(replay) = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_check(key.as_deref(), fp, now)?
        {
            return Ok(replay);
        }
        let target = self.resolve_target(sessions, &target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(target, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        if self.client_outstanding(client) >= crate::comms::bot::CLIENT_MAX_OUTSTANDING {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                "client send cap reached",
            ));
        }
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Ask,
                from: client.to_string(),
                text,
            },
        );
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind: BotConvKind::Ask,
                session: target,
                session_name: target_name_live,
                client: client.to_string(),
                from_client: true,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
            },
        );
        let result = format!(r#"{{"conversation":"{conv}","epoch":{}}}"#, self.epoch);
        self.clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_store(key.as_deref(), fp, &result, now);
        Ok(result)
    }

    fn bot_tell(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let target_name = Self::arg(args, "target")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "tell needs a target"))?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "tell needs text"))?;
        let key = Self::arg(args, "idempotency_key").filter(|s| !s.is_empty());
        if let Some(id) = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
        {
            let fp = crate::comms::bot::fingerprint("tell_session", &[&target_name, &text, &id]);
            if let Some(replay) = self
                .clients
                .get_mut(client)
                .expect("caller authenticated")
                .idem_check(key.as_deref(), fp, now)?
            {
                return Ok(replay);
            }
            let result = self.bot_tell_followup(sessions, client, &id, &target_name, &text, now)?;
            self.clients
                .get_mut(client)
                .expect("caller authenticated")
                .idem_store(key.as_deref(), fp, &result, now);
            return Ok(result);
        }
        let fp = crate::comms::bot::fingerprint("tell_session", &[&target_name, &text, ""]);
        if let Some(replay) = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_check(key.as_deref(), fp, now)?
        {
            return Ok(replay);
        }
        let target = self.resolve_target(sessions, &target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(target, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        if self.client_outstanding(client) >= crate::comms::bot::CLIENT_MAX_OUTSTANDING {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                "client send cap reached",
            ));
        }
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Tell,
                from: client.to_string(),
                text,
            },
        );
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind: BotConvKind::Tell,
                session: target,
                session_name: target_name_live,
                client: client.to_string(),
                from_client: true,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
            },
        );
        let result = format!(r#"{{"conversation":"{conv}","epoch":{}}}"#, self.epoch);
        self.clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_store(key.as_deref(), fp, &result, now);
        Ok(result)
    }

    /// Client follow-up on its own tell: the update reaches the session
    /// pane. Conversations owned by another client read as unknown.
    fn bot_tell_followup(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        id: &str,
        target_name: &str,
        text: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let (kind, state, session) = match self.bot_convs.get(id) {
            Some(c) if c.client == client => (c.kind, c.state, c.session),
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Tell {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only a tell takes follow-ups",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        let live = self.resolve_target(sessions, target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        if live != session {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "follow-up target mismatch",
            ));
        }
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(session, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, session) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.to_string(),
                kind: InjectKind::FollowUp,
                from: client.to_string(),
                text: text.to_string(),
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(id) {
            // The client owes the update exactly when the session told
            // it (session-originated tell): then this follow-up
            // satisfies courtesy for good.
            if !conv.from_client {
                conv.target_updated = true;
            }
            conv.last_update = now;
        }
        Ok(format!(
            r#"{{"conversation":"{id}","followup":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_respond(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let id = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "a conversation ID is required")
            })?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "send_response needs text")
            })?;
        let (kind, state, session, from_client) = match self.bot_convs.get(&id) {
            Some(c) if c.client == client => (c.kind, c.state, c.session, c.from_client),
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Ask {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only an ask takes a response",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        if from_client {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only the target answers",
            ));
        }
        // Group grants are live: a session or client removed from the
        // shared group since the ask loses the answer channel too.
        let granted = self
            .clients
            .get(client)
            .is_some_and(|c| self.shares_with_client(session, &c.groups));
        if !granted {
            return Err(BotError::new(
                ErrorCode::NoSharedGroup,
                "no shared group with target",
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Response,
                from: client.to_string(),
                text,
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(&id) {
            conv.state = BotConvState::Done;
            conv.last_update = now;
        }
        let _ = sessions;
        Ok(format!(
            r#"{{"conversation":"{id}","completed":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_ack_msg(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let id = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "a conversation ID is required")
            })?;
        let (kind, state, session, from_client, acked) = match self.bot_convs.get(&id) {
            Some(c) if c.client == client => {
                (c.kind, c.state, c.session, c.from_client, c.acked)
            }
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Tell {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "nothing to acknowledge",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        if from_client {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only the target acknowledges",
            ));
        }
        // Group grants are live: leaving the shared group since the
        // tell revokes the ack channel too.
        let granted = self
            .clients
            .get(client)
            .is_some_and(|c| self.shares_with_client(session, &c.groups));
        if !granted {
            return Err(BotError::new(
                ErrorCode::NoSharedGroup,
                "no shared group with target",
            ));
        }
        if acked {
            // Idempotent replay, same shape as the session path: no
            // duplicate Ack, no courtesy-clock nudge.
            return Ok(format!(
                r#"{{"conversation":"{id}","acknowledged":true,"epoch":{}}}"#,
                self.epoch
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Ack,
                from: client.to_string(),
                text: "ack_message".to_string(),
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(&id) {
            conv.acked = true;
            conv.last_update = now;
        }
        let _ = sessions;
        Ok(format!(
            r#"{{"conversation":"{id}","acknowledged":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_poll(&mut self, client: &str, args: &str) -> Result<String, BotError> {
        self.check_epoch(args)?;
        // Event IDs restart at 1 every epoch, so a resumed nonzero
        // cursor that omits the epoch would echo itself as
        // next_cursor forever while fresh events go unseen.
        let cursor_arg = Self::arg_u64(args, "cursor");
        if let Some(Ok(c)) = cursor_arg {
            if c != 0 && Self::arg_u64(args, "epoch").is_none() {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "nonzero cursor requires the epoch",
                ));
            }
        }
        let limit = match Self::arg_u64(args, "limit") {
            None => crate::comms::bot::POLL_MAX_EVENTS,
            Some(Ok(n)) => usize::try_from(n)
                .unwrap_or(crate::comms::bot::POLL_MAX_EVENTS)
                .clamp(1, crate::comms::bot::POLL_MAX_EVENTS),
            Some(Err(e)) => return Err(e),
        };
        let acked = self
            .clients
            .get(client)
            .expect("caller authenticated")
            .acked_cursor();
        let start = match cursor_arg {
            None => acked,
            Some(Ok(c)) => c.max(acked),
            Some(Err(e)) => return Err(e),
        };
        // A cursor past everything produced names no event in this
        // epoch (stale pre-restart cursor, or client bug): echoing it
        // back would skip the whole inbox, so conflict instead.
        let produced = self
            .clients
            .get(client)
            .expect("caller authenticated")
            .produced_upto();
        if start > produced {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "cursor is ahead of produced events; poll without a cursor to resume",
            ));
        }
        let events = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .poll(start, limit);
        let next = events.last().map(|e| e.id).unwrap_or(start);
        let mut out = String::from("{\"events\":[");
        for (i, e) in events.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&e.to_json());
        }
        out.push_str(&format!(r#"],"next_cursor":{next},"epoch":{}}}"#, self.epoch));
        Ok(out)
    }

    fn bot_ack_cursor(&mut self, client: &str, args: &str) -> Result<String, BotError> {
        self.check_epoch(args)?;
        // Same restart rule as polls: event IDs restart at 1 every
        // epoch, so a resumed nonzero ack without the epoch could
        // coincide with the fresh received prefix and delete new
        // events. Zero (which advances nothing) needs no epoch.
        if let Some(Ok(c)) = Self::arg_u64(args, "cursor") {
            if c != 0 && Self::arg_u64(args, "epoch").is_none() {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "nonzero cursor requires the epoch",
                ));
            }
        }
        let cursor = match Self::arg_u64(args, "cursor") {
            Some(Ok(c)) => c,
            _ => {
                return Err(BotError::new(
                    ErrorCode::InvalidArguments,
                    "bot_ack needs a cursor",
                ));
            }
        };
        let acked = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .ack(cursor)?;
        Ok(format!(
            r#"{{"acknowledged":{acked},"epoch":{}}}"#,
            self.epoch
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

        #[test]
        fn bot_registration_validates_and_revocation_bites_at_once() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            assert!(p
                .state
                .broker
                .register_client(&p.state.manager, "skippy", vec!["peers".into()], BOT_TOKEN, vec![])
                .is_err(), "duplicate registration conflicts");
            assert!(p
                .state
                .broker
                .register_client(&p.state.manager, "a", vec!["peers".into()], BOT_TOKEN, vec![])
                .is_err(), "live session name collision conflicts");
            assert!(p
                .state
                .broker
                .register_client(&p.state.manager, "tiny", vec!["peers".into()], "short", vec![])
                .is_err(), "short credential refused");
            assert!(p
                .state
                .broker
                .register_client(&p.state.manager, "nogroups", vec![], BOT_TOKEN, vec![])
                .is_err(), "group grant required");
            // Unknown names and wrong credentials share one answer: no oracle.
            assert_eq!(
                p.bcall("ghost", "list_sessions", "{}").unwrap_err().code,
                ErrorCode::Unauthorized
            );
            assert_eq!(
                p.bcall_as("skippy", &"0".repeat(32), "list_sessions", "{}")
                    .unwrap_err()
                    .code,
                ErrorCode::Unauthorized
            );
            assert!(p.state.broker.revoke_client("skippy"));
            assert!(!p.state.broker.revoke_client("skippy"));
            assert_eq!(
                p.bcall("skippy", "list_sessions", "{}").unwrap_err().code,
                ErrorCode::Unauthorized
            );
        }

        #[test]
        fn bot_list_is_scoped_to_shared_groups() {
            let mut p = live_pair();
            p.state.broker.join(&p.state.manager, p.a, "peers").unwrap();
            p.state.broker.join(&p.state.manager, p.b, "elsewhere").unwrap();
            p.register_bot("skippy", vec!["peers"]);
            let list = p.bcall("skippy", "list_sessions", "{}").expect("scoped list validates");
            assert!(list.contains(r#""you":"skippy""#), "list: {list}");
            assert!(list.contains("\"epoch\":"), "list: {list}");
            assert!(list.contains(r#""name":"a""#), "list: {list}");
            assert!(!list.contains(r#""name":"b""#), "list: {list}");
            assert!(list.contains(r#""id":"s"#), "list: {list}");
        }

        #[test]
        fn bot_retry_with_same_key_replays_and_conflicts() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let args = r#"{"target":"b","message":"ready?","idempotency_key":"k-1"}"#;
            let first = p.bcall("skippy", "ask_session", args).expect("ask validates");
            let again = p.bcall("skippy", "ask_session", args).expect("retry replays");
            assert_eq!(first, again);
            assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "asked once");
            let err = p
                .bcall("skippy", "ask_session", r#"{"target":"b","message":"other","idempotency_key":"k-1"}"#)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Conflict);
            let bad = p
                .bcall("skippy", "ask_session", r#"{"target":"b","message":"x","idempotency_key":"has space"}"#)
                .unwrap_err();
            assert_eq!(bad.code, ErrorCode::InvalidArguments);
        }

        #[test]
        fn bot_malformed_arguments_are_invalid_not_conflicts() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            for (tool, args) in [
                ("ask_session", r#"{"message":"x"}"#),
                ("ask_session", r#"{"target":"b"}"#),
                ("tell_session", r#"{"target":"b"}"#),
                ("send_response", r#"{"message":"x"}"#),
                ("bot_ack", "{}"),
                ("bot_poll", r#"{"epoch":"yesterday"}"#),
                ("bot_poll", r#"{"cursor":"soon"}"#),
                ("bot_poll", r#"{"limit":"plenty"}"#),
            ] {
                assert_eq!(
                    p.bcall("skippy", tool, args).unwrap_err().code,
                    ErrorCode::InvalidArguments,
                    "{tool} {args}"
                );
            }
        }

        #[test]
        fn bot_answers_to_unknown_and_closed_conversations_fail_typed() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            assert_eq!(
                p.bcall("skippy", "send_response", r#"{"conversation_id":"nope","message":"x"}"#)
                    .unwrap_err()
                    .code,
                ErrorCode::NotFound
            );
            let res = p
                .bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                .unwrap();
            let conv = json_field(&res, "conversation").unwrap();
            p.state.broker.take_due(p.b, 10);
            p.call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
            )
            .unwrap();
            assert_eq!(
                p.bcall(
                    "skippy",
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"late"}}"#),
                )
                .unwrap_err()
                .code,
                ErrorCode::ConversationClosed
            );
        }

        #[test]
        fn bot_sends_fail_typed_on_routing_and_pressure() {
            use crate::comms::bot::ErrorCode;
            // Ambiguous names fail rather than guess.
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let run_c = crate::infra::ids::RunId::generate();
            let c = p
                .state
                .manager
                .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_c, "shell")
                .unwrap();
            p.state.broker.join(&p.state.manager, c, "peers").unwrap();
            assert_eq!(
                p.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                    .unwrap_err()
                    .code,
                ErrorCode::AmbiguousTarget
            );
            assert!(p.state.manager.remove(c));
            // No shared group blocks transfer.
            let mut q = live_pair();
            q.register_bot("skippy", vec!["peers"]);
            assert_eq!(
                q.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                    .unwrap_err()
                    .code,
                ErrorCode::NoSharedGroup
            );
            // A full target queue rejects with pressure, not silence.
            let mut r = live_pair().grouped();
            r.register_bot("skippy", vec!["peers"]);
            for i in 0..5 {
                r.call(
                    &r.run_a.clone(),
                    "ask_session",
                    &format!(r#"{{"target":"b","message":"q{i}"}}"#),
                )
                .expect("fills pressure");
            }
            assert_eq!(
                r.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                    .unwrap_err()
                    .code,
                ErrorCode::PressureLimit
            );
        }

        #[test]
        fn bot_target_exit_arrives_as_a_failed_event() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let res = p
                .bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                .expect("client asks session");
            let conv = json_field(&res, "conversation").expect("conversation id");
            p.state.broker.target_exited(&p.state.manager, p.b);
            let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
            assert!(poll.contains(&conv), "poll: {poll}");
            assert!(poll.contains(r#""kind":"failed""#), "poll: {poll}");
            // Late answers to the failed conversation close deterministically.
            assert_eq!(
                p.bcall(
                    "skippy",
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"late"}}"#),
                )
                .unwrap_err()
                .code,
                ErrorCode::ConversationClosed
            );
        }

        #[test]
        fn bot_poll_with_a_stale_epoch_conflicts() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let epoch = p.state.broker.epoch();
            p.bcall("skippy", "bot_poll", "{}").expect("current epoch polls");
            let stale = epoch.wrapping_add(1);
            let err = p
                .bcall("skippy", "bot_poll", &format!(r#"{{"epoch":{stale}}}"#))
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Conflict);
        }

        #[test]
        fn bot_file_registration_authenticates_through_the_file() {
            let mut p = live_pair().grouped();
            let dir = std::env::temp_dir().join(format!("forge-bot-reg-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("skippy.token");
            crate::comms::bot::write_test_token(&path, BOT_TOKEN);
            p.state
                .broker
                .register_client_file(
                    &p.state.manager,
                    "skippy",
                    vec!["peers".to_string()],
                    Vec::new(),
                    path.clone(),
                )
                .expect("file registration validates");
            // The bound file supplies the credential on every call.
            let list = p.bcall("skippy", "list_sessions", "{}").expect("file credential works");
            assert!(list.contains(r#""you":"skippy""#), "list: {list}");
            // Deleting the file revokes immediately.
            std::fs::remove_file(&path).unwrap();
            assert_eq!(
                p.bcall("skippy", "list_sessions", "{}").unwrap_err().code,
                crate::comms::bot::ErrorCode::Unauthorized
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn stale_cursor_without_epoch_conflicts() {
            // After a restart event IDs begin at 1 again, so a resumed
            // nonzero cursor without the epoch would echo itself as
            // next_cursor forever while new events 1..N go unseen.
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let err = p
                .bcall("skippy", "bot_poll", r#"{"cursor":500}"#)
                .expect_err("nonzero cursor needs the epoch");
            assert_eq!(err.code, ErrorCode::Conflict);
        }

        #[test]
        fn cursor_ahead_of_produced_events_conflicts() {
            // A cursor past everything produced is a stale pre-restart
            // cursor with a fresh epoch (or a client bug): echoing it
            // back as next_cursor would skip the whole inbox.
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let first = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
            let epoch = crate::ipc::mcp::top_raw(&first, "epoch").expect("epoch echoed");
            let err = p
                .bcall(
                    "skippy",
                    "bot_poll",
                    &format!(r#"{{"cursor":500,"epoch":{epoch}}}"#),
                )
                .expect_err("ahead cursor conflicts");
            assert_eq!(err.code, ErrorCode::Conflict);
            // An up-to-date cursor still polls fine.
            assert!(p
                .bcall("skippy", "bot_poll", &format!(r#"{{"epoch":{epoch}}}"#))
                .expect("current poll validates")
                .contains("\"events\":[]"));
        }

        #[test]
        fn bot_ack_cursor_retry_replays() {
            // The server committed the ack; a lost verdict retried with
            // the exact cursor replays instead of conflicting.
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
            let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
            let epoch = crate::ipc::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
            let ack = format!(r#"{{"cursor":1,"epoch":{epoch}}}"#);
            let first = p.bcall("skippy", "bot_ack", &ack).expect("ack validates");
            assert!(first.contains(r#""acknowledged":1"#), "first: {first}");
            let retry = p
                .bcall("skippy", "bot_ack", &ack)
                .expect("exact retry replays");
            assert!(retry.contains(r#""acknowledged":1"#), "retry: {retry}");
        }

        #[test]
        fn bot_repeat_ack_replays_without_dup() {
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
            let ack = format!(r#"{{"conversation_id":"{conv}"}}"#);
            p.bcall("skippy", "ack_message", &ack)
                .expect("ack validates");
            assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
            p.bcall("skippy", "ack_message", &ack)
                .expect("retry still acknowledges");
            assert_eq!(
                p.state.broker.take_due(p.a, 10).len(),
                0,
                "no duplicate Ack queued"
            );
        }

        #[test]
        fn keyed_retry_replays_the_first_verdict() {
            // A harness that times out and retries with the same key must
            // get the original conversation back, not a duplicate. Keyless
            // calls still execute every time.
            let mut p = live_pair().grouped();
            let args = r#"{"target":"b","message":"q","idempotency_key":"k-1"}"#;
            let first = p.call(&p.run_a.clone(), "ask_session", args).expect("ask validates");
            let replay = p.call(&p.run_a.clone(), "ask_session", args).expect("retry validates");
            assert_eq!(replay, first, "retry replays instead of duplicating");
            let third = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
                .expect("keyless ask validates");
            assert_ne!(third, first, "keyless calls still execute");
        }

        #[test]
        fn keyed_retry_with_changed_args_conflicts() {
            let mut p = live_pair().grouped();
            p.call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"b","message":"q","idempotency_key":"k-2"}"#,
            )
            .expect("ask validates");
            let err = p
                .call(
                    &p.run_a.clone(),
                    "ask_session",
                    r#"{"target":"b","message":"CHANGED","idempotency_key":"k-2"}"#,
                )
                .expect_err("same key, different send conflicts");
            assert!(err.contains("different arguments"), "err: {err}");
        }

        #[test]
        fn idempotency_capacity_is_per_caller() {
            // One noisy session flooding past the cache cap must not
            // evict another caller's still-live retry record.
            use crate::comms::bot::IDEM_MAX_KEYS;
            let mut p = live_pair().grouped();
            let args = r#"{"target":"a","message":"q","idempotency_key":"bk"}"#;
            let first = p
                .call(&p.run_b.clone(), "ask_session", args)
                .expect("ask validates");
            let now = std::time::Instant::now();
            for i in 0..(IDEM_MAX_KEYS + 10) {
                p.state
                    .broker
                    .session_idem
                    .entry(p.a.to_string())
                    .or_insert_with(crate::comms::bot::IdemCache::new)
                    .store(&format!("k-{i}"), i as u64, "x", now);
            }
            let replay = p
                .call(&p.run_b.clone(), "ask_session", args)
                .expect("retry validates");
            assert_eq!(replay, first, "B's record survives A's flood");
        }

        #[test]
        fn idempotency_records_die_with_their_caller() {
            let mut p = live_pair().grouped();
            let args = r#"{"target":"a","message":"q","idempotency_key":"bk"}"#;
            p.call(&p.run_b.clone(), "ask_session", args)
                .expect("ask validates");
            assert!(p.state.broker.session_idem.len() >= 1, "record exists");
            p.state.broker.target_exited(&p.state.manager, p.b);
            assert_eq!(
                p.state.broker.session_idem.len(),
                0,
                "dead callers keep no retry records"
            );
        }
}
