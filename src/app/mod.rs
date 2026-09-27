//! Single-owner application state.
//!
//! The main loop (and, in tests, the harness directly) reduces every
//! `AppEvent` through [`AppState::apply`]. Workers never touch this struct;
//! dirty-flag rendering and shutdown flow out of the same reduction.

use crate::infra::event::AppEvent;
use crate::session::SessionManager;

pub mod board;
pub mod board_tools;
pub mod dialogs;
pub mod sessions;
pub mod telegram;
pub mod settle;
pub mod views;
pub mod tools;
pub mod visual;
pub mod walkthrough;
#[cfg(test)]
mod test_support;

/// Restore outcome counts for the status line.
pub struct RestoreReport {
    pub spawned: usize,
    pub skipped: Vec<String>,
}

pub struct AppState {
    pub manager: SessionManager,
    pub dirty: bool,
    pub should_quit: bool,
    pub term_size: (u16, u16),
    /// Hook records awaiting a policy decision. Bounded: beyond the cap
    /// newcomers are dropped and their relays fail open on timeout.
    pub pending_hooks: std::collections::VecDeque<crate::ipc::listener::HookRequest>,
    /// Open create-session dialog, if any. Captures all input while present.
    /// Telegram inbound texts awaiting routing (Phase 5 drains this).
    /// Bounded by [`crate::telegram::INBOX_CAP`]; overflow counts in
    /// `telegram_dropped` instead of growing the owner.
    pub telegram_inbox: std::collections::VecDeque<crate::telegram::InboundMessage>,
    /// Inbound Telegram texts dropped past the inbox cap.
    pub telegram_dropped: u64,
    /// The last Telegram poll failed (transport or parse). Cleared by
    /// the next successful poll.
    pub telegram_last_poll_failed: bool,
    /// Live Telegram transport config, shared by handle with the
    /// poller thread: modal saves land within one poll turn, and the
    /// token file itself is re-read every turn, so rotation and edits
    /// bite at once without a restart.
    pub telegram_config: std::sync::Arc<std::sync::Mutex<crate::infra::config::TelegramConfig>>,
    /// Open Telegram settings form (`Ctrl-b m`), if any.
    pub telegram_dialog: Option<crate::ui::dialogs::telegram::TelegramDialog>,
    /// Connection-test results from the worker thread (see
    /// [`Self::start_telegram_test`]).
    telegram_test_tx: std::sync::mpsc::Sender<crate::telegram::TelegramTested>,
    telegram_test_rx: std::sync::mpsc::Receiver<crate::telegram::TelegramTested>,
    /// Per-session `message_user` sliding rate windows, pruned on entry.
    message_user_windows: std::collections::HashMap<
        crate::session::SessionId,
        std::collections::VecDeque<std::time::Instant>,
    >,
    /// Latest in-app `message_user` badge text per session. Always
    /// records, even when forwarding is off.
    pub message_user_badges: std::collections::HashMap<crate::session::SessionId, String>,
    /// Blocked-only attention flags from `request_attention`: reason plus
    /// raise time, one entry per session at most. Focusing the session
    /// acknowledges it; removal and exit drop it like the other maps.
    pub attention_flags: std::collections::HashMap<
        crate::session::SessionId,
        (String, std::time::Instant),
    >,
    /// Fleet router cursor (session id, stable across resorting) and
    /// scroll offset into the sorted fleet rows.
    pub fleet_cursor: Option<crate::session::SessionId>,
    pub fleet_scroll: usize,
    /// Replies for the dedicated Telegram sender, shared by handle.
    /// Bounded by [`crate::telegram::OUTBOX_CAP`]; overflow counts in
    /// `telegram_outbox_dropped` instead of growing the owner.
    pub telegram_outbox: std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<crate::telegram::OutboundMessage>>,
    >,
    /// Wakes the dedicated sender immediately when a reply is queued.
    pub telegram_outbox_wake: std::sync::Arc<std::sync::Condvar>,
    /// Telegram replies dropped past the outbox cap.
    telegram_outbox_dropped: u64,
    telegram_last_send_failed: bool,
    /// Session most recently badged by `message_user`: bare operator
    /// text answers it. `None` until the first badge.
    last_telegram_badged: Option<crate::session::SessionId>,
    /// Telegram `message_id` of every delivered `message_user` forward,
    /// mapped to the session it spoke for. A native Telegram reply to one
    /// of these addresses that session directly, insertion order so the
    /// oldest entry evicts first once [`Self::TELEGRAM_REPLY_TARGET_CAP`]
    /// is reached.
    telegram_reply_targets: std::collections::HashMap<i64, crate::session::SessionId>,
    telegram_reply_target_order: std::collections::VecDeque<i64>,
    /// Poller thread running. Set once at spawn; the main loop starts
    /// the thread the first pass it sees Telegram enabled, so enabling
    /// from the modal needs no restart.
    pub telegram_poller: bool,
    /// Dedicated outbound worker; independent of the blocking long poll.
    pub telegram_sender: bool,
    /// Operator presence: false while input flows. Twenty input-free
    /// minutes flip it (see [`Self::settle_presence`]); any input flips
    /// it back.
    away: bool,
    /// Last observed operator input (key, mouse, or paste). The
    /// presence clock, not wall time, so tests drive it exactly.
    last_input: std::time::Instant,
    /// Operator-injection sequence: each routed text carries a unique
    /// `tg-{n}` conversation tag for traceability.
    telegram_seq: u64,
    /// `message_user` idempotency replays, one bounded cache shared by
    /// all sessions (keys carry their session).
    message_user_idem: crate::comms::bot::IdemCache,
    pub create_dialog: Option<crate::ui::dialogs::create::CreateDialog>,
    /// Open group-management dialog, if any. Captures all input while
    /// present; never both dialogs at once (openers are unreachable
    /// behind the other dialog).
    pub group_dialog: Option<crate::ui::dialogs::groups::GroupDialog>,
    /// Startup restore picker, if a sessions file offered entries. First
    /// input goes here until it resolves to a pick or a fresh start.
    pub restore_picker: Option<crate::session::checkpoint::RestorePicker>,
    /// Generic Yes/No confirmation modal (quit forge, kill a session).
    /// Captures all input while present; No is the default.
    pub confirm: Option<crate::ui::dialogs::quit::Confirm>,
    /// First-run setup dialog, if this boot found no `~/.forge`.
    /// Captures all input while present, above every other modal.
    pub oobe_dialog: Option<crate::ui::dialogs::oobe::OobeDialog>,
    /// "Saving sessions..." modal, shown after Yes while the quit
    /// snapshot persists. The loop saves, then exits.
    pub quit_saving: bool,
    /// Open theme picker (`Ctrl-b e`), if any. Captures all input
    /// while present like every other modal.
    pub theme_dialog: Option<crate::ui::dialogs::theme::ThemeDialog>,
    /// Directory scanned for `theme.json` files when the picker opens.
    /// `None` means builtin only (tests); production sets this from
    /// `~/.forge/themes` at startup.
    pub themes_dir: Option<std::path::PathBuf>,
    /// Live permission mode. The TUI loop rebuilds policy and persists the
    /// config whenever this diverges from the loaded one.
    pub permission_mode: crate::infra::config::PermissionMode,
    /// Cross-session message broker (Phase 4): groups, conversations, queues.
    pub broker: crate::comms::Broker,
    /// Last human key/paste per session. Injections wait out a short
    /// debounce after typing so they never interleave with user input.
    /// Per target: typing in one pane never starves another.
    pub last_human_input: std::collections::HashMap<crate::session::SessionId, std::time::Instant>,
    /// Last courtesy/timer sweep. `Broker::tick` scans every conversation
    /// and timer, so `settle_comms` runs it at most once per
    /// [`crate::comms::BROKER_TICK_INTERVAL`]; queue delivery below stays
    /// per-tick. `None` forces the next sweep (boot, tests).
    pub last_broker_tick: Option<std::time::Instant>,
    /// Last hook verdict or queued hook request, per attributed session.
    /// Injections wait out [`crate::comms::INJECT_HOOK_DEBOUNCE`] after
    /// hook activity so a body never races a mid-tool-use verdict into
    /// the same pane. Per target: hooks from a busy session never hold
    /// another session's delivery. Unattributed runs stamp nothing —
    /// with no pane to protect there is no race to debounce.
    pub last_hook_activity: std::collections::HashMap<crate::session::SessionId, std::time::Instant>,
    /// `~/.forge/hooks.log` when hook tracing is on; None in tests.
    pub hook_trace: Option<std::path::PathBuf>,
    /// `~/.forge/comms.log` when comms tracing is on; None in tests.
    /// Every send verdict, delivery, hold, and drop lands here so a
    /// message that never arrives can be debugged after the fact.
    pub comms_trace: Option<std::path::PathBuf>,
    /// Sessions owed a staged Enter: an injection body went out and its CR
    /// follows after [`crate::comms::INJECT_ENTER_DELAY`], one entry per
    /// session. Later bodies stay queued until the staged CR lands, so
    /// each body submits as its own input event. The entry remembers
    /// which conversation and kind went out, so human input that kills
    /// the staged submit can tell the sender loudly; walkthrough
    /// prompts stage with `None` (no sender to tell).
    pub pending_enter: std::collections::HashMap<
        crate::session::SessionId,
        (
            std::time::Instant,
            Option<(String, crate::comms::InjectKind)>,
        ),
    >,
    /// When each session's last staged Enter went out. The next body
    /// waits until a newer hook reports the turn it started, or
    /// [`crate::comms::INJECT_TURN_GRACE`] passes. Pruned with
    /// `pending_enter`, so it stays bounded by live sessions.
    pub enter_sent: std::collections::HashMap<crate::session::SessionId, std::time::Instant>,
    /// Last hold reason logged per session, and when: comms.log repeats
    /// an unchanged hold only every [`crate::comms::HOLD_LOG_REMINDER`].
    /// Cleared on delivery; pruned with `pending_enter`.
    pub hold_logged:
        std::collections::HashMap<crate::session::SessionId, (String, std::time::Instant)>,
    /// One read-only chrome view, scoped to the currently focused session.
    pub overlay_view: Option<(crate::session::SessionId, usize)>,
    /// Live walkthroughs by session. Entered agent-side through the
    /// walkthrough_* tools; the overlay opens on start and the human
    /// steps through with j/k, asking with Enter.
    pub walkthroughs: std::collections::HashMap<crate::session::SessionId, crate::walkthrough::Walkthrough>,
    /// Grid mode (`Ctrl-b w`): the main area tiles every session in
    /// framed cells instead of showing only the focused one.
    pub grid_mode: bool,
    /// Global kanban view (`Ctrl-b b`, sidebar Kanban button): the main
    /// area shows workspace boards instead of sessions. Never scoped to
    /// a session — boards outlive every session.
    pub board_open: bool,
    /// Sidebar Tetris (`Ctrl-b r`, sidebar Tetris button): the sidebar
    /// list region shows a playable game instead of the fleet. The game
    /// is ephemeral — never persisted, never scoped to a session.
    pub tetris_open: bool,
    /// The live Tetris well. Ticks only while `tetris_open`; toggling
    /// away keeps the game so waiting is never lost.
    pub tetris: crate::tetris::TetrisGame,
    /// Last gravity drop; `None` arms the timer without stepping.
    pub tetris_last_drop: Option<std::time::Instant>,
    /// Workspace kanban boards, mutated by the human and by MCP tools.
    pub boards: crate::kanban::board::BoardStore,
    /// Boards changed since the last atomic save; the TUI loop flushes.
    pub boards_dirty: bool,
    /// Board selection: picked board plus column/card cursor.
    pub board_focus: BoardFocus,
    /// Open board text entry; `None` outside draft mode.
    pub board_draft: Option<BoardDraft>,
    /// Full-field card editor modal (`e`); `None` when closed.
    /// Captures all board input while present, like every other modal.
    pub card_edit: Option<crate::ui::dialogs::card_edit::CardEditDialog>,
    /// One-line board notice (corrupt save quarantined, ...), shown in
    /// the board footer until dismissed by opening the board.
    pub board_notice: Option<String>,
    /// Which-key hotkey HUD (`Ctrl-b` pause shows it, `Ctrl-b ?` pins
    /// it). Pure display state: dispatch authority stays in the input
    /// router, timing ticks in the TUI loop.
    pub whichkey: crate::ui::whichkey::WhichKeyHud,
    /// Monotonic visual generation: every accepted `visual_show` takes
    /// the next number so stale renders never win. U3 scopes this per
    /// session with the raster state; the global counter stays as the
    /// tiebreak source.
    pub visual_seq: u64,
    /// Rounded pill buttons everywhere; mirrors the config flag at startup.
    pub pill_tabs: bool,
    /// Sticky per-session visuals: the newest completed generation per
    /// session. Raster failures drop the frame; the accepted verdict
    /// stands and the previous frame (if any) stays put.
    #[cfg(feature = "visual")]
    pub visual_slots: std::collections::HashMap<crate::session::SessionId, VisualSlot>,
    /// Oldest-first recency for eviction; touched on every store.
    #[cfg(feature = "visual")]
    pub visual_lru: std::collections::VecDeque<crate::session::SessionId>,
    /// Image budget, tunable in tests; production defaults below.
    #[cfg(feature = "visual")]
    pub visual_budget_bytes: usize,
    /// Slot count cap, tunable in tests; production defaults below.
    #[cfg(feature = "visual")]
    pub visual_budget_count: usize,
    /// Background raster completions; drained once per main-loop pass.
    #[cfg(feature = "visual")]
    visual_tx: std::sync::mpsc::Sender<VisualDone>,
    /// Drain end of the raster channel; see `drain_visual`.
    #[cfg(feature = "visual")]
    visual_rx: std::sync::mpsc::Receiver<VisualDone>,
    /// Terminal image on screen now; see `visual_take_show`.
    #[cfg(feature = "visual")]
    pub visual_shown: Option<VisualShown>,
    /// Placement id source; monotonic so retransmits never collide.
    #[cfg(feature = "visual")]
    visual_image_seq: u32,
}

