//! AppState visual tab state: slots, overlays, workers, and completions.

use super::*;

pub mod paint;

impl AppState {
    /// Overlay slot index of the Visual tab for one session.
    #[cfg(feature = "visual")]
    pub(super) fn visual_slot(&self, id: crate::session::SessionId) -> Option<usize> {
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
    pub(super) fn visual_complete(&mut self, done: VisualDone) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;

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
