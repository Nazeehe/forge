//! AppState MCP tool dispatch: arg helpers and per-tool handlers.

use super::*;

impl AppState {
    /// One string tool arg: JSON escapes decoded (agents must escape
    /// newlines, so multi-line steps and Markdown answers arrive
    /// intact), blanked to missing.
    pub(super) fn tool_arg(args: &str, name: &str) -> Option<String> {
        crate::hooks::policy::json_string_field(args.as_bytes(), &[name])
            .map(|s| crate::ipc::mcp::decode_json_string(&s))
            .filter(|s| !s.is_empty())
    }

    /// One boolean tool arg from a bare JSON literal.
    fn tool_bool(args: &str, name: &str) -> Option<bool> {
        match crate::ipc::mcp::top_raw(args, name)?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// One integer tool arg: bare JSON numbers, quoted ones read
    /// liberally too. Fractions, negatives, and overflow never pass.
    pub(super) fn tool_u32(args: &str, name: &str) -> Option<u32> {
        let raw = crate::ipc::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        let value = bare.parse::<f64>().ok()?;
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > u32::MAX as f64 {
            return None;
        }
        Some(value as u32)
    }

    /// Resolve a tool caller to its live session, rebound IDs included:
    /// the run must still belong to the record that holds it.
    pub(super) fn resolve_tool_caller(&self, run_id: &str) -> Result<crate::session::SessionId, String> {
        let id = self
            .manager
            .lookup_run(run_id)
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        self.manager
            .get(id)
            .filter(|rec| rec.run_id.as_str() == run_id)
            .map(|rec| rec.id)
            .ok_or_else(|| "unknown or stale run ID".to_string())
    }

    /// Execute one walkthrough MCP tool. `None` when the name is not a
    /// walkthrough tool and the broker should answer instead.
    pub(super) fn walkthrough_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "walkthrough_start" | "walkthrough_answer" | "walkthrough_end" | "walkthrough_add_step" | "walkthrough_update" => {}
            _ => return None,
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        match tool {
            "walkthrough_start" => Some(self.walkthrough_start(id, args)),
            "walkthrough_add_step" => Some(self.walkthrough_add_step(id, args)),
            "walkthrough_update" => Some(self.walkthrough_update(id, args)),
            "walkthrough_answer" => {
                let answer = match Self::tool_arg(args, "answer") {
                    Some(answer) => answer,
                    None => return Some(Err("walkthrough_answer needs an answer".to_string())),
                };
                let Some(wt) = self.walkthroughs.get_mut(&id) else {
                    return Some(Err("no walkthrough for this session".to_string()));
                };
                if wt.answer_latest(&answer) {
                    self.dirty = true;
                    Some(Ok(r#"{"answered":true}"#.to_string()))
                } else {
                    Some(Err("no walkthrough question waiting".to_string()))
                }
            }
            _ => {
                let summary = Self::tool_arg(args, "summary");
                let Some(wt) = self.walkthroughs.get_mut(&id) else {
                    return Some(Err("no walkthrough for this session".to_string()));
                };
                wt.end(summary);
                self.dirty = true;
                Some(Ok(r#"{"ended":true}"#.to_string()))
            }
        }
    }

    /// Visual-family tools: answering carries the caller's session
    /// (overlay state), screenshots capture desktop apps and need no
    /// session — the commit claim already authorized the run.
    /// `None` when the name is not a visual tool and the broker
    /// should answer instead.
    #[cfg(feature = "visual")]
    pub(super) fn visual_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        #[cfg(all(target_os = "linux", feature = "visual"))]
        if tool == "screenshot" {
            return Some(self.screenshot_tool(args));
        }
        if tool != "visual_answer" {
            return None;
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        let answer = match Self::tool_arg(args, "answer") {
            Some(answer) => answer,
            None => return Some(Err("visual_answer needs an answer".to_string())),
        };
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return Some(Err("no visual for this session".to_string()));
        };
        match slot.questions.iter_mut().rev().find(|q| q.answer.is_none()) {
            Some(q) => {
                q.answer = Some(answer);
                slot.chat_scroll = 0;
                self.dirty = true;
                Some(Ok(r#"{"answered":true}"#.to_string()))
            }
            None => Some(Err("no visual question waiting".to_string())),
        }
    }

    /// Answer stub when the feature is off: the tool name never
    /// advertises, so reaching here means a forged call.
    #[cfg(not(feature = "visual"))]
    pub(super) fn visual_tool(
        &mut self,
        _run_id: &str,
        _tool: &str,
        _args: &str,
    ) -> Option<Result<String, String>> {
        None
    }

    /// Capture one named desktop app window (Hyprland): match by
    /// class then title, refuse hidden windows without focus:true,
    /// fit the PNG to the raster budget. Failures are plain error
    /// strings so the harness always gets an answer.
    #[cfg(all(target_os = "linux", feature = "visual"))]
    fn screenshot_tool(&self, args: &str) -> Result<String, String> {
        let app = match Self::tool_arg(args, "app") {
            Some(app) => app,
            None => return Err("screenshot needs an app name".to_string()),
        };
        let focus = Self::tool_bool(args, "focus").unwrap_or(false);
        match crate::visual::screenshot::capture(&app, focus) {
            Ok(shot) => Ok(format!(
                "{{\"path\":{},\"app\":{},\"width\":{},\"height\":{}}}",
                crate::ipc::mcp::escape_json(&shot.path),
                crate::ipc::mcp::escape_json(&format!("{} — {}", shot.class, shot.title)),
                shot.width,
                shot.height,
            )),
            Err(e) => Err(e),
        }
    }

    /// Execute one session-lifecycle MCP tool. `None` when the name is
    /// not a session tool and the broker should answer instead.
    pub(super) fn session_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "start_session" | "set_session_status" | "clear_session_status" | "message_user" | "request_attention" => {}
            #[cfg(feature = "visual")]
            "visual_show" => {}
            _ => return None,
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        match tool {
            "start_session" => Some(self.start_session_tool(id, args)),
            "set_session_status" => Some(self.set_session_status(id, args)),
            "message_user" => Some(self.message_user_tool(id, args)),
            "request_attention" => Some(self.request_attention_tool(id, args)),
            #[cfg(feature = "visual")]
            "visual_show" => Some(self.visual_show_tool(id, args)),
            _ => Some(self.clear_session_status(id)),
        }
    }

    /// Accept the caller's diagram for background raster: caps run
    /// before anything spawns, the worker renders off the main loop,
    /// and the verdict carries the generation the completion will
    /// bear. Dimensions arrive with the frame (U4 paints it).
    #[cfg(feature = "visual")]
    fn visual_show_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let content = Self::tool_arg(args, "content")
            .ok_or_else(|| "visual_show needs content".to_string())?;
        let format = Self::tool_arg(args, "format").unwrap_or_else(|| "mermaid".to_string());
        crate::visual::check_request(&content, &format)?;
        self.visual_seq += 1;
        let generation = self.visual_seq;
        let title = Self::tool_arg(args, "title").unwrap_or_default();
        let alt = Self::tool_arg(args, "alt").unwrap_or_default();
        let tx = self.visual_tx.clone();
        std::thread::Builder::new()
            .name("visual-raster".to_string())
            .spawn(move || {
                let result = crate::visual::render_frame(&content);
                let _ = tx.send(VisualDone {
                    session: caller,
                    generation,
                    title,
                    alt,
                    result,
                });
            })
            .map_err(|e| format!("cannot spawn raster worker: {e}"))?;
        self.dirty = true;
        Ok(format!(
            r#"{{"accepted":true,"format":"mermaid","generation":{generation}}}"#
        ))
    }

    /// Create a local agent session: validated name/cwd/harness, an
    /// optional comm-group join (else the caller's primary group), and
    /// an optional opening prompt queued as the first idle injection.
    /// Remote flavors, internet sessions, and focus theft are refused:
    /// tool births never steal the human view.
    fn start_session_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        for (param, what) in [
            ("connection", "remote connections"),
            ("host", "remote hosts"),
            ("od_preset", "OD presets"),
            ("od_type", "OD session types"),
        ] {
            if Self::tool_arg(args, param).is_some() {
                return Err(format!("{what} are unsupported (local sessions only)"));
            }
        }
        if Self::tool_bool(args, "internet").unwrap_or(false) {
            return Err("internet sessions are unsupported (local sessions only)".to_string());
        }
        let harness_name = Self::tool_arg(args, "harness").unwrap_or_else(|| "claude".to_string());
        let harness = crate::session::harness::Harness::from_name(&harness_name)
            .ok_or_else(|| format!("unknown harness {harness_name:?} (claude/codex/muse)"))?;
        let cwd = match Self::tool_arg(args, "path") {
            Some(path) => {
                let cwd = std::path::PathBuf::from(&path);
                if !cwd.is_dir() {
                    return Err(format!("session path {path:?} is not a directory"));
                }
                cwd
            }
            None => self
                .manager
                .get(caller)
                .map(|rec| rec.cwd.clone())
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        };
        let name = match Self::tool_arg(args, "name") {
            Some(name) => {
                if self.live_names().iter().any(|taken| taken == &name) {
                    return Err(format!("session name {name:?} is taken"));
                }
                name
            }
            None => {
                let stem = harness.as_str();
                let mut n = self.manager.len() + 1;
                loop {
                    let candidate = format!("{stem}-{n}");
                    if !self.live_names().iter().any(|taken| taken == &candidate) {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        let group = Self::tool_arg(args, "communication_group")
            .or_else(|| self.broker.primary_group(caller).map(str::to_string));
        let previous = self.manager.active();
        let id = self
            .create_session(&crate::ui::dialogs::create::SessionSpec {
                kind: crate::ui::dialogs::create::SessionKind::Agent(harness),
                name: name.clone(),
                cwd,
                model: String::new(),
                group,
            })
            .map_err(|e| format!("cannot start session: {e}"))?;
        if let Some(prompt) = Self::tool_arg(args, "prompt") {
            let from = self
                .manager
                .get(caller)
                .map(|rec| rec.name.clone())
                .unwrap_or_default();
            self.broker.push(
                id,
                crate::comms::Injection {
                    conv: crate::infra::ids::ConversationId::generate().to_string(),
                    kind: crate::comms::InjectKind::Tell,
                    from,
                    text: prompt,
                },
            );
        }
        if let Some(active) = previous {
            if active != id {
                self.manager.switch(active);
            }
        }
        self.dirty = true;
        Ok(format!(
            r#"{{"session_id":"{id}","name":{}}}"#,
            crate::ipc::mcp::escape_json(&name),
        ))
    }

    /// Replace the caller's sticky status: closed kind set, message
    /// bounded with no controls or newlines.
    fn set_session_status(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let kind = Self::tool_arg(args, "kind")
            .ok_or_else(|| "set_session_status needs a kind".to_string())?;
        let kind = crate::session::status::StatusKind::from_name(&kind).ok_or_else(|| {
            "unknown status kind (info/progress/success/warning/blocked/question)".to_string()
        })?;
        let message = Self::tool_arg(args, "message")
            .ok_or_else(|| "set_session_status needs a message".to_string())?;
        let message = crate::session::status::validate(&message)?;
        let Some(rec) = self.manager.get_mut(caller) else {
            return Err("unknown or stale run ID".to_string());
        };
        rec.status = Some(crate::session::status::SessionStatus { kind, message: message.clone() });
        self.dirty = true;
        Ok(format!(
            r#"{{"kind":{},"message":{}}}"#,
            crate::ipc::mcp::escape_json(kind.as_str()),
            crate::ipc::mcp::escape_json(&message),
        ))
    }

    /// Clear the caller's sticky status; already-clear stays success.
    fn clear_session_status(&mut self, caller: crate::session::SessionId) -> Result<String, String> {
        let Some(rec) = self.manager.get_mut(caller) else {
            return Err("unknown or stale run ID".to_string());
        };
        rec.status = None;
        self.dirty = true;
        Ok(r#"{"status_cleared":true}"#.to_string())
    }

    /// Raise the caller's blocked-only attention flag: validated reason,
    /// one entry per session, plus Waiting activity so the router sorts
    /// it top. Re-raising replaces the reason; focusing clears it.
    fn request_attention_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let reason = Self::tool_arg(args, "reason")
            .ok_or_else(|| "request_attention needs a reason".to_string())?;
        let reason = crate::session::status::validate(&reason)?;
        if self.manager.get(caller).is_none() {
            return Err("unknown or stale run ID".to_string());
        }
        self.attention_flags
            .insert(caller, (reason, std::time::Instant::now()));
        self.manager
            .set_activity(caller, crate::session::Activity::Waiting);
        self.dirty = true;
        Ok(r#"{"attention_raised":true}"#.to_string())
    }

    /// Badge the operator in-app and optionally forward to Telegram.
    /// The badge always records; forwarding needs an enabled transport,
    /// a notify chat, and a readable token, and runs on a worker thread
    /// so slow I/O never stalls the loop. Retries with the same
    /// `idempotency_key` replay; a changed message under a reused key
    /// conflicts. Sends are capped per session (see
    /// [`crate::telegram::MESSAGE_USER_CAP`]).
    fn message_user_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let session_name = match self.manager.get(caller) {
            Some(rec) => rec.name.clone(),
            None => return Err("unknown or stale run ID".to_string()),
        };
        let message = Self::tool_arg(args, "message")
            .ok_or_else(|| "message_user needs a message".to_string())?;
        let now = std::time::Instant::now();
        let fingerprint = crate::comms::bot::fingerprint("message_user", &[&message]);
        let idem_key = Self::tool_arg(args, "idempotency_key").map(|k| format!("{caller:?}:{k}"));
        if let Some(ref key) = idem_key {
            match self.message_user_idem.check(key, fingerprint, now) {
                crate::comms::bot::IdemCheck::Hit(result) => return Ok(result),
                crate::comms::bot::IdemCheck::Conflict => {
                    return Err(
                        "idempotency_key conflict: same key, different message".to_string(),
                    )
                }
                crate::comms::bot::IdemCheck::Miss => {}
            }
        }
        let window = self.message_user_windows.entry(caller).or_default();
        if !crate::telegram::check_message_rate(window, now) {
            return Err("message_user rate limited: 3 per 60 seconds".to_string());
        }
        let text = crate::telegram::truncate_text(&message, crate::telegram::MAX_TEXT).to_string();
        self.message_user_badges.insert(caller, text.clone());
        self.last_telegram_badged = Some(caller);
        let conv = crate::infra::ids::ConversationId::generate().to_string();
        let cfg = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.clone())
            .unwrap_or_default();
        let outgoing = crate::telegram::forward_text(&session_name, &text);
        let forwarded = cfg.enabled
            && cfg.notify_chat_id != 0
            && crate::telegram::read_token(&cfg.token_file).is_ok()
            && self.telegram_send_for_session(cfg.notify_chat_id, &outgoing, caller);
        self.dirty = true;
        let result = format!(r#"{{"conversation_id":"{conv}","forwarded":{forwarded}}}"#);
        if let Some(key) = idem_key {
            self.message_user_idem.store(&key, fingerprint, &result, now);
        }
        Ok(result)
    }

    /// Cancel an armed timer from the sidebar button. True when one
    /// was armed; repaint follows only then.
    pub fn cancel_timer(&mut self, timer_id: &str) -> bool {
        if self.broker.cancel_timer(timer_id) {
            self.dirty = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::infra::ids::RunId;

        #[test]
        fn session_status_round_trips_to_sidebar() {
            std::env::set_var("CODEX_BIN", "cat");
            let mut state = AppState::new();
            let id = state.manager.spawn_agent(
                "agent", &std::env::temp_dir(), "exec cat",
                crate::infra::ids::RunId::generate(), "codex",
            ).unwrap();
            std::env::remove_var("CODEX_BIN");
            let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
            let set = comms_reply(
                &mut state, &live_run, "set_session_status",
                r#"{"kind":"progress","message":"compiling"}"#,
            );
            assert!(set.contains(r#""ok":true"#), "set: {set}");
            let detail = state.sidebar_info().session.expect("detail renders");
            assert!(detail.status.as_deref() == Some("progress: compiling"), "detail: {detail:?}");
            let lines = crate::ui::sidebar::sidebar_lines(&state.sidebar_info());
            assert!(lines.iter().any(|l| {
                l.spans.iter().any(|s| s.content.contains("progress: compiling"))
            }), "sidebar shows status");
            let bad_kind = comms_reply(
                &mut state, &live_run, "set_session_status",
                r#"{"kind":"urgent","message":"x"}"#,
            );
            assert!(bad_kind.contains("unknown status kind"), "bad_kind: {bad_kind}");
            let long = comms_reply(
                &mut state, &live_run, "set_session_status",
                &format!(r#"{{"kind":"info","message":"{}"}}"#, "x".repeat(81)),
            );
            assert!(long.contains("over 80"), "long: {long}");
            let cleared = comms_reply(&mut state, &live_run, "clear_session_status", "{}");
            assert!(cleared.contains(r#""status_cleared":true"#), "cleared: {cleared}");
            assert!(state.manager.get(id).unwrap().status.is_none());
            assert!(state.manager.remove(id));
        }

        #[test]
        fn request_attention_raises_flag_and_clears_on_focus() {
            let mut state = AppState::new();
            let a = state
                .manager
                .spawn(
                    "a",
                    &std::env::temp_dir(),
                    "exec sleep 30",
                    RunId::generate(),
                    "shell",
                )
                .unwrap();
            let b = state
                .manager
                .spawn(
                    "b",
                    &std::env::temp_dir(),
                    "exec sleep 30",
                    RunId::generate(),
                    "shell",
                )
                .unwrap();
            // Operator looks at a; b raises attention while in the background.
            let order = state.manager.order().to_vec();
            let (ia, ib) = (
                order.iter().position(|id| *id == a).unwrap(),
                order.iter().position(|id| *id == b).unwrap(),
            );
            assert!(state.select_session(ia), "focus a");
            let run_b = state.manager.get(b).unwrap().run_id.as_str().to_string();
            let raised = comms_reply(
                &mut state,
                &run_b,
                "request_attention",
                r#"{"reason":"need the production API key"}"#,
            );
            assert!(raised.contains(r#""ok":true"#), "raise: {raised}");
            assert!(
                state.attention_flags.contains_key(&b),
                "background session flagged"
            );
            // Focusing the flagged session acknowledges it.
            assert!(state.select_session(ib), "focus b");
            assert!(
                !state.attention_flags.contains_key(&b),
                "focus clears attention"
            );
            // Bad reasons are rejected like status text.
            let long = comms_reply(
                &mut state,
                &run_b,
                "request_attention",
                &format!(r#"{{"reason":"{}"}}"#, "x".repeat(81)),
            );
            assert!(long.contains("over 80"), "long: {long}");
            assert!(state.manager.remove(a));
            assert!(state.manager.remove(b));
        }

        #[test]
        fn message_user_badges_without_forward_when_disabled() {
            let (mut state, id, live_run) = message_user_agent();
            let ok = comms_reply(&mut state, &live_run, "message_user", r#"{"message":"hello operator"}"#);
            assert!(ok.contains(r#""ok":true"#), "ok: {ok}");
            assert!(ok.contains("conversation_id"), "ok: {ok}");
            assert!(ok.contains(r#""forwarded":false"#), "disabled never forwards: {ok}");
            assert_eq!(
                state.message_user_badges.get(&id).map(String::as_str),
                Some("hello operator")
            );
            let stale = comms_reply(&mut state, "bogus-run", "message_user", r#"{"message":"x"}"#);
            assert!(stale.contains("unknown or stale run ID"), "stale: {stale}");
            let empty = comms_reply(&mut state, &live_run, "message_user", "{}");
            assert!(empty.contains("message_user needs a message"), "empty: {empty}");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn message_user_rate_limits_at_three_per_minute() {
            let (mut state, _id, live_run) = message_user_agent();
            for i in 0..3 {
                let ok = comms_reply(
                    &mut state,
                    &live_run,
                    "message_user",
                    &format!(r#"{{"message":"note {i}"}}"#),
                );
                assert!(ok.contains(r#""ok":true"#), "send {i}: {ok}");
            }
            let fourth = comms_reply(&mut state, &live_run, "message_user", r#"{"message":"note 3"}"#);
            assert!(fourth.contains("rate limited"), "fourth: {fourth}");
        }

        #[test]
        fn message_user_replays_idempotent_retries() {
            let (mut state, id, live_run) = message_user_agent();
            let args = r#"{"message":"same","idempotency_key":"k1"}"#;
            let first = comms_reply(&mut state, &live_run, "message_user", args);
            let second = comms_reply(&mut state, &live_run, "message_user", args);
            assert!(first.contains(r#""ok":true"#), "first: {first}");
            assert_eq!(first, second, "retry replays the same verdict");
            let clash = comms_reply(
                &mut state,
                &live_run,
                "message_user",
                r#"{"message":"different","idempotency_key":"k1"}"#,
            );
            assert!(clash.contains("conflict"), "clash: {clash}");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn message_user_forward_marks_session_keeps_badge_raw() {
            let (mut state, id, live_run) = message_user_agent();
            let dir = std::env::temp_dir().join(format!("forge-tg-fwd-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let token_path = dir.join("tg.token");
            std::fs::write(&token_path, "0123456789abcdef0123456789abcdef").unwrap();
            {
                let mut cfg = state.telegram_config.lock().expect("lock");
                cfg.enabled = true;
                cfg.token_file = token_path.to_string_lossy().into_owned();
                cfg.notify_chat_id = 11;
            }
            let ok = comms_reply(&mut state, &live_run, "message_user", r#"{"message":"hello"}"#);
            assert!(ok.contains(r#""forwarded":true"#), "worker attempted: {ok}");
            assert_eq!(
                state.message_user_badges.get(&id).map(String::as_str),
                Some("hello"),
                "badge stays raw; only the Telegram text carries [name]"
            );
            let queued = tg_outbox(&state);
            assert_eq!(queued.len(), 1, "forward uses the supervised sender queue");
            assert_eq!(queued[0].text, "[agent] hello");
            let _ = std::fs::remove_dir_all(&dir);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn start_session_validates_creates_and_keeps_focus() {
            std::env::set_var("CODEX_BIN", "cat");
            let mut state = AppState::new();
            let id = state.manager.spawn_agent(
                "agent", &std::env::temp_dir(), "exec cat",
                crate::infra::ids::RunId::generate(), "codex",
            ).unwrap();
            std::env::remove_var("CODEX_BIN");
            let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
            for (args, needle) in [
                (r#"{"harness":"nope"}"#, "unknown harness"),
                (r#"{"path":"/no/such/dir"}"#, "not a directory"),
                (r#"{"name":"agent"}"#, "is taken"),
                (r#"{"host":"remote"}"#, "unsupported"),
                (r#"{"internet":true}"#, "unsupported"),
            ] {
                let err = comms_reply(&mut state, &live_run, "start_session", args);
                assert!(err.contains(r#""ok":false"#), "args {args}: {err}");
                assert!(err.contains(needle), "args {args}: {err}");
            }
            std::env::set_var("CODEX_BIN", "cat");
            let started = comms_reply(
                &mut state, &live_run, "start_session",
                r#"{"name":"helper-1","harness":"codex","prompt":"hello"}"#,
            );
            std::env::remove_var("CODEX_BIN");
            assert!(started.contains(r#""ok":true"#), "started: {started}");
            assert!(started.contains(r#""name":"helper-1""#), "started: {started}");
            assert_eq!(state.manager.active(), Some(id), "tool birth keeps focus");
            let new = state.manager.order().iter()
                .find(|&&cand| cand != id).copied().expect("second session exists");
            assert_eq!(state.broker.queued(new), 1, "prompt queued for the birth");
            let due = state.broker.take_due(new, 10);
            assert_eq!(due[0].text, "hello");
            // Default naming plus the caller's group carry over.
            state.broker.join(&state.manager, id, "team").unwrap();
            std::env::set_var("CODEX_BIN", "cat");
            let auto = comms_reply(&mut state, &live_run, "start_session", r#"{"harness":"codex"}"#);
            std::env::remove_var("CODEX_BIN");
            assert!(auto.contains("codex-"), "auto name: {auto}");
            let third = state.manager.order().iter()
                .find(|&&cand| cand != id && cand != new).copied().expect("third exists");
            assert_eq!(state.broker.primary_group(third), Some("team"));
            assert!(state.manager.remove(id));
            assert!(state.manager.remove(new));
            assert!(state.manager.remove(third));
        }
}