/// Terminal image currently on screen, if any. The TUI writes the
/// transmit/delete escapes; this only tracks what the screen holds so
/// repeats and orphans never happen.
#[cfg(feature = "visual")]
pub struct VisualShown {
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub image_id: u32,
    pub paint: crate::visual::VisualPaint,
}

/// Claim for one transmit: the TUI positions the cursor and writes it.
#[cfg(feature = "visual")]
pub struct VisualShowSpec {
    pub image_id: u32,
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub paint: crate::visual::VisualPaint,
}

/// One finished background raster, headed for a session slot.
#[cfg(feature = "visual")]
pub struct VisualDone {
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub title: String,
    pub alt: String,
    pub result: Result<crate::visual::RasterFrame, String>,
}

/// The newest completed visual for one session.
#[cfg(feature = "visual")]
pub struct VisualSlot {
    pub generation: u64,
    pub png: Vec<u8>,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub alt: String,
    /// Zoom factor for the Visual tab viewport: 1.0 scales the whole
    /// frame to fill the tab, higher zooms scrollable overflow.
    pub zoom: f32,
    /// Viewport origin in displayed cells into the zoomed frame.
    pub scroll_x: u16,
    pub scroll_y: u16,
    /// Selectable shapes in SVG units plus the SVG viewBox
    /// `[x, y, w, h]` they are measured in; empty shapes when the
    /// layout produced none.
    pub shapes: Vec<crate::visual::ShapeBox>,
    pub vb: [f32; 4],
    /// Selected shape index into `shapes`, if the human clicked one.
    /// Boxes live in zoom-independent SVG units, so the selection
    /// stays glued to its shape across zoom and scroll.
    pub selected: Option<usize>,
    /// Unsent ask-about-shape draft, typed in input mode.
    pub draft: Option<String>,
    /// Ask-row input mode: Enter focuses it, Enter submits, Esc
    /// leaves it. Outside input mode typing stays dead so `+`/`-`
    /// keep zooming.
    pub input_active: bool,
    /// Chat footer visibility: selecting a shape opens it, the strip
    /// button or `c` flips it, a new frame closes it.
    pub chat_open: bool,
    /// History rows scrolled up from the tail (0 shows latest).
    pub chat_scroll: u16,
    /// Asked questions about shapes with their answers, oldest first.
    pub questions: Vec<crate::visual::VisualQuestion>,
}

