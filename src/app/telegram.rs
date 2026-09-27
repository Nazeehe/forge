//! AppState Telegram bridge: routing, presence, dialogs, and tests hooks.

use super::*;

impl AppState {
    /// Queue one Telegram reply for the dedicated sender. Truncated and
    /// bounded: past [`crate::telegram::OUTBOX_CAP`] newcomers drop and
    /// count instead of growing the owner.
    fn telegram_send(&mut self, chat_id: i64, text: &str) -> bool {
        self.telegram_enqueue(crate::telegram::OutboundMessage::new(
            chat_id,
            crate::telegram::truncate_text(text, crate::telegram::MAX_TEXT),
        ))
    }

    /// Like [`Self::telegram_send`], but tags the message as speaking for
    /// `session` so a native Telegram reply to it can later address that
    /// session directly (see [`Self::record_telegram_reply_target`]).
    pub(super) fn telegram_send_for_session(
        &mut self,
        chat_id: i64,
        text: &str,
        session: crate::session::SessionId,
    ) -> bool {
        self.telegram_enqueue(crate::telegram::OutboundMessage::for_session(
            chat_id,
            crate::telegram::truncate_text(text, crate::telegram::MAX_TEXT),
            session,
        ))
    }

    fn telegram_enqueue(&mut self, message: crate::telegram::OutboundMessage) -> bool {
        let Ok(mut outbox) = self.telegram_outbox.lock() else {
            return false;
        };
        if outbox.len() >= crate::telegram::OUTBOX_CAP {
            self.telegram_outbox_dropped += 1;
            self.telegram_last_send_failed = true;
            self.dirty = true;
            false
        } else {
            outbox.push_back(message);
            self.telegram_outbox_wake.notify_one();
            true
        }
    }

