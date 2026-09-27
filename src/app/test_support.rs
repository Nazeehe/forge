//! Shared AppState test fixtures: cross-group call helpers.

use super::*;
use crate::infra::event::AppEvent;

pub(super) fn comms_reply(state: &mut AppState, run: &str, tool: &str, args: &str) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    state.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
        run_id: run.to_string(),
        tool: tool.to_string(),
        args: args.to_string(),
        reply: tx,
        claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
    }));
    rx.recv().expect("comms verdict arrives")
}

pub(super) fn message_user_agent() -> (AppState, crate::session::SessionId, String) {
    std::env::set_var("CODEX_BIN", "cat");
    let mut state = AppState::new();
    let id = state
        .manager
        .spawn_agent(
            "agent",
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

pub(super) fn hook_request(hook: &str, run_id: &str, body: &str) -> AppEvent {
    let (reply_tx, _) = std::sync::mpsc::channel();
    AppEvent::HookRequest(crate::ipc::listener::HookRequest {
        hook: hook.to_string(),
        body: body.to_string(),
        run_id: run_id.to_string(),
        sync: false,
        reply: reply_tx,
        timed_out: Default::default(),
    })
}

pub(super) fn tg_outbox(state: &AppState) -> Vec<crate::telegram::OutboundMessage> {
    let mut guard = state.telegram_outbox.lock().expect("outbox unlocks");
    let mut out = Vec::new();
    while let Some(m) = guard.pop_front() {
        out.push(m);
    }
    out
}