/// Production image budget: decoded bytes across all slots.
#[cfg(feature = "visual")]
pub const DEFAULT_VISUAL_BUDGET_BYTES: usize = 48 * 1024 * 1024;

/// Production slot cap: sticky visuals per session count.
#[cfg(feature = "visual")]
pub const DEFAULT_VISUAL_BUDGET_COUNT: usize = 8;

/// Overlay slot past the PTY tabs: Visual, Walkthrough. Agent
/// sessions always carry exactly three PTY tabs, so absolute indices
/// stay stable (3/4); other overlays are unreachable on single-tab
/// shells by the same gate as the topbar.
pub const OVERLAY_TABS: [&str; 2] = ["Visual", "Walkthrough"];

/// Kanban selection: picked board (by id — names rename) plus the
/// column cursor and the card cursor inside that column.
#[derive(Clone, Debug, Default)]
pub struct BoardFocus {
    pub board: Option<String>,
    pub column: usize,
    pub card: usize,
}

/// Draft input cap: titles cap at 200 chars, so the buffer never
/// holds more than a title can keep.
pub const BOARD_DRAFT_MAX: usize = 200;

/// What an open board draft submits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardDraftAction {
    NewBoard,
    NewCard,
    EditTitle,
}

/// Open text entry on the board: prompt, buffer, and what Enter
/// submits. Mirrors the walkthrough ask draft — typing mode takes
/// text, Enter submits, Esc cancels, everything else is swallowed.
#[derive(Clone, Debug)]
pub struct BoardDraft {
    pub prompt: &'static str,
    pub buffer: String,
    pub action: BoardDraftAction,
}

