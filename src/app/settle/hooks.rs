//! AppState hook settling: verdicts, attribution, and the audit trail.

use super::*;

impl AppState {
    /// Run deterministic policy over queued hook requests. Every verdict —
    /// allow, deny, or ask-for-the-harness — replies immediately and is
    /// audited; nothing waits on a human. Yolo auto-approves, Safe-Only
    /// blocks still deny, and every other Ask goes back to the harness so
    /// its native permission flow takes over. Audit failures never block.
    pub fn settle_hooks(
        &mut self,
        policy: &mut crate::hooks::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        while let Some(req) = self.pending_hooks.pop_front() {
            let (decision, reason) = policy.decide(&req.hook, &req.body);
            // Same attribution as enqueue: run ID first, harness
            // fallback second. A verdict for a fallback-attributed
            // hook protects its pane too, not just run-bound ones.
            let fallback_id = if self.manager.lookup_run(&req.run_id).is_none() {
                crate::session::session_id_from_hook_body(&req.body)
                    .and_then(|h| self.manager.lookup_harness_session(&h))
            } else {
                None
            };
            let attributed = self.manager.lookup_run(&req.run_id).or(fallback_id);
            // Codex rejects a bare PreToolUse allow (and ask) as
            // "unsupported permissionDecision"; it only accepts allow with
            // an updatedInput rewrite, which forge never emits. Render the
            // verdict per harness so attributed codex allow/ask stay silent
            // (empty stdout lets Codex's own approval flow decide) while
            // everyone else keeps the explicit shape. Unattributed senders
            // keep the long-standing explicit output.
            let cli_tool = attributed
                .and_then(|id| self.manager.get(id))
                .map(|rec| rec.cli_tool.as_str())
                .unwrap_or("");
            let line = crate::hooks::policy::decision_line_for(cli_tool, &req.hook, decision, reason);
            let _ = req.reply.send(line);
            // The verdict races bodies only in its own pane: stamp the
            // attributed session, never the whole app.
            if let Some(id) = attributed {
                self.last_hook_activity
                    .insert(id, std::time::Instant::now());
            }
            self.audit_hook(audit_path, &req.hook, &req.body, decision, reason);
            // A verdict changes audit state, so only a fired hook
            // repaints; an empty queue leaves the frame clean and the idle
            // loop skips the 60Hz full redraw.
            self.dirty = true;
        }
    }

