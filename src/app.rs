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

    /// Overlay slot index of the Visual tab for one session.
    #[cfg(feature = "visual")]
    fn visual_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Visual")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The active session's stored visual, if it sits on its Visual
    /// slot: the (session, generation) the terminal should show.
    #[cfg(feature = "visual")]
    fn visual_overlay_active(&self) -> Option<(crate::session::SessionId, u64)> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.visual_slot(active) {
            return None;
        }
        let slot = self.visual_slots.get(&active)?;
        Some((active, slot.generation))
    }

    /// Claim the next terminal transmit, if the overlay wants an image
    /// the screen does not already show. The paint fingerprint covers
    /// zoom, scroll, crop, and cursor, so scrolling or zooming
    /// re-transmits under the same image id while an untouched frame
    /// claims nothing. The fixed placement id swaps the re-display in
    /// place without flicker. The TUI positions the cursor and writes
    /// the escape; `None` means nothing to do.
    #[cfg(feature = "visual")]
    pub fn visual_take_show(
        &mut self,
        kitty: bool,
        paint: crate::visual::VisualPaint,
    ) -> Option<VisualShowSpec> {
        if !kitty {
            return None;
        }
        let (session, generation) = self.visual_overlay_active()?;
        let same_slot = matches!(&self.visual_shown, Some(s) if s.session == session && s.generation == generation);
        if same_slot && matches!(&self.visual_shown, Some(s) if s.paint == paint) {
            return None;
        }
        // Same frame, new viewport: reuse the image id so the fixed
        // placement id swaps it in place; anything else is a fresh
        // placement.
        let image_id = match &self.visual_shown {
            Some(shown) if shown.session == session && shown.generation == generation => {
                shown.image_id
            }
            _ => {
                self.visual_image_seq += 1;
                self.visual_image_seq
            }
        };
        self.visual_shown = Some(VisualShown { session, generation, image_id, paint });
        Some(VisualShowSpec { image_id, session, generation, paint })
    }

    /// Whether the focused tab is a session Visual tab: arrows,
    /// `+`/`-`, wheel, zoom clicks, and image clicks route to its
    /// viewport.
    #[cfg(feature = "visual")]
    pub fn visual_tab_focused(&self) -> bool {
        self.visual_overlay_active().is_some()
    }

    /// Layout for one session's visual inside its tab image region:
    /// contained display size from zoom, clamped scroll, source crop,
    /// and the centered cursor. `cell_w`/`cell_h` come from the
    /// terminal's pixel report (Kitty path) or the exact 8x16 the
    /// half-block fallback draws with.
    #[cfg(feature = "visual")]
    pub fn visual_paint(
        &self,
        id: crate::session::SessionId,
        image: ratatui::layout::Rect,
        cell_w: f64,
        cell_h: f64,
    ) -> Option<crate::visual::VisualPaint> {
        use crate::visual::{clamp_scroll, crop_for_view, fit_display};
        let slot = self.visual_slots.get(&id)?;
        let (disp_cols, disp_rows) = fit_display(
            slot.width,
            slot.height,
            slot.zoom,
            image.width,
            image.height,
            cell_w,
            cell_h,
        );
        let (ox, oy) = clamp_scroll(
            slot.scroll_x,
            slot.scroll_y,
            disp_cols,
            disp_rows,
            image.width,
            image.height,
        );
        let crop =
            crop_for_view(slot.width, slot.height, disp_cols, disp_rows, image.width, image.height, ox, oy);
        Some(crate::visual::VisualPaint {
            zoom_bits: slot.zoom.to_bits(),
            ox,
            oy,
            out_cols: crop.out_cols,
            out_rows: crop.out_rows,
            cursor_x: image.x.saturating_add(image.width.saturating_sub(crop.out_cols) / 2),
            cursor_y: image.y.saturating_add(image.height.saturating_sub(crop.out_rows) / 2),
            sx: crop.sx,
            sy: crop.sy,
            sw: crop.sw,
            sh: crop.sh,
            selected: slot.selected,
        })
    }

    /// Step one session's visual zoom, re-clamping the scroll offset
    /// to the new overflow. Geometry is the tab image region plus the
    /// cell size the paint uses. Returns whether anything changed.
    #[cfg(feature = "visual")]
    pub fn visual_zoom(
        &mut self,
        id: crate::session::SessionId,
        dir: crate::ui::visual::VisualButton,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
    ) -> bool {
        use crate::visual::{ZoomDir, clamp_scroll, fit_display, zoom_step};
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let dir = match dir {
            crate::ui::visual::VisualButton::ZoomIn => ZoomDir::In,
            crate::ui::visual::VisualButton::ZoomOut => ZoomDir::Out,
            // The chat toggle never reaches zoom: the mouse path
            // routes it to the toggle first. Unreachable by design.
            crate::ui::visual::VisualButton::Chat => return false,
        };
        let next = zoom_step(slot.zoom, dir);
        if next == slot.zoom {
            return false;
        }
        slot.zoom = next;
        let (disp_cols, disp_rows) =
            fit_display(slot.width, slot.height, slot.zoom, area_cols, area_rows, cell_w, cell_h);
        (slot.scroll_x, slot.scroll_y) = clamp_scroll(
            slot.scroll_x,
            slot.scroll_y,
            disp_cols,
            disp_rows,
            area_cols,
            area_rows,
        );
        self.dirty = true;
        true
    }

    /// Pan one session's visual viewport by (`dx`, `dy`) displayed
    /// cells, positive right and down, clamped to the zoom overflow.
    /// Returns whether anything changed.
    #[cfg(feature = "visual")]
    pub fn visual_scroll(
        &mut self,
        id: crate::session::SessionId,
        dx: i16,
        dy: i16,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
    ) -> bool {
        use crate::visual::{clamp_scroll, fit_display};
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let (disp_cols, disp_rows) =
            fit_display(slot.width, slot.height, slot.zoom, area_cols, area_rows, cell_w, cell_h);
        let max_x = disp_cols.saturating_sub(area_cols);
        let max_y = disp_rows.saturating_sub(area_rows);
        let (nx, ny) = (
            slot.scroll_x.saturating_add_signed(dx).min(max_x),
            slot.scroll_y.saturating_add_signed(dy).min(max_y),
        );
        let (nx, ny) = clamp_scroll(nx, ny, disp_cols, disp_rows, area_cols, area_rows);
        if (nx, ny) == (slot.scroll_x, slot.scroll_y) {
            return false;
        }
        slot.scroll_x = nx;
        slot.scroll_y = ny;
        self.dirty = true;
        true
    }

    /// Flip one slot's chat footer, dirtying on change. Dismissing
    /// reclaims the footer rows for the diagram; the selection and
    /// history survive underneath until the next selection reopens.
    #[cfg(feature = "visual")]
    pub fn visual_toggle_chat(&mut self, id: crate::session::SessionId) -> bool {
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        slot.chat_open = !slot.chat_open;
        // Reopening onto a selection arms the prompt, same as a
        // fresh selection: typing starts immediately.
        if slot.chat_open && slot.selected.is_some() {
            slot.input_active = true;
            slot.draft.get_or_insert_with(String::new);
        }
        self.dirty = true;
        true
    }

    /// Reserved chat footer rows for one slot: 30% of the content
    /// height while open, zero while dismissed. Render and mouse
    /// handling both derive from this, so the footer never desyncs
    /// from the image region.
    #[cfg(feature = "visual")]
    pub fn visual_footer_rows(&self, id: crate::session::SessionId, content_h: u16) -> u16 {
        match self.visual_slots.get(&id) {
            Some(slot) if slot.chat_open => crate::ui::visual::visual_chat_footer_rows(content_h),
            _ => 0,
        }
    }

    /// Content width the chat footer wraps to: the same pane content
    /// rect the chrome derives from, so wrapped rows match the tab.
    #[cfg(feature = "visual")]
    fn visual_footer_width(&self) -> u16 {
        // History wraps to the box interior: content minus borders
        // and side pads, so sided rows stay exactly content-wide.
        let (rows, cols) = self.term_size;
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        crate::ui::layout::pane_content_area(&areas).width.saturating_sub(6)
    }

    /// All chat history rows for one slot, oldest first: one wrapped
    /// question row plus markdown answer rows (or the waiting marker)
    /// per pair, with one blank row between pairs like the
    /// walkthrough Q&A log. The viewport shows the tail of these.
    #[cfg(feature = "visual")]
    fn visual_chat_history_lines(&self, slot: &VisualSlot) -> Vec<Vec<crate::ui::SpanView>> {
        use crate::ui::theme::{style, Role};
        let width = self.visual_footer_width();
        let text = style(Role::Text);
        let muted = style(Role::Muted);
        let mut rows = Vec::new();
        for (n, q) in slot.questions.iter().enumerate() {
            if n > 0 {
                rows.push(Vec::new());
            }
            rows.extend(crate::ui::text::wrap_spans(
                vec![crate::ui::SpanView {
                    text: format!(
                        "Q ({}): {}",
                        crate::infra::safe_text::encode_for_display(&q.shape_label),
                        crate::infra::safe_text::encode_for_display(&q.question)
                    ),
                    style: text,
                }],
                width,
            ));
            match q.answer.as_deref() {
                Some(a) => {
                    for line in crate::walkthrough::highlight::md_text(a).lines {
                        let spans: Vec<crate::ui::SpanView> = line
                            .spans
                            .iter()
                            .map(|s| crate::ui::SpanView {
                                text: s.content.to_string(),
                                style: s.style,
                            })
                            .collect();
                        if spans.is_empty() {
                            rows.push(Vec::new());
                        } else {
                            rows.extend(crate::ui::text::wrap_spans(spans, width));
                        }
                    }
                }
                None => rows.push(vec![crate::ui::SpanView {
                    text: crate::visual::VISUAL_WAITING_TEXT.to_string(),
                    style: muted,
                }]),
            }
        }
        rows
    }

    /// Scroll one slot's chat history by rows up from the tail
    /// (positive reads back, negative comes forward), clamped to the
    /// rendered history. `visible` is the history viewport rows the
    /// tab currently shows (footer minus the ask row). Returns whether
    /// the offset changed.
    #[cfg(feature = "visual")]
    pub fn visual_chat_scroll(&mut self, id: crate::session::SessionId, delta: i16, visible: u16) -> bool {
        let max = match self.visual_slots.get(&id) {
            Some(slot) => self
                .visual_chat_history_lines(slot)
                .len()
                .saturating_sub(visible as usize) as i16,
            None => return false,
        };
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let next = (slot.chat_scroll as i16 + delta).clamp(0, max.max(0)) as u16;
        if next == slot.chat_scroll {
            return false;
        }
        slot.chat_scroll = next;
        self.dirty = true;
        true
    }

    /// Clear one slot's shape selection, dirtying on change. Margin
    /// clicks share it: empty space always means no selection.
    #[cfg(feature = "visual")]
    fn visual_clear_selection(state: &mut AppState, id: crate::session::SessionId) -> bool {
        let Some(slot) = state.visual_slots.get_mut(&id) else {
            return false;
        };
        if slot.selected.take().is_some() {
            state.dirty = true;
            true
        } else {
            false
        }
    }

    /// Select the shape under an image-region click (`vx`, `vy` in
    /// region cells), zoom- and scroll-aware. `centered` must match
    /// the backend that painted: the Kitty image centers a smaller
    /// diagram in the region (same offset the paint cursor adds)
    /// while the half-block fallback draws from the top-left.
    /// Clicking the selected shape toggles it off; clicking empty
    /// space clears. Returns whether the selection changed.
    #[cfg(feature = "visual")]
    pub fn visual_select_at(
        &mut self,
        id: crate::session::SessionId,
        vx: u16,
        vy: u16,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
        centered: bool,
    ) -> bool {
        use crate::visual::{fit_display, hit_shape, source_to_svg, view_to_source};
        let hit = {
            let Some(slot) = self.visual_slots.get(&id) else {
                return false;
            };
            if slot.shapes.is_empty() {
                return false;
            }
            let (disp_cols, disp_rows) = fit_display(
                slot.width,
                slot.height,
                slot.zoom,
                area_cols,
                area_rows,
                cell_w,
                cell_h,
            );
            // Mirror crop_for_view's bounds, then drop the centering
            // offset the paint cursor adds on the Kitty path. Clicks
            // landing in the margin resolve to no shape (clear).
            let out_cols = disp_cols.saturating_sub(slot.scroll_x).min(area_cols).max(1);
            let out_rows = disp_rows.saturating_sub(slot.scroll_y).min(area_rows).max(1);
            let (off_x, off_y) = if centered {
                (
                    area_cols.saturating_sub(out_cols) / 2,
                    area_rows.saturating_sub(out_rows) / 2,
                )
            } else {
                (0, 0)
            };
            let inside = match (vx.checked_sub(off_x), vy.checked_sub(off_y)) {
                (Some(rx), Some(ry)) if rx < out_cols && ry < out_rows => Some((rx, ry)),
                _ => None,
            };
            let Some((rx, ry)) = inside else {
                return Self::visual_clear_selection(self, id);
            };
            let (px, py) = view_to_source(
                rx,
                ry,
                slot.scroll_x,
                slot.scroll_y,
                disp_cols,
                disp_rows,
                slot.width,
                slot.height,
            );
            let (sx, sy) =
                source_to_svg(px, py, slot.width, slot.height, &slot.vb);
            hit_shape(&slot.shapes, sx, sy)
        };
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let next = match (slot.selected, hit) {
            (Some(a), Some(b)) if a == b => None,
            _ => hit,
        };
        if next == slot.selected {
            return false;
        }
        slot.selected = next;
        // A fresh selection opens the dismissed chat and arms the
        // prompt at once (the draft survives): typing starts
        // immediately, no Enter needed. Deselecting disarms.
        // Toggling off keeps both.
        slot.input_active = next.is_some();
        if slot.input_active {
            slot.draft.get_or_insert_with(String::new);
        }
        if next.is_some() {
            slot.chat_open = true;
        }
        self.dirty = true;
        true
    }

    /// The focused Visual tab's (session, generation), if the overlay
    /// sits on a visual slot with a stored frame.
    #[cfg(feature = "visual")]
    pub fn visual_focused_frame(&self) -> Option<(crate::session::SessionId, u64)> {
        self.visual_overlay_active()
    }

    /// Paint the terminal already shows, if the overlay still wants
    /// exactly it. Lets the TUI skip all byte work for an unchanged
    /// frame: byte building (clone or crop+encode) happens only when
    /// this differs from the fresh paint.
    #[cfg(feature = "visual")]
    pub fn visual_current_paint(&self) -> Option<crate::visual::VisualPaint> {
        let shown = self.visual_shown.as_ref()?;
        let (session, generation) = self.visual_overlay_active()?;
        (shown.session == session && shown.generation == generation).then_some(shown.paint)
    }

    /// PNG bytes for one paint of a stored frame: the frame itself
    /// when the viewport shows it whole (no re-encode), else the
    /// cropped region re-encoded for transmit. A selection always
    /// re-encodes so the highlight border bakes in. Independent of
    /// the show gate, so the TUI only claims a transmit it can render.
    #[cfg(feature = "visual")]
    pub fn visual_frame_png(
        &self,
        id: crate::session::SessionId,
        generation: u64,
        paint: crate::visual::VisualPaint,
    ) -> Option<Vec<u8>> {
        let slot = self.visual_slots.get(&id)?;
        if slot.generation != generation {
            return None;
        }
        let selected = slot.selected.and_then(|i| slot.shapes.get(i));
        if selected.is_none()
            && paint.sx == 0
            && paint.sy == 0
            && paint.sw == slot.width
            && paint.sh == slot.height
        {
            return Some(slot.png.clone());
        }
        let crop = crate::visual::ViewCrop {
            sx: paint.sx,
            sy: paint.sy,
            sw: paint.sw,
            sh: paint.sh,
            out_cols: paint.out_cols,
            out_rows: paint.out_rows,
        };
        let mut cut = crate::visual::crop_rgba(&slot.rgba, slot.width, slot.height, crop);
        if let Some(shape) = selected {
            // Shape box (SVG units) to source pixels, clipped to the
            // crop so off-view shapes draw nothing.
            let (ex, ey) = (
                paint.sx.saturating_add(paint.sw),
                paint.sy.saturating_add(paint.sh),
            );
            let (bx0, by0) = crate::visual::svg_to_source(
                shape.x,
                shape.y,
                slot.width,
                slot.height,
                &slot.vb,
            );
            let (bx1, by1) = crate::visual::svg_to_source(
                shape.x + shape.width,
                shape.y + shape.height,
                slot.width,
                slot.height,
                &slot.vb,
            );
            let x0 = bx0.clamp(paint.sx, ex);
            let x1 = bx1.clamp(paint.sx, ex);
            let y0 = by0.clamp(paint.sy, ey);
            let y1 = by1.clamp(paint.sy, ey);
            if x1 > x0 && y1 > y0 {
                crate::visual::stroke_rect(
                    &mut cut,
                    paint.sw,
                    paint.sh,
                    x0 - paint.sx,
                    y0 - paint.sy,
                    x1 - paint.sx,
                    y1 - paint.sy,
                    crate::visual::SELECT_RGB,
                    crate::visual::SELECT_BORDER_PX,
                );
            }
        }
        crate::visual::encode_png(&cut, paint.sw, paint.sh).ok()
    }

    /// Release the terminal image when the overlay no longer wants it:
    /// tab moved away, newer generation stored, or session gone.
    /// Returns the placement id for the TUI to delete.
    #[cfg(feature = "visual")]
    pub fn visual_take_hide(&mut self) -> Option<u32> {
        let shown = self.visual_shown.as_ref()?;
        if self.visual_overlay_active() == Some((shown.session, shown.generation)) {
            return None;
        }
        let image_id = shown.image_id;
        self.visual_shown = None;
        Some(image_id)
    }

    /// PNG bytes of the image the terminal currently shows, if any.
    #[cfg(feature = "visual")]
    /// Unconditional take of the shown image id, for shutdown cleanup
    /// while the overlay still wants it.
    #[cfg(feature = "visual")]
    pub fn visual_take_shown(&mut self) -> Option<u32> {
        self.visual_shown.take().map(|s| s.image_id)
    }

    /// Visual pane content: zoom strip first, then title and alt
    /// text; half-block art when the terminal cannot take a Kitty
    /// image (the image paints over the image region in Kitty mode,
    /// so no art is emitted there).
    #[cfg(feature = "visual")]
    pub fn visual_view(&self, id: crate::session::SessionId, kitty: bool) -> crate::ui::PaneView {
        use ratatui::style::{Color, Style};
        let rec = self.manager.get(id).expect("ordered session exists");
        let live = rec.state.is_live();
        let title = format!("{} · Visual", rec.name);
        let text = crate::ui::theme::style(crate::ui::theme::Role::Text);
        let muted = crate::ui::theme::style(crate::ui::theme::Role::Muted);
        let line = |content: &str, style: Style| {
            vec![crate::ui::SpanView {
                text: content.to_string(),
                style,
            }]
        };
        let Some(slot) = self.visual_slots.get(&id) else {
            return crate::ui::PaneView {
                title,
                lines: vec![
                    line("Visual renders diagrams to visualize code flow.", text),
                    line(
                        "Ask this session, e.g. \"visualize the flow for <...>\"",
                        muted,
                    ),
                ],
                live,
                focused: true,
                cursor: None,
            };
        };
        let mut lines = Vec::new();
        // Row zero is always blank: breathing room between the tab
        // strip and the zoom strip, which rides row one with the
        // title. Both backends share this: the Kitty image paints only
        // the region below the strip, and the mouse hit test assumes
        // these exact rows and columns.
        lines.push(Vec::new());
        // One chrome for both backends: the strip, image, and footer
        // rows below must match the rects the mouse path hit-tests,
        // or clicks and wheel routing desync from the paint.
        let (rows, cols) = self.term_size;
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot_rows = self.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, self.pill_tabs, foot_rows);
        let mut strip = crate::ui::visual::visual_button_spans(self.pill_tabs, slot.chat_open);
        strip.push(crate::ui::SpanView {
            text: "  ←→↑↓ scroll · wheel scrolls".to_string(),
            style: muted,
        });
        // The title rides the strip row: the fitted image fills every
        // row below it, so a title line there would be painted over.
        // A selection appends its shape label the same way. In Kitty
        // mode the alt text rides here too, shortened to fit: its own
        // line would hide under the transmitted image.
        if !slot.title.is_empty() {
            strip.push(crate::ui::SpanView {
                text: format!("  ·  {}", slot.title),
                style: text,
            });
        }
        if let Some(shape) = slot.selected.and_then(|i| slot.shapes.get(i)) {
            strip.push(crate::ui::SpanView {
                text: format!(
                    "  ▸  {}",
                    crate::infra::safe_text::encode_for_display(&shape.label)
                ),
                style: text,
            });
        }
        if kitty && !slot.alt.is_empty() {
            // Shortened: the strip is chrome, so the description
            // arrives capped with an ellipsis, never a sentence.
            let alt = vec![crate::ui::SpanView {
                text: format!(
                    "  ·  {}",
                    crate::infra::safe_text::encode_for_display(&slot.alt)
                ),
                style: muted,
            }];
            strip.extend(crate::ui::text::truncate_spans(
                alt,
                crate::ui::visual::VISUAL_STRIP_ALT_MAX,
            ));
        }
        lines.push(strip);
        if !kitty {
            // Half-block cells are exactly 1:2 by construction, so the
            // fallback layout uses the fixed 8x16 cell, never the
            // terminal pixel report. Art covers the visible crop only,
            // capped at the tab image region like the Kitty paint.
            let image = chrome.image;
            if let Some(paint) =
                self.visual_paint(id, image, crate::visual::FALLBACK_CELL_PX.0, crate::visual::FALLBACK_CELL_PX.1)
            {
                let crop = crate::visual::ViewCrop {
                    sx: paint.sx,
                    sy: paint.sy,
                    sw: paint.sw,
                    sh: paint.sh,
                    out_cols: paint.out_cols,
                    out_rows: paint.out_rows,
                };
                let cut = crate::visual::crop_rgba(&slot.rgba, slot.width, slot.height, crop);
                let selected = slot.selected.and_then(|i| slot.shapes.get(i));
                let cols = (paint.sw as usize).min(paint.out_cols.max(1) as usize).max(1);
                for (r, row) in crate::visual::halfblock_rows(
                    &cut,
                    paint.sw,
                    paint.sh,
                    paint.out_cols.max(1) as usize,
                )
                .into_iter()
                .enumerate()
                {
                    lines.push(
                        row.into_iter()
                            .enumerate()
                            .map(|(dx, cell)| {
                                let mut style = Style::default()
                                    .fg(Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2))
                                    .bg(Color::Rgb(cell.bg.0, cell.bg.1, cell.bg.2));
                                // Reverse cells inside the selected shape:
                                // the Kitty backend cannot style cells,
                                // so it bakes a border into the bytes
                                // instead (see visual_frame_png).
                                if let Some(shape) = selected {
                                    let px = paint.sx.saturating_add(
                                        (dx as u32).saturating_mul(paint.sw) / cols.max(1) as u32,
                                    );
                                    let py = paint.sy.saturating_add((r as u32).saturating_mul(2));
                                    let (sx, sy) = crate::visual::source_to_svg(
                                        px,
                                        py,
                                        slot.width,
                                        slot.height,
                                        &slot.vb,
                                    );
                                    if crate::visual::shape_contains(shape, sx, sy) {
                                        style = style.add_modifier(
                                            ratatui::style::Modifier::REVERSED,
                                        );
                                    }
                                }
                                crate::ui::SpanView {
                                    text: cell.ch.to_string(),
                                    style,
                                }
                            })
                            .collect(),
                    );
                }
            }
        }
        // Fallback only: in Kitty mode the alt text rides the strip
        // row, since this line would hide under the image.
        if !kitty && !slot.alt.is_empty() {
            lines.push(line(&slot.alt, muted));
        }
        // Dismissable Q/A footer: a bordered box while open, none
        // while dismissed. Blank filler first: the Kitty backend
        // emits no art and small diagrams leave empty image rows, so
        // without it the box would paint under the strip (beneath
        // the image in Kitty mode) while the click map and wheel
        // routing use the chrome footer pinned to the bottom.
        if slot.chat_open {
            let used = lines.len().saturating_sub(2) as u16;
            for _ in used..chrome.image.height {
                lines.push(Vec::new());
            }
            // Box metrics: full content width, two-cell side pads, one
            // pad row under the title; the history viewport is what
            // remains (see visual_chat_history_rows). Claude-style
            // order: history on top, a divider, then the prompt row
            // docked above the bottom border. Every emitted row is
            // exactly content-wide so the sides align.
            let width = content.width as usize;
            let inner = width.saturating_sub(6);
            let side = |pad: &str| crate::ui::SpanView {
                text: pad.to_string(),
                style: muted,
            };
            let fill_content = |mut row: Vec<crate::ui::SpanView>| {
                let w = crate::ui::text::spans_width(&row);
                let style = row.last().map(|s| s.style).unwrap_or(muted);
                row.push(crate::ui::SpanView {
                    text: " ".repeat(inner.saturating_sub(w)),
                    style,
                });
                row
            };
            let foot_start = lines.len();
            let mut top = String::from("╭─ Q/A ");
            top.push_str(&"─".repeat(width.saturating_sub(8)));
            top.push('╮');
            lines.push(line(&top, muted));
            lines.push(vec![
                side("│"),
                side(&" ".repeat(width.saturating_sub(2))),
                side("│"),
            ]);
            let history = self.visual_chat_history_lines(slot);
            let tail = history.len().saturating_sub(slot.chat_scroll as usize);
            let start = tail.saturating_sub(
                crate::ui::visual::visual_chat_history_rows(foot_rows) as usize,
            );
            for row in history[start..tail.min(history.len())].iter().cloned() {
                let mut history_line = vec![side("│  ")];
                history_line.extend(fill_content(row));
                history_line.push(side("  │"));
                lines.push(history_line);
            }
            while lines.len() - foot_start < foot_rows.saturating_sub(3) as usize {
                let mut blank = vec![side("│  ")];
                blank.extend(fill_content(Vec::new()));
                blank.push(side("  │"));
                lines.push(blank);
            }
            let mut divider = String::from("├");
            divider.push_str(&"─".repeat(width.saturating_sub(2)));
            divider.push('┤');
            lines.push(line(&divider, muted));
            // Docked prompt row: `>` plus a gray hint until the first
            // keystroke swaps it for the draft. Typing is armed by
            // selection itself, so Enter is only ever submit.
            let input_spans: Vec<crate::ui::SpanView> = match (slot.selected.is_some(), slot.draft.as_deref()) {
                (false, _) => vec![
                    crate::ui::SpanView { text: "> ".to_string(), style: text },
                    crate::ui::SpanView {
                        text: "Click a shape to ask · c toggles chat".to_string(),
                        style: muted,
                    },
                ],
                (true, Some(d)) if !d.is_empty() => {
                    let cursor = if slot.input_active { "▌" } else { "" };
                    vec![
                        crate::ui::SpanView { text: "> ".to_string(), style: text },
                        crate::ui::SpanView {
                            text: format!("{}{}", crate::infra::safe_text::encode_for_display(d), cursor),
                            style: text,
                        },
                    ]
                }
                _ => vec![
                    crate::ui::SpanView { text: "> ".to_string(), style: text },
                    crate::ui::SpanView { text: "type question here".to_string(), style: muted },
                ],
            };
            let mut input_line = vec![side("│  ")];
            input_line.extend(fill_content(crate::ui::text::truncate_spans(
                input_spans,
                inner as u16,
            )));
            input_line.push(side("  │"));
            lines.push(input_line);
            let mut bottom = String::from("╰");
            bottom.push_str(&"─".repeat(width.saturating_sub(2)));
            bottom.push('╯');
            lines.push(line(&bottom, muted));
        }
        crate::ui::PaneView {
            title,
            lines,
            live,
            focused: true,
            cursor: None,
        }
    }

    /// Submit the visual ask draft as a question about the selected
    /// shape: the markup goes straight into the agent pane and the
    /// Enter stages for a later tick — the same split write comms
    /// injections use. The question is logged only after the pane
    /// write lands, so the footer never claims an undelivered ask.
    /// The highlight stays so the answer reads against its shape.
    #[cfg(feature = "visual")]
    pub fn submit_visual_question(&mut self, id: crate::session::SessionId) -> bool {
        let Some(slot) = self.visual_slots.get(&id) else {
            return false;
        };
        let draft = match slot.draft.as_ref() {
            Some(buf) if !buf.trim().is_empty() => buf.trim().to_string(),
            _ => return false,
        };
        let (shape_id, shape_label) = match slot.selected.and_then(|i| slot.shapes.get(i)) {
            Some(shape) => (shape.id.clone(), shape.label.clone()),
            None => return false,
        };
        let markup = crate::visual::question_markup(
            &slot.title,
            &shape_id,
            &shape_label,
            &draft,
        );
        if self.manager.inject_write(id, markup.as_bytes()).is_err() {
            return false;
        }
        self.pending_enter.insert(id, (std::time::Instant::now(), None));
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        slot.draft = None;
        // Typing stays armed so the next question needs no Enter
        // either; Esc disarms back to viewport keys.
        slot.chat_scroll = 0;
        slot.questions.push(crate::visual::VisualQuestion {
            shape_id,
            shape_label,
            question: draft,
            answer: None,
        });
        while slot.questions.len() > crate::visual::MAX_VISUAL_QUESTIONS {
            // Oldest answered go first; an all-pending log drops its
            // head rather than growing unbounded.
            match slot.questions.iter().position(|q| q.answer.is_some()) {
                Some(i) => slot.questions.remove(i),
                None => slot.questions.remove(0),
            };
        }
        self.dirty = true;
        true
    }

    /// Collect finished background rasters into sticky per-session
    /// slots. Called once per main-loop pass; never blocks.
    #[cfg(feature = "visual")]
    pub fn drain_visual(&mut self) {
        let done: Vec<VisualDone> = self.visual_rx.try_iter().collect();
        for d in done {
            self.visual_complete(d);
        }
    }

    /// No-op drain when the feature is off, so the main loop needs no
    /// feature gate at the call site.
    #[cfg(not(feature = "visual"))]
    pub fn drain_visual(&mut self) {}

    /// Store a finished raster unless it is stale (an older generation
    /// than the slot holds) or its session is gone. Raster failures
    /// drop the frame and keep any previous one.
    #[cfg(feature = "visual")]
    fn visual_complete(&mut self, done: VisualDone) {
        if self.manager.get(done.session).is_none() {
            return;
        }
        if let Some(slot) = self.visual_slots.get(&done.session) {
            if done.generation <= slot.generation {
                return;
            }
        }
        let Ok(frame) = done.result else {
            return;
        };
        self.visual_lru.retain(|id| *id != done.session);
        self.visual_lru.push_back(done.session);
        self.visual_slots.insert(
            done.session,
            VisualSlot {
                generation: done.generation,
                png: frame.png,
                rgba: frame.rgba,
                width: frame.width,
                height: frame.height,
                title: done.title,
                alt: done.alt,
                // A new frame resets the viewport: whole diagram
                // fitted to the tab, no scroll, no selection, no
                // questions (shape references belong to the old art),
                // chat closed.
                zoom: 1.0,
                scroll_x: 0,
                scroll_y: 0,
                shapes: frame.shapes,
                vb: frame.vb,
                selected: None,
                draft: None,
                input_active: false,
                chat_open: false,
                chat_scroll: 0,
                questions: Vec::new(),
            },
        );
        self.visual_evict();
        self.focus_visual_tab(done.session);
        self.dirty = true;
    }

    /// Bring a fresh visual forward: switch the session to its Visual
    /// tab. A background session only takes the overlay slot when the
    /// active session is not using it, so a diagram never yanks the
    /// operator off a Visual or Walkthrough view they opened. Narrow
    /// terminals (under 100 columns) have no overlay tabs.
    #[cfg(feature = "visual")]
    fn focus_visual_tab(&mut self, id: crate::session::SessionId) {
        if self.term_size.1 < 100 {
            return;
        }
        if self.manager.active() != Some(id) && self.overlay_active() {
            return;
        }
        if let Some(slot) = self.visual_slot(id) {
            self.overlay_view = Some((id, slot));
        }
    }

    /// Image bytes held across all slots: transmit PNG plus fallback RGBA.
    #[cfg(feature = "visual")]
    fn visual_bytes(&self) -> usize {
        self.visual_slots
            .values()
            .map(|slot| slot.png.len() + slot.rgba.len())
            .sum()
    }

    /// Enforce the budget: oldest sessions first, but never the
    /// focused one until nothing else can go.
    #[cfg(feature = "visual")]
    fn visual_evict(&mut self) {
        let active = self.manager.active();
        while self.visual_slots.len() > self.visual_budget_count
            || self.visual_bytes() > self.visual_budget_bytes
        {
            let victim = self
                .visual_lru
                .iter()
                .position(|id| Some(*id) != active)
                .or_else(|| (!self.visual_lru.is_empty()).then_some(0));
            let Some(index) = victim else {
                break;
            };
            let id = self.visual_lru.remove(index).expect("eviction index live");
            self.visual_slots.remove(&id);
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
    use super::test_support::*;
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

    #[cfg(feature = "visual")]
    #[test]
    fn visual_empty_state_describes_tab_and_invocation() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let view = state.visual_view(id, false);
        let text: String = view.lines.iter().flatten().map(|span| span.text.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("renders diagrams"), "describes the tab: {text:?}");
        assert!(text.contains("visualize the flow for"), "shows how to invoke it: {text:?}");
        assert!(state.manager.remove(id));
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

    #[cfg(feature = "visual")]
    #[test]
    fn visual_show_accepts_mermaid_for_background_render() {
        let mut state = AppState::new();
        let (_id, live_run) = spawn_visual_agent(&mut state, "agent");
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B","format":"mermaid","title":"flow"}"#,
        );
        assert!(out.contains(r#""ok":true"#), "accepted: {out}");
        assert!(out.contains("\"accepted\":true"), "queued: {out}");
        assert!(out.contains("\"generation\":1"), "stamped: {out}");
        assert!(!out.contains("\"width\""), "no sync dimensions: {out}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_stores_newest_and_drops_stale() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 2, fake_png(64, 10, 10));
        assert_eq!(state.visual_slots.get(&id).unwrap().generation, 2);
        complete(&mut state, id, 1, fake_png(64, 20, 20));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.generation, 2, "stale render dropped");
        assert_eq!((slot.width, slot.height), (10, 10));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_drops_missing_session() {
        let mut state = AppState::new();
        let ghost = crate::session::SessionId::fresh();
        complete(&mut state, ghost, 1, fake_png(64, 10, 10));
        assert!(state.visual_slots.is_empty(), "no slot for dead session");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_focuses_the_visual_tab() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let (a, _) = spawn_visual_agent(&mut state, "a");
        let (b, _) = spawn_visual_agent(&mut state, "b");
        state.manager.switch(a);
        complete(&mut state, a, 1, fake_png(64, 400, 200));
        assert!(state.visual_tab_focused(), "active session jumps to Visual");
        // A background visual never steals an overlay the operator has open.
        complete(&mut state, b, 1, fake_png(64, 400, 200));
        assert_eq!(state.overlay_view, Some((a, state.visual_slot(a).unwrap())));
        // With no overlay open, it pre-selects Visual for when they switch.
        state.overlay_view = None;
        complete(&mut state, b, 2, fake_png(64, 400, 200));
        state.manager.switch(b);
        assert!(state.visual_tab_focused(), "background visual waits in its tab");
        assert!(state.manager.remove(a));
        assert!(state.manager.remove(b));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_skips_focus_on_narrow_terminals() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 90));
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 400, 200));
        assert!(state.overlay_view.is_none(), "no overlay tabs under 100 cols");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_lru_evicts_oldest_inactive_first() {
        let mut state = AppState::new();
        state.visual_budget_count = 2;
        let (a, _) = spawn_visual_agent(&mut state, "a");
        let (b, _) = spawn_visual_agent(&mut state, "b");
        let (c, _) = spawn_visual_agent(&mut state, "c");
        assert!(state.select_session(0), "focus oldest");
        assert_eq!(state.manager.active(), Some(a));
        complete(&mut state, a, 1, fake_png(64, 10, 10));
        complete(&mut state, b, 2, fake_png(64, 10, 10));
        complete(&mut state, c, 3, fake_png(64, 10, 10));
        assert_eq!(state.visual_slots.len(), 2);
        assert!(state.visual_slots.contains_key(&a), "active survives");
        assert!(state.visual_slots.contains_key(&c), "newest survives");
        assert!(!state.visual_slots.contains_key(&b), "oldest inactive evicted");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_budget_counts_decoded_bytes() {
        let mut state = AppState::new();
        state.visual_budget_bytes = 100;
        let (a, _) = spawn_visual_agent(&mut state, "a");
        let (b, _) = spawn_visual_agent(&mut state, "b");
        assert!(state.select_session(1), "focus newest");
        complete(&mut state, a, 1, fake_png(60, 10, 10));
        complete(&mut state, b, 2, fake_png(60, 10, 10));
        assert_eq!(state.visual_slots.len(), 1);
        assert!(state.visual_slots.contains_key(&b), "newest survives bytes");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_drain_collects_worker_results() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_tx.clone().send(crate::app::VisualDone {
            session: id,
            generation: 7,
            title: "t".to_string(),
            alt: String::new(),
            result: Ok(crate::visual::RasterFrame {
                png: fake_png(64, 10, 10),
                rgba: Vec::new(),
                width: 10,
                height: 10,
                shapes: Vec::new(),
                vb: [0.0, 0.0, 10.0, 10.0],
            }),
        }).unwrap();
        state.drain_visual();
        assert_eq!(state.visual_slots.get(&id).unwrap().generation, 7);
        assert_eq!(state.visual_slots.get(&id).unwrap().title, "t");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_show_rejects_bad_format_empty_and_stale_caller() {
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        // Unknown formats name the supported set.
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"<b>x</b>","format":"html"}"#,
        );
        assert!(out.contains(r#""ok":false"#), "rejected: {out}");
        assert!(out.contains("mermaid"), "names supported: {out}");
        // Missing format defaults to mermaid.
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B"}"#,
        );
        assert!(out.contains(r#""ok":true"#), "default format: {out}");
        // Empty content and stale callers fail closed.
        let out = comms_reply(&mut state, &live_run, "visual_show", r#"{"content":""}"#);
        assert!(out.contains(r#""ok":false"#), "empty: {out}");
        let out = comms_reply(
            &mut state,
            "run-dead",
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B"}"#,
        );
        assert!(out.contains("stale run ID"), "stale: {out}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_take_show_hide_tracks_terminal_image() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show once");
        assert_eq!(spec.generation, 1);
        assert!(state.visual_take_show(true, paint).is_none(), "already shown");
        assert!(state.visual_take_show(false, paint).is_none(), "no kitty no show");
        assert_eq!(state.visual_frame_png(id, 1, paint).unwrap().len(), 64);
        // New generation hides the old image, then shows the new one.
        complete(&mut state, id, 2, fake_png(64, 10, 10));
        assert_eq!(state.visual_take_hide(), Some(spec.image_id));
        let spec2 = state.visual_take_show(true, paint_for(&state, id)).expect("reshow");
        assert_ne!(spec2.image_id, spec.image_id, "fresh placement id");
        // Overlay away hides; double hide is silent.
        state.overlay_view = None;
        assert_eq!(state.visual_take_hide(), Some(spec2.image_id));
        assert_eq!(state.visual_take_hide(), None);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_take_show_repaints_viewport_changes_under_the_same_id() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 100, 100));
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show");
        // Zooming changes the fingerprint but reuses the image id, so
        // the fixed placement id swaps the re-display without a
        // delete flash in between.
        assert!(state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        let zoomed = paint_for(&state, id);
        assert_ne!(zoomed, paint, "zoom moves the paint");
        let reshow = state.visual_take_show(true, zoomed).expect("repaint");
        assert_eq!(reshow.image_id, spec.image_id, "no fresh id for a viewport change");
        assert!(state.visual_take_show(true, zoomed).is_none(), "paint now current");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_current_paint_tracks_what_the_terminal_shows() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 100, 100));
        show_visual_overlay(&mut state, id);
        assert_eq!(state.visual_current_paint(), None, "nothing shown yet");
        let paint = paint_for(&state, id);
        state.visual_take_show(true, paint).expect("show");
        assert_eq!(state.visual_current_paint(), Some(paint));
        // Viewport moved without a reshow: the terminal is stale, so
        // the TUI must build bytes again.
        assert!(state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        let zoomed = paint_for(&state, id);
        assert_ne!(state.visual_current_paint(), Some(zoomed), "stale paint");
        state.visual_take_show(true, zoomed).expect("reshow");
        assert_eq!(state.visual_current_paint(), Some(zoomed));
        // Leaving the tab clears it: nothing current anymore.
        state.overlay_view = None;
        assert_eq!(state.visual_current_paint(), None);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_zoom_and_scroll_clamp_to_the_overflow() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 1000, 1000));
        // Zoomed out at minimum: further out changes nothing.
        for _ in 0..20 {
            state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomOut, 200, 50, 8.0, 16.0);
        }
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.zoom, crate::visual::MIN_ZOOM);
        state.dirty = false;
        assert!(!state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomOut, 200, 50, 8.0, 16.0));
        assert!(!state.dirty, "no-op zoom stays clean");
        // Zoom to maximum: 1000x1000 at 8x overflows a 200x50 tab by
        // (600, 350) cells.
        for _ in 0..30 {
            state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0);
        }
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, crate::visual::MAX_ZOOM);
        assert!(!state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        state.dirty = false;
        assert!(state.visual_scroll(id, 5, 7, 200, 50, 8.0, 16.0));
        assert!(state.dirty, "scroll marks dirty");
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (5, 7));
        assert!(!state.visual_scroll(id, 0, 0, 200, 50, 8.0, 16.0), "no-op scroll");
        assert!(state.visual_scroll(id, 10_000, 10_000, 200, 50, 8.0, 16.0));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (600, 350), "pinned to overflow");
        assert!(state.visual_scroll(id, -1, -1, 200, 50, 8.0, 16.0));
        // Unknown sessions never dirty.
        state.dirty = false;
        let ghost = crate::session::SessionId::fresh();
        assert!(!state.visual_scroll(ghost, 1, 1, 200, 50, 8.0, 16.0));
        assert!(!state.visual_zoom(ghost, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        assert!(!state.dirty);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_paint_fits_and_centers_a_small_diagram() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 400, 400));
        // 400x400 at 8x16 cells is natively 50x25: scaled up until the
        // 50 rows are full, then centered across the 200 columns.
        let paint = state
            .visual_paint(id, ratatui::layout::Rect::new(1, 2, 200, 50), 8.0, 16.0)
            .expect("paint");
        assert_eq!((paint.out_cols, paint.out_rows), (100, 50));
        assert_eq!((paint.cursor_x, paint.cursor_y), (51, 2), "centered");
        assert_eq!((paint.sx, paint.sy, paint.sw, paint.sh), (0, 0, 400, 400));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_paint_png_skips_reencode_for_full_frames() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        let mut rgba = Vec::new();
        for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
            rgba.extend_from_slice(&p);
        }
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: fake_png(30, 2, 2),
            rgba,
            width: 2,
            height: 2,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            shapes: Vec::new(),
            vb: [0.0, 0.0, 2.0, 2.0],
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
        });
        let full = crate::visual::VisualPaint {
            zoom_bits: 1f32.to_bits(),
            ox: 0, oy: 0, out_cols: 1, out_rows: 1,
            cursor_x: 0, cursor_y: 0,
            sx: 0, sy: 0, sw: 2, sh: 2,
            selected: None,
        };
        assert_eq!(state.visual_frame_png(id, 1, full).unwrap(), fake_png(30, 2, 2));
        // A cropped viewport re-encodes just its region: top-left red.
        let cut = crate::visual::VisualPaint { sw: 1, sh: 1, ..full };
        let png = state.visual_frame_png(id, 1, cut).expect("cropped bytes");
        let (back, w, h) = crate::visual::decode_rgba(&png).expect("decodes");
        assert_eq!((w, h), (1, 1));
        assert_eq!(&back[..4], &[255, 0, 0, 255]);
        assert!(state.visual_frame_png(id, 2, full).is_none(), "stale generation");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_kitty_view_carries_the_zoom_strip_without_art() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        let view = state.visual_view(id, true);
        let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(blank.is_empty(), "spacer row: {blank:?}");
        let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
        assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(!text.contains('▀'), "image paints over the pane");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_leaves_a_spacer_row_above_the_strip() {
        // Row zero is always blank so the zoom strip and title sit one
        // row below the tab strip; the strip rides row one.
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        let view = state.visual_view(id, true);
        let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(blank.is_empty(), "spacer row: {blank:?}");
        let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
        assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_click_selects_shape_toggles_and_clears() {
        // Click resolution honors zoom and scroll: the same shape
        // selects whether the viewport is fitted or zoomed in and
        // panned. Clicking the selected shape toggles off; clicking
        // empty space clears.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 200,
            height: 100,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "big".to_string(), label: "Big".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
                ShapeBox { id: "small".to_string(), label: "Small".to_string(), x: 50.0, y: 30.0, width: 40.0, height: 20.0 },
            ],
            vb: [0.0, 0.0, 200.0, 100.0],
        });
        // Fitted: display matches the area, so pixel == SVG unit.
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(1));
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "toggle off");
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
        assert!(!state.visual_select_at(id, 5, 5, 200, 50, 8.0, 16.0, false), "empty space, nothing selected");
        // Zoomed and panned: the same shape still resolves.
        assert!(state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        assert!(state.visual_scroll(id, 10, 5, 200, 50, 8.0, 16.0));
        assert!(state.visual_select_at(id, 65, 19, 200, 50, 8.0, 16.0, false));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(1));
        // Empty space with a selection clears it.
        assert!(state.visual_select_at(id, 0, 0, 200, 50, 8.0, 16.0, false));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_click_selects_real_node_through_full_chain() {
        // End to end on a real render: node A's box center maps to a
        // viewport cell that selects A, and the fallback view shows
        // its label highlighted. This exercises layout, inversion,
        // selection, and rendering together instead of synthetics.
        let source = "flowchart LR\n    A[Start] --> B{Decision}\n    B -->|Yes| C[OK]\n    B -->|No| D[Cancel]\n";
        let frame = crate::visual::render_frame(source).expect("renders");
        let a = frame.shapes.iter().position(|s| s.id == "A").expect("node A");
        let node = &frame.shapes[a];
        let (disp_cols, disp_rows) =
            crate::visual::fit_display(frame.width, frame.height, 1.0, 200, 50, 8.0, 16.0);
        let (cpx, cpy) = crate::visual::svg_to_source(
            node.x + node.width / 2.0,
            node.y + node.height / 2.0,
            frame.width,
            frame.height,
            &frame.vb,
        );
        let vx = cpx.saturating_mul(disp_cols as u32) / frame.width.max(1);
        let vy = cpy.saturating_mul(disp_rows as u32) / frame.height.max(1);
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: frame.png,
            rgba: frame.rgba,
            width: frame.width,
            height: frame.height,
            title: "chain".to_string(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: frame.shapes,
            vb: frame.vb,
        });
        assert!(
            state.visual_select_at(id, vx.min(199) as u16, vy.min(49) as u16, 200, 50, 8.0, 16.0, false),
            "center of A selects (cell {vx},{vy})"
        );
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(a));
        let view = state.visual_view(id, false);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("Start"), "strip names the node: {text:?}");
        assert!(
            view.lines.iter().flat_map(|r| r.iter()).any(|s| {
                s.style.add_modifier.contains(ratatui::style::Modifier::REVERSED)
            }),
            "highlight renders"
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_click_accounts_for_centered_diagram() {
        // Live Ghostty geometry: a tall fitted diagram centers in a
        // wider region on the Kitty path, so the picker must subtract
        // the same offset the paint cursor adds. The click below hit
        // the raster edge left-aligned and the CFG node centered.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 408,
            height: 1496,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "CFG".to_string(), label: "Cfg".to_string(), x: 70.0, y: 311.0, width: 227.0, height: 51.0 },
            ],
            vb: [0.0, 0.0, 408.0, 1496.0],
        });
        assert!(
            !state.visual_select_at(id, 92, 15, 184, 68, 8.0, 16.0, false),
            "left-aligned misses the centered diagram"
        );
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
        assert!(
            state.visual_select_at(id, 92, 15, 184, 68, 8.0, 16.0, true),
            "centered hits the node"
        );
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(0));
        assert!(
            state.visual_select_at(id, 5, 5, 184, 68, 8.0, 16.0, true),
            "margin click clears"
        );
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_selection_marks_fallback_cells() {
        // The half-block fallback shows selection as reversed cells;
        // with no selection no cell reverses.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 64 * 64 * 4],
            width: 64,
            height: 64,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "left".to_string(), label: "Left".to_string(), x: 0.0, y: 0.0, width: 20.0, height: 64.0 },
            ],
            vb: [0.0, 0.0, 64.0, 64.0],
        });
        let plain: String = state.visual_view(id, false).lines.iter()
            .flat_map(|r| r.iter()).map(|s| s.text.as_str()).collect();
        assert!(!plain.is_empty());
        let reversed = |view: crate::ui::PaneView| {
            view.lines.iter().flat_map(|r| r.iter()).filter(|s| {
                s.style.add_modifier.contains(ratatui::style::Modifier::REVERSED)
            }).count()
        };
        assert_eq!(reversed(state.visual_view(id, false)), 0, "no selection, no marks");
        state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
        assert!(reversed(state.visual_view(id, false)) > 0, "selection marks cells");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_kitty_frame_bakes_selection_border() {
        // Kitty has no cell styles: the selection rides as a baked
        // border in the transmitted bytes, so the fingerprint must
        // change with it or the gate would swallow the highlight.
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        let rgba = vec![0u8; 64 * 64 * 4];
        let png = crate::visual::encode_png(&rgba, 64, 64).expect("encodes");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png,
            rgba,
            width: 64,
            height: 64,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: vec![
                crate::visual::ShapeBox { id: "box".to_string(), label: "Box".to_string(), x: 8.0, y: 8.0, width: 32.0, height: 32.0 },
            ],
            vb: [0.0, 0.0, 64.0, 64.0],
        });
        show_visual_overlay(&mut state, id);
        let has_border = |bytes: &[u8]| {
            let (rgba, _, _) = crate::visual::decode_rgba(bytes).expect("decodes");
            rgba.chunks_exact(4).any(|p| {
                p[0] == crate::visual::SELECT_RGB.0
                    && p[1] == crate::visual::SELECT_RGB.1
                    && p[2] == crate::visual::SELECT_RGB.2
            })
        };
        let paint = paint_for(&state, id);
        let plain = state.visual_frame_png(id, 1, paint).expect("bytes");
        assert!(!has_border(&plain), "no selection, no border");
        state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
        let paint = paint_for(&state, id);
        assert_ne!(paint.selected, None, "fingerprint carries selection");
        let marked = state.visual_frame_png(id, 1, paint).expect("bytes");
        assert_ne!(marked, plain, "bytes change with selection");
        assert!(has_border(&marked), "border baked in");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_question_submit_needs_selection_and_draft() {
        // Asking mirrors the walkthrough: selection plus a typed draft
        // submits shape context as markup, logs the pending question,
        // and clears the draft while keeping the highlight.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 64,
            height: 64,
            title: "flow".to_string(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 64.0, height: 64.0 },
            ],
            vb: [0.0, 0.0, 64.0, 64.0],
        });
        assert!(!state.submit_visual_question(id), "no selection, no draft");
        state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
        assert!(!state.submit_visual_question(id), "no draft yet");
        state.visual_slots.get_mut(&id).unwrap().draft = Some("  what does it do? ".to_string());
        assert!(state.submit_visual_question(id));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.questions.len(), 1);
        assert_eq!(slot.questions[0].shape_id, "A");
        assert_eq!(slot.questions[0].shape_label, "Start");
        assert_eq!(slot.questions[0].question, "what does it do?");
        assert!(slot.questions[0].answer.is_none());
        assert_eq!(slot.draft, None, "draft clears");
        assert_eq!(slot.selected, Some(0), "highlight stays");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_answer_resolves_latest_pending_only() {
        // The agent answers through the visual_answer tool, latest
        // pending first; thin air errors like the walkthrough twin.
        let mut state = AppState::new();
        let (id, live_run) = spawn_visual_agent(&mut state, "agent");
        let early = comms_reply(&mut state, &live_run, "visual_answer", r#"{"answer":"x"}"#);
        assert!(early.contains("no visual"), "early: {early}");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
            shapes: Vec::new(),
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        state.visual_slots.get_mut(&id).unwrap().shapes =
            vec![crate::visual::ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 }];
        state.visual_slots.get_mut(&id).unwrap().vb = [0.0, 0.0, 8.0, 8.0];
        state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
        state.visual_slots.get_mut(&id).unwrap().draft = Some("why?".to_string());
        assert!(state.submit_visual_question(id));
        let answered = comms_reply(&mut state, &live_run, "visual_answer", r#"{"answer":"because"}"#);
        assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
        assert_eq!(
            state.visual_slots.get(&id).unwrap().questions[0].answer.as_deref(),
            Some("because")
        );
        let stale = comms_reply(&mut state, &live_run, "visual_answer", r#"{"answer":"again"}"#);
        assert!(stale.contains("no visual question waiting"), "stale: {stale}");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_toggle_chat_flips_footer_rows() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        assert_eq!(state.visual_footer_rows(id, 20), 0, "no slot yet");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        assert_eq!(state.visual_footer_rows(id, 20), 0, "closed by default");
        assert!(state.visual_toggle_chat(id));
        assert!(state.visual_slots.get(&id).unwrap().chat_open);
        assert_eq!(state.visual_footer_rows(id, 20), 6, "open reserves 30%");
        assert_eq!(state.visual_footer_rows(id, 100), 30, "scales with the tab");
        assert!(state.visual_toggle_chat(id));
        assert!(!state.visual_slots.get(&id).unwrap().chat_open);
        assert_eq!(state.visual_footer_rows(id, 20), 0);
        let (other, _) = spawn_visual_agent(&mut state, "other");
        assert!(!state.visual_toggle_chat(other), "no slot, no toggle");
        assert!(state.manager.remove(other));
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_selection_opens_chat() {
        // Clicking a shape opens the dismissed chat again; the
        // toggle only wins until the next selection.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 200,
            height: 100,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            draft: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "b".to_string(), label: "B".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
            ],
            vb: [0.0, 0.0, 200.0, 100.0],
        });
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
        assert!(state.visual_slots.get(&id).unwrap().chat_open, "select opens");
        assert!(state.visual_toggle_chat(id), "dismissed");
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "toggle off");
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "reselect opens");
        assert!(state.visual_slots.get(&id).unwrap().chat_open);
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_footer_draws_a_titled_qa_box() {
        // The Q/A panel reads as one bordered box: rounded top with
        // the title, padded ask and history rows with solid sides,
        // rounded bottom. Every footer row is exactly content-wide so
        // the box edges align and nothing wraps.
        use ratatui::layout::Rect;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        assert!(state.visual_toggle_chat(id));
        let view = state.visual_view(id, true);
        let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
        let ask_idx = (chrome.footer.y - content.y) as usize;
        let block = &view.lines[ask_idx..ask_idx + foot as usize];
        let width = content.width as usize;
        for (i, row) in block.iter().enumerate() {
            assert_eq!(crate::ui::text::spans_width(row), width, "box row {i} fills");
        }
        let text = |row: &Vec<crate::ui::SpanView>| {
            row.iter().map(|s| s.text.as_str()).collect::<String>()
        };
        assert!(text(&block[0]).starts_with("╭") && text(&block[0]).contains("Q/A"), "titled top");
        assert!(text(&block[0]).ends_with("╮"), "top closes");
        assert!(text(&block[1]).starts_with("│"), "pad row sided");
        assert!(text(&block[foot as usize - 3]).starts_with("├"), "divider above input");
        assert!(text(&block[foot as usize - 2]).contains("Click a shape to ask"), "prompt docked at bottom");
        assert!(text(&block[foot as usize - 1]).starts_with("╰"), "bottom closes");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_ask_row_shows_prompt_placeholder_until_typed() {
        // Selected but nothing typed yet: `Ask about "S": >` plus a
        // gray `type question here` hint. The first keystroke swaps
        // the hint for the draft text.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 8 * 8 * 4],
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: Some(0),
            draft: None,
            input_active: false,
            chat_open: true,
            chat_scroll: 0,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
            ],
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        let find_ask = |view: crate::ui::PaneView| {
            view.lines
                .into_iter()
                .find(|r| {
                    r.iter().any(|s| s.text.contains(">"))
                        && !r.iter().any(|s| s.text.contains("waiting"))
                })
                .expect("prompt row")
        };
        let row = find_ask(state.visual_view(id, true));
        let text: String = row.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains(">"), "prompt marker: {text:?}");
        assert!(text.contains("type question here"), "placeholder: {text:?}");
        let hint = row.iter().find(|s| s.text.contains("type question here")).unwrap();
        assert_eq!(
            hint.style,
            crate::ui::theme::style(crate::ui::theme::Role::Muted),
            "placeholder is gray"
        );
        state.visual_slots.get_mut(&id).unwrap().draft = Some("why?".to_string());
        let row = find_ask(state.visual_view(id, true));
        let text: String = row.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains(">") && text.contains("why?"), "typed text: {text:?}");
        assert!(!text.contains("type question here"), "hint swapped out");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_select_arms_typing_without_enter() {
        // Selecting a shape arms the prompt at once: the first
        // keystroke types, no Enter needed to focus first.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 200,
            height: 100,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            draft: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            questions: Vec::new(),
            shapes: vec![
                ShapeBox { id: "b".to_string(), label: "B".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
            ],
            vb: [0.0, 0.0, 200.0, 100.0],
        });
        assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
        let slot = state.visual_slots.get(&id).unwrap();
        assert!(slot.input_active, "select arms typing");
        assert_eq!(slot.draft.as_deref(), Some(""), "empty draft ready");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_input_row_docks_at_box_bottom_with_divider() {
        // Claude-style layout: history on top, a divider line, then
        // the prompt row pinned above the bottom border.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 8 * 8 * 4],
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: Some(0),
            draft: Some("why?".to_string()),
            input_active: true,
            chat_open: true,
            chat_scroll: 0,
            questions: vec![crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "Start".to_string(),
                question: "what?".to_string(),
                answer: Some("because".to_string()),
            }],
            shapes: vec![
                ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
            ],
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        let view = state.visual_view(id, true);
        use ratatui::layout::Rect;
        let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
        let base = (chrome.footer.y - content.y) as usize;
        let text = |i: usize| {
            view.lines[i].iter().map(|s| s.text.as_str()).collect::<String>()
        };
        assert!(text(base + foot as usize - 1).starts_with("╰"), "bottom closes");
        assert!(text(base + foot as usize - 3).starts_with("├"), "divider above input");
        let input = text(base + foot as usize - 2);
        assert!(input.contains(">") && input.contains("why?"), "prompt row: {input:?}");
        let history: String = view.lines[base + 2..base + foot as usize - 3].iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(history.contains("what?") && history.contains("because"), "history on top");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_strip_carries_a_shortened_alt_in_kitty_mode() {
        // Kitty mode paints no art, so a standalone alt line would
        // hide under the image: the description rides the strip row
        // instead, capped with an ellipsis, and no second copy paints
        // below it. Short descriptions arrive whole.
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        state.visual_slots.get_mut(&id).unwrap().alt =
            "a very long diagram description that cannot fit the strip row at all".to_string();
        let view = state.visual_view(id, true);
        let strip = &view.lines[1];
        let text: String = strip.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("a very long diagram"), "alt head: {text:?}");
        assert!(text.contains("…"), "long alt shortens: {text:?}");
        assert!(!text.contains("at all"), "tail cut: {text:?}");
        let rest: String = view.lines[2..].iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(!rest.contains("a very long diagram"), "no occluded copy");
        state.visual_slots.get_mut(&id).unwrap().alt = "a diagram".to_string();
        let view = state.visual_view(id, true);
        let text: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("a diagram") && !text.contains("…"), "short alt whole: {text:?}");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_footer_grows_to_thirty_percent_of_a_tall_tab() {
        // A tall tab gives the Q/A panel room: 30% of content height
        // (ask row plus history), not the old fixed seven rows.
        use ratatui::layout::Rect;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        assert!(state.visual_toggle_chat(id));
        assert_eq!(state.visual_footer_rows(id, 36), 11);
        let view = state.visual_view(id, true);
        let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::layout::pane_content_area(&areas);
        assert_eq!(content.height, 36);
        let chrome = crate::ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        let ask_idx = (chrome.footer.y - content.y) as usize;
        assert_eq!(view.lines.len() - ask_idx, 11, "ask plus ten history rows");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_footer_renders_fixed_rows_with_markdown() {
        // Open chat costs its 30% footer rows: the ask row plus the
        // history viewport padded with blanks. Answers reuse the
        // walkthrough markdown skin, not plain text. A tall term
        // leaves the whole fixture visible in the viewport.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 8 * 8 * 4],
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: Some(0),
            draft: Some("why?".to_string()),
            input_active: true,
            chat_open: true,
            chat_scroll: 0,
            questions: vec![crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "Start".to_string(),
                question: "what?".to_string(),
                answer: Some("**bold** done".to_string()),
            }],
            shapes: vec![
                ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
            ],
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        let view = state.visual_view(id, false);
        let foot = state.visual_footer_rows(id, 36) as usize;
        assert!(view.lines.len() >= foot, "footer present");
        let tail = &view.lines[view.lines.len() - foot..];
        let text: String = tail.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("why?"), "ask row with draft: {text:?}");
        assert!(text.contains("what?"), "question: {text:?}");
        assert!(text.contains("bold") && text.contains("done"), "answer: {text:?}");
        assert!(
            tail.iter().flat_map(|r| r.iter()).any(|s| {
                s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)
            }),
            "markdown skin, not plain text"
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_history_separates_entries_with_blank_row() {
        // A second question must not sit directly under the previous
        // answer: entries are separated by one blank row, like the
        // walkthrough Q&A log.
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            draft: None,
            input_active: false,
            chat_open: true,
            chat_scroll: 0,
            questions: vec![
                crate::visual::VisualQuestion {
                    shape_id: "A".to_string(),
                    shape_label: "S".to_string(),
                    question: "q1".to_string(),
                    answer: Some("a1".to_string()),
                },
                crate::visual::VisualQuestion {
                    shape_id: "A".to_string(),
                    shape_label: "S".to_string(),
                    question: "q2".to_string(),
                    answer: Some("a2".to_string()),
                },
            ],
            shapes: Vec::new(),
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        let slot = state.visual_slots.get(&id).unwrap();
        let rows = state.visual_chat_history_lines(slot);
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect())
            .collect();
        let second_q = text
            .iter()
            .position(|r| r.contains("q2"))
            .expect("second question renders");
        assert!(
            second_q > 0 && text[second_q - 1].is_empty(),
            "blank row before second question: {text:?}"
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_scroll_clamps_and_retails() {
        // Wheel offset counts rows up from the tail; answering or
        // asking re-tails so fresh content is never stranded above.
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: None,
            draft: None,
            input_active: false,
            chat_open: true,
            chat_scroll: 0,
            questions: (0..10)
                .map(|i| crate::visual::VisualQuestion {
                    shape_id: "A".to_string(),
                    shape_label: "S".to_string(),
                    question: format!("q{i}"),
                    answer: Some(format!("a{i}")),
                })
                .collect(),
            shapes: Vec::new(),
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        assert!(state.visual_chat_scroll(id, 3, 5));
        assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 3);
        assert!(state.visual_chat_scroll(id, -99, 5));
        assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 0, "clamped to tail");
        assert!(state.visual_chat_scroll(id, 9999, 5));
        let max = state.visual_slots.get(&id).unwrap().chat_scroll;
        assert!(max > 0, "history exceeds the viewport");
        assert!(!state.visual_chat_scroll(id, 9999, 5), "clamped at head");
        assert!(!state.visual_chat_scroll(id, 0, 5), "no-op at rest");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_submit_and_answer_retail_the_chat() {
        // Asking or answering re-tails so fresh content is never
        // stranded above the viewport.
        let mut state = AppState::new();
        let (id, live_run) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: Vec::new(),
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: Some(0),
            draft: Some("why?".to_string()),
            input_active: true,
            chat_open: true,
            chat_scroll: 5,
            questions: Vec::new(),
            shapes: vec![
                crate::visual::ShapeBox { id: "A".to_string(), label: "S".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
            ],
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        assert!(state.submit_visual_question(id));
        assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 0, "ask re-tails");
        assert!(state.visual_slots.get(&id).unwrap().input_active, "submit stays armed");
        state.visual_slots.get_mut(&id).unwrap().chat_scroll = 5;
        let out = comms_reply(&mut state, &live_run, "visual_answer", r#"{"answer":"because"}"#);
        assert!(out.contains(r#""answered":true"#), "answered: {out}");
        assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 0, "answer re-tails");
        assert!(state.manager.remove(id));
    }

    #[cfg(all(target_os = "linux", feature = "visual"))]
    #[test]
    fn screenshot_tool_needs_an_app_name() {
        // Broker wiring without touching the compositor: arg
        // validation answers before any subprocess spawns.
        let mut state = AppState::new();
        let (id, live_run) = spawn_visual_agent(&mut state, "agent");
        let out = comms_reply(&mut state, &live_run, "screenshot", "{}");
        assert!(out.contains("needs an app name"), "out: {out}");
        let out = comms_reply(&mut state, &live_run, "screenshot", r#"{"app":""}"#);
        assert!(out.contains("needs an app name"), "blank: {out}");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_shows_input_row_and_pending_answer() {
        // The footer pins the prompt row under a selection and the
        // latest Q&A above it: question plus waiting marker until
        // answered. A tall term leaves both visible in the viewport.
        use crate::visual::ShapeBox;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 8 * 8 * 4],
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            selected: Some(0),
            input_active: false,
            chat_open: true,
            chat_scroll: 0,
            draft: Some("why?".to_string()),
            questions: vec![crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "Start".to_string(),
                question: "what?".to_string(),
                answer: None,
            }],
            shapes: vec![
                ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
            ],
            vb: [0.0, 0.0, 8.0, 8.0],
        });
        let view = state.visual_view(id, false);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("Start") && text.contains("why?"), "ask row: {text:?}");
        assert!(text.contains("what?"), "question: {text:?}");
        assert!(text.contains(crate::visual::VISUAL_WAITING_TEXT), "waiting: {text:?}");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_pins_footer_to_chrome_footer_in_kitty_mode() {
        // Kitty mode emits no art: blank filler must push the ask row
        // onto the chrome footer rows. Without it the footer paints
        // under the strip, the transmitted image paints over it, and
        // wheel routing (which uses the chrome rect) desyncs from
        // what is on screen.
        use ratatui::layout::Rect;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        assert!(state.visual_toggle_chat(id));
        let view = state.visual_view(id, true);
        let (rows, cols) = state.term_size;
        let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, cols, rows));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
        let ask_idx = (chrome.footer.y - content.y) as usize;
        assert!(
            ask_idx < view.lines.len(),
            "ask row on screen: {} lines for footer at {ask_idx}",
            view.lines.len()
        );
        let top: String = view.lines[ask_idx].iter().map(|s| s.text.as_str()).collect();
        assert!(top.contains("Q/A"), "box opens the footer: {top:?}");
        let ask: String = view.lines[ask_idx + foot as usize - 2].iter().map(|s| s.text.as_str()).collect();
        assert!(ask.contains("Click a shape to ask"), "prompt docked: {ask:?}");
        assert_eq!(view.lines.len() - ask_idx, foot as usize, "footer is the last block");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_pins_footer_below_small_fallback_art() {
        // A diagram smaller than the image region leaves blank rows;
        // the footer must still land on the chrome footer, not float
        // directly under the art.
        use ratatui::layout::Rect;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba: vec![0u8; 8 * 8 * 4],
            width: 8,
            height: 8,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            shapes: Vec::new(),
            vb: [0.0, 0.0, 8.0, 8.0],
            selected: None,
            input_active: false,
            chat_open: true,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
        });
        let view = state.visual_view(id, false);
        let (rows, cols) = state.term_size;
        let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, cols, rows));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
        let ask_idx = (chrome.footer.y - content.y) as usize;
        assert!(
            ask_idx < view.lines.len(),
            "ask row on screen: {} lines for footer at {ask_idx}",
            view.lines.len()
        );
        let top: String = view.lines[ask_idx].iter().map(|s| s.text.as_str()).collect();
        assert!(top.contains("Q/A"), "box opens the footer: {top:?}");
        let ask: String = view.lines[ask_idx + foot as usize - 2].iter().map(|s| s.text.as_str()).collect();
        assert!(ask.contains("Click a shape to ask"), "prompt docked: {ask:?}");
        assert_eq!(view.lines.len() - ask_idx, foot as usize, "footer is the last block");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_renders_on_the_bottom_rows_of_the_real_screen() {
        // End to end through the real renderer: a diagram with an
        // open chat and a waiting question must paint the strip at
        // the top and the footer pinned to the bottom, with nothing
        // leaking into the image rows between.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        let mut state = AppState::new();
        state.term_size = (40, 100);
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        assert!(state.visual_toggle_chat(id));
        state
            .visual_slots
            .get_mut(&id)
            .unwrap()
            .questions
            .push(crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "Start".to_string(),
                question: "what is this?".to_string(),
                answer: None,
            });
        let view = state.visual_view(id, true);
        let (rows, cols) = state.term_size;
        let area = Rect::new(0, 0, cols, rows);
        let areas = crate::ui::layout::chrome_areas(area);
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
        let chrome_ui = crate::ui::Chrome {
            tabs: vec![crate::ui::session_bar::SessionTab {
                title: "agent".to_string(),
                live: true,
                focused: true,
                group: None,
                group_color: None,
            }],
            topbar: crate::ui::topbar::TopBar { tabs: Vec::new() },
            detail: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
            board_open: false,
            board: None,
            tetris_open: false,
            tetris: None,
            grid: false,
            pills: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|f| crate::ui::render(f, area, &[view], &chrome_ui)).unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let screen: Vec<String> = buf
            .content
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
            .collect();
        assert!(screen[content.y as usize + 1].contains("chat"), "strip: {:?}", screen[3]);
        let foot_y = chrome.footer.y as usize;
        assert!(screen[foot_y].contains("╭─ Q/A"), "box opens on screen");
        assert!(screen[foot_y + foot as usize - 1].contains("╰"), "box closes on screen");
        let bottom: String = screen[foot_y..foot_y + foot as usize].join("\n");
        assert!(bottom.contains("Click a shape to ask"), "ask row: {bottom:?}");
        assert!(bottom.contains("what is this?"), "question: {bottom:?}");
        assert!(
            bottom.contains(crate::visual::VISUAL_WAITING_TEXT),
            "waiting: {bottom:?}"
        );
        let middle: String = screen[content.y as usize + 2..foot_y].join("\n");
        assert!(
            !middle.contains("what is this?"),
            "footer must not float under the strip"
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_falls_back_to_halfblock_with_alt() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        // 2x2: red/green over blue/white, titled with alt text.
        let mut rgba = Vec::new();
        for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
            rgba.extend_from_slice(&p);
        }
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba,
            width: 2,
            height: 2,
            title: "flow".to_string(),
            alt: "a diagram".to_string(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            shapes: Vec::new(),
            vb: [0.0, 0.0, 2.0, 2.0],
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
        });
        let view = state.visual_view(id, false);
        assert!(view.title.contains("Visual"), "pane title: {}", view.title);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("flow"), "title line: {text:?}");
        assert!(text.contains("a diagram"), "alt line: {text:?}");
        assert!(text.contains('▀'), "half-block art: {text:?}");
        // Row zero is the blank spacer; the zoom strip carrying the
        // title rides row one and art follows, since the fitted image
        // fills every row below the strip.
        let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(blank.is_empty(), "spacer row: {blank:?}");
        let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
        assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
        assert!(strip.contains("flow"), "title on the strip: {strip:?}");
        let cell = &view.lines[2][0];
        assert_eq!(cell.style.fg, Some(ratatui::style::Color::Rgb(255, 0, 0)));
        assert_eq!(cell.style.bg, Some(ratatui::style::Color::Rgb(0, 0, 255)));
        // Kitty mode carries title and alt for the record, no art.
        let view = state.visual_view(id, true);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("flow") && text.contains("a diagram"));
        assert!(!text.contains('▀'), "image paints over the pane");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_tool_round_trip_renders_stores_and_shows() {
        // Acceptance: tool call → background worker → sticky slot →
        // fallback view → one terminal show claim. Bounded wait, same
        // drain the main loop performs.
        let mut state = AppState::new();
        let (id, live_run) = spawn_visual_agent(&mut state, "agent");
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B","format":"mermaid","title":"flow","alt":"a to b"}"#,
        );
        assert!(out.contains(r#""accepted":true"#), "queued: {out}");
        let mut stored = None;
        for _ in 0..100 {
            state.drain_visual();
            if let Some(slot) = state.visual_slots.get(&id) {
                stored = Some((slot.generation, slot.width, slot.height));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let (generation, width, height) =
            stored.expect("worker stored the frame");
        assert_eq!(generation, 1);
        // Content-scaled, font-measured: bounds stay loose on purpose.
        assert!(width > 50 && height > 20, "real raster: {width}x{height}");
        let view = state.visual_view(id, false);
        let text: String = view
            .lines
            .iter()
            .flat_map(|row| row.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(text.contains("flow"), "title: {text:?}");
        assert!(text.contains("a to b"), "alt: {text:?}");
        assert!(text.contains('▀'), "art: {text:?}");
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show");
        assert_eq!(spec.generation, 1);
        assert!(state.visual_frame_png(id, 1, paint).is_some(), "bytes behind the claim");
    }

    #[test]
    fn visual_tab_keeps_its_label_under_hook_traffic() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let run = crate::infra::ids::RunId::generate();
        let id = state.manager.spawn_agent("agent", &std::env::temp_dir(), "exec cat", run.clone(), "codex").unwrap();
        let (reply, _) = std::sync::mpsc::channel();
        state.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
            hook: "PreToolUse".into(), body: "{}".into(), run_id: run.as_str().into(),
            sync: false, reply, timed_out: Default::default(),
        }));
        assert_eq!(state.topbar().tabs[3].label, "📷 Visual");
        assert!(state.manager.remove(id));
    }
}