impl AppState {
    pub fn new() -> Self {
        #[cfg(feature = "visual")]
        let (visual_tx, visual_rx) = std::sync::mpsc::channel();
        let (telegram_test_tx, telegram_test_rx) = std::sync::mpsc::channel();
        AppState {
            manager: SessionManager::new(),
            dirty: true,
            should_quit: false,
            term_size: (24, 80),
            pending_hooks: std::collections::VecDeque::new(),
            telegram_inbox: std::collections::VecDeque::new(),
            telegram_dropped: 0,
            telegram_last_poll_failed: false,
            telegram_config: std::sync::Arc::new(std::sync::Mutex::new(
                crate::infra::config::TelegramConfig::default(),
            )),
            telegram_dialog: None,
            telegram_test_tx,
            telegram_test_rx,
            message_user_windows: std::collections::HashMap::new(),
            message_user_badges: std::collections::HashMap::new(),
            attention_flags: std::collections::HashMap::new(),
            fleet_cursor: None,
            fleet_scroll: 0,
            message_user_idem: crate::comms::bot::IdemCache::new(),
            telegram_outbox: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            telegram_outbox_wake: std::sync::Arc::new(std::sync::Condvar::new()),
            telegram_outbox_dropped: 0,
            telegram_last_send_failed: false,
            last_telegram_badged: None,
            telegram_reply_targets: std::collections::HashMap::new(),
            telegram_reply_target_order: std::collections::VecDeque::new(),
            telegram_poller: false,
            telegram_sender: false,
            away: false,
            last_input: std::time::Instant::now(),
            telegram_seq: 0,
            create_dialog: None,
            group_dialog: None,
            restore_picker: None,
            confirm: None,
            oobe_dialog: None,
            quit_saving: false,
            theme_dialog: None,
            themes_dir: None,
            permission_mode: crate::infra::config::PermissionMode::Yolo,
            broker: crate::comms::Broker::new(),
            last_human_input: std::collections::HashMap::new(),
            last_broker_tick: None,
            last_hook_activity: std::collections::HashMap::new(),
            hook_trace: crate::infra::logging::hook_trace_path(),
            comms_trace: crate::infra::logging::comms_trace_path(),
            pending_enter: std::collections::HashMap::new(),
            enter_sent: std::collections::HashMap::new(),
            hold_logged: std::collections::HashMap::new(),
            overlay_view: None,
            walkthroughs: std::collections::HashMap::new(),
            grid_mode: false,
            board_open: false,
            tetris_open: false,
            tetris: crate::tetris::TetrisGame::new(),
            tetris_last_drop: None,
            boards: crate::kanban::board::BoardStore::new(),
            boards_dirty: false,
            board_focus: BoardFocus::default(),
            board_draft: None,
            card_edit: None,
            board_notice: None,
            whichkey: crate::ui::whichkey::WhichKeyHud::new(),
            visual_seq: 0,
            pill_tabs: true,
            #[cfg(feature = "visual")]
            visual_slots: std::collections::HashMap::new(),
            #[cfg(feature = "visual")]
            visual_lru: std::collections::VecDeque::new(),
            #[cfg(feature = "visual")]
            visual_budget_bytes: DEFAULT_VISUAL_BUDGET_BYTES,
            #[cfg(feature = "visual")]
            visual_budget_count: DEFAULT_VISUAL_BUDGET_COUNT,
            #[cfg(feature = "visual")]
            visual_tx,
            #[cfg(feature = "visual")]
            visual_rx,
            #[cfg(feature = "visual")]
            visual_shown: None,
            #[cfg(feature = "visual")]
            visual_image_seq: 0,
        }
    }

