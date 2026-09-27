//! Busy-session inbox: queued peer messages ride the session's own
//! forge tool replies, so a session that never goes idle still hears
//! its peers without anything typed into its busy pane.

use super::*;

impl AppState {
    /// Attach the caller's queued peer messages to its own successful
    /// forge tool reply as `forge_inbox`: rendered exactly as the pane
    /// would show them, reply guidance included. A caller mid-tool-call
    /// is mid-turn, so this is the one channel that reaches a session
    /// that never goes idle, without typing into its busy pane. Only
    /// object results take the field; anything else stays queued.
    pub(crate) fn attach_inbox(&mut self, run_id: &str, tool: &str, result: String) -> String {
        let Some(id) = self.manager.lookup_run(run_id) else {
            return result;
        };
        let trimmed = result.trim();
        let Some(inner) = trimmed.strip_prefix('{').and_then(|r| r.strip_suffix('}')) else {
            return result;
        };
        let inbox = self.broker.take_inbox(id);
        if inbox.is_empty() {
            return result;
        }
        let to = self
            .manager
            .get(id)
            .map(|rec| rec.name.clone())
            .unwrap_or_default();
        let mut items = Vec::with_capacity(inbox.len());
        for inj in &inbox {
            items.push(crate::ipc::mcp::escape_json(&String::from_utf8_lossy(
                &inj.render_body(),
            )));
            self.trace_comms(&format!(
                "inbox to={} conv={} kind={} from={} via={tool}",
                crate::comms::log_quote(&to),
                crate::comms::log_quote(&inj.conv),
                inj.kind.trace_label(),
                crate::comms::log_quote(&inj.from),
            ));
        }
        self.hold_logged.remove(&id);
        self.dirty = true;
        let sep = if inner.trim().is_empty() { "" } else { "," };
        format!("{{{inner}{sep}\"forge_inbox\":[{}]}}", items.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::ids::RunId;

    #[test]
    fn busy_callers_receive_queued_peer_messages_in_tool_replies() {
        // A session in one long turn never goes idle, so its queue never
        // drains (comms.log: claude -> mu_2 held ~50 min). Its own forge
        // tool calls carry the waiting peer messages instead: nothing is
        // typed into a busy pane, and the messages count as delivered.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let run_b = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let call = |s: &mut AppState, run: &RunId, tool: &str, args: &str| {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
                run_id: run.to_string(),
                tool: tool.to_string(),
                args: args.to_string(),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                    crate::ipc::listener::CLAIM_PENDING,
                )),
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("verdict")
        };
        assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
        call(&mut s, &run_a, "tell_session", r#"{"target":"b","text":"while-busy"}"#);
        let asked = call(&mut s, &run_a, "ask_session", r#"{"target":"b","message":"status?"}"#);
        let conv = crate::hooks::policy::json_string_field(asked.as_bytes(), &["conversation"])
            .expect("ask conversation");
        // A CLI command (presence ping, scheduled prompt) must still be
        // typed as a prompt, so it stays queued.
        s.broker.push(
            b,
            crate::comms::Injection {
                conv: "presence-1".to_string(),
                kind: crate::comms::InjectKind::Command,
                from: "forge".to_string(),
                text: "user is away".to_string(),
            },
        );
        let reply = call(&mut s, &run_b, "list_sessions", "{}");
        assert!(reply.contains("\"ok\":true"), "{reply}");
        assert!(reply.contains("\"forge_inbox\":["), "{reply}");
        assert!(reply.contains("while-busy"), "{reply}");
        assert!(reply.contains("status?"), "{reply}");
        assert!(
            reply.contains(&format!("send_response conversation_id {conv}")),
            "reply guidance rides along: {reply}"
        );
        assert!(!reply.contains("user is away"), "{reply}");
        assert_eq!(s.broker.queued(b), 1, "only the command is left");
        assert_eq!(
            s.broker.peek_due(b).map(|h| h.kind),
            Some(crate::comms::InjectKind::Command)
        );
        // The ask counts as delivered: b can answer it right away.
        call(
            &mut s,
            &run_b,
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","text":"all good"}}"#),
        );
        assert!(
            s.broker.take_due(a, 10).iter().any(|i| i.text == "all good"),
            "response reaches the asker"
        );
        // Nothing waiting: replies carry no inbox.
        let again = call(&mut s, &run_b, "list_sessions", "{}");
        assert!(!again.contains("forge_inbox"), "{again}");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }
}