    /// Resolve an exact live session by name for the operator. Ambiguous
    /// names fail rather than guess; exited and unknown names fail with
    /// the same dead end so replies never reach a live pane by mistake.
    fn resolve_telegram_session(
        &self,
        name: &str,
    ) -> Result<crate::session::SessionId, String> {
        let mut found = None;
        let mut ambiguous = false;
        for &id in self.manager.order() {
            let live = self
                .manager
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

    /// One-line session roster for `/sessions`, each tagged live/exited.
    fn telegram_session_list(&self) -> String {
        let mut lines = vec!["sessions:".to_string()];
        for &id in self.manager.order() {
            if let Some(rec) = self.manager.get(id) {
                if rec.state.is_live() {
                    lines.push(format!("{} (live)", rec.name));
                }
            }
        }
        if lines.len() == 1 {
            lines.push("(none)".to_string());
        }
        lines.join("\n")
    }

    /// Split a leading `[name] body` address. The name must be a
    /// single non-empty token and the body non-empty; anything else is
    /// bare text for the badge path.
    fn parse_bracketed(text: &str) -> Option<(String, String)> {
        let tail = text.strip_prefix('[')?;
        let (head, body) = tail.split_once(']')?;
        let name = head.trim();
        let body = body.trim();
        if name.is_empty() || name.contains(char::is_whitespace) || body.is_empty() {
            return None;
        }
        Some((name.to_string(), body.to_string()))
    }

    /// Drain inbox messages into replies and session injections, at most
    /// [`crate::telegram::ROUTE_BATCH`] per settle so a flood drains over
    /// ticks. Called from [`Self::settle_comms`], ahead of delivery, so
    /// freshly routed injections can go out on the same tick when idle.
    pub(super) fn drain_telegram(&mut self) {
        for _ in 0..crate::telegram::ROUTE_BATCH {
            let Some(msg) = self.telegram_inbox.pop_front() else {
                break;
            };
            self.route_telegram(msg);
        }
    }

    /// Collect finished Telegram connection tests into the open dialog.
    /// Called once per main-loop pass; never blocks. A closed dialog
    /// drops the verdict.
    pub fn drain_telegram_test(&mut self) {
        let results: Vec<crate::telegram::TelegramTested> =
            self.telegram_test_rx.try_iter().collect();
        for tested in results {
            if let Some(dialog) = self.telegram_dialog.as_mut() {
                dialog.set_test_result(tested.ok, tested.detail);
                self.dirty = true;
            }
        }
    }

    /// Remember that Telegram `message_id` speaks for `session`, so a
    /// native Telegram reply to it can address that session directly
    /// without the operator typing `[name]`. Bounded FIFO: the oldest
    /// link evicts once the cap is reached.
    pub(super) fn record_telegram_reply_target(&mut self, message_id: i64, session: crate::session::SessionId) {
        if self.telegram_reply_targets.len() >= Self::TELEGRAM_REPLY_TARGET_CAP {
            if let Some(oldest) = self.telegram_reply_target_order.pop_front() {
                self.telegram_reply_targets.remove(&oldest);
            }
        }
        self.telegram_reply_targets.insert(message_id, session);
        self.telegram_reply_target_order.push_back(message_id);
    }

    /// Route one allowlisted operator text: `/` commands and errors
    /// answer directly, while `<name>:` prefixes and bare text (to the
    /// last badged session) queue a silent idle-gated injection.
    pub(super) fn route_telegram(&mut self, msg: crate::telegram::InboundMessage) {
        let text = msg.text.trim().to_string();
        if let Some(command) = text.strip_prefix('/') {
            let mut parts = command.split_whitespace();
            let name = parts.next().unwrap_or("").to_lowercase();
            let name = name.split('@').next().unwrap_or("").to_string();
            let rest = parts.collect::<Vec<_>>().join(" ");
            match name.as_str() {
                "sessions" => { self.telegram_send(msg.chat_id, &self.telegram_session_list()); }
                "help" => { self.telegram_send(msg.chat_id, Self::TELEGRAM_HELP); }
                "int" => self.telegram_interrupt(msg.chat_id, &rest),
                "clear" => self.telegram_clear(msg.chat_id, &rest),
                _ => { self.telegram_send(
                    msg.chat_id,
                    &format!("unknown command /{name}; /help lists commands"),
                ); }
            }
            return;
        }
        // A native Telegram "reply" to a message forge can trace back to
        // a still-live session addresses it directly — the natural
        // mobile-UI way to keep talking to the same agent without typing
        // `[name]`. An explicit `[name]` bracket is the more deliberate
        // signal and always wins when both are present.
        let reply_target_name = msg
            .reply_to_message_id
            .and_then(|mid| self.telegram_reply_targets.get(&mid).copied())
            .and_then(|id| self.manager.get(id))
            .filter(|rec| rec.state.is_live())
            .map(|rec| rec.name.clone());
        let (target, body) = match Self::parse_bracketed(&text) {
            Some((name, body)) => (Some(name), body),
            None => (reply_target_name, text.clone()),
        };
        let target = match target {
            Some(name) => Some(name),
            None => {
                // The old `name:` form with a live name hints at
                // brackets instead of landing on the wrong session;
                // anything else falls through to the badge.
                if let Some((head, _)) = text.split_once(':') {
                    let head = head.trim();
                    if !head.is_empty()
                        && !head.contains(char::is_whitespace)
                        && self.resolve_telegram_session(head).is_ok()
                    {
                        self.telegram_send(
                            msg.chat_id,
                            &format!(
                                "address sessions as [{head}] message; /sessions lists live sessions"
                            ),
                        );
                        return;
                    }
                }
                self.last_telegram_badged
                    .and_then(|id| self.manager.get(id))
                    .filter(|rec| rec.state.is_live())
                    .map(|rec| rec.name.clone())
            }
        };
        let Some(name) = target else {
            self.telegram_send(
                msg.chat_id,
                "no session addressed; reply with `[name] message`, or /sessions to list live sessions",
            );
            return;
        };
        let id = match self.resolve_telegram_session(&name) {
            Ok(id) => id,
            Err(e) => {
                self.telegram_send(msg.chat_id, &e);
                return;
            }
        };
        if self.broker.queued(id) >= crate::comms::QUEUE_CAP {
            self.telegram_send(
                msg.chat_id,
                &format!("queue full for {name}; /clear {name} drops pending"),
            );
            return;
        }
        self.telegram_seq += 1;
        let conv = format!("tg-{}", self.telegram_seq);
        self.broker.push(
            id,
            crate::comms::Injection {
                conv,
                kind: crate::comms::InjectKind::Command,
                from: "operator".to_string(),
                text: format!("{body}\n\n({})", Self::TELEGRAM_REPLY_HINT),
            },
        );
        self.telegram_send(msg.chat_id, &format!("queued for {name}"));
        self.dirty = true;
    }

    /// `/int <session>`: Ctrl-C straight to the agent pane. Bypasses the
    /// idle gate on purpose: interrupts must land in a busy session.
    fn telegram_interrupt(&mut self, chat_id: i64, rest: &str) {
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        if name.is_empty() {
            self.telegram_send(chat_id, "usage: /int <session>");
            return;
        }
        let id = match self.resolve_telegram_session(&name) {
            Ok(id) => id,
            Err(e) => {
                self.telegram_send(chat_id, &e);
                return;
            }
        };
        match self.manager.inject_write(id, b"\x03") {
            Ok(()) => self.telegram_send(chat_id, &format!("interrupted {name}")),
            Err(e) => self.telegram_send(chat_id, &format!("cannot interrupt {name}: {e}")),
        };
    }

    /// `/clear <session>`: drop its queued (undelivered) injections and
    /// report the count. Delivered work is untouched.
    fn telegram_clear(&mut self, chat_id: i64, rest: &str) {
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        if name.is_empty() {
            self.telegram_send(chat_id, "usage: /clear <session>");
            return;
        }
        let id = match self.resolve_telegram_session(&name) {
            Ok(id) => id,
            Err(e) => {
                self.telegram_send(chat_id, &e);
                return;
            }
        };
        let dropped = self.broker.clear_queue(id);
        self.telegram_send(chat_id, &format!("cleared {dropped} queued for {name}"));
    }

    /// Settle operator presence for one main-loop pass. Any input
    /// stamps the clock and, when coming back, announces the return;
    /// twenty input-free minutes announce the departure. Both notices
    /// go to every live session with room, exactly once per flip —
    /// exited panes and full queues are skipped, never grown.
    pub fn settle_presence(&mut self, input_this_tick: bool, now: std::time::Instant) {
        if input_this_tick {
            self.last_input = now;
            if self.away {
                self.away = false;
                self.broadcast_presence(Self::BACK_NOTICE);
            }
            return;
        }
        if !self.away && now.duration_since(self.last_input) > Self::AWAY_AFTER {
            self.away = true;
            self.broadcast_presence(Self::AWAY_NOTICE);
        }
    }

    /// Queue `text` as an operator-from-forge injection on every live
    /// session with queue room. Best-effort by design: skipped sessions
    /// simply miss the notice.
    fn broadcast_presence(&mut self, text: &str) {
        let mut pushed = false;
        for &id in self.manager.order() {
            let live = self
                .manager
                .get(id)
                .is_some_and(|rec| rec.state.is_live());
            if !live || self.broker.queued(id) >= crate::comms::QUEUE_CAP {
                continue;
            }
            self.telegram_seq += 1;
            self.broker.push(
                id,
                crate::comms::Injection {
                    conv: format!("presence-{}", self.telegram_seq),
                    kind: crate::comms::InjectKind::Command,
                    from: "forge".to_string(),
                    text: text.to_string(),
                },
            );
            pushed = true;
        }
        if pushed {
            self.dirty = true;
        }
    }

    /// Whether the main loop should start the poller thread: enabled
    /// but not yet running. One-shot by construction — the loop flips
    /// the flag at spawn, so the thread starts exactly once.
    pub fn telegram_poller_wanted(&self) -> bool {
        !self.telegram_poller
            && self
                .telegram_config
                .lock()
                .map(|cfg| cfg.enabled)
                .unwrap_or(false)
    }

    pub fn telegram_sender_wanted(&self) -> bool {
        !self.telegram_sender
            && self
                .telegram_config
                .lock()
                .map(|cfg| cfg.enabled)
                .unwrap_or(false)
    }

    /// Open the Telegram settings form prefilled from the live section.
    pub fn open_telegram_dialog(&mut self) {
        let cfg = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.clone())
            .unwrap_or_default();
        self.telegram_dialog = Some(crate::ui::dialogs::telegram::TelegramDialog::new(&cfg, self.pill_tabs));
        self.dirty = true;
    }

    /// Apply a validated settings submit: optional token-file write
    /// (atomic `0600`), section save to `config.toml`, live-config
    /// swap for the poller thread, then close. Errors leave the dialog
    /// open for the caller to surface.
    pub fn apply_telegram_form(
        &mut self,
        loaded: &mut crate::infra::config::LoadedConfig,
        home: &std::path::Path,
        form: crate::ui::dialogs::telegram::TelegramForm,
    ) -> Result<(), String> {
        if !form.token.is_empty() {
            let path =
                crate::ui::dialogs::telegram::expand_token_path(&form.config.token_file, home);
            crate::infra::fs_atomic::write_private(std::path::Path::new(&path), form.token.as_bytes())
                .map_err(|e| format!("cannot write token file: {e}"))?;
        }
        loaded.config.telegram = form.config.clone();
        loaded
            .save_home(home)
            .map_err(|e| format!("cannot save config: {e}"))?;
        if let Ok(mut live) = self.telegram_config.lock() {
            *live = form.config;
        }
        self.telegram_outbox_wake.notify_all();
        self.telegram_dialog = None;
        self.dirty = true;
        Ok(())
    }

    /// Start a connection test for the open dialog. A dialog-provided
    /// token wins; otherwise the fixed token file reads now and
    /// reports inline. Network runs on a worker thread; the verdict
    /// returns as [`crate::infra::event::AppEvent::TelegramTested`].
    pub fn start_telegram_test(&mut self, token: String) {
        let Some(dialog) = self.telegram_dialog.as_mut() else {
            return;
        };
        let token = if token.is_empty() {
            match crate::telegram::read_token(crate::telegram::TELEGRAM_TOKEN_FILE) {
                Ok(token) => token,
                Err(e) => {
                    dialog.set_test_result(false, e);
                    self.dirty = true;
                    return;
                }
            }
        } else {
            token
        };
        dialog.set_testing(true);
        self.dirty = true;
        let tx = self.telegram_test_tx.clone();
        if std::thread::Builder::new()
            .name("telegram-test".to_string())
            .spawn(move || {
                let tested = match crate::telegram::fetch_me(&token) {
                    Ok(detail) => crate::telegram::TelegramTested { ok: true, detail },
                    Err(_) => crate::telegram::TelegramTested {
                        ok: false,
                        detail: "request failed".to_string(),
                    },
                };
                let _ = tx.send(tested);
            })
            .is_err()
        {
            if let Some(dialog) = self.telegram_dialog.as_mut() {
                dialog.set_test_result(false, "cannot spawn test worker".to_string());
            }
        }
    }
}

pub(super) fn tg_agent(name: &str) -> (AppState, crate::session::SessionId, String) {
    std::env::set_var("CODEX_BIN", "cat");
    let mut state = AppState::new();
    let id = state
        .manager
        .spawn_agent(
            name,
            &std::env::temp_dir(),
            "exec cat",
            crate::infra::ids::RunId::generate(),
            "codex",
        )
        .unwrap();
    std::env::remove_var("CODEX_BIN");
    let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    (state, id, live_run)
}

pub(super) fn tg_inbox(state: &mut AppState, chat: i64, texts: &[&str]) {
    for text in texts {
        state.telegram_inbox.push_back(crate::telegram::InboundMessage {
            user_id: 11,
            chat_id: chat,
            text: text.to_string(),
            reply_to_message_id: None,
        });
    }
}

pub(super) fn tg_inbox_reply(state: &mut AppState, chat: i64, text: &str, reply_to: i64) {
    state.telegram_inbox.push_back(crate::telegram::InboundMessage {
        user_id: 11,
        chat_id: chat,
        text: text.to_string(),
        reply_to_message_id: Some(reply_to),
    });
}

pub(super) fn tg_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "forge-tg-form-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(crate::infra::branding::config_dir(&dir)).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::infra::ids::RunId;