    /// Operator command help surfaced by `/help`.
    const TELEGRAM_HELP: &'static str = "forge operator commands:\n/sessions — list live sessions\n/help — this help\n/int <session> — interrupt the agent (Ctrl-C)\n/clear <session> — drop queued injections\nAddress a session with `[name] message`; bare text answers the last badged session.";

    /// Operator reply guidance appended to every routed injection. The
    /// operator is on their phone, not watching the pane, so a request
    /// that will take more than a moment gets a quick message_user ack
    /// first — otherwise there is no signal it even landed until the
    /// (possibly much later) real reply.
    const TELEGRAM_REPLY_HINT: &'static str = "Reply to the operator with message_user. If this will take more than a moment, first send a brief message_user acknowledging the ask before you start working on it.";

    /// Most `message_user` forwards remembered for reply-link routing.
    /// `message_user` is already capped at
    /// [`crate::telegram::MESSAGE_USER_CAP`] per session per minute, so this
    /// comfortably covers every session an operator could plausibly still
    /// want to reply to; past it the oldest link evicts first.
    pub const TELEGRAM_REPLY_TARGET_CAP: usize = 256;

    /// Input-free minutes before the operator counts as away.
    const AWAY_AFTER: std::time::Duration = std::time::Duration::from_secs(20 * 60);

