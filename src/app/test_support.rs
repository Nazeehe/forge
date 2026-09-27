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

pub(super) fn spawn_visual_agent(state: &mut AppState, name: &str) -> (crate::session::SessionId, String) {
    let id = state.manager.spawn_agent(
        name, &std::env::temp_dir(), "exec cat",
        crate::infra::ids::RunId::generate(), "codex",
    ).unwrap();
    let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    (id, run)
}

#[cfg(feature = "visual")]
pub(super) fn fake_png(len: usize, w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A,
        0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.resize(len.max(24), 0);
    v
}

#[cfg(feature = "visual")]
pub(super) fn complete(
    state: &mut AppState,
    session: crate::session::SessionId,
    generation: u64,
    png: Vec<u8>,
) {
    let (width, height) = crate::visual::png_dimensions(&png).unwrap();
    state.visual_complete(crate::app::VisualDone {
        session,
        generation,
        title: String::new(),
        alt: String::new(),
        result: Ok(crate::visual::RasterFrame {
            png,
            rgba: Vec::new(),
            width,
            height,
            shapes: Vec::new(),
            vb: [0.0, 0.0, width as f32, height as f32],
        }),
    });
}

#[cfg(feature = "visual")]
pub(super) fn show_visual_overlay(state: &mut AppState, id: crate::session::SessionId) {
    // Selection clears the overlay, so focus first, then open it.
    state.select_session(0);
    let slot = state.visual_slot(id).expect("visual slot exists");
    state.overlay_view = Some((id, slot));
}

#[cfg(feature = "visual")]
pub(super) fn paint_for(state: &AppState, id: crate::session::SessionId) -> crate::visual::VisualPaint {
    state
        .visual_paint(id, ratatui::layout::Rect::new(0, 0, 200, 50), 8.0, 16.0)
        .expect("paint")
}