    fn audit_hook(
        &self,
        audit_path: &std::path::Path,
        hook: &str,
        body: &str,
        decision: crate::hooks::policy::Decision,
        reason: &str,
    ) {
        let tool = crate::hooks::policy::tool_name(body);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("{decision:?}").to_lowercase();
        let _ = crate::hooks::audit::append(audit_path, hook, &tool, &name, reason, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::infra::ids::RunId;

        #[test]
        fn codex_pre_tool_use_allow_is_silent_claude_stays_explicit() {
            use crate::infra::config::PermissionMode;
            let audit = std::env::temp_dir().join(format!(
                "forge-codex-silent-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&audit);
            let mut policy =
                crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
            let mut s = AppState::new();
            let codex_run = RunId::generate();
            let codex_id = s
                .manager
                .spawn_agent(
                    "cx",
                    &std::env::temp_dir(),
                    "exec sleep 30",
                    codex_run.clone(),
                    "codex",
                )
                .unwrap();
            let claude_run = RunId::generate();
            let claude_id = s
                .manager
                .spawn_agent(
                    "cl",
                    &std::env::temp_dir(),
                    "exec sleep 30",
                    claude_run.clone(),
                    "claude",
                )
                .unwrap();
            let body = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string();
            let (codex_tx, codex_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: body.clone(),
                run_id: codex_run.to_string(),
                sync: true,
                reply: codex_tx,
                timed_out: Default::default(),
            }));
            let (claude_tx, claude_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body,
                run_id: claude_run.to_string(),
                sync: true,
                reply: claude_tx,
                timed_out: Default::default(),
            }));
            s.settle_hooks(&mut policy, &audit);
            let codex_line = codex_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(
                codex_line.is_empty(),
                "codex bare allow must be silent, else codex reports unsupported permissionDecision:allow: {codex_line:?}"
            );
            let claude_line = claude_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(
                claude_line.contains(r#""permissionDecision":"allow""#),
                "claude keeps explicit allow: {claude_line:?}"
            );
            assert!(s.manager.remove(codex_id));
            assert!(s.manager.remove(claude_id));
            let _ = std::fs::remove_file(&audit);
        }

        #[test]
        fn unattributed_muse_hooks_bootstrap_and_attribute() {
            use crate::session::Activity;
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let run = RunId::generate();
            let id = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", run.clone(), "muse")
                .unwrap();
            // Scrubbed relay: empty run_id, muse SessionStart body shape.
            let start = format!(
                "{{\"v\":1,\"hook\":\"SessionStart\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"muse-9\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("SessionStart", "", &start));
            assert_eq!(
                s.manager.get(id).unwrap().harness_session_id.as_deref(),
                Some("muse-9"),
                "bootstrap captured the resume ID"
            );
            assert_eq!(s.manager.get(id).unwrap().activity, Activity::Thinking);
            // Later edges carry no run either; the bound ID attributes them.
            let stop = format!(
                "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"muse-9\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("Stop", "", &stop));
            assert_eq!(s.manager.get(id).unwrap().activity, Activity::Stopped);
            // An unknown harness ID still touches nothing.
            let strange = format!(
                "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"ghost\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("Stop", "", &strange));
            assert_eq!(s.manager.get(id).unwrap().activity, Activity::Stopped);
            assert!(s.manager.remove(id));
        }

        #[test]
        fn resumed_muse_hooks_bind_without_session_start() {
            // Grounded externally: `muse resume <id>` fires no SessionStart,
            // only UserPromptSubmit and Stop. A restored session that never
            // saw SessionStart must still learn its resume ID from those
            // edges, or every re-save writes null again.
            use crate::session::Activity;
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let run = RunId::generate();
            let id = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", run.clone(), "muse")
                .unwrap();
            assert_eq!(s.manager.get(id).unwrap().harness_session_id, None);
            let prompt = format!(
                "{{\"v\":1,\"hook\":\"UserPromptSubmit\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"muse-r\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("UserPromptSubmit", "", &prompt));
            assert_eq!(
                s.manager.get(id).unwrap().harness_session_id.as_deref(),
                Some("muse-r"),
                "resumed-session edge captured the resume ID"
            );
            assert_eq!(s.manager.get(id).unwrap().activity, Activity::Thinking);
            assert!(s.manager.remove(id));
        }

        #[test]
        fn foreign_session_hooks_never_strand_a_pane_busy() {
            // muse runs background sub-sessions in the pane's process group
            // after its turn ends: their tool hooks resolve to the pane by
            // source_sid but carry a different session_id and never Stop.
            // They must not park the pane in ToolUse, or queued messages
            // wait forever (comms.log: 486 holds, mu_1/mu_2 never delivered).
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let run_a = RunId::generate();
            let a = s
                .manager
                .spawn("a", &cwd, "exec sleep 30", run_a.clone(), "shell")
                .unwrap();
            let m = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            let sid = s.manager.process_session_id(m).expect("pty process session");
            let edge = |hook: &str, session: &str| {
                format!(
                    "{{\"v\":1,\"hook\":\"{hook}\",\"run_id\":\"\",\"forge_pid\":0,\"source_sid\":{sid},\"body\":{{\"session_id\":\"{session}\",\"cwd\":{cwd_json}}}}}"
                )
            };
            s.apply(hook_request("UserPromptSubmit", "", &edge("UserPromptSubmit", "muse-main")));
            // The pane's own tool hook still marks it busy.
            s.apply(hook_request("PreToolUse", "", &edge("PreToolUse", "muse-main")));
            assert_eq!(s.manager.get(m).unwrap().activity, crate::session::Activity::ToolUse);
            s.apply(hook_request("Stop", "", &edge("Stop", "muse-main")));
            assert_eq!(s.manager.get(m).unwrap().activity, crate::session::Activity::Stopped);
            // A background sub-session's tool hook: same pane, foreign id.
            s.apply(hook_request("PreToolUse", "", &edge("PreToolUse", "muse-sub")));
            assert_eq!(
                s.manager.get(m).unwrap().activity,
                crate::session::Activity::Stopped,
                "foreign session_id must not change pane activity"
            );
            // End to end: a tell queued for the pane is delivered once the
            // hook debounce settles.
            s.broker.join(&s.manager, a, "peers").unwrap();
            s.broker.join(&s.manager, m, "peers").unwrap();
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: "{\"target\":\"m\",\"text\":\"hello-m\"}".to_string(),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
            }));
            assert_eq!(s.broker.queued(m), 1);
            s.last_hook_activity.remove(&m);
            s.settle_comms();
            assert_eq!(s.broker.queued(m), 0, "stopped pane receives the tell");
            assert!(s.manager.remove(a));
            assert!(s.manager.remove(m));
        }

        #[test]
        fn hook_trace_records_attribution_and_snapshot() {
            let dir = std::env::temp_dir().join(format!("forge-hook-trace-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let log = dir.join("hooks.log");
            let mut s = AppState::new();
            s.hook_trace = Some(log.clone());
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let id = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            let sid = s.manager.process_session_id(id).expect("pty process session");
            let prompt = format!(
                "{{\"v\":1,\"hook\":\"UserPromptSubmit\",\"run_id\":\"\",\"forge_pid\":0,\"source_sid\":{sid},\"body\":{{\"session_id\":\"muse-t\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("UserPromptSubmit", "", &prompt));
            // A stray relay from outside every pane: nothing resolves.
            let stray = r#"{"v":1,"hook":"Stop","run_id":"","source_sid":1,"body":{"session_id":"ghost"}}"#;
            s.apply(hook_request("Stop", "", stray));
            s.trace_snapshot("quit");
            let text = std::fs::read_to_string(&log).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3, "{text}");
            for needle in [
                "tui hook=UserPromptSubmit",
                "run=none",
                &format!("source_sid={sid}"),
                &format!("by_sid={id}"),
                "session_id=muse-t",
                &format!("attributed={id}"),
                "harness_after=muse-t",
            ] {
                assert!(lines[0].contains(needle), "{needle} missing: {}", lines[0]);
            }
            for needle in ["tui hook=Stop", "by_sid=none", "attributed=none"] {
                assert!(lines[1].contains(needle), "{needle} missing: {}", lines[1]);
            }
            for needle in [
                "snapshot quit",
                &format!("{id}"),
                "name=m",
                "tool=muse",
                &format!("sid={sid}"),
                "harness=muse-t",
            ] {
                assert!(lines[2].contains(needle), "{needle} missing: {}", lines[2]);
            }
            assert!(s.manager.remove(id));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn user_prompt_submit_captures_harness_id_when_session_start_missed() {
            // Grounded: a harness that booted while Forge was down (fail-open
            // relay) is never seen starting, yet every later UserPromptSubmit
            // body still carries the true conversation ID.
            let mut s = AppState::new();
            let run = RunId::generate();
            let id = s
                .manager
                .spawn_agent("c", &std::env::temp_dir(), "exec sleep 30", run.clone(), "claude")
                .unwrap();
            assert_eq!(s.manager.get(id).unwrap().harness_session_id, None);
            s.apply(hook_request(
                "UserPromptSubmit",
                run.as_str(),
                r#"{"v":1,"hook":"UserPromptSubmit","run_id":"r","body":{"session_id":"harness-7","cwd":"/tmp"}}"#,
            ));
            assert_eq!(
                s.manager.get(id).unwrap().harness_session_id.as_deref(),
                Some("harness-7"),
                "prompt is the true resume ID when SessionStart was missed"
            );
            assert!(s.manager.remove(id));
        }

        #[test]
        fn user_prompt_submit_refreshes_rotated_harness_id() {
            // A manual in-pane resume swaps the harness conversation with no
            // SessionStart; the next prompt carries the ID the pane actually
            // runs now, so the record must follow it.
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let run = RunId::generate();
            let id = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", run.clone(), "muse")
                .unwrap();
            s.manager.set_harness_session(id, "muse-old".to_string());
            let sid = s
                .manager
                .process_session_id(id)
                .expect("pty process session");
            let prompt = format!(
                "{{\"v\":1,\"hook\":\"UserPromptSubmit\",\"run_id\":\"\",\"forge_pid\":0,\"source_sid\":{sid},\"body\":{{\"session_id\":\"muse-new\",\"cwd\":{cwd_json}}}}}"
            );
            s.apply(hook_request("UserPromptSubmit", "", &prompt));
            assert_eq!(
                s.manager.get(id).unwrap().harness_session_id.as_deref(),
                Some("muse-new"),
                "prompt always carries the live conversation ID"
            );
            assert!(s.manager.remove(id));
        }

        #[test]
        fn resumed_same_cwd_muse_hooks_bind_by_pty_process_session_after_window() {
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let a = s
                .manager
                .spawn_agent("a", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            let b = s
                .manager
                .spawn_agent("b", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            let old = std::time::Instant::now()
                - crate::session::BOOTSTRAP_WINDOW
                - std::time::Duration::from_secs(1);
            s.manager.get_mut(a).unwrap().spawned_at = old;
            s.manager.get_mut(b).unwrap().spawned_at = old;
            let b_sid = s
                .manager
                .process_session_id(b)
                .expect("second PTY process session");
            let prompt = format!(
                "{{\"v\":1,\"hook\":\"UserPromptSubmit\",\"run_id\":\"\",\"forge_pid\":0,\"source_sid\":{b_sid},\"body\":{{\"session_id\":\"muse-b\",\"cwd\":{cwd_json}}}}}"
            );

            s.apply(hook_request("UserPromptSubmit", "", &prompt));

            assert_eq!(s.manager.get(a).unwrap().harness_session_id, None);
            assert_eq!(
                s.manager.get(b).unwrap().harness_session_id.as_deref(),
                Some("muse-b"),
                "source PTY identifies the resumed session despite shared cwd and elapsed window"
            );
            assert!(s.manager.remove(a));
            assert!(s.manager.remove(b));
        }

        #[test]
        fn process_attributed_hook_conflict_never_spills_to_same_cwd_session() {
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let a = s
                .manager
                .spawn_agent("a", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            let b = s
                .manager
                .spawn_agent("b", &cwd, "exec sleep 30", RunId::generate(), "muse")
                .unwrap();
            s.manager.set_harness_session(b, "muse-b".to_string());
            let b_sid = s
                .manager
                .process_session_id(b)
                .expect("second PTY process session");
            let stop = format!(
                "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":0,\"source_sid\":{b_sid},\"body\":{{\"session_id\":\"conflict\",\"cwd\":{cwd_json}}}}}"
            );

            s.apply(hook_request("Stop", "", &stop));

            assert_eq!(
                s.manager.get(a).unwrap().harness_session_id,
                None,
                "a rejected claim from b must not fall through to cwd attribution"
            );
            assert_eq!(
                s.manager.get(b).unwrap().harness_session_id.as_deref(),
                Some("muse-b"),
                "an established process binding is never overwritten"
            );
            assert!(s.manager.remove(a));
            assert!(s.manager.remove(b));
        }

        #[test]
        fn subagent_tool_hooks_never_bind_a_session() {
            // Subagent PreToolUse hooks carry the child's session ID; binding
            // one would point the session at a transcript it cannot resume.
            // Only main-session edges (SessionStart, UserPromptSubmit, Stop)
            // may claim an unbound session.
            let mut s = AppState::new();
            let cwd = std::env::temp_dir();
            let cwd_json = crate::ipc::mcp::escape_json(&cwd.to_string_lossy());
            let run = RunId::generate();
            let id = s
                .manager
                .spawn_agent("m", &cwd, "exec sleep 30", run.clone(), "muse")
                .unwrap();
            let tool = format!(
                "{{\"v\":1,\"hook\":\"PreToolUse\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"child-1\",\"cwd\":{cwd_json},\"tool\":\"Bash\"}}}}"
            );
            s.apply(hook_request("PreToolUse", "", &tool));
            assert_eq!(
                s.manager.get(id).unwrap().harness_session_id,
                None,
                "child tool hook binds nothing"
            );
            assert!(s.manager.remove(id));
        }

        #[test]
        fn hook_requests_queue_bounded_and_dirty() {
            let mut s = AppState::new();
            s.dirty = false;
            let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: "{}".to_string(),
                run_id: String::new(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            assert!(s.dirty);
            assert_eq!(s.pending_hooks.len(), 1);
            for _ in 0..crate::ipc::listener::MAX_PENDING_HOOKS + 10 {
                let (tx, _rx) = std::sync::mpsc::channel();
                s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                    hook: "Stop".to_string(),
                    body: "{}".to_string(),
                    run_id: String::new(),
                    sync: false,
                    reply: tx,
                    timed_out: Default::default(),
                }));
            }
            assert_eq!(s.pending_hooks.len(), crate::ipc::listener::MAX_PENDING_HOOKS);
        }

        #[test]
        fn pre_tool_use_replies_use_hook_specific_output() {
            // Newer Claude (and muse) reject the legacy top-level `decision`
            // field on PreToolUse replies: "unsupported legacy PreToolUse
            // output; use hookSpecificOutput.permissionDecision".
            use crate::infra::config::PermissionMode;
            let audit = std::env::temp_dir().join(format!(
                "forge-hookshape-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&audit);
            let mut yolo = crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
            let mut s = AppState::new();
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
                run_id: String::new(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            s.settle_hooks(&mut yolo, &audit);
            let line = reply_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(line.trim()).expect("reply is JSON");
            assert_eq!(
                v["hookSpecificOutput"]["hookEventName"],
                serde_json::Value::String("PreToolUse".to_string()),
                "line: {line:?}"
            );
            assert_eq!(
                v["hookSpecificOutput"]["permissionDecision"],
                serde_json::Value::String("allow".to_string()),
                "line: {line:?}"
            );
            assert!(
                v["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty()),
                "deny requires a non-empty reason; allow carries one too: {line:?}"
            );
            assert!(v.get("decision").is_none(), "no legacy field: {line:?}");
            let _ = std::fs::remove_file(&audit);
        }

        #[test]
        fn settle_hooks_replies_every_verdict_immediately() {
            use crate::infra::config::PermissionMode;
            let audit = std::env::temp_dir().join(format!(
                "forge-settle-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&audit);
            let mut yolo = crate::hooks::policy::Policy::new(
                PermissionMode::Yolo,
                &[],
                &[],
            )
            .unwrap();
            let mut s = AppState::new();
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
                run_id: String::new(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            s.settle_hooks(&mut yolo, &audit);
            assert!(s.pending_hooks.is_empty());
            let line = reply_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(
                line.contains(r#""permissionDecision":"allow""#),
                "line: {line:?}"
            );
            let logged = std::fs::read_to_string(&audit).unwrap();
            assert_eq!(logged.lines().count(), 1);
            assert!(logged.contains(r#""decision":"allow""#), "audit: {logged:?}");

            let mut off = crate::hooks::policy::Policy::new(
                PermissionMode::Off,
                &[],
                &[],
            )
            .unwrap();
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: "{}".to_string(),
                run_id: String::new(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            s.settle_hooks(&mut off, &audit);
            assert!(s.pending_hooks.is_empty(), "no modal queue anymore");
            // Non-yolo Ask goes straight back so the harness handles it.
            let line = reply_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(line.contains(r#""permissionDecision":"ask""#), "line: {line:?}");
            let logged = std::fs::read_to_string(&audit).unwrap();
            assert_eq!(logged.lines().count(), 2, "audit: {logged:?}");
            assert!(logged.contains(r#""decision":"ask""#), "audit: {logged:?}");
            let _ = std::fs::remove_file(&audit);
        }

        #[test]
        fn settle_hooks_dirties_only_when_a_hook_fires() {
            use crate::infra::config::PermissionMode;
            let audit = std::env::temp_dir().join(format!(
                "forge-settle-idle-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&audit);
            let mut policy = crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[])
                .unwrap();
            let mut s = AppState::new();
            // Empty queue: the 60Hz loop must not repaint from this.
            s.dirty = false;
            s.settle_hooks(&mut policy, &audit);
            assert!(!s.dirty, "idle settle must leave the frame clean");
            // A real verdict still repaints (counters/audit change).
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
                run_id: String::new(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            s.dirty = false;
            s.settle_hooks(&mut policy, &audit);
            assert!(s.dirty, "a settled hook must repaint");
            let _ = reply_rx.recv_timeout(std::time::Duration::from_secs(2));
            let _ = std::fs::remove_file(&audit);
        }

        #[test]
        fn hook_requests_and_verdicts_stamp_hook_activity() {
            use crate::infra::config::PermissionMode;
            let mut s = AppState::new();
            let run_a = RunId::generate();
            let a = s
                .manager
                .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
                .unwrap();
            assert!(s.last_hook_activity.is_empty());
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: "{}".to_string(),
                run_id: run_a.to_string(),
                sync: true,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            assert!(
                s.last_hook_activity.contains_key(&a),
                "enqueue stamps the attributed session"
            );
            // A verdict re-stamps: backdate first so only the settle can renew.
            s.last_hook_activity.insert(
                a,
                std::time::Instant::now() - std::time::Duration::from_secs(3600),
            );
            let audit = std::env::temp_dir().join(format!(
                "forge-hook-stamp-test-{}",
                std::process::id()
            ));
            let mut yolo =
                crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
            s.settle_hooks(&mut yolo, &audit);
            let age = std::time::Instant::now()
                .duration_since(*s.last_hook_activity.get(&a).expect("verdict stamps"));
            assert!(age < std::time::Duration::from_secs(5), "verdict stamps: {age:?}");
            let _ = std::fs::remove_file(&audit);
            assert!(s.manager.remove(a));
        }

        #[test]
        fn hook_requests_attribute_activity_to_sender() {
            let mut s = AppState::new();
            let run = RunId::generate();
            let id = s
                .manager
                .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
                .unwrap();
            fn hook(s: &mut AppState, hook: &str, run_id: String) {
                let (reply_tx, _) = std::sync::mpsc::channel();
                s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                    hook: hook.to_string(),
                    body: "{}".to_string(),
                    run_id,
                    sync: false,
                    reply: reply_tx,
                    timed_out: Default::default(),
                }));
            }
            hook(&mut s, "PreToolUse", run.to_string());
            assert_eq!(
                s.manager.get(id).unwrap().activity,
                crate::session::Activity::ToolUse
            );
            hook(&mut s, "Stop", run.to_string());
            assert_eq!(
                s.manager.get(id).unwrap().activity,
                crate::session::Activity::Stopped
            );
            // Unknown runs never touch live sessions.
            hook(&mut s, "PreToolUse", "f".repeat(32));
            assert_eq!(
                s.manager.get(id).unwrap().activity,
                crate::session::Activity::Stopped
            );
            assert!(s.manager.remove(id));
        }

        #[test]
        fn hook_attribution_marks_activity_and_settles() {
            let mut s = AppState::new();
            let run = RunId::generate();
            let id = s
                .manager
                .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
                .unwrap();
            // Tool-gated hook: activity flips to ToolUse, hook queued.
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: "{}".to_string(),
                run_id: run.to_string(),
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            assert_eq!(
                s.manager.get(id).unwrap().activity,
                crate::session::Activity::ToolUse
            );
            assert_eq!(s.pending_hooks.len(), 1);
            // Yolo settle drains the queue and stamps hook activity.
            let audit = std::env::temp_dir().join(format!(
                "forge-attribution-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&audit);
            let mut yolo =
                crate::hooks::policy::Policy::new(crate::infra::config::PermissionMode::Yolo, &[], &[]).unwrap();
            s.settle_hooks(&mut yolo, &audit);
            assert!(s.pending_hooks.is_empty(), "queue drains");
            assert!(s.last_hook_activity.contains_key(&id), "verdict stamps activity");
            let _ = std::fs::remove_file(&audit);
            assert!(s.manager.remove(id));
        }

        #[test]
        fn verdict_stamp_uses_harness_fallback_attribution() {
            // Enqueue attributes by run or harness fallback; the verdict
            // stamp must use the same attribution, or a slow batch ages
            // the enqueue stamp past the beat and the verdict protects
            // nobody.
            use crate::infra::config::PermissionMode;
            let mut s = AppState::new();
            let run_a = RunId::generate();
            let a = s
                .manager
                .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
                .unwrap();
            s.manager.set_harness_session(a, "h-1".to_string());
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
                hook: "PreToolUse".to_string(),
                body: r#"{"body":{"session_id":"h-1"}}"#.to_string(),
                run_id: "unknown-run".to_string(),
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
            assert!(s.last_hook_activity.contains_key(&a), "enqueue attributes");
            // Age the enqueue stamp out, so only a verdict stamp can renew.
            s.last_hook_activity.insert(
                a,
                std::time::Instant::now() - std::time::Duration::from_secs(3600),
            );
            let audit = std::env::temp_dir().join(format!(
                "forge-hook-fallback-test-{}",
                std::process::id()
            ));
            let mut yolo =
                crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
            s.settle_hooks(&mut yolo, &audit);
            let age = std::time::Instant::now()
                .duration_since(*s.last_hook_activity.get(&a).expect("verdict stamps"));
            assert!(age < std::time::Duration::from_secs(5), "verdict stamps: {age:?}");
            let _ = std::fs::remove_file(&audit);
            assert!(s.manager.remove(a));
        }
}
