//! Single-owner application state.
//!
//! The main loop (and, in tests, the harness directly) reduces every
//! `AppEvent` through [`AppState::apply`]. Workers never touch this struct;
//! dirty-flag rendering and shutdown flow out of the same reduction.

use crate::event::AppEvent;
use crate::session::SessionManager;

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
    pub pending_hooks: std::collections::VecDeque<crate::listener::HookRequest>,
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
    pub telegram_config: std::sync::Arc<std::sync::Mutex<crate::config::TelegramConfig>>,
    /// Open Telegram settings form (`Ctrl-b m`), if any.
    pub telegram_dialog: Option<crate::telegram_dialog::TelegramDialog>,
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
    /// Replies for the Telegram poller thread to send, shared by handle.
    /// Bounded by [`crate::telegram::OUTBOX_CAP`]; overflow counts in
    /// `telegram_outbox_dropped` instead of growing the owner.
    pub telegram_outbox: std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<crate::telegram::OutboundMessage>>,
    >,
    /// Telegram replies dropped past the outbox cap.
    telegram_outbox_dropped: u64,
    /// Session most recently badged by `message_user`: bare operator
    /// text answers it. `None` until the first badge.
    last_telegram_badged: Option<crate::session::SessionId>,
    /// Poller thread running. Set once at spawn; the main loop starts
    /// the thread the first pass it sees Telegram enabled, so enabling
    /// from the modal needs no restart.
    pub telegram_poller: bool,
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
    message_user_idem: crate::bot::IdemCache,
    pub create_dialog: Option<crate::create::CreateDialog>,
    /// Open group-management dialog, if any. Captures all input while
    /// present; never both dialogs at once (openers are unreachable
    /// behind the other dialog).
    pub group_dialog: Option<crate::groups::GroupDialog>,
    /// Startup restore picker, if a sessions file offered entries. First
    /// input goes here until it resolves to a pick or a fresh start.
    pub restore_picker: Option<crate::checkpoint::RestorePicker>,
    /// Quit-confirmation modal, if the quit chord is pending an answer.
    /// Captures all input while present; No is the default.
    pub quit_confirm: Option<crate::quit::QuitConfirm>,
    /// Live permission mode. The TUI loop rebuilds policy and persists the
    /// config whenever this diverges from the loaded one.
    pub permission_mode: crate::config::PermissionMode,
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