    /// Away notice, injected into every live session on the flip. Some
    /// models read "the user is away" as a stop signal and idle waiting
    /// for a reply instead of continuing the task, so this spells out
    /// that away means keep going, not pause.
    const AWAY_NOTICE: &'static str = "user is away at the moment; keep working on whatever you were doing rather than waiting for a reply, and use message_user if you need to update them or ask something";
    /// Back notice, injected into every live session on return.
    const BACK_NOTICE: &'static str =
        "user is back, don't use message_user - communicate normally";

    /// Reduce one event. Input routing arrives with the input slice; until
    /// then input events are acknowledged but change nothing.
    pub fn apply(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Tick => {}
            AppEvent::Shutdown => self.should_quit = true,
            AppEvent::SessionOutput { .. } => {
                self.dirty = true;
            }
            AppEvent::SessionExited { id, .. } => {
                // An exit fails every conversation touching it; sources of
                // open asks/tells are notified through the broker queue.
                // Its debounce entries go too, so the maps stay
                // bounded over session churn. The dropped queue depth is
                // traced: an exit between queue and delivery is the other
                // classic "sent but never received".
                let name = self
                    .manager
                    .get(id)
                    .map(|rec| rec.name.clone())
                    .unwrap_or_default();
                let dropped = self.broker.queued(id);
                self.broker.target_exited(&self.manager, id);
                self.trace_comms(&format!(
                    "exit name={} dropped_queue={dropped}",
                    crate::comms::log_quote(&name),
                ));
                self.last_human_input.remove(&id);
                self.last_hook_activity.remove(&id);
                self.attention_flags.remove(&id);
                self.dirty = true;
            }
            AppEvent::CommsRequest(req) => {
                // The commit claim runs atomically for a reason: a
                // plain flag could read false, stall on the scheduler
                // past the deadline, then mutate after the caller gave
                // up. Losing the claim means the handler already timed
                // out, so the send is skipped and the retry guidance
                // goes on the reply instead of a stale ok.
                if !crate::ipc::listener::claim_execute(&req.claim) {
                    self.trace_comms(&format!(
                        "comms tool={} {} {} -> dropped reason=\"caller timed out; retry with the same idempotency_key\"",
                        req.tool,
                        self.comms_caller(&req.run_id),
                        crate::comms::summarize_call(&req.tool, &req.args),
                    ));
                    let _ = req.reply.send(
                        "{\"ok\":false,\"error\":\"caller timed out; retry with the same idempotency_key\"}\n"
                            .to_string(),
                    );
                    self.dirty = true;
                    return;
                }
                // Walkthrough, visual, session, and board tools answer
                // here (they own overlay, manager, and board state the
                // broker cannot see); everything else goes to the broker
                // at once. The verdict goes straight back to `mcp-serve`.
                // Failures stay single-line JSON, escaped.
                let now = std::time::Instant::now();
                let verdict = match self.walkthrough_tool(&req.run_id, &req.tool, &req.args) {
                    Some(verdict) => verdict,
                    None => match self.visual_tool(&req.run_id, &req.tool, &req.args) {
                        Some(verdict) => verdict,
                        None => match self.session_tool(&req.run_id, &req.tool, &req.args) {
                            Some(verdict) => verdict,
                            None => match self.board_tool(&req.run_id, &req.tool, &req.args) {
                                Some(verdict) => verdict,
                                None => self.broker.call(&self.manager, &req.run_id, &req.tool, &req.args, now),
                            },
                        },
                    },
                };
                self.trace_comms_verdict(&req.tool, &req.run_id, &req.args, &verdict);
                let verdict =
                    verdict.map(|result| self.attach_inbox(&req.run_id, &req.tool, result));
                let line = match verdict {
                    Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                    Err(e) => format!(
                        "{{\"ok\":false,\"error\":{}}}\n",
                        crate::ipc::mcp::escape_json(&e)
                    ),
                };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::BotRequest(req) => {
                // Same commit claim as comms requests: no bot mutation
                // starts after its caller stopped waiting.
                if !crate::ipc::listener::claim_execute(&req.claim) {
                    let mut line = String::from("{\"ok\":false,\"error\":");
                    line.push_str(
                        &crate::comms::bot::BotError::new(
                            crate::comms::bot::ErrorCode::Timeout,
                            "caller timed out; retry with the same idempotency_key",
                        )
                        .to_json(),
                    );
                    line.push_str("}\n");
                    let _ = req.reply.send(line);
                    self.dirty = true;
                    return;
                }
                let now = std::time::Instant::now();
                let line = match self.broker.bot_call(
                    &self.manager,
                    &req.name,
                    &req.token,
                    &req.tool,
                    &req.args,
                    now,
                ) {
                    Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                    Err(e) => {
                        let mut line = String::from("{\"ok\":false,\"error\":");
                        line.push_str(&e.to_json());
                        line.push_str("}\n");
                        line
                    }
                };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::TelegramPoll(report) => {
                self.telegram_last_poll_failed = report.failed;
                let mut routed = false;
                for msg in report.messages {
                    self.route_telegram(msg);
                    routed = true;
                }
                if routed || report.failed {
                    self.dirty = true;
                }
            }
            AppEvent::TelegramMessageSent { message_id, session } => {
                self.record_telegram_reply_target(message_id, session);
            }
            AppEvent::TelegramSendStatus { failed, dropped } => {
                self.telegram_last_send_failed = failed;
                if dropped {
                    self.telegram_outbox_dropped = self.telegram_outbox_dropped.saturating_add(1);
                }
                self.dirty = true;
            }
            AppEvent::TelegramWorkerStopped(kind) => {
                match kind {
                    crate::telegram::WorkerKind::Poller => self.telegram_poller = false,
                    crate::telegram::WorkerKind::Sender => self.telegram_sender = false,
                }
                self.dirty = true;
            }
            AppEvent::Resize(rows, cols) => {
                self.term_size = (rows, cols);
                if cols < 100 { self.overlay_view = None; }
                self.dirty = true;
            }
            AppEvent::HookRequest(req) => {
                // Portable-pty makes every pane child a Unix session leader.
                // Hook descendants retain that process-session ID even when
                // muse scrubs their environment, so it identifies the exact
                // pane when several muse sessions share a cwd. Old relays do
                // not send it; retain the conservative cwd/window fallback.
                let source_sid = crate::ipc::mcp::top_raw(&req.body, "source_sid")
                    .and_then(|raw| raw.parse::<u32>().ok())
                    .filter(|sid| *sid != 0);
                let source_session =
                    source_sid.and_then(|sid| self.manager.lookup_process_session(sid));
                // SessionStart alone is not enough: an explicitly resumed
                // muse session fires no SessionStart, only UserPromptSubmit
                // and Stop. Subagent tool hooks carry the child's session ID,
                // so only edges a subagent cannot produce may claim a session.
                // The bind runs first so the record below attributes normally.
                if matches!(req.hook.as_str(), "SessionStart" | "UserPromptSubmit" | "Stop")
                    && self.manager.lookup_run(&req.run_id).is_none()
                {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if source_session.is_some() {
                            self.manager.bind_harness_session_to_process(
                                &harness,
                                source_sid.expect("resolved process source has an ID"),
                            );
                        } else if let Some(cwd) = crate::session::cwd_from_hook_body(&req.body) {
                            self.manager.bind_harness_session(&harness, &cwd);
                        }
                    }
                }
                // Attribute hook activity before queuing: the sender's run
                // ID resolves to its session; records without one resolve by
                // PTY process-session ID, then by an established harness ID.
                // Unknown sources stay untouched.
                let fallback_id = if self.manager.lookup_run(&req.run_id).is_none() {
                    source_session.or_else(|| {
                        crate::session::session_id_from_hook_body(&req.body)
                            .and_then(|h| self.manager.lookup_harness_session(&h))
                    })
                } else {
                    None
                };
                let attributed = self.manager.lookup_run(&req.run_id).or(fallback_id);
                // Background sub-sessions share the pane's process group,
                // so their hooks attribute here too, but they carry their
                // own session ID and may never Stop. Only the pane's own
                // harness session moves its activity; the edges that
                // (re)bind the harness ID below always apply.
                let own_session = matches!(req.hook.as_str(), "SessionStart" | "UserPromptSubmit")
                    || attributed.and_then(|id| self.manager.get(id)).is_none_or(|rec| {
                        match (
                            rec.harness_session_id.as_deref(),
                            crate::session::session_id_from_hook_body(&req.body),
                        ) {
                            (Some(known), Some(sent)) => known == sent,
                            _ => true,
                        }
                    });
                if let Some(activity) = crate::session::activity_for_hook(&req.hook) {
                    if let Some(id) = attributed.filter(|_| own_session) {
                        self.manager.set_activity(id, activity);
                    }
                }
                // SessionStart carries the harness-side conversation ID the
                // restore path resumes with, and every UserPromptSubmit
                // carries the live one: muse fires no SessionStart on
                // boot or resume, so the prompt is the true resume ID.
                // Bodies without one (or from unknown runs) leave any
                // earlier value alone.
                if req.hook == "SessionStart" || req.hook == "UserPromptSubmit" {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if let Some(id) = attributed {
                            self.manager.set_harness_session(id, harness);
                        }
                    }
                }
                if let Some(path) = self.hook_trace.as_deref() {
                    let show = |id: Option<crate::session::SessionId>| {
                        id.map_or_else(|| "none".to_string(), |id| id.to_string())
                    };
                    let harness_after = attributed
                        .and_then(|id| self.manager.get(id))
                        .and_then(|rec| rec.harness_session_id.clone())
                        .unwrap_or_else(|| "-".to_string());
                    crate::infra::logging::hook_trace(
                        path,
                        &format!(
                            "tui hook={} run={} source_sid={} by_sid={} session_id={} cwd={} attributed={} harness_after={harness_after}",
                            req.hook,
                            show(self.manager.lookup_run(&req.run_id)),
                            source_sid.map_or_else(|| "-".to_string(), |sid| sid.to_string()),
                            show(source_session),
                            crate::session::session_id_from_hook_body(&req.body)
                                .unwrap_or_else(|| "-".to_string()),
                            crate::session::cwd_from_hook_body(&req.body)
                                .unwrap_or_else(|| "-".to_string()),
                            show(attributed),
                        ),
                    );
                }
                if self.pending_hooks.len() < crate::ipc::listener::MAX_PENDING_HOOKS {
                    if let Some(id) = attributed {
                        self.last_hook_activity
                            .insert(id, std::time::Instant::now());
                    }
                    self.pending_hooks.push_back(req);
                }
                self.dirty = true;
            }
            AppEvent::Input(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::event::AppEvent;
    use crate::infra::ids::RunId;

    #[test]
    fn fresh_state_needs_paint_and_runs() {
        let s = AppState::new();
        assert!(s.dirty);
        assert!(!s.should_quit);
        assert!(s.manager.is_empty());
    }

    #[test]
    fn shutdown_quits() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Shutdown);
        assert!(s.should_quit);
    }

