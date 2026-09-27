//! Broker tool dispatcher: caller/target resolution, arg helpers, and `call`.

use super::*;

impl Broker {
    /// Resolve the caller by current run ID: bound, live, and matching the
    /// record (a rebound ID never resolves to its previous holder).
    fn resolve_caller(
        &self,
        sessions: &SessionManager,
        caller_run: &str,
    ) -> Result<SessionId, String> {
        let id = sessions
            .lookup_run(caller_run)
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        let rec = sessions
            .get(id)
            .filter(|rec| rec.run_id.as_str() == caller_run && rec.state.is_live())
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        Ok(rec.id)
    }

    /// Resolve an exact live target by name. Ambiguous names fail rather
    /// than guess: names route for humans, run IDs confer authority.
    pub(super) fn resolve_target(
        &self,
        sessions: &SessionManager,
        name: &str,
    ) -> Result<SessionId, String> {
        let mut found = None;
        let mut ambiguous = false;
        for &id in sessions.order() {
            let live = sessions
                .get(id)
                .is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                if found.is_some() {
                    ambiguous = true;
                }
                found = Some(id);
            }
        }
        if ambiguous {
            return Err(format!("ambiguous session name {name:?}"));
        }
        found.ok_or_else(|| format!("no live session named {name:?}"))
    }

    pub(super) fn names(&self, sessions: &SessionManager, id: SessionId) -> String {
        sessions
            .get(id)
            .map(|rec| rec.name.clone())
            .unwrap_or_default()
    }

    /// Human activity label for list output (sessions and bots share it).
    /// Also feeds the delivery-hold trace, so a stuck message names the
    /// pane state that holds it.
    pub(crate) fn activity_label(activity: crate::session::Activity) -> &'static str {
        match activity {
            crate::session::Activity::Idle => "Idle",
            crate::session::Activity::Thinking => "Thinking",
            crate::session::Activity::ToolUse => "ToolUse",
            crate::session::Activity::Waiting => "Waiting",
            crate::session::Activity::Stopped => "Stopped",
        }
    }

    /// Parse an optional u64 tool arg: bare JSON numbers or quoted
    /// digits. Absent reads as missing; present-but-garbled is a
    /// client bug, never silent.
    pub(super) fn arg_u64(args: &str, name: &str) -> Option<Result<u64, BotError>> {
        let raw = crate::ipc::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        Some(bare.parse::<u64>().map_err(|_| {
            BotError::new(
                ErrorCode::InvalidArguments,
                format!("{name} must be a non-negative integer"),
            )
        }))
    }

    /// Reject calls presenting a stale epoch: cursors and references
    /// from a previous broker run must never read as current.
    pub(super) fn check_epoch(&self, args: &str) -> Result<(), BotError> {
        match Self::arg_u64(args, "epoch") {
            None => Ok(()),
            Some(Ok(e)) if e == self.epoch => Ok(()),
            Some(Ok(_)) => Err(BotError::new(
                ErrorCode::Conflict,
                "epoch changed; re-list and resume",
            )),
            Some(Err(e)) => Err(e),
        }
    }

    /// Execute one comms tool. Returns the JSON `result` fragment; every
    /// failure is a harness-visible string, never a panic.
    pub fn call(
        &mut self,
        sessions: &SessionManager,
        caller_run: &str,
        tool: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let caller = self.resolve_caller(sessions, caller_run)?;
        if tool == "list_sessions" {
            return Ok(self.list_sessions(sessions, caller));
        }
        // Keyed sends fingerprint the tool plus the caller's exact
        // argument bytes, so a byte-identical retry replays while any
        // changed send under the same key conflicts loudly instead
        // of silently duplicating or, worse, returning a wrong
        // conversation. Broker rejections are never stored.
        let caller_s = caller.to_string();
        let keyed = match Self::arg(args, "idempotency_key").filter(|s| !s.is_empty()) {
            None => None,
            Some(key) => {
                crate::comms::bot::validate_key(&key).map_err(|e| e.message)?;
                let fp = crate::comms::bot::fingerprint(tool, &[&caller_s, args]);
                let cached = self
                    .session_idem
                    .entry(caller_s.clone())
                    .or_insert_with(IdemCache::new)
                    .check(&key, fp, now);
                match cached {
                    IdemCheck::Hit(result) => return Ok(result),
                    IdemCheck::Miss => {}
                    IdemCheck::Conflict => {
                        return Err(
                            "idempotency key reused with different arguments".to_string()
                        );
                    }
                }
                Some((key, fp))
            }
        };
        // New work gates on the caller's own backlog: completions
        // bypass deliberately, so without this a session that never
        // drains could pile responses behind itself without limit.
        // Retries replay above, so only genuinely new sends wait.
        if (tool == "ask_session" || tool == "tell_session")
            && self.queued(caller) >= QUEUE_CAP
        {
            return Err("caller queue full; drain it before sending".to_string());
        }
        let result = match tool {
            "ask_session" => self.ask(sessions, caller, args, now),
            "send_response" => self.send_response(sessions, caller, args, now),
            "tell_session" => self.tell(sessions, caller, args, now),
            "ack_message" => self.ack(sessions, caller, args, now),
            "compact_session" => self.compact(sessions, caller, args),
            "schedule_prompt" => self.schedule(sessions, caller, args, now),
            "cancel_scheduled_prompt" => self.cancel_scheduled(caller, args),
            _ => Err("unknown tool".to_string()),
        };
        if let (Some((key, fp)), Ok(line)) = (keyed, &result) {
            if let Some(cache) = self.session_idem.get_mut(&caller_s) {
                cache.store(&key, fp, line, now);
            }
        }
        result
    }

    pub(super) fn arg(args: &str, name: &str) -> Option<String> {
        crate::hooks::policy::json_string_field(args.as_bytes(), &[name])
    }

    /// Canonical blueprint names first (`message`, `conversation_id`), with
    /// the Phase 4a short forms accepted as aliases.
    pub(super) fn arg2(args: &str, names: &[&str]) -> Option<String> {
        crate::hooks::policy::json_string_field(args.as_bytes(), names)
    }

    /// Raw numeric arg: JSON numbers arrive bare, quoted ones read
    /// liberally too. Non-finite values never pass.
    pub(super) fn arg_num(args: &str, name: &str) -> Option<f64> {
        let raw = crate::ipc::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        bare.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    pub(super) fn arg_bool(args: &str, name: &str) -> Option<bool> {
        match crate::ipc::mcp::top_raw(args, name)?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// The `you` field names the calling session first, so a session
    /// reading the list can spot its own entry without guessing.
    fn list_sessions(&self, sessions: &SessionManager, caller: SessionId) -> String {
        let mut out = format!(
            r#"{{"you":{},"sessions":["#,
            crate::ipc::mcp::escape_json(&self.names(sessions, caller)),
        );
        let mut first = true;
        for &id in sessions.order() {
            let Some(rec) = sessions.get(id) else {
                continue;
            };
            if !rec.state.is_live() {
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
                r#"{{"name":{},"live":true,"activity":"{activity}","group":{group}}}"#,
                crate::ipc::mcp::escape_json(&rec.name),
            ));
        }
        // Bot peers sharing a group with the caller ride along, marked,
        // so sessions can address the clients they may ask or tell.
        // Nothing is listed when no clients exist: output is unchanged.
        let mut bots: Vec<String> = self
            .clients
            .values()
            .filter(|c| self.shares_with_client(caller, &c.groups))
            .map(|c| c.name.clone())
            .collect();
        bots.sort();
        for name in bots {
            let group = self
                .membership
                .get(&caller)
                .and_then(|mine| {
                    self.clients.get(&name).and_then(|c| {
                        mine.iter().find(|g| c.groups.contains(g)).map(|g| {
                            crate::ipc::mcp::escape_json(g)
                        })
                    })
                })
                .unwrap_or_else(|| "null".to_string());
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                r#"{{"name":{},"live":true,"activity":"Idle","group":{group},"bot":true}}"#,
                crate::ipc::mcp::escape_json(&name),
            ));
        }
        out.push_str("]}");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

    #[test]
    fn forged_run_id_is_rejected() {
        let mut p = live_pair().grouped();
        let err = p
            .call(
                &"f".repeat(32),
                "ask_session",
                r#"{"target":"b","message":"hi"}"#,
            )
            .expect_err("forged run ID must fail");
        assert!(err.contains("run ID"), "err: {err}");
    }

    #[test]
    fn no_shared_group_blocks_transfer() {
        let mut p = live_pair();
        // a joins alone; b is groupless.
        p.state.broker.join(&p.state.manager, p.a, "solo").unwrap();
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"hi"}"#)
            .expect_err("no shared group must fail");
        assert!(err.contains("shared group"), "err: {err}");
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn legacy_short_params_still_accepted() {
        let mut p = live_pair().grouped();
        // Phase 4a short forms are aliases for the canonical blueprint names.
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("canonical message validates");
        let conv = json_field(&res, "conversation").unwrap();
        p.state.broker.take_due(p.b, 10);
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
        )
        .expect("canonical conversation_id validates");
        // ...and the old shorts keep working.
        p.call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"old"}"#)
            .expect("legacy text alias validates");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "old");
    }

    #[test]
    fn ask_returns_an_id_and_queues_target_injection() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        assert!(res.contains(r#""conversation":""#), "res: {res}");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "ready?");
        assert!(matches!(due[0].kind, InjectKind::Ask));
    }

    #[test]
    fn list_sessions_reports_live_peers() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "list_sessions", "{}")
            .expect("listing works");
        assert!(res.contains(r#""name":"a""#), "res: {res}");
        assert!(res.contains(r#""name":"b""#), "res: {res}");
        assert!(res.contains(r#""live":true"#), "res: {res}");
        // The caller sees its own name up front in `you`.
        assert!(res.starts_with(r#"{"you":"a","#), "res: {res}");
        let res_b = p
            .call(&p.run_b.clone(), "list_sessions", "{}")
            .expect("listing works for b too");
        assert!(res_b.starts_with(r#"{"you":"b","#), "res: {res_b}");
    }

    #[test]
    fn session_list_shows_shared_bots_marked() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        p.register_bot("spy", vec!["elsewhere"]);
        let list = p
            .call(&p.run_a.clone(), "list_sessions", "{}")
            .expect("list validates");
        assert!(list.contains(r#""name":"skippy""#), "list: {list}");
        assert!(list.contains(r#""bot":true"#), "list: {list}");
        assert!(!list.contains("spy"), "list: {list}");
    }

    #[test]
    fn bot_control_tools_are_denied_and_unknown_is_not_found() {
        use crate::comms::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        assert_eq!(
            p.bcall("skippy", "start_session", r#"{"harness":"codex"}"#)
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            p.bcall("skippy", "frobnicate", "{}").unwrap_err().code,
            ErrorCode::NotFound
        );
    }
}
