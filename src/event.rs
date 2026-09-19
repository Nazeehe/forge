//! Typed cross-thread event protocol.
//!
//! Workers never mutate `AppState` directly; they emit `AppEvent` values
//! through MPSC. If a new background producer cannot express its result as
//! one of these variants, the protocol — not the locking — must change.

use crossterm::event::Event as TermEvent;

use crate::session::SessionId;

#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Input(TermEvent),
    Resize(u16, u16),
    /// A pane produced output. Carries no bytes: the vt100 screen is
    /// already updated by the reader thread before this fires (see
    /// `pty::PtyPane::spawn_reader`), and `AppState::apply` only ever uses
    /// this to flip `dirty` — forwarding the chunk here would just be an
    /// allocation nobody reads.
    SessionOutput { id: SessionId },
    SessionExited { id: SessionId, code: Option<i32> },
    /// A synchronous hook record from the IPC listener. Policy sends the
    /// one-line decision through `reply`; dropping it leaves the relay to
    /// time out fail-open.
    HookRequest(crate::listener::HookRequest),
    /// A comms tool call from `mcp-serve`. The broker answers at once; the
    /// one-line verdict travels back through `reply`.
    CommsRequest(crate::listener::CommsRequest),
    /// An external bot call from the IPC listener. Auth is the client
    /// credential in the transport envelope; the broker answers at once
    /// and the handler relays the one-line verdict with a typed error
    /// object on failure.
    BotRequest(crate::listener::BotRequest),
    /// Telegram long-poll results. The poller thread sends only when
    /// there is something to act on (fresh inbound text or a poll
    /// failure); quiet polls stay silent so the loop never wakes.
    TelegramPoll(crate::telegram::PollReport),
    /// Outbound worker health and a terminal retry-budget drop.
    TelegramSendStatus { failed: bool, dropped: bool },
    /// A transport worker exited or panicked; the owner clears its live flag
    /// so the next loop turn can supervise a replacement.
    TelegramWorkerStopped(crate::telegram::WorkerKind),
    Shutdown,
}

impl AppEvent {
    pub fn from_pty(id: SessionId, ev: crate::pty::PtyEvent) -> Self {
        match ev {
            crate::pty::PtyEvent::Output(_) => AppEvent::SessionOutput { id },
            crate::pty::PtyEvent::Exited(code) => AppEvent::SessionExited { id, code },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn events_cross_threads() {
        // The whole protocol must travel over MPSC from worker threads.
        assert_send::<AppEvent>();
    }

    #[test]
    fn from_pty_maps_both_kinds() {
        use crate::pty::PtyEvent;
        let id = SessionId::fresh();
        match AppEvent::from_pty(id, PtyEvent::Output(vec![9])) {
            AppEvent::SessionOutput { id: got } => {
                assert_eq!(got, id);
            }
            _ => panic!("wrong variant"),
        }
        match AppEvent::from_pty(id, PtyEvent::Exited(None)) {
            AppEvent::SessionExited { id: got, code } => {
                assert_eq!(got, id);
                assert_eq!(code, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn telegram_poll_carries_messages_and_failure() {
        let poll = AppEvent::TelegramPoll(crate::telegram::PollReport {
            messages: vec![crate::telegram::InboundMessage {
                user_id: 11,
                chat_id: 11,
                text: "hi".to_string(),
                reply_to_message_id: None,
            }],
            failed: false,
        });
        match poll {
            AppEvent::TelegramPoll(report) => {
                assert_eq!(report.messages.len(), 1);
                assert!(!report.failed);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn variants_carry_their_payload() {
        let id = SessionId::fresh();
        let out = AppEvent::SessionOutput { id };
        match out {
            AppEvent::SessionOutput { id: got } => assert_eq!(got, id),
            _ => panic!("wrong variant"),
        }
        let exited = AppEvent::SessionExited { id, code: Some(3) };
        match exited {
            AppEvent::SessionExited { code, .. } => assert_eq!(code, Some(3)),
            _ => panic!("wrong variant"),
        }
    }
}