    #[test]
    fn cancelled_request_skips_apply_and_says_so() {
        // The listener marks a request whose caller already timed
        // out; applying it would run a send nobody waits for and
        // report ok to nobody. Skip the send, say so on the reply.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_TIMED_OUT)),
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(line.contains("timed out"), "line: {line:?}");
        assert_eq!(s.broker.queued(b), 0, "cancelled send creates nothing");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn tick_is_quiet() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Tick);
        assert!(!s.dirty);
        assert!(!s.should_quit);
    }

    fn bot_reply(
        state: &mut AppState,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
    ) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::BotRequest(crate::ipc::listener::BotRequest {
            name: name.to_string(),
            token: token.to_string(),
            tool: tool.to_string(),
            args: args.to_string(),
            reply: tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::ipc::listener::CLAIM_PENDING,
            )),
        }));
        rx.recv().expect("bot verdict arrives")
    }

    #[test]
    fn timed_out_bot_request_skips_and_says_so() {
        // The claim lost means the handler already timed out: no bot
        // mutation runs, and the reply guides the retry to its key.
        let mut state = bot_state();
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::BotRequest(crate::ipc::listener::BotRequest {
            name: "skippy".to_string(),
            token: BOT_TOKEN.to_string(),
            tool: "list_sessions".to_string(),
            args: "{}".to_string(),
            reply: tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::ipc::listener::CLAIM_TIMED_OUT,
            )),
        }));
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("skip verdict arrives");
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(line.contains("timed out"), "line: {line:?}");
    }

    const BOT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn bot_state() -> AppState {
        let mut state = AppState::new();
        let run = RunId::generate();
        let id = state
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run, "shell")
            .unwrap();
        state.broker.join(&state.manager, id, "peers").unwrap();
        state
            .broker
            .register_client(
                &state.manager,
                "skippy",
                vec!["peers".to_string()],
                BOT_TOKEN,
                Vec::new(),
            )
            .expect("registration validates");
        state
    }

    #[test]
    fn bot_dispatch_answers_and_object_errors() {
        let mut state = bot_state();
        let list = bot_reply(&mut state, "skippy", BOT_TOKEN, "list_sessions", "{}");
        assert!(list.contains(r#""ok":true"#), "list: {list}");
        assert!(list.contains(r#""you":"skippy""#), "list: {list}");
        assert!(list.contains("\"epoch\":"), "list: {list}");
        let bad = bot_reply(&mut state, "skippy", "wrong-credential-0000000000000000", "list_sessions", "{}");
        assert!(bad.contains(r#""ok":false"#), "bad: {bad}");
        assert!(bad.contains(r#""code":"unauthorized""#), "bad: {bad}");
        assert!(state.manager.remove(state.manager.order()[0]));
    }
}