/// Overlay slot past the PTY tabs: Events, Tasks, Visual, Walkthrough.
/// Agent sessions always carry exactly three PTY tabs, so absolute
/// indices stay stable (3/4/5/6); other overlays are unreachable on
/// single-tab shells by the same gate as the topbar.
pub const OVERLAY_TABS: [&str; 4] = ["Events", "Tasks", "Visual", "Walkthrough"];

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
                crate::config::TelegramConfig::default(),
            )),
            telegram_dialog: None,
            telegram_test_tx,
            telegram_test_rx,
            message_user_windows: std::collections::HashMap::new(),
            message_user_badges: std::collections::HashMap::new(),
            message_user_idem: crate::bot::IdemCache::new(),
            telegram_outbox: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            telegram_outbox_dropped: 0,
            last_telegram_badged: None,
            telegram_poller: false,
            away: false,
            last_input: std::time::Instant::now(),
            telegram_seq: 0,
            create_dialog: None,
            group_dialog: None,
            restore_picker: None,
            quit_confirm: None,
            permission_mode: crate::config::PermissionMode::Yolo,
            broker: crate::comms::Broker::new(),
            last_human_input: std::collections::HashMap::new(),
            last_broker_tick: None,
            last_hook_activity: std::collections::HashMap::new(),
            pending_enter: std::collections::HashMap::new(),
            overlay_view: None,
            walkthroughs: std::collections::HashMap::new(),
            grid_mode: false,
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

    /// Flip grid mode; selecting a session by number leaves it.
    pub fn toggle_grid(&mut self) {
        self.grid_mode = !self.grid_mode;
        self.dirty = true;
    }

    /// Set the live permission mode; true when it changed. Non Off/Yolo
    /// modes collapse to Yolo on toggle, never back (toggle only spans
    /// the two sidebar buttons).
    pub fn set_permission_mode(&mut self, mode: crate::config::PermissionMode) -> bool {
        if self.permission_mode == mode {
            return false;
        }
        self.permission_mode = mode;
        self.dirty = true;
        true
    }

    /// Toggle Off <-> Yolo for the keyboard path.
    pub fn toggle_permission_mode(&mut self) -> bool {
        let next = match self.permission_mode {
            crate::config::PermissionMode::Yolo => crate::config::PermissionMode::Off,
            _ => crate::config::PermissionMode::Yolo,
        };
        self.set_permission_mode(next)
    }

    /// Per-session tab strip for the focused session: agent CLI, human
    /// terminal, and lazygit SCM tabs, plus read-only overlay views.
    /// Empty when nothing is focused.
    pub fn topbar(&self) -> crate::ui::TopBar {
        let Some(id) = self.manager.active() else {
            return crate::ui::TopBar::default();
        };
        let Some(rec) = self.manager.get(id) else {
            return crate::ui::TopBar::default();
        };
        let mut tabs: Vec<crate::ui::TopTab> = rec
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| crate::ui::TopTab {
                label: match tab.kind {
                    crate::session::TabKind::Agent => {
                        let mut chars = rec.cli_tool.chars();
                        match chars.next() {
                            None => "Agent".to_string(),
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                        }
                    }
                    crate::session::TabKind::Terminal => "Terminal".to_string(),
                    crate::session::TabKind::Scm => "SCM".to_string(),
                },
                active: i == rec.active_tab,
            })
            .collect();
        if rec.tabs.len() > 1 && self.term_size.1 >= 100 {
            for (index, label) in OVERLAY_TABS.iter().enumerate() {
                tabs.push(crate::ui::TopTab {
                    label: (*label).to_string(),
                    active: self.overlay_view == Some((id, index + rec.tabs.len())),
                });
            }
            if self.overlay_view.is_some_and(|(view_id, _)| view_id == id) {
                for tab in tabs.iter_mut().take(rec.tabs.len()) { tab.active = false; }
            }
        }
        // Emoji icons: each is one codepoint with default emoji
        // presentation, so every icon is unambiguously two cells wide
        // (no VS16, no ambiguous-width glyphs) and `Line::width`
        // measures the buttons exactly.
        if self.term_size.1 >= 100 {
            for (tab, icon) in tabs.iter_mut().zip(["🤖", "💻", "🔀", "🔔", "📝", "📷", "📖"]) {
                tab.label = format!("{icon} {}", tab.label);
            }
        }
        crate::ui::TopBar { tabs }
    }

    /// Select a PTY tab or one of the read-only view slots shown in the
    /// agent's top bar. Extra views never create a PTY or accept typing.
    pub fn select_top_tab(&mut self, index: usize) -> bool {
        let Some(id) = self.manager.active() else { return false; };
        let Some(rec) = self.manager.get(id) else { return false; };
        if index >= self.topbar().tabs.len() { return false; }
        if index < rec.tabs.len() {
            let was_overlay = self.overlay_view.take().is_some();
            let changed = self.manager.select_tab(id, index);
            self.dirty |= was_overlay || changed;
            was_overlay || changed
        } else {
            let next = Some((id, index));
            let changed = self.overlay_view != next;
            self.overlay_view = next;
            self.dirty |= changed;
            changed
        }
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay_view.is_some_and(|(id, _)| self.manager.active() == Some(id))
    }

    /// Absolute topbar index of the Walkthrough overlay slot for one
    /// session, or `None` for an unknown session.
    fn walkthrough_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Walkthrough")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The focused walkthrough, if the active session sits on the
    /// Walkthrough overlay slot and the agent opened a tour. The TUI
    /// renders it immediate-mode over the pane grid.
    pub fn walkthrough_overlay(&self) -> Option<&crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get(&active)
    }

    /// Mutable twin of [`Self::walkthrough_overlay`], for key routing.
    pub fn walkthrough_overlay_mut(
        &mut self,
    ) -> Option<&mut crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get_mut(&active)
    }

    /// True while walkthrough keys own input: the overlay slot is
    /// focused and a tour is open for the active session.
    pub fn walkthrough_overlay_active(&self) -> bool {
        self.walkthrough_overlay().is_some()
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
        dir: crate::ui::VisualButton,
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
            crate::ui::VisualButton::ZoomIn => ZoomDir::In,
            crate::ui::VisualButton::ZoomOut => ZoomDir::Out,
            // The chat toggle never reaches zoom: the mouse path
            // routes it to the toggle first. Unreachable by design.
            crate::ui::VisualButton::Chat => return false,
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
            Some(slot) if slot.chat_open => crate::ui::visual_chat_footer_rows(content_h),
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
        let areas = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        crate::ui::pane_content_area(&areas).width.saturating_sub(6)
    }

    /// All chat history rows for one slot, oldest first: one wrapped
    /// question row plus markdown answer rows (or the waiting marker)
    /// per pair. The viewport shows the tail of these.
    #[cfg(feature = "visual")]
    fn visual_chat_history_lines(&self, slot: &VisualSlot) -> Vec<Vec<crate::ui::SpanView>> {
        use crate::theme::{style, Role};
        let width = self.visual_footer_width();
        let text = style(Role::Text);
        let muted = style(Role::Muted);
        let mut rows = Vec::new();
        for q in &slot.questions {
            rows.extend(crate::ui::wrap_spans(
                vec![crate::ui::SpanView {
                    text: format!(
                        "Q ({}): {}",
                        crate::safe_text::encode_for_display(&q.shape_label),
                        crate::safe_text::encode_for_display(&q.question)
                    ),
                    style: text,
                }],
                width,
            ));
            match q.answer.as_deref() {
                Some(a) => {
                    for line in crate::walkthrough::md_text(a).lines {
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
                            rows.extend(crate::ui::wrap_spans(spans, width));
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
        let text = crate::theme::style(crate::theme::Role::Text);
        let muted = crate::theme::style(crate::theme::Role::Muted);
        let line = |content: &str, style: Style| {
            vec![crate::ui::SpanView {
                text: content.to_string(),
                style,
            }]
        };
        let Some(slot) = self.visual_slots.get(&id) else {
            return crate::ui::PaneView {
                title,
                lines: vec![line(
                    "No visualization yet — ask this session to show one",
                    muted,
                )],
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
        let areas = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        let content = crate::ui::pane_content_area(&areas);
        let foot_rows = self.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, self.pill_tabs, foot_rows);
        let mut strip = crate::ui::visual_button_spans(self.pill_tabs, slot.chat_open);
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
                    crate::safe_text::encode_for_display(&shape.label)
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
                    crate::safe_text::encode_for_display(&slot.alt)
                ),
                style: muted,
            }];
            strip.extend(crate::ui::truncate_spans(
                alt,
                crate::ui::VISUAL_STRIP_ALT_MAX,
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
                let w = crate::ui::spans_width(&row);
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
                crate::ui::visual_chat_history_rows(foot_rows) as usize,
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
                            text: format!("{}{}", crate::safe_text::encode_for_display(d), cursor),
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
            input_line.extend(fill_content(crate::ui::truncate_spans(
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

    /// Placeholder pane behind the immediate-mode tour render: the TUI
    /// draws the walkthrough over the grid, so this only carries the
    /// title (and a hint when no tour is open yet).
    pub fn walkthrough_view(&self, id: crate::session::SessionId) -> crate::ui::PaneView {
        let rec = self.manager.get(id).expect("ordered session exists");
        let live = rec.state.is_live();
        let hint = if self.walkthroughs.contains_key(&id) {
            "Walkthrough tour"
        } else {
            "No walkthrough started for this session"
        };
        crate::ui::PaneView {
            title: format!("{} · Walkthrough", rec.name),
            lines: vec![vec![crate::ui::SpanView {
                text: hint.to_string(),
                style: crate::theme::style(crate::theme::Role::Muted),
            }]],
            live,
            focused: true,
            cursor: None,
        }
    }

    /// Submit the overlay draft as a question: the markup goes straight
    /// into the agent pane and the Enter stages for a later tick — the
    /// same split write comms injections use. The question is logged
    /// only after the pane write lands, so the overlay never claims an
    /// undelivered ask. No human-input note: that would drop the staged
    /// CR we just armed.
    pub fn submit_walkthrough_question(&mut self, id: crate::session::SessionId) -> bool {
        let Some(wt) = self.walkthroughs.get(&id) else {
            return false;
        };
        let draft = match wt.input.as_ref() {
            Some(buf) if !buf.trim().is_empty() => buf.trim().to_string(),
            _ => return false,
        };
        let markup = crate::walkthrough::Walkthrough::question_markup(
            &wt.title,
            wt.index,
            wt.steps.len(),
            wt.current_step(),
            &draft,
        );
        if self.manager.inject_write(id, markup.as_bytes()).is_err() {
            return false;
        }
        self.pending_enter.insert(id, (std::time::Instant::now(), None));
        let Some(wt) = self.walkthroughs.get_mut(&id) else {
            return false;
        };
        wt.take_draft();
        wt.push_question(draft);
        self.dirty = true;
        true
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

    /// One string tool arg: JSON escapes decoded (agents must escape
    /// newlines, so multi-line steps and Markdown answers arrive
    /// intact), blanked to missing.
    fn tool_arg(args: &str, name: &str) -> Option<String> {
        crate::policy::json_string_field(args.as_bytes(), &[name])
            .map(|s| crate::mcp::decode_json_string(&s))
            .filter(|s| !s.is_empty())
    }

    /// One boolean tool arg from a bare JSON literal.
    fn tool_bool(args: &str, name: &str) -> Option<bool> {
        match crate::mcp::top_raw(args, name)?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// One integer tool arg: bare JSON numbers, quoted ones read
    /// liberally too. Fractions, negatives, and overflow never pass.
    fn tool_u32(args: &str, name: &str) -> Option<u32> {
        let raw = crate::mcp::top_raw(args, name)?.trim();
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

    /// Resolve a tool caller to its live session, rebound IDs included:
    /// the run must still belong to the record that holds it.
    fn resolve_tool_caller(&self, run_id: &str) -> Result<crate::session::SessionId, String> {
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
    fn walkthrough_tool(
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
    fn visual_tool(
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
        match crate::screenshot::capture(&app, focus) {
            Ok(shot) => Ok(format!(
                "{{\"path\":{},\"app\":{},\"width\":{},\"height\":{}}}",
                crate::mcp::escape_json(&shot.path),
                crate::mcp::escape_json(&format!("{} — {}", shot.class, shot.title)),
                shot.width,
                shot.height,
            )),
            Err(e) => Err(e),
        }
    }

    /// Answer stub when the feature is off: the tool name never
    /// advertises, so reaching here means a forged call.
    #[cfg(not(feature = "visual"))]
    fn visual_tool(
        &mut self,
        _run_id: &str,
        _tool: &str,
        _args: &str,
    ) -> Option<Result<String, String>> {
        None
    }

    /// Open a tour over a file: relative paths resolve against the
    /// session cwd, oversize files are refused, and the overlay opens
    /// on the tour at once so the human sees it.
    fn walkthrough_start(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let file = Self::tool_arg(args, "file")
            .ok_or_else(|| "walkthrough_start needs a file".to_string())?;
        let steps_text = Self::tool_arg(args, "steps")
            .ok_or_else(|| "walkthrough_start needs steps".to_string())?;
        let title = Self::tool_arg(args, "title").unwrap_or_else(|| file.clone());
        let cwd = self
            .manager
            .get(id)
            .map(|rec| rec.cwd.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = std::path::PathBuf::from(&file);
        let path = if path.is_relative() { cwd.join(path) } else { path };
        let bytes = std::fs::read(&path)
            .map_err(|_| format!("cannot read walkthrough file {file:?}"))?;
        if bytes.len() > crate::walkthrough::MAX_FILE_BYTES {
            return Err(format!(
                "walkthrough file {file:?} exceeds {} bytes",
                crate::walkthrough::MAX_FILE_BYTES
            ));
        }
        let content = String::from_utf8_lossy(&bytes);
        let steps = crate::walkthrough::Walkthrough::parse_steps(&steps_text, content.lines().count())?;
        let tour = crate::walkthrough::Walkthrough::start(title, file, &content, steps)?;
        let count = tour.step_count();
        self.walkthroughs.insert(id, tour);
        if let Some(slot) = self.walkthrough_slot(id) {
            self.overlay_view = Some((id, slot));
        }
        self.dirty = true;
        Ok(format!(r#"{{"started":true,"steps":{count}}}"#))
    }

    /// Insert one step into the open tour. A `file_path` that is not
    /// the tour file fails: tours cover one file. `position` is
    /// 1-based like the displayed counter and appends when absent.
    fn walkthrough_add_step(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let start = Self::tool_u32(args, "start_line")
            .ok_or_else(|| "walkthrough_add_step needs start_line".to_string())?;
        let end = Self::tool_u32(args, "end_line")
            .ok_or_else(|| "walkthrough_add_step needs end_line".to_string())?;
        let explanation = Self::tool_arg(args, "explanation").unwrap_or_default();
        let position = Self::tool_u32(args, "position")
            .map(|p| (p.max(1) as usize).saturating_sub(1));
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        if let Some(file) = Self::tool_arg(args, "file_path") {
            if file != tour.file_path {
                return Err("walkthrough_add_step targets the open tour file only".to_string());
            }
        }
        tour.add_step(
            crate::walkthrough::Step { start, end, explanation },
            position,
        )?;
        let count = tour.step_count();
        self.dirty = true;
        Ok(format!(r#"{{"added":true,"steps":{count}}}"#))
    }

    /// Update one step of the open tour. `step_index` is 1-based like
    /// the displayed counter; absent fields keep their values.
    fn walkthrough_update(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let one_based = Self::tool_u32(args, "step_index")
            .ok_or_else(|| "walkthrough_update needs step_index".to_string())?;
        let Some(index) = one_based.checked_sub(1) else {
            return Err("step_index starts at 1".to_string());
        };
        let start = Self::tool_u32(args, "start_line");
        let end = Self::tool_u32(args, "end_line");
        let explanation = Self::tool_arg(args, "explanation");
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        tour.update_step(index as usize, start, end, explanation.as_deref())?;
        self.dirty = true;
        Ok(r#"{"updated":true}"#.to_string())
    }

    /// Execute one session-lifecycle MCP tool. `None` when the name is
    /// not a session tool and the broker should answer instead.
    fn session_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "start_session" | "set_session_status" | "clear_session_status" | "message_user" => {}
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
    /// operator off an Events or Walkthrough view they opened. Narrow
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
        let harness = crate::harness::Harness::from_name(&harness_name)
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
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Agent(harness),
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
                    conv: crate::ids::ConversationId::generate().to_string(),
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
            crate::mcp::escape_json(&name),
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
        let kind = crate::session_status::StatusKind::from_name(&kind).ok_or_else(|| {
            "unknown status kind (info/progress/success/warning/blocked/question)".to_string()
        })?;
        let message = Self::tool_arg(args, "message")
            .ok_or_else(|| "set_session_status needs a message".to_string())?;
        let message = crate::session_status::validate(&message)?;
        let Some(rec) = self.manager.get_mut(caller) else {
            return Err("unknown or stale run ID".to_string());
        };
        rec.status = Some(crate::session_status::SessionStatus { kind, message: message.clone() });
        self.dirty = true;
        Ok(format!(
            r#"{{"kind":{},"message":{}}}"#,
            crate::mcp::escape_json(kind.as_str()),
            crate::mcp::escape_json(&message),
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
        let fingerprint = crate::bot::fingerprint("message_user", &[&message]);
        let idem_key = Self::tool_arg(args, "idempotency_key").map(|k| format!("{caller:?}:{k}"));
        if let Some(ref key) = idem_key {
            match self.message_user_idem.check(key, fingerprint, now) {
                crate::bot::IdemCheck::Hit(result) => return Ok(result),
                crate::bot::IdemCheck::Conflict => {
                    return Err(
                        "idempotency_key conflict: same key, different message".to_string(),
                    )
                }
                crate::bot::IdemCheck::Miss => {}
            }
        }
        let window = self.message_user_windows.entry(caller).or_default();
        if !crate::telegram::check_message_rate(window, now) {
            return Err("message_user rate limited: 3 per 60 seconds".to_string());
        }
        let text = crate::telegram::truncate_text(&message, crate::telegram::MAX_TEXT).to_string();
        self.message_user_badges.insert(caller, text.clone());
        self.last_telegram_badged = Some(caller);
        let conv = crate::ids::ConversationId::generate().to_string();
        let cfg = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.clone())
            .unwrap_or_default();
        let outgoing = crate::telegram::forward_text(&session_name, &text);
        let forwarded = cfg.enabled
            && cfg.notify_chat_id != 0
            && crate::telegram::read_token(&cfg.token_file).is_ok()
            && std::thread::Builder::new()
                .name("telegram-send".to_string())
                .spawn(move || {
                    let Ok(token) = crate::telegram::read_token(&cfg.token_file) else {
                        return;
                    };
                    if crate::telegram::send_message(&token, cfg.notify_chat_id, &outgoing).is_err()
                    {
                        eprintln!("warning: telegram forward failed");
                    }
                })
                .is_ok();
        self.dirty = true;
        let result = format!(r#"{{"conversation_id":"{conv}","forwarded":{forwarded}}}"#);
        if let Some(key) = idem_key {
            self.message_user_idem.store(&key, fingerprint, &result, now);
        }
        Ok(result)
    }

    /// Operator command help surfaced by `/help`.
    const TELEGRAM_HELP: &'static str = "forge operator commands:\n/sessions — list live sessions\n/help — this help\n/int <session> — interrupt the agent (Ctrl-C)\n/clear <session> — drop queued injections\nAddress a session with `[name] message`; bare text answers the last badged session.";

    /// Queue one Telegram reply for the poller thread. Truncated and
    /// bounded: past [`crate::telegram::OUTBOX_CAP`] newcomers drop and
    /// count instead of growing the owner.
    fn telegram_send(&mut self, chat_id: i64, text: &str) {
        let text =
            crate::telegram::truncate_text(text, crate::telegram::MAX_TEXT).to_string();
        let Ok(mut outbox) = self.telegram_outbox.lock() else {
            return;
        };
        if outbox.len() >= crate::telegram::OUTBOX_CAP {
            self.telegram_outbox_dropped += 1;
        } else {
            outbox.push_back(crate::telegram::OutboundMessage { chat_id, text });
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
                let state = if rec.state.is_live() { "live" } else { "exited" };
                lines.push(format!("{} ({state})", rec.name));
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
    fn drain_telegram(&mut self) {
        for _ in 0..crate::telegram::ROUTE_BATCH {
            let Some(msg) = self.telegram_inbox.pop_front() else {
                break;
            };
            self.route_telegram(msg);
        }
    }

    /// Operator reply guidance appended to every routed injection, so
    /// the agent knows how to answer.
    const TELEGRAM_REPLY_HINT: &'static str = "Reply to the operator with message_user.";

    /// Route one allowlisted operator text: `/` commands and errors
    /// answer directly, while `<name>:` prefixes and bare text (to the
    /// last badged session) queue a silent idle-gated injection.
    fn route_telegram(&mut self, msg: crate::telegram::InboundMessage) {
        let text = msg.text.trim().to_string();
        if let Some(command) = text.strip_prefix('/') {
            let mut parts = command.split_whitespace();
            let name = parts.next().unwrap_or("").to_lowercase();
            let name = name.split('@').next().unwrap_or("").to_string();
            let rest = parts.collect::<Vec<_>>().join(" ");
            match name.as_str() {
                "sessions" => self.telegram_send(msg.chat_id, &self.telegram_session_list()),
                "help" => self.telegram_send(msg.chat_id, Self::TELEGRAM_HELP),
                "int" => self.telegram_interrupt(msg.chat_id, &rest),
                "clear" => self.telegram_clear(msg.chat_id, &rest),
                _ => self.telegram_send(
                    msg.chat_id,
                    &format!("unknown command /{name}; /help lists commands"),
                ),
            }
            return;
        }
        // A bracketed `[name]` head is the only explicit address.
        let (target, body) = match Self::parse_bracketed(&text) {
            Some((name, body)) => (Some(name), body),
            None => (None, text.clone()),
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
        }
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

    /// Input-free minutes before the operator counts as away.
    const AWAY_AFTER: std::time::Duration = std::time::Duration::from_secs(20 * 60);

    /// Away notice, injected into every live session on the flip.
    const AWAY_NOTICE: &'static str =
        "user is away at the moment, use message_user to message them if need be";
    /// Back notice, injected into every live session on return.
    const BACK_NOTICE: &'static str =
        "user is back, don't use message_user - communicate normally";

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

    /// Open the Telegram settings form prefilled from the live section.
    pub fn open_telegram_dialog(&mut self) {
        let cfg = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.clone())
            .unwrap_or_default();
        self.telegram_dialog = Some(crate::telegram_dialog::TelegramDialog::new(&cfg));
        self.dirty = true;
    }

    /// Apply a validated settings submit: optional token-file write
    /// (atomic `0600`), section save to `config.toml`, live-config
    /// swap for the poller thread, then close. Errors leave the dialog
    /// open for the caller to surface.
    pub fn apply_telegram_form(
        &mut self,
        loaded: &mut crate::config::LoadedConfig,
        home: &std::path::Path,
        form: crate::telegram_dialog::TelegramForm,
    ) -> Result<(), String> {
        if !form.token.is_empty() {
            let path =
                crate::telegram_dialog::expand_token_path(&form.config.token_file, home);
            crate::fs_atomic::write_private(std::path::Path::new(&path), form.token.as_bytes())
                .map_err(|e| format!("cannot write token file: {e}"))?;
        }
        loaded.config.telegram = form.config.clone();
        loaded
            .save_home(home)
            .map_err(|e| format!("cannot save config: {e}"))?;
        if let Ok(mut live) = self.telegram_config.lock() {
            *live = form.config;
        }
        self.telegram_dialog = None;
        self.dirty = true;
        Ok(())
    }

    /// Start a connection test for the open dialog. A dialog-provided
    /// token wins; otherwise the file reads now and reports inline.
    /// Network runs on a worker thread; the verdict returns as
    /// [`crate::event::AppEvent::TelegramTested`].
    pub fn start_telegram_test(&mut self, token: String, token_file: String) {
        let Some(dialog) = self.telegram_dialog.as_mut() else {
            return;
        };
        let token = if token.is_empty() {
            match crate::telegram::read_token(&token_file) {
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

    /// Snapshot the grid: one view per session in manager order.
    pub fn views(&self) -> Vec<crate::ui::PaneView> {
        let active = self.manager.active();
        self.manager
            .order()
            .iter()
            .map(|&id| {
                let rec = self.manager.get(id).expect("ordered session exists");
                let live = rec.state.is_live();
                if let Some((view_id, index)) = self.overlay_view {
                    if Some(id) == active && view_id == id {
                        let label = ["", "", "", "Events", "Tasks", "Visual", "Walkthrough"]
                            .get(index).copied().unwrap_or("View");
                        if label == "Walkthrough" {
                            return self.walkthrough_view(id);
                        }
                        #[cfg(feature = "visual")]
                        if label == "Visual" {
                            return self.visual_view(id, crate::visual::kitty_supported_env());
                        }
                        return crate::ui::PaneView {
                            title: format!("{} · {label}", rec.name),
                            lines: vec![vec![crate::ui::SpanView {
                                text: format!("{label} view unavailable in this build"),
                                style: crate::theme::style(crate::theme::Role::Muted),
                            }]],
                            live,
                            focused: true,
                            cursor: None,
                        };
                    }
                }
                let mut lines: Vec<Vec<crate::ui::SpanView>> = self
                    .manager
                    .styled_rows(id)
                    .iter()
                    .map(|row| row.iter().map(crate::ui::span_for).collect())
                    .collect();
                if lines.is_empty() {
                    // Blank scrollback means opposite things by state: a
                    // live pane simply hasn't printed yet, an exited one
                    // is gone.
                    let text = if live {
                        "(running — no output yet)"
                    } else {
                        "(exited)"
                    };
                    lines = vec![vec![crate::ui::SpanView {
                        text: text.to_string(),
                        style: ratatui::style::Style::default(),
                    }]];
                }
                crate::ui::PaneView {
                    title: rec.name.clone(),
                    lines,
                    live,
                    focused: Some(id) == active,
                    cursor: self.manager.cursor(id),
                }
            })
            .collect()
    }

    /// Record human typing into one session: injections debounce until it
    /// settles, and any staged Enter for that session is dropped — the
    /// human owns the prompt now, and our CR must never submit their
    /// half-typed draft.
    pub fn note_human_input(&mut self, id: crate::session::SessionId) {
        self.last_human_input
            .insert(id, std::time::Instant::now());
        // Dropping the staged submit protects the human's draft, but
        // the body is already in the prompt: it may mix or be
        // discarded, so its sender is told loudly (walkthrough
        // prompts have no sender and stay silent).
        if let Some((_, staged)) = self.pending_enter.remove(&id) {
            if let Some((conv, kind)) = staged {
                let typer = self
                    .manager
                    .get(id)
                    .map(|rec| rec.name.clone())
                    .unwrap_or_else(|| id.to_string());
                self.broker.clobber_notice(id, &typer, &conv, kind);
            }
        }
    }

    /// Live session names for dialog validation.
    pub fn live_names(&self) -> Vec<String> {
        self.manager
            .order()
            .iter()
            .filter_map(|&id| self.manager.get(id))
            .filter(|rec| rec.state.is_live())
            .map(|rec| rec.name.clone())
            .collect()
    }

    /// First free `shell-N` name for the dialog prefill.
    /// Prefill name for the create dialog. The dialog defaults to the
    /// claude tool, so the prefix matches; the user can rename freely.
    pub fn suggested_session_name(&self) -> String {
        let taken = self.live_names();
        let mut n = self.manager.len() + 1;
        loop {
            let candidate = format!("claude-{n}");
            if !taken.iter().any(|t| t == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Open the create-session dialog, prefilled from current state.
    pub fn open_create_dialog(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let name = self.suggested_session_name();
        let groups = self.broker.group_names();
        self.create_dialog = Some(crate::create::CreateDialog::new(&name, &cwd, &groups, self.pill_tabs));
        self.dirty = true;
    }

    /// Open the group management dialog.
    pub fn open_group_dialog(&mut self) {
        self.group_dialog = Some(crate::groups::GroupDialog::new());
        self.dirty = true;
    }

    pub fn open_quit_confirm(&mut self) {
        self.quit_confirm = Some(crate::quit::QuitConfirm::new(self.pill_tabs));
        self.dirty = true;
    }

    /// Fresh snapshot for the group dialog: groups with live member
    /// counts, sessions in bar order, and the selected group's current
    /// members for the checkbox pre-check.
    pub fn group_ctx(&self) -> crate::groups::GroupCtx {
        let groups = self
            .broker
            .group_names()
            .into_iter()
            .map(|name| crate::groups::GroupRow {
                members: self.broker.group_members(&name).len(),
                member_names: self.broker.group_members(&name).into_iter()
                    .filter_map(|id| self.manager.get(id).map(|rec| rec.name.clone()))
                    .collect(),
                color: self.broker.group_color(&name).unwrap_or(0),
                name,
            })
            .collect();
        let sessions = self
            .manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| crate::groups::SessionRow {
                    id,
                    name: rec.name.clone(),
                })
            })
            .collect();
        let selected = self
            .group_dialog
            .as_ref()
            .map(|d| d.selected_name())
            .unwrap_or_default();
        let members = self.broker.group_members(selected);
        crate::groups::GroupCtx {
            groups,
            sessions,
            members,
        }
    }

    /// Apply one group-dialog mutation to the broker. The dialog validates
    /// names against a fresh snapshot, so these are infallible in practice;
    /// failures (exit races) stay silent and keep the dialog open.
    /// Unknown session IDs (exited mid-dialog) are skipped.
    pub fn apply_group(&mut self, outcome: crate::groups::GroupOutcome) {
        use crate::groups::GroupOutcome as Out;
        match outcome {
            Out::Create(name) => {
                let _ = self.broker.create_group(&name);
            }
            Out::Rename { from, to } => {
                let _ = self.broker.rename_group(&from, &to);
            }
            Out::Delete(name) => {
                self.broker.remove_group(&name);
            }
            Out::SetMembers { group, members } => {
                for id in self.manager.order().to_vec() {
                    if members.contains(&id) {
                        let _ = self.broker.join(&self.manager, id, &group);
                    } else {
                        self.broker.leave(id, &group);
                    }
                }
            }
            Out::Pending | Out::Closed => {}
        }
        self.dirty = true;
    }

    /// Live agent sessions as restorable records, in bar order. Shells
    /// have no resume form and exited sessions are gone, so both are
    /// left out; groups ride along for exact rejoins.
    pub fn snapshot_sessions(&self) -> Vec<crate::checkpoint::SavedSession> {
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                let rec = self.manager.get(id)?;
                if !rec.state.is_live() {
                    return None;
                }
                if crate::harness::Harness::from_name(&rec.cli_tool).is_none() {
                    return None;
                }
                Some(crate::checkpoint::SavedSession {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    groups: self.broker.groups_of(id),
                    harness_session_id: rec.harness_session_id.clone(),
                })
            })
            .collect()
    }

    /// Recreate every restorable session in an entry: agents relaunch
    /// with resume argv (or the cwd-scoped fallback), rejoin their
    /// groups, and fit the main pane. Unknown tools and vanished working
    /// directories skip with a reason instead of failing the batch.
    pub fn restore_entry(&mut self, entry: &crate::checkpoint::SavedEntry) -> RestoreReport {
        let mut report = RestoreReport {
            spawned: 0,
            skipped: Vec::new(),
        };
        for saved in &entry.sessions {
            let Some(harness) = crate::harness::Harness::from_name(&saved.cli_tool) else {
                report.skipped.push(format!("{}: unknown tool {}", saved.name, saved.cli_tool));
                continue;
            };
            let cwd = std::path::PathBuf::from(&saved.cwd);
            if !cwd.is_dir() {
                report.skipped.push(format!("{}: missing directory {}", saved.name, saved.cwd));
                continue;
            }
            let spec = harness.spec();
            let argv = harness.resume_argv(&spec.resolve_binary(), saved.harness_session_id.as_deref());
            let mut cmd = String::from("exec ");
            cmd.push_str(&crate::create::shell_join(&argv));
            let run = crate::ids::RunId::generate();
            match self.manager.spawn_agent(&saved.name, &cwd, &cmd, run, &saved.cli_tool) {
                Ok(id) => {
                    for group in &saved.groups {
                        let _ = self.broker.join(&self.manager, id, group);
                    }
                    report.spawned += 1;
                }
                Err(e) => report.skipped.push(format!("{}: spawn failed ({e})", saved.name)),
            }
        }
        self.dirty = true;
        report
    }

    /// Spawn exactly what a submitted dialog describes: shells run the
    /// login shell, agents run their registry argv. Returns the new id.
    pub fn create_session(
        &mut self,
        spec: &crate::create::SessionSpec,
    ) -> std::io::Result<crate::session::SessionId> {
        use crate::create::SessionKind;
        let (cmd, cli_tool) = match spec.kind {
            SessionKind::Shell => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                (format!("exec {shell} -i"), "shell".to_string())
            }
            SessionKind::Agent(h) => {
                let hs = h.spec();
                let model = if spec.model.is_empty() {
                    None
                } else {
                    Some(spec.model.as_str())
                };
                let argv = hs.launch_argv(&hs.resolve_binary(), model);
                let mut cmd = String::from("exec ");
                cmd.push_str(&crate::create::shell_join(&argv));
                (cmd, h.as_str().to_string())
            }
        };
        // Agent CLIs get the dual-tab layout (agent on tab 0, a lazy
        // human terminal on tab 1); plain shells stay single-tab.
        let run = crate::ids::RunId::generate();
        let id = match spec.kind {
            SessionKind::Agent(_) => {
                self.manager
                    .spawn_agent(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
            SessionKind::Shell => {
                self.manager
                    .spawn(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
        }?;
        // The dialog's comm-group choice joins at birth (a group deleted
        // mid-dialog is recreated by the join — the user explicitly picked
        // it a moment ago).
        if let Some(group) = spec.group.as_deref() {
            let _ = self.broker.join(&self.manager, id, group);
        }
        // Creating focuses the new session: the user just asked for it,
        // and the caller fits the active pane to the dialog's area.
        self.manager.switch(id);
        Ok(id)
    }

    /// Deliver due injections into idle, non-recently-typed panes. Targets
    /// whose hook activity is Thinking/ToolUse/Waiting keep waiting, as do
    /// panes the human just typed into.
    /// Delivery gate for bodies and staged Enters alike, per target:
    /// quiet human hands plus quiet hooks for that session only. Either
    /// recent activity holds that target, never its neighbors.
    fn injection_settled_for(
        &self,
        id: crate::session::SessionId,
        now: std::time::Instant,
    ) -> bool {
        let hands_off = self.last_human_input.get(&id).is_none_or(|t| {
            now.duration_since(*t) >= crate::comms::INJECT_DEBOUNCE
        });
        let hooks_quiet = self.last_hook_activity.get(&id).is_none_or(|t| {
            now.duration_since(*t) >= crate::comms::INJECT_HOOK_DEBOUNCE
        });
        hands_off && hooks_quiet
    }

    /// Whether the courtesy/timer sweep is due: always on a fresh
    /// window (`None`), otherwise once the interval lapses. Pure so
    /// the throttle itself is unit-testable; the sweep stays live.
    fn tick_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
        last.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::BROKER_TICK_INTERVAL
        })
    }

    pub fn settle_comms(&mut self) {
        use crate::session::Activity;
        let now = std::time::Instant::now();
        // Operator texts route first so fresh injections can deliver on
        // this same tick when their target is idle.
        self.drain_telegram();
        // Second-scale sweep on a 16ms loop: skip inside the window.
        // Everything below (debounce gates, queue delivery, staged
        // Enters) stays per-tick, so injections never wait on this.
        if Self::tick_due(self.last_broker_tick, now) {
            self.broker.tick(now);
            self.last_broker_tick = Some(now);
        }
        let order = self.manager.order().to_vec();
        for id in order {
            if self.broker.queued(id) == 0 {
                continue;
            }
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !idle {
                continue;
            }
            if !self.injection_settled_for(id, now) {
                continue;
            }
            // One body per Enter: a staged CR means the previous body
            // is still awaiting submission, so later bodies stay queued
            // instead of merging into the same burst. Each body gets
            // its own staged Enter below.
            if self.pending_enter.contains_key(&id) {
                continue;
            }
            // Peek before pop: the message leaves the queue only after
            // its bytes reach the pane, so a failed write retries next
            // settle with pressure untouched. Single owner, so the head
            // cannot change between peek and pop.
            let Some(head) = self.broker.peek_due(id) else {
                continue;
            };
            // Panes that opted into paste mode (DECSET 2004) take the
            // whole payload as one bracketed-paste transaction; the rest
            // take it raw, exactly as before.
            let bracketed = self.manager.bracketed_paste(id);
            if self.manager.inject_write(id, &head.render_framed(bracketed)).is_ok() {
                let taken = self.broker.take_due(id, 1);
                debug_assert!(taken.first().map(|t| &t.conv) == Some(&head.conv));
                // Arm the staged Enter: the CR goes out on a later tick,
                // never in the same burst as the text. Remember what
                // went out, so human input that kills the submit can
                // tell the sender.
                self.pending_enter.insert(
                    id,
                    (now, Some((head.conv.clone(), head.kind))),
                );
                self.dirty = true;
            }
        }
        self.settle_enters(now);
    }

    /// Send staged Enters whose beat has elapsed. Same gates as bodies —
    /// settled debounce plus an idle target — so a CR never lands mid-turn
    /// or into a human draft (human input clears the entry outright).
    /// Sessions that exited meanwhile are pruned.
    fn settle_enters(&mut self, now: std::time::Instant) {
        use crate::session::Activity;
        let due: Vec<crate::session::SessionId> = self
            .pending_enter
            .iter()
            .filter(|(_, (at, _))| now.duration_since(*at) >= crate::comms::INJECT_ENTER_DELAY)
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !idle || !self.injection_settled_for(id, now) {
                continue;
            }
            // A CR that never reaches the pane stays staged for retry:
            // removing it would strand an unsubmitted body. Exited
            // sessions are pruned below, so a dead pane stops here.
            if self.manager.inject_write(id, &[crate::comms::INJECT_ENTER_CR]).is_ok() {
                self.pending_enter.remove(&id);
                self.dirty = true;
            }
        }
        // Records outlive their sessions (a natural exit marks the
        // record, it does not remove it), so mere presence prunes
        // nothing: only live sessions keep a staged Enter.
        self.pending_enter.retain(|id, _| {
            self.manager.get(*id).is_some_and(|rec| rec.state.is_live())
        });
    }

    /// Cycle the active session; wraps around. No-op when empty.
    pub fn step_session(&mut self, dir: i32) {
        let order = self.manager.order().to_vec();
        if order.is_empty() {
            return;
        }
        let cur = self
            .manager
            .active()
            .and_then(|a| order.iter().position(|&id| id == a))
            .unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(order.len() as i32) as usize;
        self.manager.switch(order[next]);
        self.overlay_view = None;
    }

    /// Focus session by order index (`Ctrl-b 1` is index 0). Returns false
    /// when out of range, leaving focus untouched.
    pub fn select_session(&mut self, index: usize) -> bool {
        match self.manager.order().to_vec().get(index) {
            Some(&id) => {
                self.manager.switch(id);
                self.overlay_view = None;
                // Picking a number always returns to the focused view.
                self.grid_mode = false;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Terminate a session (`Ctrl-b x`): leave every comm group, fail its
    /// open conversations and timers, then drop the record so it leaves
    /// the UI. Focus falls to the first remaining session. False when the
    /// id is unknown.
    pub fn terminate_session(&mut self, id: crate::session::SessionId) -> bool {
        self.broker.leave_all(id);
        self.broker.target_exited(&self.manager, id);
        if !self.manager.remove(id) {
            return false;
        }
        // Same cleanup as a natural exit: debounce entries die with
        // the session, or the maps grow with every termination.
        self.last_human_input.remove(&id);
        self.last_hook_activity.remove(&id);
        self.overlay_view = None;
        self.dirty = true;
        true
    }

    /// Session-bar tabs in order with live/focus flags.
    pub fn tabs(&self) -> Vec<crate::ui::SessionTab> {
        let active = self.manager.active();
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| {
                    let group = self.broker.primary_group(id).map(str::to_string);
                    let group_color = group
                        .as_deref()
                        .and_then(|g| self.broker.group_color(g));
                    crate::ui::SessionTab {
                        title: rec.name.clone(),
                        live: rec.state.is_live(),
                        focused: Some(id) == active,
                        group,
                        group_color,
                    }
                })
            })
            .collect()
    }

    /// Sidebar content: the focused session's detail plus pending hooks
    /// and the live permission mode.
    pub fn sidebar_info(&self) -> crate::ui::SidebarInfo {
        let now = std::time::Instant::now();
        let session = self.manager.active().and_then(|id| {
            self.manager.get(id).map(|rec| {
                let state = if rec.state.is_live() {
                    match rec.activity {
                        crate::session::Activity::Idle => "running".to_string(),
                        activity => format!("running · {activity:?}"),
                    }
                } else {
                    match rec.exit_code {
                        Some(code) => format!("exited({code})"),
                        None => "exited".to_string(),
                    }
                };
                crate::ui::SessionDetail {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    state,
                    status: rec.status.as_ref().map(|s| s.display()),
                    // Armed timers show for the focused session only;
                    // other sessions keep their own countdowns hidden.
                    timers: self
                        .broker
                        .timers_for(id)
                        .into_iter()
                        .map(|(timer_id, due)| crate::ui::TimerView {
                            id: timer_id,
                            remaining: crate::ui::format_countdown(
                                due.saturating_duration_since(now),
                            ),
                        })
                        .collect(),
                    uptime_secs: rec.spawned_at.elapsed().as_secs(),
                    tool_calls: rec.tool_calls,
                    approvals: rec.approvals,
                    denials: rec.denials,
                }
            })
        });
        let telegram_on = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.enabled)
            .unwrap_or(false);
        // A failing last poll shows as retrying: without it a stalled
        // poller (e.g. long backoff after early failures) looks exactly
        // like a quiet healthy one.
        let telegram_state = if telegram_on && self.telegram_last_poll_failed {
            "on · retrying"
        } else if telegram_on {
            "on"
        } else {
            "off"
        };
        let telegram_badge = self.last_telegram_badged.and_then(|id| {
            let name = self.manager.get(id)?.name.clone();
            let text = self.message_user_badges.get(&id)?.clone();
            Some((name, text))
        });
        crate::ui::SidebarInfo {
            session,
            pending: self.pending_hooks.len(),
            mode: self.permission_mode.as_str(),
            telegram: telegram_state,
            telegram_badge,
        }
    }

    /// Run deterministic policy over queued hook requests. Every verdict —
    /// allow, deny, or ask-for-the-harness — replies immediately and is
    /// audited; nothing waits on a human. Yolo auto-approves, Safe-Only
    /// blocks still deny, and every other Ask goes back to the harness so
    /// its native permission flow takes over. Audit failures never block.
    pub fn settle_hooks(
        &mut self,
        policy: &mut crate::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        while let Some(req) = self.pending_hooks.pop_front() {
            let (decision, reason) = policy.decide(&req.hook, &req.body);
            let line = crate::policy::decision_line(&req.hook, decision, reason);
            let _ = req.reply.send(line);
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
            // The verdict races bodies only in its own pane: stamp the
            // attributed session, never the whole app.
            if let Some(id) = attributed {
                self.last_hook_activity
                    .insert(id, std::time::Instant::now());
            }
            // Attribute the verdict to the sender's sidebar counters.
            if let Some(id) = attributed {
                self.manager.note_verdict(id, decision);
            }
            self.audit_hook(audit_path, &req.hook, &req.body, decision, reason);
            // A verdict changes sidebar counters and audit state, so only
            // a fired hook repaints; an empty queue leaves the frame clean
            // and the idle loop skips the 60Hz full redraw.
            self.dirty = true;
        }
    }

    fn audit_hook(
        &self,
        audit_path: &std::path::Path,
        hook: &str,
        body: &str,
        decision: crate::policy::Decision,
        reason: &str,
    ) {
        let tool = crate::policy::tool_name(body);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("{decision:?}").to_lowercase();
        let _ = crate::audit::append(audit_path, hook, &tool, &name, reason, now);
    }

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
                // bounded over session churn.
                self.broker.target_exited(&self.manager, id);
                self.last_human_input.remove(&id);
                self.last_hook_activity.remove(&id);
                self.dirty = true;
            }
            AppEvent::CommsRequest(req) => {
                // The commit claim runs atomically for a reason: a
                // plain flag could read false, stall on the scheduler
                // past the deadline, then mutate after the caller gave
                // up. Losing the claim means the handler already timed
                // out, so the send is skipped and the retry guidance
                // goes on the reply instead of a stale ok.
                if !crate::listener::claim_execute(&req.claim) {
                    let _ = req.reply.send(
                        "{\"ok\":false,\"error\":\"caller timed out; retry with the same idempotency_key\"}\n"
                            .to_string(),
                    );
                    self.dirty = true;
                    return;
                }
                // Walkthrough, visual, and session tools answer here
                // (they own overlay and manager state the broker
                // cannot see); everything else goes to the broker at
                // once. The verdict goes straight back to `mcp-serve`.
                // Failures stay single-line JSON, escaped.
                let now = std::time::Instant::now();
                let verdict = match self.walkthrough_tool(&req.run_id, &req.tool, &req.args) {
                    Some(verdict) => verdict,
                    None => match self.visual_tool(&req.run_id, &req.tool, &req.args) {
                        Some(verdict) => verdict,
                        None => match self.session_tool(&req.run_id, &req.tool, &req.args) {
                            Some(verdict) => verdict,
                            None => self.broker.call(&self.manager, &req.run_id, &req.tool, &req.args, now),
                        },
                    },
                };
                let line = match verdict {
                    Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                    Err(e) => format!(
                        "{{\"ok\":false,\"error\":{}}}\n",
                        crate::mcp::escape_json(&e)
                    ),
                };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::BotRequest(req) => {
                // Same commit claim as comms requests: no bot mutation
                // starts after its caller stopped waiting.
                if !crate::listener::claim_execute(&req.claim) {
                    let mut line = String::from("{\"ok\":false,\"error\":");
                    line.push_str(
                        &crate::bot::BotError::new(
                            crate::bot::ErrorCode::Timeout,
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
                // Inbound texts queue for routing; failures only flip
                // the flag Phase 5 surfaces. The inbox is bounded: past
                // the cap newcomers drop and count, never grow the owner.
                self.telegram_last_poll_failed = report.failed;
                let mut queued = false;
                for msg in report.messages {
                    if self.telegram_inbox.len() >= crate::telegram::INBOX_CAP {
                        self.telegram_dropped += 1;
                    } else {
                        self.telegram_inbox.push_back(msg);
                        queued = true;
                    }
                }
                if queued || report.failed {
                    self.dirty = true;
                }
            }
            AppEvent::Resize(rows, cols) => {
                self.term_size = (rows, cols);
                if cols < 100 { self.overlay_view = None; }
                self.dirty = true;
            }
            AppEvent::HookRequest(req) => {
                // Unattributed SessionStarts bind by working directory when
                // unambiguous (muse scrubs hook-child environments, so its
                // relays send no run ID). The bind runs first so the record
                // below attributes like any other SessionStart.
                if req.hook == "SessionStart" && self.manager.lookup_run(&req.run_id).is_none()
                {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if let Some(cwd) = crate::session::cwd_from_hook_body(&req.body) {
                            self.manager.bind_harness_session(&harness, &cwd);
                        }
                    }
                }
                // Attribute hook activity before queuing: the sender's run
                // ID resolves to its session; records without one resolve
                // by the harness session ID instead. Unknown runs stay
                // untouched. Tool-gated hooks also count one sidebar call.
                let fallback_id = if self.manager.lookup_run(&req.run_id).is_none() {
                    crate::session::session_id_from_hook_body(&req.body)
                        .and_then(|h| self.manager.lookup_harness_session(&h))
                } else {
                    None
                };
                let attributed = self.manager.lookup_run(&req.run_id).or(fallback_id);
                if let Some(activity) = crate::session::activity_for_hook(&req.hook) {
                    if let Some(id) = attributed {
                        self.manager.set_activity(id, activity);
                        if activity == crate::session::Activity::ToolUse {
                            self.manager.note_tool_call(id);
                        }
                    }
                }
                // SessionStart carries the harness-side conversation ID the
                // restore path resumes with. Bodies without one (or from
                // unknown runs) leave any earlier value alone.
                if req.hook == "SessionStart" {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if let Some(id) = attributed {
                            self.manager.set_harness_session(id, harness);
                        }
                    }
                }
                if self.pending_hooks.len() < crate::listener::MAX_PENDING_HOOKS {
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
    use crate::event::AppEvent;
    use crate::ids::RunId;
    use crate::session::SessionId;

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

    fn hook_request(hook: &str, run_id: &str, body: &str) -> AppEvent {
        let (reply_tx, _) = std::sync::mpsc::channel();
        AppEvent::HookRequest(crate::listener::HookRequest {
            hook: hook.to_string(),
            body: body.to_string(),
            run_id: run_id.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        })
    }

    #[test]
    fn session_start_captures_harness_id() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run.clone(), "claude")
            .unwrap();
        assert_eq!(s.manager.get(id).unwrap().harness_session_id, None);
        // Claude envelope nests the harness JSON under body.
        s.apply(hook_request(
            "SessionStart",
            run.as_str(),
            r#"{"v":1,"hook":"SessionStart","run_id":"r","body":{"session_id":"harness-9","cwd":"/tmp"}}"#,
        ));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Bodies without an ID (or other hooks) leave the value alone.
        s.apply(hook_request("SessionStart", run.as_str(), "{}"));
        s.apply(hook_request("PreToolUse", run.as_str(), "{}"));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Unknown runs touch nothing.
        s.apply(hook_request(
            "SessionStart",
            "nope",
            r#"{"v":1,"body":{"session_id":"other"}}"#,
        ));
        assert!(s.manager.remove(id));
    }

    #[test]
    fn unattributed_muse_hooks_bootstrap_and_attribute() {
        use crate::session::Activity;
        let mut s = AppState::new();
        let cwd = std::env::temp_dir();
        let cwd_json = crate::mcp::escape_json(&cwd.to_string_lossy());
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
    fn snapshot_keeps_live_agents_with_groups() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "claude")
            .unwrap();
        s.manager.set_harness_session(a, "h-1".to_string());
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, a, "team").unwrap();
        // Shells and exited sessions never snapshot.
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("sh", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        let snap = s.snapshot_sessions();
        assert_eq!(snap.len(), 1, "agent only: {snap:?}");
        assert_eq!(snap[0].name, "a");
        assert_eq!(snap[0].cli_tool, "claude");
        assert_eq!(snap[0].groups, vec!["peers".to_string(), "team".to_string()]);
        assert_eq!(snap[0].harness_session_id.as_deref(), Some("h-1"));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn restore_respawns_with_resume_and_rejoins() {
        // Hermetic stand-in binary; the record outlives its instant exit.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let entry = crate::checkpoint::SavedEntry {
            label: "a, gone, weird".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![
                crate::checkpoint::SavedSession {
                    name: "a".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec!["peers".to_string()],
                    harness_session_id: Some("uuid-a".to_string()),
                },
                crate::checkpoint::SavedSession {
                    name: "gone".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: "/no/such/dir-anywhere".to_string(),
                    groups: vec![],
                    harness_session_id: None,
                },
                crate::checkpoint::SavedSession {
                    name: "weird".to_string(),
                    cli_tool: "shell".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec![],
                    harness_session_id: None,
                },
            ],
        };
        let report = s.restore_entry(&entry);
        assert_eq!(report.spawned, 1, "report: {:?}", report.skipped);
        assert_eq!(report.skipped.len(), 2, "missing dir + shell: {:?}", report.skipped);
        let id = s.manager.order().to_vec().pop().unwrap();
        assert_eq!(s.manager.get(id).unwrap().name, "a");
        assert!(s.broker.is_member(id, "peers"), "rejoined");
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
        assert!(s.manager.remove(id));
    }

    #[test]
    fn hook_requests_queue_bounded_and_dirty() {
        let mut s = AppState::new();
        s.dirty = false;
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert!(s.dirty);
        assert_eq!(s.pending_hooks.len(), 1);
        for _ in 0..crate::listener::MAX_PENDING_HOOKS + 10 {
            let (tx, _rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: "Stop".to_string(),
                body: "{}".to_string(),
                run_id: String::new(),
                sync: false,
                reply: tx,
                timed_out: Default::default(),
            }));
        }
        assert_eq!(s.pending_hooks.len(), crate::listener::MAX_PENDING_HOOKS);
    }

    #[test]
    fn pre_tool_use_replies_use_hook_specific_output() {
        // Newer Claude (and muse) reject the legacy top-level `decision`
        // field on PreToolUse replies: "unsupported legacy PreToolUse
        // output; use hookSpecificOutput.permissionDecision".
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-hookshape-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo = crate::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-settle-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo = crate::policy::Policy::new(
            PermissionMode::Yolo,
            &[],
            &[],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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

        let mut off = crate::policy::Policy::new(
            PermissionMode::Off,
            &[],
            &[],
        )
        .unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-settle-idle-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy = crate::policy::Policy::new(PermissionMode::Yolo, &[], &[])
            .unwrap();
        let mut s = AppState::new();
        // Empty queue: the 60Hz loop must not repaint from this.
        s.dirty = false;
        s.settle_hooks(&mut policy, &audit);
        assert!(!s.dirty, "idle settle must leave the frame clean");
        // A real verdict still repaints (counters/audit change).
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
    fn broker_tick_throttles_but_delivery_stays_per_tick() {
        // The courtesy/timer sweep is second-scale work on a 16ms loop:
        // rapid settles must skip it, while queue delivery below stays
        // per-tick. A delay-0 timer scheduled inside the window proves
        // the skip (still pending), and one after a lapsed window
        // proves the sweep resumes.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        assert!(s.manager.set_activity(a, crate::session::Activity::Idle));
        let schedule = |s: &mut AppState, prompt: &str| {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "schedule_prompt".to_string(),
                args: format!("{{\"prompt\":{prompt:?},\"delay_seconds\":0}}"),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
            }));
            let line = reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("schedule answers at once");
            assert!(line.contains("\"ok\":true"), "schedule accepted: {line}");
        };
        // First settle sweeps: the due timer fires.
        schedule(&mut s, "tick-one");
        s.settle_comms();
        assert!(s.broker.timers_for(a).is_empty(), "first settle sweeps");
        // A timer scheduled inside the throttle window waits out the
        // next sweep instead of firing on the very next settle.
        schedule(&mut s, "tick-two");
        s.settle_comms();
        assert_eq!(
            s.broker.timers_for(a).len(),
            1,
            "rapid settle skips the sweep"
        );
        // A lapsed window sweeps again: delivery never waited, only
        // the sweep did.
        s.last_broker_tick = None;
        s.settle_comms();
        assert!(s.broker.timers_for(a).is_empty(), "lapsed window sweeps");
        assert!(s.manager.remove(a));
    }

    #[test]
    fn broker_tick_window_opens_once_per_interval() {
        use std::time::{Duration, Instant};
        let window = crate::comms::BROKER_TICK_INTERVAL;
        let start = Instant::now();
        // Fresh windows (boot, tests) always sweep at once.
        assert!(AppState::tick_due(None, start));
        // Inside the window the sweep waits...
        assert!(!AppState::tick_due(Some(start), start));
        assert!(!AppState::tick_due(Some(start), start + window - Duration::from_millis(1)));
        // ...then opens again once it lapses.
        assert!(AppState::tick_due(Some(start), start + window));
    }

    #[test]
    fn safe_only_blocks_still_deny_without_a_modal() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-block-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy = crate::policy::Policy::new(
            PermissionMode::SafeOnly,
            &[],
            &["doom".to_string()],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"doom"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut policy, &audit);
        assert!(s.pending_hooks.is_empty());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""permissionDecision":"deny""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1, "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn comms_ask_flows_through_apply_and_replies() {
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
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":true"#), "line: {line:?}");
        assert!(line.contains(r#""conversation":""#), "line: {line:?}");
        assert!(line.ends_with('\n'));
        assert_eq!(s.broker.queued(b), 1);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
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
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_TIMED_OUT)),
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
    fn comms_rejections_are_single_line_json() {
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: "f".repeat(32),
            tool: "ask_session".to_string(),
            args: "{}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(!line.trim_end().contains('\n'), "one line: {line:?}");
    }

    #[test]
    fn session_exit_fails_open_conversations() {
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
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"q?"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        assert!(s.manager.kill(b));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(b).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source learns the failure");
        assert!(matches!(
            due[0].kind,
            crate::comms::InjectKind::Failed
        ));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_to_idle_panes_only() {
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
        // Busy target: the injection waits.
        assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target waits");
        // Idle target: delivered into the pane.
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(b).is_some_and(|t| t.contains("hello-b")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "injection never reached the pane"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn reply_round_trip_needs_stop_to_reach_first_session() {
        // Live shape: both agents have fired hooks, so neither is Idle.
        // The ask goes out, the target answers, and the reply waits until
        // the first session's turn ends (Stop parks Stopped). Before the
        // Stop edge was installed, that wait never ended.
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
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        let comms = |s: &mut AppState, run_id: String, tool: &str, args: &str| -> String {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id,
                tool: tool.to_string(),
                args: args.to_string(),
                reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("broker answers at once")
        };
        // Both sessions mid-turn, like live agents that called tools.
        hook(&mut s, "PreToolUse", run_a.to_string());
        hook(&mut s, "PreToolUse", run_b.to_string());
        // A's ask waits while B is busy, then lands when B's turn ends.
        let ask_line = comms(&mut s, run_a.to_string(), "ask_session", "{\"target\":\"b\",\"message\":\"ready?\"}");
        assert!(ask_line.contains("\"ok\":true"), "ask accepted: {ask_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target holds the ask");
        hook(&mut s, "Stop", run_b.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity.insert(
            b,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "turn end delivers the ask");
        // B answers; A is still mid-turn so the reply waits.
        let conv = crate::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("ask returns a conversation");
        let resp_line = comms(
            &mut s,
            run_b.to_string(),
            "send_response",
            &format!("{{\"conversation_id\":\"{conv}\",\"message\":\"got-it\"}}"),
        );
        assert!(resp_line.contains("\"ok\":true"), "reply accepted: {resp_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "reply waits while A works");
        // A's turn ends: the reply lands in A's pane.
        hook(&mut s, "Stop", run_a.to_string());
        s.last_hook_activity.insert(
            a,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers the reply");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(a).is_some_and(|t| t.contains("got-it")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reply never reached the first session"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn prompt_submit_holds_injections_until_stop() {
        // A freshly prompted session looks settled but its agent is
        // generating: UserPromptSubmit must hold injections (where Enter
        // would be eaten and the text left as a draft) until Stop.
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
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        // A was parked (Stopped) but just got prompted: generating.
        hook(&mut s, "Stop", run_a.to_string());
        hook(&mut s, "UserPromptSubmit", run_a.to_string());
        assert_eq!(
            s.manager.get(a).unwrap().activity,
            crate::session::Activity::Thinking
        );
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_b.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"a\",\"text\":\"hold\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "generating target waits");
        hook(&mut s, "Stop", run_a.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity.insert(
            a,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_stages_enter_beat_after_body() {
        // Byte-level human parity through a raw-mode slave (no CR/LF
        // translation anywhere, no echo): the body arrives whole with no
        // Enter bundled in, and the CR follows as its own input event
        // after the beat — type text, press Enter, never one burst.
        use crate::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Wait out the spawn: anything the starting shell echoes (including
        // canonical-mode translations) predates raw mode and must not
        // pollute the byte assertions below.
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(8).any(|w| w == b"STTYFAIL") {
                panic!("stty raw failed in the probe session");
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"message\":\"ping-body\"}".to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "raw session is idle: body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Drain raw output until cat echoes the full body back.
        let body = b"[forge tell_session from a]: ping-body";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(body.len()).any(|w| w == body) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "body never echoed, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!raw.contains(&b'\r'), "no Enter bundled with the body");
        // After the beat the CR goes out as its own event, trailing the body.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter sent");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.contains(&b'\r') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "staged enter never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pos = raw
            .windows(body.len())
            .position(|w| w == body)
            .expect("body present");
        assert!(
            raw[pos + body.len()..].contains(&b'\r'),
            "enter trails the body: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_one_body_per_enter() {
        // Two queued tells must not merge into one submission: the first
        // settle writes only the head body and stages its Enter; the
        // second body waits for its own Enter after the first CR lands.
        use crate::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let tell = |s: &mut AppState, text: &str| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: format!("{{\"target\":\"b\",\"text\":{text:?}}}"),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
            }));
        };
        tell(&mut s, "first-one");
        tell(&mut s, "second-two");
        // Only the head body goes out; the second waits in queue.
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "second body waits for its own enter");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Past the beat the first CR lands while the second body is
        // still queued — never in the same burst.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "first enter sent");
        assert_eq!(s.broker.queued(b), 1, "second body still queued");
        // The next settle writes the second body and stages its Enter.
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "second body delivered");
        assert!(s.pending_enter.contains_key(&b), "second enter staged");
        // Byte order on the wire: body1, CR, body2, CR — the second
        // body never appears before the first Enter.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(b"second-two".len()).any(|w| w == b"second-two") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second body never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let first_cr = raw.iter().position(|&byte| byte == b'\r').expect("CR sent");
        let second_at = raw
            .windows(b"second-two".len())
            .position(|w| w == b"second-two")
            .expect("second body present");
        assert!(
            second_at > first_cr,
            "second body follows the first enter: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn failed_body_write_keeps_message_queued() {
        // Popping is not delivery: if the bytes never reach the pane,
        // the message must stay queued for the next settle instead of
        // vanishing with its pressure already moved.
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
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","text":"held"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        // Kill the writer while the record stays idle (exit undrained):
        // every write now fails deterministically.
        s.manager.active_pane_mut(b).expect("live pane").close();
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "failed write retries, never drops");
        assert!(
            !s.pending_enter.contains_key(&b),
            "no enter staged without a body"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn failed_enter_write_stays_staged() {
        // A CR that never reaches the pane must be retried, not
        // forgotten: removing it strands an unsubmitted body.
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
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","text":"held"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        s.settle_comms();
        assert!(s.pending_enter.contains_key(&b), "enter staged");
        // Kill the writer with the CR still staged, then age past the beat.
        s.manager.active_pane_mut(b).expect("live pane").close();
        s.pending_enter.insert(
            b,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(
            s.pending_enter.contains_key(&b),
            "failed enter stays staged for retry"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_during_human_typing() {
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
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        // Typing debounces the typed-in pane only (never its neighbors).
        s.note_human_input(b);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "fresh typing debounces delivery");
        // Human input also drops a staged Enter for that session: our CR
        // must never submit the human's draft.
        s.pending_enter.insert(b, (std::time::Instant::now(), None));
        s.note_human_input(b);
        assert!(!s.pending_enter.contains_key(&b), "human owns the prompt");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn typed_over_tell_notifies_the_source() {
        // The tell body reached B's prompt with its Enter staged;
        // B types first, so the staged submit dies to protect the
        // draft. The body may have mixed or been discarded, so A is
        // told loudly instead of assuming it landed cleanly.
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
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::listener::CLAIM_PENDING,
            )),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged");
        s.note_human_input(b);
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source hears the clobber");
        assert!(matches!(due[0].kind, crate::comms::InjectKind::Failed));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn typed_over_response_notifies_the_answerer() {
        // B's answer sits in A's prompt awaiting its staged Enter;
        // A types first. The answerer (not the asker) is told.
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
        let mut converse = |run: &str, tool: &str, args: String| {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id: run.to_string(),
                tool: tool.to_string(),
                args,
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                    crate::listener::CLAIM_PENDING,
                )),
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("verdict arrives")
        };
        let ask_line = converse(
            &run_a.to_string(),
            "ask_session",
            r#"{"target":"b","message":"q?"}"#.to_string(),
        );
        assert!(ask_line.contains(r#""ok":true"#), "ask accepted: {ask_line}");
        let conv = crate::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("conversation id");
        let resp_line = converse(
            &run_b.to_string(),
            "send_response",
            format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        );
        assert!(resp_line.contains(r#""ok":true"#), "answer accepted: {resp_line}");
        s.settle_comms();
        assert!(s.pending_enter.contains_key(&a), "enter staged");
        s.note_human_input(a);
        let due = s.broker.take_due(b, 10);
        assert_eq!(due.len(), 1, "answerer hears the clobber");
        assert!(matches!(due[0].kind, crate::comms::InjectKind::Failed));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn terminate_session_prunes_debounce_entries() {
        // Manual termination must clean the per-target debounce maps
        // like a natural exit does, or they grow with every session.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        s.note_human_input(a);
        s.last_hook_activity.insert(a, std::time::Instant::now());
        assert!(s.terminate_session(a));
        assert!(!s.last_human_input.contains_key(&a), "typing entry pruned");
        assert!(!s.last_hook_activity.contains_key(&a), "hook entry pruned");
    }

    #[test]
    fn verdict_stamp_uses_harness_fallback_attribution() {
        // Enqueue attributes by run or harness fallback; the verdict
        // stamp must use the same attribution, or a slow batch ages
        // the enqueue stamp past the beat and the verdict protects
        // nobody.
        use crate::config::PermissionMode;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        s.manager.set_harness_session(a, "h-1".to_string());
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
            crate::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let age = std::time::Instant::now()
            .duration_since(*s.last_hook_activity.get(&a).expect("verdict stamps"));
        assert!(age < std::time::Duration::from_secs(5), "verdict stamps: {age:?}");
        let _ = std::fs::remove_file(&audit);
        assert!(s.manager.remove(a));
    }

    #[test]
    fn staged_enter_prunes_exited_records() {
        // A naturally exited session keeps its record (marked not
        // live); its staged Enter must still go, or the map — and
        // futile pane retries — survive forever.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        assert!(s.manager.kill(a));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(a).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.get(a).is_some(), "record retained");
        assert!(!s.manager.get(a).unwrap().state.is_live(), "marked exited");
        s.pending_enter.insert(
            a,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&a), "exited entry pruned");
    }

    #[test]
    fn activity_in_one_pane_never_holds_another() {
        // Typing in A and hook traffic from A must not starve idle B:
        // debounce gates delivery per target, never app-wide.
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
        assert!(s.manager.set_activity(a, crate::session::Activity::Idle));
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let tell = |s: &mut AppState| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id: run_a.to_string(),
                tool: "tell_session".to_string(),
                args: r#"{"target":"b","message":"wait"}"#.to_string(),
                reply: reply_tx,
                claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
            }));
        };
        // Human typing in A leaves B's delivery alone.
        tell(&mut s);
        s.note_human_input(a);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "A typing holds only A");
        // Flush the staged Enter so the second half tests the hook
        // gate, not one-body-per-Enter staging.
        s.pending_enter.insert(
            b,
            (std::time::Instant::now()
                - crate::comms::INJECT_ENTER_DELAY
                - std::time::Duration::from_millis(100),
            None),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter flushed");
        // Hook traffic attributed to A leaves B's delivery alone.
        tell(&mut s);
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: run_a.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "A hooks hold only A");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_after_hook_activity() {
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
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        // Fresh hook activity holds delivery even to an idle target.
        s.last_hook_activity.insert(b, std::time::Instant::now());
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "hook debounce holds delivery");
        // Past the beat the same settle delivers.
        s.last_hook_activity.insert(
            b,
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn hook_requests_and_verdicts_stamp_hook_activity() {
        use crate::config::PermissionMode;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        assert!(s.last_hook_activity.is_empty());
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
            crate::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let age = std::time::Instant::now()
            .duration_since(*s.last_hook_activity.get(&a).expect("verdict stamps"));
        assert!(age < std::time::Duration::from_secs(5), "verdict stamps: {age:?}");
        let _ = std::fs::remove_file(&audit);
        assert!(s.manager.remove(a));
    }

    #[test]
    fn create_session_spawns_named_shell() {
        let mut s = AppState::new();
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Shell,
            name: "work".to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.name, "work");
        assert_eq!(rec.cli_tool, "shell");
        assert_eq!(rec.cwd, std::env::temp_dir());
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_focuses_the_new_session() {
        let mut s = AppState::new();
        let spec = |name: &str| crate::create::SessionSpec {
            kind: crate::create::SessionKind::Shell,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let first = s.create_session(&spec("a")).unwrap();
        assert_eq!(s.manager.active(), Some(first));
        let second = s.create_session(&spec("b")).unwrap();
        assert_eq!(s.manager.active(), Some(second), "creating focuses the new one");
        assert!(s.manager.remove(first));
        assert!(s.manager.remove(second));
    }

    #[test]
    fn create_session_with_group_joins_at_birth() {
        let mut s = AppState::new();
        s.broker.create_group("team").unwrap();
        let id = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
                name: "work".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: Some("team".to_string()),
            })
            .unwrap();
        assert!(s.broker.is_member(id, "team"));
        assert_eq!(s.tabs().iter().find(|t| t.title == "work").and_then(|t| t.group.clone()), Some("team".to_string()), "bar reflects it");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_agent_records_cli_tool() {
        // Hermetic: stand in for the codex binary; argv shape is locked in
        // the harness registry tests.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Agent(
                crate::harness::Harness::from_name("codex").unwrap(),
            ),
            name: "coder".to_string(),
            cwd: std::env::temp_dir(),
            model: "gpt-5".to_string(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.cli_tool, "codex");
        assert!(s.manager.remove(id));
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
    }

    #[test]
    fn suggested_name_skips_taken_names() {
        let mut s = AppState::new();
        assert_eq!(s.suggested_session_name(), "claude-1");
        s.open_create_dialog();
        assert!(s.create_dialog.is_some());
        let id = s
            .manager
            .spawn("claude-1", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(s.suggested_session_name(), "claude-2");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn session_traffic_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        let id = SessionId::fresh();
        s.apply(AppEvent::SessionOutput { id, data: vec![1] });
        assert!(s.dirty);
        s.dirty = false;
        s.apply(AppEvent::SessionExited { id, code: Some(0) });
        assert!(s.dirty);
    }

    #[test]
    fn resize_records_and_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Resize(40, 120));
        assert_eq!(s.term_size, (40, 120));
        assert!(s.dirty);
    }

    #[test]
    fn tick_is_quiet() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Tick);
        assert!(!s.dirty);
        assert!(!s.should_quit);
    }

    #[test]
    fn views_begin_empty() {
        let s = AppState::new();
        assert!(s.views().is_empty());
    }

    #[test]
    fn views_reflect_sessions() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("one", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("two", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let views = s.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].title, "one");
        assert_eq!(views[1].title, "two");
        assert!(views[0].focused && !views[1].focused);
        assert!(views.iter().all(|v| v.live));
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(b));
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(a));
        s.step_session(-1);
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn select_session_focuses_by_index() {
        let mut s = AppState::new();
        assert!(!s.select_session(0), "empty: no-op");
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert!(s.select_session(1));
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.select_session(0));
        assert_eq!(s.manager.active(), Some(a));
        assert!(!s.select_session(9), "out of range keeps focus");
        assert_eq!(s.manager.active(), Some(a));
        let tabs = s.tabs();
        assert_eq!(tabs.len(), 2);
        assert!(tabs[0].focused && !tabs[1].focused);
        assert_eq!(s.sidebar_info().pending, 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn terminate_session_drops_ui_and_groups() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, a, "other").unwrap();
        assert!(s.terminate_session(a));
        assert!(s.manager.get(a).is_none(), "record gone");
        assert_eq!(s.manager.order().len(), 1);
        assert!(!s.broker.is_member(a, "peers"), "left peers");
        assert!(!s.broker.is_member(a, "other"), "left other");
        assert_eq!(s.manager.active(), Some(b), "focus falls through");
        assert!(!s.terminate_session(a), "unknown id is a no-op");
        assert!(s.manager.remove(b));
    }

    #[test]
    fn toggle_grid_flips_and_select_exits() {
        let mut s = AppState::new();
        assert!(!s.grid_mode);
        s.toggle_grid();
        assert!(s.grid_mode);
        s.toggle_grid();
        assert!(!s.grid_mode);
        // Picking a number always returns to the focused view.
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.toggle_grid();
        assert!(s.select_session(1));
        assert!(!s.grid_mode, "select exits grid");
        assert_eq!(s.manager.active(), Some(b));
        s.toggle_grid();
        assert!(!s.select_session(9), "out of range");
        assert!(s.grid_mode, "failed select stays in grid");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
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
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
    fn permission_mode_toggle_spans_off_and_yolo() {
        let mut s = AppState::new();
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        s.dirty = false;
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Off);
        assert!(s.dirty);
        assert!(!s.set_permission_mode(crate::config::PermissionMode::Off));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        // Foreign modes collapse to Yolo, never back through the toggle.
        assert!(s.set_permission_mode(crate::config::PermissionMode::SafeOnly));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
    }

    #[test]
    fn hook_attribution_feeds_sidebar_counters() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
            .unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.tool_calls, rec.approvals, rec.denials), (0, 0, 0));
        // Tool-gated hook: activity + one tool call.
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: run.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert_eq!(s.manager.get(id).unwrap().tool_calls, 1);
        // Yolo settle: one approval, no denial.
        let audit = std::env::temp_dir().join(format!(
            "forge-counters-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo =
            crate::policy::Policy::new(crate::config::PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.approvals, rec.denials), (1, 0));
        let _ = std::fs::remove_file(&audit);
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_routes_agent_to_dual_tabs() {
        // Point the codex binary at `cat` so the test never depends on a
        // real agent CLI being installed; no other test reads this var.
        std::env::set_var("CODEX_BIN", "cat");
        let mut s = AppState::new();
        let agent = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Agent(
                    crate::harness::Harness::from_name("codex").unwrap(),
                ),
                name: "codex-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        assert_eq!(s.manager.tab_count(agent), 3);
        assert!(s.manager.switch_tab(agent));
        let shell = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
                name: "shell-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        assert_eq!(s.manager.tab_count(shell), 1);
        assert!(s.manager.remove(agent));
        assert!(s.manager.remove(shell));
    }

    #[test]
    fn agent_topbar_exposes_clickable_read_only_views() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert_eq!(labels, ["🤖 Codex", "💻 Terminal", "🔀 SCM", "🔔 Events", "📝 Tasks", "📷 Visual", "📖 Walkthrough"]);
        assert!(state.select_top_tab(3));
        assert!(state.topbar().tabs[3].active);
        let view = state.views().into_iter().find(|v| v.focused).unwrap();
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("Events")));
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("unavailable")));
        assert!(state.select_top_tab(0));
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    fn comms_reply(state: &mut AppState, run: &str, tool: &str, args: &str) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run.to_string(),
            tool: tool.to_string(),
            args: args.to_string(),
            reply: tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::listener::CLAIM_PENDING)),
        }));
        rx.recv().expect("comms verdict arrives")
    }

    fn bot_reply(
        state: &mut AppState,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
    ) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::BotRequest(crate::listener::BotRequest {
            name: name.to_string(),
            token: token.to_string(),
            tool: tool.to_string(),
            args: args.to_string(),
            reply: tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::listener::CLAIM_PENDING,
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
        state.apply(AppEvent::BotRequest(crate::listener::BotRequest {
            name: "skippy".to_string(),
            token: BOT_TOKEN.to_string(),
            tool: "list_sessions".to_string(),
            args: "{}".to_string(),
            reply: tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                crate::listener::CLAIM_TIMED_OUT,
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

    #[test]
    fn telegram_poll_queues_inbound_and_flags_failure() {
        let mut s = AppState::new();
        s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
            messages: vec![crate::telegram::InboundMessage {
                user_id: 11,
                chat_id: 11,
                text: "hi".to_string(),
            }],
            failed: false,
        }));
        assert_eq!(s.telegram_inbox.len(), 1);
        assert!(!s.telegram_last_poll_failed);
        s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
            messages: Vec::new(),
            failed: true,
        }));
        assert!(s.telegram_last_poll_failed);
        assert_eq!(s.telegram_inbox.len(), 1, "failures queue nothing");
    }

    #[test]
    fn telegram_inbox_is_bounded_and_counts_drops() {
        let mut s = AppState::new();
        for i in 0..(crate::telegram::INBOX_CAP as i64 + 5) {
            s.apply(AppEvent::TelegramPoll(crate::telegram::PollReport {
                messages: vec![crate::telegram::InboundMessage {
                    user_id: i,
                    chat_id: i,
                    text: "m".to_string(),
                }],
                failed: false,
            }));
        }
        assert_eq!(s.telegram_inbox.len(), crate::telegram::INBOX_CAP);
        assert_eq!(s.telegram_dropped, 5);
        assert_eq!(s.telegram_inbox[0].user_id, 0, "oldest kept: FIFO drain");
    }

    #[cfg(feature = "visual")]
    fn spawn_visual_agent(state: &mut AppState, name: &str) -> (crate::session::SessionId, String) {
        let id = state.manager.spawn_agent(
            name, &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        (id, run)
    }

    #[cfg(feature = "visual")]
    fn fake_png(len: usize, w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A,
            0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.resize(len.max(24), 0);
        v
    }

    #[cfg(feature = "visual")]
    fn complete(
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
            crate::ids::RunId::generate(), "codex",
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
    fn show_visual_overlay(state: &mut AppState, id: crate::session::SessionId) {
        // Selection clears the overlay, so focus first, then open it.
        state.select_session(0);
        let slot = state.visual_slot(id).expect("visual slot exists");
        state.overlay_view = Some((id, slot));
    }

    #[cfg(feature = "visual")]
    fn paint_for(state: &AppState, id: crate::session::SessionId) -> crate::visual::VisualPaint {
        state
            .visual_paint(id, ratatui::layout::Rect::new(0, 0, 200, 50), 8.0, 16.0)
            .expect("paint")
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
        assert!(state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
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
        assert!(state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
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
            state.visual_zoom(id, crate::ui::VisualButton::ZoomOut, 200, 50, 8.0, 16.0);
        }
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.zoom, crate::visual::MIN_ZOOM);
        state.dirty = false;
        assert!(!state.visual_zoom(id, crate::ui::VisualButton::ZoomOut, 200, 50, 8.0, 16.0));
        assert!(!state.dirty, "no-op zoom stays clean");
        // Zoom to maximum: 1000x1000 at 8x overflows a 200x50 tab by
        // (600, 350) cells.
        for _ in 0..30 {
            state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0);
        }
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, crate::visual::MAX_ZOOM);
        assert!(!state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
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
        assert!(!state.visual_zoom(ghost, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
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
        assert!(state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
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
        let areas = crate::ui::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, state.pill_tabs, foot);
        let ask_idx = (chrome.footer.y - content.y) as usize;
        let block = &view.lines[ask_idx..ask_idx + foot as usize];
        let width = content.width as usize;
        for (i, row) in block.iter().enumerate() {
            assert_eq!(crate::ui::spans_width(row), width, "box row {i} fills");
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
            crate::theme::style(crate::theme::Role::Muted),
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
        let areas = crate::ui::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, state.pill_tabs, foot);
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
        let areas = crate::ui::chrome_areas(Rect::new(0, 0, 100, 40));
        let content = crate::ui::pane_content_area(&areas);
        assert_eq!(content.height, 36);
        let chrome = crate::ui::visual_chrome(
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
        let areas = crate::ui::chrome_areas(Rect::new(0, 0, cols, rows));
        let content = crate::ui::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, state.pill_tabs, foot);
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
        let areas = crate::ui::chrome_areas(Rect::new(0, 0, cols, rows));
        let content = crate::ui::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, state.pill_tabs, foot);
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
        let areas = crate::ui::chrome_areas(area);
        let content = crate::ui::pane_content_area(&areas);
        let foot = state.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual_chrome(content, state.pill_tabs, foot);
        let chrome_ui = crate::ui::Chrome {
            tabs: vec![crate::ui::SessionTab {
                title: "agent".to_string(),
                live: true,
                focused: true,
                group: None,
                group_color: None,
            }],
            topbar: crate::ui::TopBar { tabs: Vec::new() },
            detail: None,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
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
    fn walkthrough_tools_drive_tour_lifecycle() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        // The fake harness calls with the record's own run ID.
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-test-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let args = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::mcp::escape_json(&path.to_string_lossy()),
        );
        let started = comms_reply(&mut state, &live_run, "walkthrough_start", &args);
        assert!(started.contains(r#""ok":true"#), "started: {started}");
        assert!(started.contains(r#""steps":2"#), "started: {started}");
        assert!(state.walkthrough_overlay_active());
        assert_eq!(state.walkthrough_overlay().unwrap().title, path.to_string_lossy());
        // Answering with nothing waiting fails instead of inventing Q&A.
        let early = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(early.contains("no walkthrough question waiting"), "early: {early}");
        // Ask through the overlay: the draft submits, the markup stages
        // an Enter, and the question waits for the agent.
        state.walkthrough_overlay_mut().unwrap().input = Some("why three?".to_string());
        assert!(state.submit_walkthrough_question(id));
        assert!(state.pending_enter.contains_key(&id));
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.questions.len(), 1);
        assert_eq!(tour.questions[0].question, "why three?");
        assert!(tour.questions[0].answer.is_none());
        assert!(tour.input.is_none());
        let answered = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"because"}"#);
        assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
        assert_eq!(
            state.walkthrough_overlay().unwrap().questions[0].answer.as_deref(),
            Some("because")
        );
        let ended = comms_reply(&mut state, &live_run, "walkthrough_end", r#"{"summary":"done"}"#);
        assert!(ended.contains(r#""ended":true"#), "ended: {ended}");
        assert!(state.walkthrough_overlay().unwrap().completed);
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_start_rejects_bad_calls() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let missing = comms_reply(
            &mut state, &live_run, "walkthrough_start",
            r#"{"file":"/no/such/forge-walk-missing.rs","steps":"1:1:x"}"#,
        );
        assert!(missing.contains(r#""ok":false"#), "missing: {missing}");
        assert!(missing.contains("cannot read"), "missing: {missing}");
        assert!(!state.walkthrough_overlay_active());
        let stale = comms_reply(&mut state, "bogus-run", "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(stale.contains("unknown or stale run ID"), "stale: {stale}");
        let no_tour = comms_reply(&mut state, &live_run, "walkthrough_end", "{}");
        assert!(no_tour.contains("no walkthrough for this session"), "no_tour: {no_tour}");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn session_status_round_trips_to_sidebar() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
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
        let lines = crate::ui::sidebar_lines(&state.sidebar_info());
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

    fn message_user_agent() -> (AppState, crate::session::SessionId, String) {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state
            .manager
            .spawn_agent(
                "agent",
                &std::env::temp_dir(),
                "exec cat",
                crate::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        (state, id, live_run)
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

    fn tg_agent(name: &str) -> (AppState, crate::session::SessionId, String) {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state
            .manager
            .spawn_agent(
                name,
                &std::env::temp_dir(),
                "exec cat",
                crate::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        (state, id, live_run)
    }

    fn tg_inbox(state: &mut AppState, chat: i64, texts: &[&str]) {
        for text in texts {
            state.telegram_inbox.push_back(crate::telegram::InboundMessage {
                user_id: 11,
                chat_id: chat,
                text: text.to_string(),
            });
        }
    }

    fn tg_outbox(state: &AppState) -> Vec<crate::telegram::OutboundMessage> {
        let mut guard = state.telegram_outbox.lock().expect("outbox unlocks");
        let mut out = Vec::new();
        while let Some(m) = guard.pop_front() {
            out.push(m);
        }
        out
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
        assert!(replies.is_empty(), "routed text stays silent: {replies:?}");
        assert!(state.manager.remove(id));
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
    fn telegram_clear_drops_queued_injections() {
        let (mut state, id, _run) = tg_agent("agent");
        tg_inbox(&mut state, 11, &["[agent] one", "[agent] two"]);
        state.drain_telegram();
        assert_eq!(state.broker.queued(id), 2);
        assert!(tg_outbox(&state).is_empty(), "routes stay silent");
        tg_inbox(&mut state, 11, &["/clear agent"]);
        state.drain_telegram();
        let replies = tg_outbox(&state);
        assert_eq!(replies.len(), 1);
        assert!(replies[0].text.contains("cleared 2"), "reply: {}", replies[0].text);
        assert_eq!(state.broker.queued(id), 0);
        assert!(state.manager.remove(id));
    }

    fn tg_home() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-tg-form-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(crate::branding::config_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn telegram_form_save_writes_token_and_lives() {
        let home = tg_home();
        let path = crate::branding::config_file(&home);
        let mut loaded = crate::config::LoadedConfig::load(&path).expect("defaults load");
        let token_path = home.join("tg.token");
        let mut state = AppState::new();
        state.telegram_dialog = Some(crate::telegram_dialog::TelegramDialog::new(
            &crate::config::TelegramConfig::default(),
        ));
        let form = crate::telegram_dialog::TelegramForm {
            config: crate::config::TelegramConfig {
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
            let mode = std::fs::metadata(&token_path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token file is owner-only");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn telegram_tested_lands_in_open_dialog() {
        let mut state = AppState::new();
        state.telegram_dialog = Some(crate::telegram_dialog::TelegramDialog::new(
            &crate::config::TelegramConfig::default(),
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
        let mut state = AppState::new();
        state.telegram_dialog = Some(crate::telegram_dialog::TelegramDialog::new(
            &crate::config::TelegramConfig::default(),
        ));
        state.start_telegram_test(String::new(), "/nonexistent-forge-tg.token".to_string());
        let dialog = state.telegram_dialog.as_ref().expect("dialog stays open");
        assert!(
            dialog.test_result().is_some_and(|(ok, detail)| !ok && detail.contains("unreadable")),
            "inline failure, no thread: {:?}",
            dialog.test_result()
        );
    }

    #[test]
    fn sidebar_info_marks_failing_telegram_poll() {
        let mut state = AppState::new();
        state.telegram_config.lock().expect("lock").enabled = true;
        state.telegram_last_poll_failed = true;
        assert_eq!(state.sidebar_info().telegram, "on · retrying");
        state.telegram_last_poll_failed = false;
        assert_eq!(state.sidebar_info().telegram, "on");
        state.telegram_config.lock().expect("lock").enabled = false;
        assert_eq!(state.sidebar_info().telegram, "off");
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
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.manager.remove(id));
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
                crate::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        let gone = state
            .manager
            .spawn_agent(
                "gone",
                &std::env::temp_dir(),
                "exec cat",
                crate::ids::RunId::generate(),
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
    fn sidebar_shows_timers_only_for_focused_session() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let a = state.manager.spawn_agent(
            "a", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let b = state.manager.spawn_agent(
            "b", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let run_a = state.manager.get(a).unwrap().run_id.as_str().to_string();
        let out = comms_reply(
            &mut state, &run_a, "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#,
        );
        let timer = crate::policy::json_string_field(out.as_bytes(), &["timer_id"]).unwrap();
        assert_eq!(state.manager.active(), Some(a));
        let focused = state.sidebar_info().session.expect("detail renders");
        assert_eq!(focused.timers.len(), 1, "focused session shows its timer");
        assert_eq!(focused.timers[0].id, timer);
        state.manager.switch(b);
        let other = state.sidebar_info().session.expect("detail renders");
        assert!(other.timers.is_empty(), "unfocused timers stay hidden");
        assert!(state.cancel_timer(&timer), "sidebar cancel drops it");
        assert!(state.broker.timers_for(a).is_empty());
        assert!(!state.cancel_timer(&timer), "second cancel stays false");
        assert!(state.manager.remove(a));
        assert!(state.manager.remove(b));
    }

    #[test]
    fn start_session_validates_creates_and_keeps_focus() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
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

    #[test]
    fn walkthrough_add_and_update_steps() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        std::env::set_var("CODEX_BIN", "cat");
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-steps-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let start = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::mcp::escape_json(&path.to_string_lossy()),
        );
        comms_reply(&mut state, &live_run, "walkthrough_start", &start);
        let added = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":7,"end_line":8,"explanation":"new"}"#,
        );
        assert!(added.contains(r#""added":true"#), "added: {added}");
        assert!(added.contains(r#""steps":3"#), "added: {added}");
        let first = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":9,"end_line":9,"explanation":"top","position":1}"#,
        );
        assert!(first.contains(r#""steps":4"#), "first: {first}");
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.steps[0].explanation, "top");
        assert_eq!(tour.index, 1, "insert before current shifts it");
        let wrong_file = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":1,"end_line":1,"explanation":"x","file_path":"other.rs"}"#,
        );
        assert!(wrong_file.contains("open tour file only"), "wrong_file: {wrong_file}");
        let updated = comms_reply(
            &mut state, &live_run, "walkthrough_update",
            r#"{"step_index":1,"explanation":"revised"}"#,
        );
        assert!(updated.contains(r#""updated":true"#), "updated: {updated}");
        assert_eq!(state.walkthrough_overlay().unwrap().steps[0].explanation, "revised");
        for (args, needle) in [
            (r#"{"step_index":0}"#, "starts at 1"),
            (r#"{"step_index":99}"#, "no walkthrough step"),
            (r#"{"step_index":1,"start_line":9,"end_line":2}"#, "bad walkthrough range"),
        ] {
            let err = comms_reply(&mut state, &live_run, "walkthrough_update", args);
            assert!(err.contains(needle), "args {args}: {err}");
        }
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }

    #[test]
    fn scm_tab_selects_a_live_lazygit_pane() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        // Index 2 is a real PTY tab now, not an overlay: selecting it
        // lazily spawns the pane (the shell reports a missing binary as
        // an exited child, so this holds with or without lazygit).
        assert!(state.select_top_tab(2));
        assert!(state.topbar().tabs[2].active);
        assert_eq!(
            state.manager.active_tab_kind(id),
            Some(crate::session::TabKind::Scm)
        );
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wide_agent_topbar_uses_labeled_icons_from_reference() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert!(labels[0].starts_with("🤖 "));
        assert!(labels[1].starts_with("💻 "));
        assert!(labels[2].starts_with("🔀 "));
        assert!(labels[3].starts_with("🔔 "));
        assert!(labels[4].starts_with("📝 "));
        assert!(labels[5].starts_with("📷 "));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn shrinking_hides_and_closes_extra_view() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert!(state.select_top_tab(3));
        state.apply(AppEvent::Resize(24, 80));
        assert!(!state.overlay_active());
        assert_eq!(state.topbar().tabs.len(), 3);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn ordinary_wide_terminal_keeps_all_topbar_views_visible() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 120));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert_eq!(state.topbar().tabs.len(), 7);
        let bar = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 120, 30)).topbar;
        assert_eq!(crate::ui::layout_topbar(bar, &state.topbar().tabs, false).len(), 7);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn events_tab_does_not_mislabel_tool_calls_as_event_count() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let run = crate::ids::RunId::generate();
        let id = state.manager.spawn_agent("agent", &std::env::temp_dir(), "exec cat", run.clone(), "codex").unwrap();
        let (reply, _) = std::sync::mpsc::channel();
        state.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".into(), body: "{}".into(), run_id: run.as_str().into(),
            sync: false, reply, timed_out: Default::default(),
        }));
        assert_eq!(state.topbar().tabs[3].label, "🔔 Events");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn step_is_safe_when_empty() {
        let mut s = AppState::new();
        s.step_session(1);
        assert!(s.manager.active().is_none());
    }

    #[test]
    fn manager_spawn_flows_through_state() {
        let mut s = AppState::new();
        let id = s
            .manager
            .spawn("w", &std::env::temp_dir(), "exit 0", RunId::generate(), "shell")
            .unwrap();
        assert!(s.manager.get(id).is_some());
        assert!(s.manager.remove(id));
    }
}