        #[test]
        fn telegram_poll_routes_inbound_and_flags_failure() {
            let mut s = AppState::new();
            s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
                messages: vec![crate::telegram::InboundMessage {
                    user_id: 11,
                    chat_id: 11,
                    text: "hi".to_string(),
                    reply_to_message_id: None,
                }],
                failed: false,
            }));
            assert!(s.telegram_inbox.is_empty(), "owner routes directly without a lossy second queue");
            assert_eq!(s.telegram_outbox.lock().unwrap().len(), 1, "unaddressed text gets guidance");
            assert!(!s.telegram_last_poll_failed);
            s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
                messages: Vec::new(),
                failed: true,
            }));
            assert!(s.telegram_last_poll_failed);
            assert!(s.telegram_inbox.is_empty(), "failures queue nothing");
        }

        #[test]
        fn telegram_poll_routes_directly_without_a_lossy_second_queue() {
            let (mut state, id, _run) = tg_agent("agent");
            state.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
                messages: vec![crate::telegram::InboundMessage {
                    user_id: 11,
                    chat_id: 11,
                    text: "/sessions".to_string(),
                    reply_to_message_id: None,
                }],
                failed: false,
            }));
            assert!(state.telegram_inbox.is_empty());
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("agent"));
            assert!(state.manager.remove(id));
        }

        #[test]
        fn stopped_telegram_workers_are_restartable() {
            let mut state = AppState::new();
            state.telegram_config.lock().unwrap().enabled = true;
            state.telegram_poller = true;
            state.telegram_sender = true;
            state.apply(AppEvent::TelegramWorkerStopped(crate::telegram::WorkerKind::Poller));
            state.apply(AppEvent::TelegramWorkerStopped(crate::telegram::WorkerKind::Sender));
            assert!(state.telegram_poller_wanted());
            assert!(state.telegram_sender_wanted());
        }

        #[test]
        fn telegram_poll_burst_stays_bounded_at_the_outbox() {
            let mut s = AppState::new();
            for i in 0..(crate::telegram::OUTBOX_CAP as i64 + 5) {
                s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
                    messages: vec![crate::telegram::InboundMessage {
                        user_id: i,
                        chat_id: i,
                        text: "/help".to_string(),
                        reply_to_message_id: None,
                    }],
                    failed: false,
                }));
            }
            assert!(s.telegram_inbox.is_empty());
            assert_eq!(s.telegram_outbox.lock().unwrap().len(), crate::telegram::OUTBOX_CAP);
            assert_eq!(s.telegram_outbox_dropped, 5);
            assert!(s.telegram_last_send_failed, "overflow is visible, never silent");
            s.telegram_config.lock().unwrap().enabled = true;
            assert_eq!(s.sidebar_info().telegram, "on · delivery failed");
        }

        #[test]
        fn presence_goes_away_after_twenty_idle_minutes() {
            let (mut state, id, _run) = tg_agent("agent");
            let t0 = std::time::Instant::now();
            state.settle_presence(false, t0 + std::time::Duration::from_secs(19 * 60));
            assert!(!state.away, "nineteen minutes is still here");
            assert_eq!(state.broker.queued(id), 0);
            state.settle_presence(false, t0 + std::time::Duration::from_secs(21 * 60));
            assert!(state.away, "twenty minutes idle is away");
            assert_eq!(state.broker.queued(id), 1);
            let head = state.broker.peek_due(id).expect("away notice queued");
            assert_eq!(head.kind, crate::comms::InjectKind::Command);
            assert!(head.text.contains("user is away"), "body: {}", head.text);
            assert!(head.text.contains("message_user"), "guidance: {}", head.text);
            // Some models read "the user is away" as a stop signal and idle
            // waiting for a reply instead of continuing the task; the notice
            // must explicitly tell them to keep going.
            assert!(
                head.text.contains("keep working") || head.text.contains("continue working"),
                "must tell the model to keep working, not stop and wait: {}",
                head.text
            );
            // Further idle settles never duplicate the notice.
            state.settle_presence(false, t0 + std::time::Duration::from_secs(40 * 60));
            assert_eq!(state.broker.queued(id), 1, "away announced once");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn presence_back_clears_on_input() {
            let (mut state, id, _run) = tg_agent("agent");
            let t0 = std::time::Instant::now();
            state.settle_presence(false, t0 + std::time::Duration::from_secs(21 * 60));
            assert!(state.away);
            state.settle_presence(true, t0 + std::time::Duration::from_secs(22 * 60));
            assert!(!state.away, "any input ends away");
            assert_eq!(state.broker.queued(id), 2);
            state.broker.take_due(id, 1);
            let head = state.broker.peek_due(id).expect("back notice queued");
            assert!(head.text.contains("user is back"), "body: {}", head.text);
            assert!(head.text.contains("don't use message_user"), "stand-down: {}", head.text);
            // Further input is just presence, never another notice.
            state.settle_presence(true, t0 + std::time::Duration::from_secs(23 * 60));
            assert_eq!(state.broker.queued(id), 1, "back announced once");
            state.broker.take_due(id, 1);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn presence_skips_exited_and_full_sessions() {
            std::env::set_var("CODEX_BIN", "cat");
            let mut state = AppState::new();
            let live = state
                .manager
                .spawn_agent(
                    "agent",
                    &std::env::temp_dir(),
                    "exec cat",
                    crate::infra::ids::RunId::generate(),
                    "codex",
                )
                .unwrap();
            let gone = state
                .manager
                .spawn_agent(
                    "gone",
                    &std::env::temp_dir(),
                    "exec cat",
                    crate::infra::ids::RunId::generate(),
                    "codex",
                )
                .unwrap();
            std::env::remove_var("CODEX_BIN");
            state.manager.get_mut(gone).expect("gone").state = crate::session::SessionState::Exited(None);
            // Fill the live session to its queue cap.
            for _ in 0..crate::comms::QUEUE_CAP {
                state.broker.push(
                    live,
                    crate::comms::Injection {
                        conv: "pad".to_string(),
                        kind: crate::comms::InjectKind::Command,
                        from: "pad".to_string(),
                        text: "pad".to_string(),
                    },
                );
            }
            let t0 = std::time::Instant::now();
            state.settle_presence(false, t0 + std::time::Duration::from_secs(21 * 60));
            assert!(state.away, "away still flips");
            assert_eq!(
                state.broker.queued(live),
                crate::comms::QUEUE_CAP,
                "full queues are skipped, never grown"
            );
            assert_eq!(state.broker.queued(gone), 0, "exited panes get nothing");
            state.broker.take_due(live, crate::comms::QUEUE_CAP + 1);
            assert!(state.manager.remove(live));
            assert!(state.manager.remove(gone));
        }

        #[test]
        fn telegram_poller_starts_once_when_enabled() {
            let mut state = AppState::new();
            assert!(!state.telegram_poller_wanted(), "disabled wants nothing");
            state.telegram_config.lock().expect("lock").enabled = true;
            assert!(state.telegram_poller_wanted(), "enabling wants a start");
            state.telegram_poller = true;
            assert!(!state.telegram_poller_wanted(), "started never restarts");
        }

        #[test]
        fn telegram_bare_text_follows_last_badge() {
            let (mut state, id, live_run) = tg_agent("agent");
            let ok = comms_reply(&mut state, &live_run, "message_user", r#"{"message":"ping"}"#);
            assert!(ok.contains(r#""ok":true"#), "badge: {ok}");
            tg_inbox(&mut state, 11, &["pong"]);
            state.drain_telegram();
            assert_eq!(state.broker.queued(id), 1, "operator answers the badge");
            // A colon in free text is not an address: unresolvable heads
            // still follow the badge.
            tg_inbox(&mut state, 11, &["note: buy milk"]);
            state.drain_telegram();
            assert_eq!(state.broker.queued(id), 2, "free text follows the badge");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_bare_text_without_badge_guides() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["hello?"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("/sessions"), "guides to discovery: {}", replies[0].text);
            assert_eq!(state.broker.queued(id), 0, "nothing routed blind");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_clear_drops_queued_injections() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["[agent] one", "[agent] two"]);
            state.drain_telegram();
            assert_eq!(state.broker.queued(id), 2);
            assert_eq!(tg_outbox(&state).len(), 2, "each accepted route is acknowledged");
            tg_inbox(&mut state, 11, &["/clear agent"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("cleared 2"), "reply: {}", replies[0].text);
            assert_eq!(state.broker.queued(id), 0);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_explicit_bracket_still_wins_over_a_reply_link() {
            let (mut state, a, _run_a) = tg_agent("a");
            let b = state
                .manager
                .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
                .unwrap();
            state.apply(AppEvent::TelegramMessageSent { message_id: 501, session: a });
            tg_inbox_reply(&mut state, 11, "[b] ignore the reply link", 501);
            state.drain_telegram();
            assert_eq!(state.broker.queued(a), 0, "explicit [name] overrides the reply link");
            assert_eq!(state.broker.queued(b), 1);
            assert!(state.manager.remove(a));
            assert!(state.manager.remove(b));
        }

        #[test]
        fn telegram_help_names_commands() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["/help"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            for cmd in ["/sessions", "/help", "/int", "/clear"] {
                assert!(replies[0].text.contains(cmd), "help: {}", replies[0].text);
            }
            assert!(replies[0].text.contains("[name]"), "address form: {}", replies[0].text);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_int_interrupts_live_session() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["/int agent"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("interrupted"), "reply: {}", replies[0].text);
            tg_inbox(&mut state, 11, &["/int ghost"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert!(replies[0].text.contains("no live session"), "reply: {}", replies[0].text);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_old_colon_form_hints_at_brackets() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["agent: hi"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("[agent]"), "hint: {}", replies[0].text);
            assert_eq!(state.broker.queued(id), 0, "old form never routes");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_prefixed_text_routes_to_session() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["[agent] do the thing"]);
            state.drain_telegram();
            assert_eq!(state.broker.queued(id), 1);
            let head = state.broker.peek_due(id).expect("queued");
            assert_eq!(head.kind, crate::comms::InjectKind::Command);
            assert_eq!(head.from, "operator");
            assert!(head.text.contains("do the thing"), "body: {}", head.text);
            assert!(head.text.contains("message_user"), "reply guidance: {}", head.text);
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].text, "queued for agent");
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_refuses_when_session_queue_full() {
            let (mut state, id, _run) = tg_agent("agent");
            for _ in 0..crate::comms::QUEUE_CAP {
                state.broker.push(
                    id,
                    crate::comms::Injection {
                        conv: "pad".to_string(),
                        kind: crate::comms::InjectKind::Command,
                        from: "pad".to_string(),
                        text: "pad".to_string(),
                    },
                );
            }
            tg_inbox(&mut state, 11, &["[agent] one more"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("queue full"), "reply: {}", replies[0].text);
            assert_eq!(state.broker.queued(id), crate::comms::QUEUE_CAP, "never past the cap");
            // Drain the padding so the session record can leave cleanly.
            state.broker.take_due(id, crate::comms::QUEUE_CAP + 1);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_reply_routes_directly_to_the_replied_session() {
            let (mut state, a, _run_a) = tg_agent("a");
            let b = state
                .manager
                .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
                .unwrap();
            // "a" forwarded a message_user to Telegram as message 501; the
            // operator never badged "a" as last-addressed (b's spawn, or any
            // other traffic, could have moved that), so only the reply link
            // can route this correctly.
            state.apply(AppEvent::TelegramMessageSent { message_id: 501, session: a });
            state.last_telegram_badged = Some(b);
            tg_inbox_reply(&mut state, 11, "keep going with the refactor", 501);
            state.drain_telegram();
            assert_eq!(state.broker.queued(a), 1, "reply must reach the session that sent 501");
            assert_eq!(state.broker.queued(b), 0, "not the merely last-badged session");
            let queued = state.broker.peek_due(a).expect("queued");
            assert!(queued.text.contains("keep going with the refactor"));
            assert!(state.manager.remove(a));
            assert!(state.manager.remove(b));
        }

        #[test]
        fn telegram_reply_to_unknown_or_exited_session_falls_back_to_badge() {
            let (mut state, a, _run_a) = tg_agent("a");
            state.last_telegram_badged = Some(a);
            // Replying to a message_id forge never recorded (or whose session
            // has since exited) must degrade to the ordinary badge fallback,
            // not silently drop the operator's text.
            tg_inbox_reply(&mut state, 11, "still here?", 999);
            state.drain_telegram();
            assert_eq!(state.broker.queued(a), 1, "falls back to the last-badged session");
            assert!(state.manager.remove(a));
        }

        #[test]
        fn telegram_routed_text_acknowledges_the_queue() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["[agent] do the thing"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].text, "queued for agent");
            assert_eq!(state.broker.queued(id), 1);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_routed_text_asks_for_an_immediate_ack() {
            // A Telegram operator is on their phone, not watching the pane:
            // if the ask will take a moment, the agent should send a quick
            // message_user acknowledging it before diving in, not leave the
            // operator wondering whether it landed.
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["[agent] do the thing"]);
            state.drain_telegram();
            let inj = state.broker.peek_due(id).expect("injection queued");
            assert_eq!(inj.kind, crate::comms::InjectKind::Command);
            let body = String::from_utf8(inj.render_body()).unwrap();
            assert!(body.contains("do the thing"), "body: {body}");
            assert!(
                body.contains("acknowledg") && body.contains("message_user"),
                "guidance must ask for an immediate ack: {body}"
            );
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_sessions_command_lists_live_sessions() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["/sessions"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("agent"), "list: {}", replies[0].text);
            assert_eq!(replies[0].chat_id, 11);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_sessions_command_omits_exited_sessions() {
            let (mut state, id, _run) = tg_agent("gone");
            assert!(state.manager.remove(id));
            tg_inbox(&mut state, 11, &["/sessions"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(!replies[0].text.contains("gone"), "only live sessions: {}", replies[0].text);
            assert!(replies[0].text.contains("(none)"));
        }

        #[test]
        fn telegram_unknown_command_errors() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["/bogus"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("unknown command"), "reply: {}", replies[0].text);
            assert!(state.manager.remove(id));
        }

        #[test]
        fn telegram_unknown_session_errors_without_queueing() {
            let (mut state, id, _run) = tg_agent("agent");
            tg_inbox(&mut state, 11, &["[ghost] hi"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("no live session"), "reply: {}", replies[0].text);
            assert_eq!(state.broker.queued(id), 0);
            assert!(state.manager.remove(id));
            // Replying after exit names the same dead end, never a live pane.
            tg_inbox(&mut state, 11, &["[agent] hi"]);
            state.drain_telegram();
            let replies = tg_outbox(&state);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].text.contains("no live session"), "reply: {}", replies[0].text);
        }

        #[test]
        fn telegram_reply_target_map_stays_bounded() {
            let mut state = AppState::new();
            for i in 0..(crate::app::AppState::TELEGRAM_REPLY_TARGET_CAP as i64 + 5) {
                state.apply(AppEvent::TelegramMessageSent {
                    message_id: i,
                    session: crate::session::SessionId::fresh(),
                });
            }
            assert_eq!(
                state.telegram_reply_targets.len(),
                crate::app::AppState::TELEGRAM_REPLY_TARGET_CAP,
                "oldest entries must evict, never grow past the cap"
            );
            assert!(
                !state.telegram_reply_targets.contains_key(&0),
                "message 0 is the oldest and must be the first evicted"
            );
        }

        #[test]
        fn telegram_form_save_writes_token_and_lives() {
            let home = tg_home();
            let path = crate::infra::branding::config_file(&home);
            let mut loaded = crate::infra::config::LoadedConfig::load(&path).expect("defaults load");
            let token_path = home.join("tg.token");
            let mut state = AppState::new();
            state.telegram_dialog = Some(crate::ui::dialogs::telegram::TelegramDialog::new(
                &crate::infra::config::TelegramConfig::default(),
                true,
            ));
            let form = crate::ui::dialogs::telegram::TelegramForm {
                config: crate::infra::config::TelegramConfig {
                    enabled: true,
                    token_file: token_path.to_string_lossy().into_owned(),
                    allowed_user_ids: vec![11],
                    notify_chat_id: 11,
                    poll_seconds: 20,
                    backoff_min_seconds: 60,
                    backoff_max_seconds: 900,
                },
                token: "0123456789abcdef0123456789abcdef".to_string(),
            };
            state.apply_telegram_form(&mut loaded, &home, form).expect("save applies");
            assert_eq!(
                std::fs::read_to_string(&token_path).expect("token written"),
                "0123456789abcdef0123456789abcdef"
            );
            assert!(loaded.config.telegram.enabled);
            assert_eq!(loaded.config.telegram.allowed_user_ids, vec![11]);
            let live = state.telegram_config.lock().expect("live config");
            assert!(live.enabled, "poll thread sees the save at once");
            assert!(state.telegram_dialog.is_none(), "save closes the dialog");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&token_path).expect("token metadata").permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "token file is owner-only");
            }
            let _ = std::fs::remove_dir_all(&home);
        }

        #[test]
        fn telegram_tested_lands_in_open_dialog() {
            let mut state = AppState::new();
            state.telegram_dialog = Some(crate::ui::dialogs::telegram::TelegramDialog::new(
                &crate::infra::config::TelegramConfig::default(),
                true,
            ));
            state
                .telegram_test_tx
                .send(crate::telegram::TelegramTested {
                    ok: true,
                    detail: "@forkbot".to_string(),
                })
                .expect("test channel accepts");
            state.drain_telegram_test();
            let dialog = state.telegram_dialog.as_ref().expect("dialog stays open");
            assert_eq!(
                dialog.test_result(),
                Some((true, "@forkbot".to_string())),
                "result surfaces in the form"
            );
            // No dialog: the result drops without panic.
            state.telegram_dialog = None;
            state
                .telegram_test_tx
                .send(crate::telegram::TelegramTested {
                    ok: false,
                    detail: "request failed".to_string(),
                })
                .expect("test channel accepts");
            state.drain_telegram_test();
        }

        #[test]
        fn telegram_test_with_unreadable_token_reports_inline() {
            // The fixed token path resolves against HOME: point it at a
            // scratch dir with no token file so the read fails inline.
            let prior = std::env::var("HOME").ok();
            let scratch = std::env::temp_dir().join(format!("forge-tg-nofile-{}", std::process::id()));
            std::fs::create_dir_all(&scratch).unwrap();
            std::env::set_var("HOME", &scratch);
            let mut state = AppState::new();
            state.telegram_dialog = Some(crate::ui::dialogs::telegram::TelegramDialog::new(
                &crate::infra::config::TelegramConfig::default(),
                true,
            ));
            state.start_telegram_test(String::new());
            let dialog = state.telegram_dialog.as_ref().expect("dialog stays open");
            assert!(
                dialog.test_result().is_some_and(|(ok, detail)| !ok && detail.contains("unreadable")),
                "inline failure, no thread: {:?}",
                dialog.test_result()
            );
            match prior {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
}
