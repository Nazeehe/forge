//! Session walkthroughs (Phase 6): agent-led file tours with human Q&A.
//!
//! An agent opens a walkthrough over a file with ordered line-range
//! steps; the human steps through them with `j`/`k` and asks questions
//! with `Enter`. A submitted question is injected into the agent pane
//! in `<walkthrough-question>` markup (same staged-Enter path as comms
//! injections); the agent answers by calling the `walkthrough_answer`
//! MCP tool, which resolves the latest pending question. Answers and
//! explanations render as Markdown in the Q&A half of the view.

use crate::theme::{style, Role};

/// Rejected file size: walkthroughs tour source, not dumps. The agent
/// gets a one-line error and picks a smaller slice instead.
pub const MAX_FILE_BYTES: usize = 512 * 1024;

/// Longest accepted question draft, in chars. Paste overflows are cut,
/// never grown unbounded.
pub const MAX_INPUT_CHARS: usize = 4000;

/// Placeholder while the agent's answer is in flight.
pub const WAITING_TEXT: &str = "<waiting for answer>";

/// One tour stop: 1-based inclusive line range plus a Markdown
/// explanation of what those lines do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub start: u32,
    pub end: u32,
    pub explanation: String,
}

/// One human question plus its answer once `walkthrough_answer` lands.
/// `None` renders as the waiting placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub answer: Option<String>,
}

/// One highlighted token: a static color over a byte range of its
/// line. Code ignores the OS theme on purpose: one hand-picked dark
/// palette that always contrasts, like any editor theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HLSpan {
    pub color: ratatui::style::Color,
    pub start: usize,
    pub end: usize,
}

/// Static code palette (OneDark hues): body text, dim comments,
/// green strings, purple keywords, blue names, teal types, orange
/// constants, the step-row wash, and the gutter bar.
pub mod code {
    use ratatui::style::Color;
    pub const FG: Color = Color::Rgb(171, 178, 191);
    pub const COMMENT: Color = Color::Rgb(92, 99, 112);
    pub const STRING: Color = Color::Rgb(152, 195, 121);
    pub const KEYWORD: Color = Color::Rgb(198, 120, 221);
    pub const NAME: Color = Color::Rgb(97, 175, 239);
    pub const TYPE: Color = Color::Rgb(86, 182, 194);
    pub const CONSTANT: Color = Color::Rgb(209, 154, 102);
    pub const INVALID: Color = Color::Rgb(224, 108, 117);
    pub const WASH: Color = Color::Rgb(44, 49, 60);
    pub const GUTTER: Color = Color::Rgb(86, 182, 194);
}

/// Live walkthrough for one session: tour position plus the running
/// Q&A log. `input` is `Some` while the bottom bar is a `> ` prompt.
/// `hl` holds one token row per source line, computed once at open.
pub struct Walkthrough {
    pub title: String,
    pub file_path: String,
    pub lines: Vec<String>,
    pub hl: Vec<Vec<HLSpan>>,
    pub steps: Vec<Step>,
    pub index: usize,
    pub completed: bool,
    pub summary: Option<String>,
    pub questions: Vec<Question>,
    pub input: Option<String>,
    pub code_scroll: usize,
    pub status: Option<String>,
}

impl Walkthrough {
    /// Open over already-loaded file content. At least one step is
    /// required; callers without steps pass a whole-file default.
    pub fn start(
        title: String,
        file_path: String,
        content: &str,
        steps: Vec<Step>,
    ) -> Result<Self, String> {
        if steps.is_empty() {
            return Err("walkthrough needs at least one step".to_string());
        }
        let lines: Vec<String> = content.lines().map(str::to_string).collect();
        if lines.is_empty() {
            return Err("walkthrough file is empty".to_string());
        }
        let mut wt = Walkthrough {
            title,
            file_path,
            lines,
            hl: Vec::new(),
            steps: Vec::new(),
            index: 0,
            completed: false,
            summary: None,
            questions: Vec::new(),
            input: None,
            code_scroll: 0,
            status: None,
        };
        for step in steps {
            wt.check_step(step.start, step.end, &step.explanation)?;
            wt.steps.push(step);
        }
        // Token roles once at open: renders stay cheap no matter how
        // often keys repaint, and unknown extensions stay plain.
        wt.hl = highlight_lines(&wt.file_path, &wt.lines);
        Ok(wt)
    }

    fn check_step(&self, start: u32, end: u32, explanation: &str) -> Result<(), String> {
        if start < 1 || end < start {
            return Err(format!("bad walkthrough range {start}-{end}"));
        }
        if end as usize > self.lines.len() {
            return Err(format!(
                "walkthrough range {start}-{end} past file end ({})",
                self.lines.len()
            ));
        }
        if explanation.trim().is_empty() {
            return Err("walkthrough step needs an explanation".to_string());
        }
        Ok(())
    }

    /// Parse the `steps` tool arg: one `start:end:explanation` step
    /// header per line (colons included in the explanation), with the
    /// explanation running on: a line starting with a space or tab
    /// continues the open step with exactly one blank stripped, so
    /// Markdown nesting survives, and blank lines become paragraph
    /// breaks. A header may leave the explanation empty when
    /// continuation lines carry it.
    pub fn parse_steps(text: &str, line_count: usize) -> Result<Vec<Step>, String> {
        let mut steps: Vec<Step> = Vec::new();
        let mut current: Option<(Step, usize)> = None;
        // Trailing paragraph breaks never survive: the explanation is
        // trimmed when its step closes, and emptiness fails there.
        let close = |current: &mut Option<(Step, usize)>,
                         steps: &mut Vec<Step>|
         -> Result<(), String> {
            if let Some((mut step, header)) = current.take() {
                step.explanation = step.explanation.trim_end().to_string();
                if step.explanation.is_empty() {
                    return Err(format!("step {header} needs an explanation"));
                }
                steps.push(step);
            }
            Ok(())
        };
        for (i, raw) in text.lines().enumerate() {
            let no = i + 1;
            if raw.trim().is_empty() {
                if let Some((step, _)) = current.as_mut() {
                    step.explanation.push('\n');
                }
                continue;
            }
            if raw.starts_with(' ') || raw.starts_with('\t') {
                let Some((step, _)) = current.as_mut() else {
                    return Err(format!("line {no} continues no open step"));
                };
                let cont = raw
                    .strip_prefix(' ')
                    .or_else(|| raw.strip_prefix('\t'))
                    .unwrap_or(raw);
                step.explanation.push('\n');
                step.explanation.push_str(cont);
                continue;
            }
            close(&mut current, &mut steps)?;
            let mut parts = raw.trim().splitn(3, ':');
            let (start, end, explanation) = match (parts.next(), parts.next(), parts.next()) {
                (Some(s), Some(e), Some(x)) => (s.trim(), e.trim(), x.trim()),
                _ => return Err(format!("step {no} must be start:end:explanation")),
            };
            let parse = |v: &str| {
                v.parse::<u32>()
                    .map_err(|_| format!("step {no} has non-numeric range"))
            };
            let (start, end) = (parse(start)?, parse(end)?);
            if start < 1 || end < start || end as usize > line_count {
                return Err(format!("step {no} range out of bounds"));
            }
            current = Some((
                Step {
                    start,
                    end,
                    explanation: explanation.to_string(),
                },
                no,
            ));
        }
        close(&mut current, &mut steps)?;
        if steps.is_empty() {
            return Err("no walkthrough steps found".to_string());
        }
        Ok(steps)
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    pub fn current_step(&self) -> &Step {
        &self.steps[self.index.min(self.steps.len() - 1)]
    }

    /// Step navigation clamps at both ends and re-anchors the code
    /// window; the Q&A log survives (questions may cite earlier steps).
    pub fn step_next(&mut self) {
        if self.index + 1 < self.steps.len() {
            self.index += 1;
            self.code_scroll = 0;
        }
    }

    pub fn step_prev(&mut self) {
        self.index = self.index.saturating_sub(1);
        self.code_scroll = 0;
    }

    pub fn add_step(&mut self, step: Step, position: Option<usize>) -> Result<(), String> {
        self.check_step(step.start, step.end, &step.explanation)?;
        let at = position.unwrap_or(self.steps.len()).min(self.steps.len());
        self.steps.insert(at, step);
        if at <= self.index {
            self.index += 1;
        }
        Ok(())
    }

    pub fn update_step(
        &mut self,
        index: usize,
        start: Option<u32>,
        end: Option<u32>,
        explanation: Option<&str>,
    ) -> Result<(), String> {
        let (old_start, old_end, old_explanation) = match self.steps.get(index) {
            Some(slot) => (slot.start, slot.end, slot.explanation.clone()),
            None => return Err(format!("no walkthrough step {index}")),
        };
        let start = start.unwrap_or(old_start);
        let end = end.unwrap_or(old_end);
        let explanation = explanation.unwrap_or(old_explanation.as_str());
        self.check_step(start, end, explanation)?;
        let slot = &mut self.steps[index];
        slot.start = start;
        slot.end = end;
        slot.explanation = explanation.to_string();
        Ok(())
    }

    pub fn end(&mut self, summary: Option<String>) {
        self.completed = true;
        self.summary = summary.filter(|s| !s.trim().is_empty());
    }

    /// Latest question still awaiting an answer, if any.
    pub fn pending_question(&self) -> bool {
        self.questions.iter().any(|q| q.answer.is_none())
    }

    /// Resolve the latest pending question. False when none is waiting
    /// (the tool call then errors instead of answering thin air).
    pub fn answer_latest(&mut self, answer: &str) -> bool {
        match self.questions.iter_mut().rev().find(|q| q.answer.is_none()) {
            Some(q) => {
                q.answer = Some(answer.to_string());
                true
            }
            None => false,
        }
    }

    /// Append one input char; returns false past the draft cap (extra
    /// typing is dropped, the buffer keeps its head).
    pub fn push_input(&mut self, c: char) -> bool {
        match self.input.as_mut() {
            Some(buf) if buf.chars().count() < MAX_INPUT_CHARS => {
                buf.push(c);
                true
            }
            Some(_) => false,
            None => false,
        }
    }

    pub fn backspace_input(&mut self) {
        if let Some(buf) = self.input.as_mut() {
            buf.pop();
        }
    }

    /// Sanitize pasted text into a single-line draft fragment: line
    /// breaks and tabs collapse to one space, other controls (a raw
    /// ESC in a Span would reach the terminal) are dropped. Capped so
    /// the buffer keeps its head. False outside input mode.
    pub fn push_paste(&mut self, text: &str) -> bool {
        let Some(buf) = self.input.as_mut() else {
            return false;
        };
        let mut changed = false;
        for c in text.chars() {
            if buf.chars().count() >= MAX_INPUT_CHARS {
                break;
            }
            if c == '\n' || c == '\r' || c == '\t' {
                if buf.chars().last().is_some_and(|l| l != ' ') {
                    buf.push(' ');
                    changed = true;
                }
            } else if !c.is_control() {
                buf.push(c);
                changed = true;
            }
        }
        changed
    }

    /// Drain a non-blank draft for submission. Blank drafts stay in
    /// input mode (nothing to send); the buffer is returned on success.
    pub fn take_draft(&mut self) -> Option<String> {
        match self.input.as_ref() {
            Some(buf) if !buf.trim().is_empty() => {
                let draft = buf.trim().to_string();
                self.input = None;
                Some(draft)
            }
            _ => None,
        }
    }

    /// Record a delivered question. Called only after the pane write
    /// succeeds, so the log never claims an undelivered ask.
    pub fn push_question(&mut self, question: String) {
        self.questions.push(Question {
            question,
            answer: None,
        });
    }

    /// Injection body for one question: step context for the agent plus
    /// the blueprint `<walkthrough-question>` markup it answers via the
    /// `walkthrough_answer` tool.
    pub fn question_markup(title: &str, index: usize, steps: usize, step: &Step, question: &str) -> String {
        format!(
            "[forge walkthrough \"{title}\" step {}/{} (lines {}-{})]:\n<walkthrough-question>\n{question}\n</walkthrough-question>",
            index + 1,
            steps,
            step.start,
            step.end,
        )
    }

    /// Context lines shown above the step range, like the reference
    /// view (signatures and imports stay visible above the focus).
    pub const STEP_CONTEXT: usize = 3;

    /// Code rows for the visible window: the step opens below a few
    /// context lines, plus the scroll offset, clamped so the window
    /// never runs dry.
    pub fn code_window(&self, height: usize) -> Vec<(u32, &str, bool)> {
        if height == 0 || self.lines.is_empty() {
            return Vec::new();
        }
        let step = self.current_step();
        let anchor = (step.start as usize)
            .saturating_sub(1)
            .saturating_sub(Self::STEP_CONTEXT);
        let max_top = self.lines.len().saturating_sub(height.min(self.lines.len()));
        let top = anchor.saturating_add(self.code_scroll).min(max_top);
        self.lines
            .iter()
            .enumerate()
            .skip(top)
            .take(height)
            .map(|(i, line)| {
                let no = i as u32 + 1;
                let in_range = no >= step.start && no <= step.end;
                (no, line.as_str(), in_range)
            })
            .collect()
    }

    /// Scroll the code window a line at a time, clamped to content.
    /// Positive climbs toward the file top.
    pub fn scroll_code(&mut self, lines: i32) {
        if lines >= 0 {
            self.code_scroll = self.code_scroll.saturating_sub(lines as usize);
        } else {
            self.code_scroll = self.code_scroll.saturating_add(lines.unsigned_abs() as usize);
        }
    }

    /// One key inside the overlay. Typing mode takes text, Enter and
    /// Esc; browse mode takes the step/scroll chords from the key bar.
    /// Everything else is swallowed: the overlay owns input while open.
    /// Any key clears a flashed status first.
    pub fn key(&mut self, ke: &crossterm::event::KeyEvent) -> WalkKey {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        self.status = None;
        if !matches!(ke.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return WalkKey::Ignored;
        }
        let mods = ke.modifiers;
        if self.input.is_some() {
            return match ke.code {
                KeyCode::Enter if mods.is_empty() => {
                    // Peek first: take_draft drains the buffer, so only
                    // take it when the caller can use the result. The
                    // caller takes the draft itself on Submitted.
                    let blank = self
                        .input
                        .as_ref()
                        .is_none_or(|b| b.trim().is_empty());
                    if blank {
                        self.status = Some("type a question first".to_string());
                        WalkKey::Edited
                    } else {
                        WalkKey::Submitted
                    }
                }
                KeyCode::Esc if mods.is_empty() => {
                    self.input = None;
                    WalkKey::CancelledInput
                }
                KeyCode::Backspace if mods.is_empty() => {
                    self.backspace_input();
                    WalkKey::Edited
                }
                KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                    self.push_input(c);
                    WalkKey::Edited
                }
                _ => WalkKey::Ignored,
            };
        }
        let shifted = mods.contains(KeyModifiers::SHIFT);
        match ke.code {
            KeyCode::Enter if mods.is_empty() => {
                self.input = Some(String::new());
                WalkKey::Edited
            }
            KeyCode::Esc if mods.is_empty() => WalkKey::Closed,
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                match c.to_ascii_lowercase() {
                    'j' => {
                        self.step_next();
                        WalkKey::Moved
                    }
                    'k' => {
                        self.step_prev();
                        WalkKey::Moved
                    }
                    'g' if !shifted => {
                        self.scroll_code(1);
                        WalkKey::Moved
                    }
                    'g' => {
                        self.scroll_code(-1);
                        WalkKey::Moved
                    }
                    _ => WalkKey::Ignored,
                }
            }
            KeyCode::Down | KeyCode::Right if mods.is_empty() => {
                self.step_next();
                WalkKey::Moved
            }
            KeyCode::Up | KeyCode::Left if mods.is_empty() => {
                self.step_prev();
                WalkKey::Moved
            }
            _ => WalkKey::Ignored,
        }
    }
}

/// Wheel notches scroll the code window this many lines.
pub const WHEEL_SCROLL_LINES: i32 = 3;

/// Shared Sublime grammar set, loaded once: every tour detects its
/// language from the file extension against the same tables.
fn syntax_set() -> &'static syntect::parsing::SyntaxSet {
    static SET: std::sync::OnceLock<syntect::parsing::SyntaxSet> = std::sync::OnceLock::new();
    SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

/// Static color for one scope stack, innermost first. Unknown scopes
/// fall through to the next outer scope, so partial grammars degrade
/// gracefully to body text.
fn color_for(stack: &[syntect::parsing::Scope]) -> ratatui::style::Color {
    fn sel(name: &str) -> syntect::parsing::Scope {
        syntect::parsing::Scope::new(name).expect("builtin scope selector parses")
    }
    let (comment, string, constant) = (sel("comment"), sel("string"), sel("constant"));
    let (keyword, storage) = (sel("keyword"), sel("storage"));
    let names = [
        sel("entity.name.function"),
        sel("entity.name.macro"),
        sel("support.function"),
        sel("variable.function"),
    ];
    let types = [
        sel("entity.name.type"),
        sel("entity.name.class"),
        sel("entity.name.struct"),
        sel("entity.name.enum"),
        sel("support.class"),
        sel("support.type"),
        sel("meta.annotation"),
        sel("entity.other.attribute-name"),
    ];
    let invalid = sel("invalid");
    for scope in stack.iter().rev() {
        if invalid.is_prefix_of(*scope) {
            return code::INVALID;
        }
        if comment.is_prefix_of(*scope) {
            return code::COMMENT;
        }
        if string.is_prefix_of(*scope) {
            return code::STRING;
        }
        if constant.is_prefix_of(*scope) {
            return code::CONSTANT;
        }
        if keyword.is_prefix_of(*scope) || storage.is_prefix_of(*scope) {
            return code::KEYWORD;
        }
        if names.iter().any(|n| n.is_prefix_of(*scope)) {
            return code::NAME;
        }
        if types.iter().any(|t| t.is_prefix_of(*scope)) {
            return code::TYPE;
        }
    }
    code::FG
}

/// Token roles for every line, parsed as one document so multi-line
/// strings and comments carry across rows. A failed line (or an
/// unknown extension) renders plain; ranges clamp to the line so a
/// grammar can never panic the overlay on odd bytes.
fn highlight_lines(file_path: &str, lines: &[String]) -> Vec<Vec<HLSpan>> {
    use syntect::parsing::{ParseState, ScopeStack};
    let set = syntax_set();
    let ext = file_path.rsplit('.').next().unwrap_or("");
    let Some(syntax) = set.find_syntax_by_extension(ext) else {
        return Vec::new();
    };
    let mut state = ParseState::new(syntax);
    lines
        .iter()
        .map(|line| {
            let probe = format!("{line}\n");
            let ops = match state.parse_line(&probe, set) {
                Ok(ops) => ops,
                Err(_) => return Vec::new(),
            };
            let mut stack = ScopeStack::new();
            let mut spans = Vec::new();
            let mut prev = 0usize;
            let mut flush = |upto: usize, stack: &ScopeStack, spans: &mut Vec<HLSpan>| {
                let end = upto.min(line.len());
                if end > prev && line.is_char_boundary(prev) && line.is_char_boundary(end) {
                    let color = color_for(stack.as_slice());
                    // Adjacent plain runs merge; anything else (or a
                    // leading plain run) starts its own span.
                    match spans.last_mut() {
                        Some(last) if last.color == code::FG && color == code::FG => {
                            last.end = end;
                        }
                        _ => spans.push(HLSpan { color, start: prev, end }),
                    }
                }
                prev = end;
            };
            for (idx, op) in ops {
                flush(idx, &stack, &mut spans);
                let _ = stack.apply(&op);
            }
            flush(line.len(), &stack, &mut spans);
            spans
        })
        .collect()
}

/// Forge markdown skin: every color comes from a theme role so
/// answers stay readable on light OS themes too (the default sheet
/// hardcodes dark-theme colors). Alert icons are ASCII: several
/// defaults carry variation selectors with ambiguous widths.
#[derive(Clone, Copy, Default)]
pub struct ForgeSheet;

impl tui_markdown::StyleSheet for ForgeSheet {

    fn heading(&self, level: u8) -> ratatui::style::Style {
        let mut s = style(Role::Brand).add_modifier(ratatui::style::Modifier::BOLD);
        if level == 1 {
            s = s.add_modifier(ratatui::style::Modifier::UNDERLINED);
        }
        s
    }

    fn code(&self) -> ratatui::style::Style {
        style(Role::Command)
    }

    fn link(&self) -> ratatui::style::Style {
        style(Role::Info).add_modifier(ratatui::style::Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(ratatui::style::Modifier::ITALIC)
    }

    fn heading_meta(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn metadata_block(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn html(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn math_inline(&self) -> ratatui::style::Style {
        style(Role::Info).add_modifier(ratatui::style::Modifier::ITALIC)
    }

    fn math_display(&self) -> ratatui::style::Style {
        style(Role::Info)
    }

    fn footnote_ref(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(ratatui::style::Modifier::ITALIC)
    }

    fn footnote_def(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn definition_term(&self) -> ratatui::style::Style {
        style(Role::Text).add_modifier(ratatui::style::Modifier::BOLD)
    }

    fn alert(&self, kind: tui_markdown::AlertKind) -> ratatui::style::Style {
        use tui_markdown::AlertKind;
        match kind {
            AlertKind::Note => style(Role::Info),
            AlertKind::Tip => style(Role::Success),
            AlertKind::Important => style(Role::Brand),
            AlertKind::Warning => style(Role::Warning),
            AlertKind::Caution => style(Role::Danger),
        }
    }

    fn alert_icon(&self, kind: tui_markdown::AlertKind) -> &str {
        use tui_markdown::AlertKind;
        match kind {
            AlertKind::Note => "i",
            AlertKind::Tip => "+",
            AlertKind::Important => "!",
            AlertKind::Warning => "!",
            AlertKind::Caution => "x",
        }
    }

    fn table_header(&self) -> ratatui::style::Style {
        style(Role::Brand).add_modifier(ratatui::style::Modifier::BOLD)
    }

    fn table_border(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn image_alt(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(ratatui::style::Modifier::ITALIC)
    }
}

fn md_opts() -> tui_markdown::Options<ForgeSheet> {
    tui_markdown::Options::new(ForgeSheet)
}

/// Theme-skinned Markdown for agent text, shared with the visual
/// chat footer so answers render identically in both places.
pub(crate) fn md_text(source: &str) -> ratatui::text::Text<'_> {
    tui_markdown::from_str_with_options(source, &md_opts())
}

impl Walkthrough {
    /// Render the tour takeover: header, code window, explanation with
    /// an optional Q&A half, key bar. Opaque (Clear first) like every
    /// modal surface; untrusted file/agent text passes display
    /// encoding, Markdown through the theme-skinned renderer.
    pub fn view(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::layout::Rect;
        use ratatui::style::Modifier;
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
        use crate::safe_text::encode_for_display;
        use crate::theme::{focus_row, style, Role};
        frame.render_widget(Clear, area);
        let (x, y, w, h) = (area.x, area.y, area.width, area.height);
        if w < 10 || h < 2 {
            return;
        }
        let mut header = vec![
            Span::styled("📖 ", style(Role::Brand)),
            Span::styled(encode_for_display(&self.title), style(Role::Text)),
            Span::styled(
                format!("   Step {} of {}", self.index + 1, self.steps.len()),
                style(Role::Muted),
            ),
        ];
        if self.completed {
            header.push(Span::styled("   ✓ Complete", style(Role::Success)));
        }
        frame.render_widget(Paragraph::new(Line::from(header)), Rect::new(x, y, w, 1));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                encode_for_display(&self.file_path),
                style(Role::Muted),
            ))),
            Rect::new(x, y + 1, w, 1),
        );
        // Key bar always survives: Esc must stay reachable on tiny terms.
        self.render_keybar(frame, Rect::new(x, y + h - 1, w, 1));
        if h < 6 {
            return;
        }
        let body_end = y + h - 1;
        let mut cy = y + 2;
        let mut code_h = body_end.saturating_sub(cy);
        let mut lower_h = 0u16;
        if code_h >= 6 {
            code_h = (code_h * 55 / 100).clamp(4, code_h.saturating_sub(2).max(4));
            lower_h = body_end.saturating_sub(cy + code_h);
        }
        self.render_code(frame, Rect::new(x, cy, w, code_h));
        cy += code_h;
        if lower_h >= 2 {
            let rule: String = "─".repeat(w as usize);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(rule, style(Role::Muted)))),
                Rect::new(x, cy, w, 1),
            );
            cy += 1;
            self.render_lower(frame, Rect::new(x, cy, w, lower_h.saturating_sub(1)));
        }
    }

    fn render_code(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;
        use crate::safe_text::encode_for_display;
        use crate::theme::{style, Role};
        if area.height == 0 {
            return;
        }
        let num_w = self.lines.len().to_string().len().max(2);
        let rows = self.code_window(area.height as usize);
        // Step rows sit on the static wash so the bar and tokens share
        // one surface; every span carries it or the tint gets holes.
        // All code colors are static (see `code`), never theme roles.
        use ratatui::style::Style;
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|(no, text, in_range)| {
                let tint = if in_range { Some(code::WASH) } else { None };
                let mut base = Style::default().fg(code::FG);
                base.bg = tint;
                let mut gutter = Style::default().fg(code::GUTTER);
                gutter.bg = tint;
                let mut num = Style::default().fg(code::COMMENT);
                num.bg = tint;
                let mut row = vec![
                    Span::styled(if in_range { "▌" } else { " " }, gutter),
                    Span::styled(format!("{no:>num_w$} "), num),
                ];
                let idx = (no as usize).saturating_sub(1);
                match self.hl.get(idx) {
                    Some(tokens) if !tokens.is_empty() => {
                        for tok in tokens {
                            let Some(slice) = text.get(tok.start..tok.end) else {
                                continue;
                            };
                            let mut style = Style::default().fg(tok.color);
                            style.bg = tint;
                            row.push(Span::styled(encode_for_display(slice), style));
                        }
                    }
                    _ => row.push(Span::styled(encode_for_display(text), base)),
                }
                Line::from(row)
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_lower(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
        use crate::theme::{style, Role};
        if area.height == 0 {
            return;
        }
        let mut explanation = self.current_step().explanation.clone();
        if let Some(summary) = self.summary.as_deref() {
            explanation.push_str("\n\n--- Summary ---\n");
            explanation.push_str(summary);
        }
        if self.questions.is_empty() {
            frame.render_widget(
                Paragraph::new(md_text(&explanation)).wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let left_w = (area.width / 2).max(8);
        let right_w = area.width.saturating_sub(left_w).max(8);
        frame.render_widget(
            Paragraph::new(md_text(&explanation)).wrap(Wrap { trim: true }),
            ratatui::layout::Rect::new(area.x, area.y, left_w, area.height),
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(style(Role::BorderModal))
            .title(Line::from(Span::styled(" Q&A ", style(Role::Muted))));
        let inner = block.inner(ratatui::layout::Rect::new(
            area.x + left_w,
            area.y,
            right_w,
            area.height,
        ));
        frame.render_widget(
            block,
            ratatui::layout::Rect::new(area.x + left_w, area.y, right_w, area.height),
        );
        frame.render_widget(
            Paragraph::new(self.qa_text(inner.height as usize)).wrap(Wrap { trim: true }),
            inner,
        );
    }

    /// One entry as lines: the `Q:` row plus the Markdown answer or
    /// the waiting placeholder.
    fn entry_lines(q: &Question) -> Vec<ratatui::text::Line<'_>> {
        use ratatui::style::Modifier;
        use ratatui::text::{Line, Span};
        use crate::theme::{style, Role};
        let mut lines = vec![Line::from(vec![
            Span::styled("Q: ", style(Role::Brand).add_modifier(Modifier::BOLD)),
            Span::styled(q.question.clone(), style(Role::Text)),
        ])];
        match &q.answer {
            Some(a) => lines.extend(md_text(a)),
            None => lines.push(Line::from(Span::styled(
                WAITING_TEXT.to_string(),
                style(Role::Muted),
            ))),
        }
        lines
    }

    /// Q&A log as one text, tail-kept to the panel height at whole-entry
    /// granularity — a clipped answer could hide the verdict. The newest
    /// entry is always kept (the panel clips around it); older entries
    /// shed oldest-first behind a `... N earlier` note.
    fn qa_text(&self, height: usize) -> ratatui::text::Text<'_> {
        use ratatui::text::{Line, Span, Text};
        use crate::theme::{style, Role};
        if height == 0 || self.questions.is_empty() {
            return Text::default();
        }
        let blocks: Vec<Vec<Line>> = self.questions.iter().map(Self::entry_lines).collect();
        let mut start = blocks.len();
        let mut used = 0usize;
        for (i, block) in blocks.iter().enumerate().rev() {
            let cost = block.len() + usize::from(start != blocks.len());
            if used + cost > height && start != blocks.len() {
                break;
            }
            used += cost;
            start = i;
        }
        let mut dropped = start;
        let mut lines: Vec<Line> = Vec::new();
        for (n, block) in blocks[start..].iter().enumerate() {
            if n > 0 {
                lines.push(Line::from(""));
            }
            lines.extend(block.iter().cloned());
        }
        while dropped > 0 && lines.len() + 1 > height && start + 1 < blocks.len() {
            let shed = blocks[start].len() + 1;
            lines.drain(..shed.min(lines.len()));
            start += 1;
            dropped += 1;
        }
        if dropped > 0 && lines.len() < height {
            lines.insert(
                0,
                Line::from(Span::styled(
                    format!("... {dropped} earlier"),
                    style(Role::Muted),
                )),
            );
        }
        Text::from(lines)
    }

    fn render_keybar(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::style::Modifier;
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;
        use crate::theme::{style, Role};
        if let Some(status) = self.status.as_deref() {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("! ", style(Role::Danger)),
                    Span::styled(status.to_string(), style(Role::Danger)),
                ])),
                area,
            );
            return;
        }
        if let Some(buf) = self.input.as_deref() {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("> ", style(Role::KeyHint)),
                    Span::styled(buf.to_string(), style(Role::Text)),
                    Span::styled(
                        " ",
                        ratatui::style::Style::default().add_modifier(Modifier::REVERSED),
                    ),
                ])),
                area,
            );
            return;
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", style(Role::KeyHint)),
                Span::styled(" ask   ", style(Role::KeyDesc)),
                Span::styled("j/k/<>", style(Role::KeyHint)),
                Span::styled(" steps   ", style(Role::KeyDesc)),
                Span::styled("g/G", style(Role::KeyHint)),
                Span::styled(" scroll   ", style(Role::KeyDesc)),
                Span::styled("Esc", style(Role::KeyHint)),
                Span::styled(" back", style(Role::KeyDesc)),
            ])),
            area,
        );
    }
}

/// Main-area rect for the tour: the pane grid below the topbar strip,
/// so the session tabs stay visible and clickable above it. Wide
/// layouts inset the strip into the grid's first row, which is cut
/// here (the grid itself keeps that row for PTY sizing).
pub fn walk_area(term: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let areas = crate::ui::chrome_areas(term);
    let mut grid = crate::ui::pane_grid_area(&areas);
    if areas.topbar.height > 0
        && areas.topbar.y >= grid.y
        && areas.topbar.y < grid.y.saturating_add(grid.height)
    {
        let cut = areas.topbar.y + areas.topbar.height - grid.y;
        grid.y += cut;
        grid.height = grid.height.saturating_sub(cut);
    }
    grid
}

/// Outcome of one key for the TUI loop to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkKey {
    /// Step index or scroll moved; repaint.
    Moved,
    /// Draft or status changed; repaint.
    Edited,
    /// A draft is ready: the caller injects it, then records it.
    Submitted,
    /// Input cancelled; repaint.
    CancelledInput,
    /// Browse-mode Esc: leave the overlay.
    Closed,
    /// Swallowed (overlay captures all input while open).
    Ignored,
}




#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> String {
        (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")
    }

    fn two_steps() -> Vec<Step> {
        vec![
            Step { start: 5, end: 9, explanation: "first".to_string() },
            Step { start: 20, end: 25, explanation: "second".to_string() },
        ]
    }

    fn open() -> Walkthrough {
        Walkthrough::start("Tour".to_string(), "f.rs".to_string(), &content(), two_steps())
            .expect("valid fixture")
    }

    #[test]
    fn start_rejects_empty_steps_and_empty_files() {
        assert!(Walkthrough::start("t".into(), "f".into(), &content(), vec![]).is_err());
        assert!(Walkthrough::start("t".into(), "f".into(), "", two_steps()).is_err());
    }

    #[test]
    fn parse_steps_accepts_colons_in_explanations() {
        let steps = Walkthrough::parse_steps("1:3:uses a:b literals\n5:5:done", 10).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].explanation, "uses a:b literals");
    }

    #[test]
    fn parse_steps_joins_continuations_and_blank_lines() {
        let steps = Walkthrough::parse_steps(
            "1:3:First line.\n Second line.\n\n - bullet\n   - nested\n5:5:Next step.",
            10,
        )
        .unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].explanation,
            "First line.\nSecond line.\n\n- bullet\n  - nested"
        );
        assert_eq!(steps[1].explanation, "Next step.");
    }

    #[test]
    fn parse_steps_rejects_orphan_continuations_and_empty_ends() {
        assert!(Walkthrough::parse_steps("  orphan", 10).is_err());
        assert!(Walkthrough::parse_steps("1:3:ok\n  \n\n", 10).is_ok());
        let steps = Walkthrough::parse_steps("1:3:ok\n  \n\n", 10).unwrap();
        assert_eq!(steps[0].explanation, "ok", "trailing breaks trimmed");
        assert!(Walkthrough::parse_steps("1:3:\n  carried", 10).is_ok());
    }

    #[test]
    fn parse_steps_rejects_bad_ranges() {
        assert!(Walkthrough::parse_steps("0:3:x", 10).is_err());
        assert!(Walkthrough::parse_steps("5:3:x", 10).is_err());
        assert!(Walkthrough::parse_steps("1:99:x", 10).is_err());
        assert!(Walkthrough::parse_steps("1:3:", 10).is_err());
        assert!(Walkthrough::parse_steps("nope", 10).is_err());
        assert!(Walkthrough::parse_steps("\n  \n", 10).is_err());
    }

    #[test]
    fn nav_clamps_and_resets_scroll() {
        let mut wt = open();
        wt.code_scroll = 7;
        wt.step_next();
        assert_eq!(wt.index, 1);
        assert_eq!(wt.code_scroll, 0);
        wt.step_next();
        assert_eq!(wt.index, 1, "clamps at the last step");
        wt.step_prev();
        assert_eq!(wt.index, 0);
        wt.step_prev();
        assert_eq!(wt.index, 0, "clamps at the first step");
    }

    #[test]
    fn add_step_before_current_shifts_index() {
        let mut wt = open();
        wt.step_next();
        wt.add_step(Step { start: 1, end: 2, explanation: "pre".to_string() }, Some(0))
            .unwrap();
        assert_eq!(wt.index, 2);
        assert_eq!(wt.steps[0].explanation, "pre");
        assert!(wt
            .add_step(Step { start: 99, end: 1, explanation: "bad".to_string() }, None)
            .is_err());
    }

    #[test]
    fn update_step_rejects_unknown_index_and_bad_range() {
        let mut wt = open();
        assert!(wt.update_step(7, None, None, None).is_err());
        assert!(wt.update_step(0, Some(30), Some(45), None).is_err());
        wt.update_step(0, None, None, Some("new words")).unwrap();
        assert_eq!(wt.steps[0].explanation, "new words");
    }

    #[test]
    fn answer_resolves_latest_pending_only() {
        let mut wt = open();
        assert!(!wt.pending_question());
        assert!(!wt.answer_latest("thin air"));
        wt.push_question("first?".to_string());
        wt.push_question("second?".to_string());
        assert!(wt.pending_question());
        assert!(wt.answer_latest("two"));
        assert_eq!(wt.questions[1].answer.as_deref(), Some("two"));
        assert!(wt.pending_question(), "first still waits");
        assert!(wt.answer_latest("one"));
        assert!(!wt.pending_question());
    }

    #[test]
    fn draft_trims_and_ignores_blanks() {
        let mut wt = open();
        wt.input = Some("   ".to_string());
        assert_eq!(wt.take_draft(), None);
        assert!(wt.input.is_some(), "blank draft stays in input mode");
        wt.input = Some("  why?  ".to_string());
        assert_eq!(wt.take_draft().as_deref(), Some("why?"));
        assert!(wt.input.is_none(), "submitted draft leaves input mode");
    }

    #[test]
    fn input_cap_drops_overflow() {
        let mut wt = open();
        wt.input = Some("x".repeat(MAX_INPUT_CHARS));
        assert!(!wt.push_input('y'));
        assert_eq!(wt.input.as_ref().unwrap().chars().count(), MAX_INPUT_CHARS);
    }

    #[test]
    fn question_markup_carries_step_context_and_tag() {
        let wt = open();
        let body = Walkthrough::question_markup(&wt.title, 0, 2, &wt.steps[0], "why?");
        assert!(body.contains("<walkthrough-question>"), "body: {body}");
        assert!(body.contains("step 1/2 (lines 5-9)"), "body: {body}");
        assert!(body.contains("why?"), "body: {body}");
    }

    #[test]
    fn code_window_anchors_step_with_context_and_range_flags() {
        let wt = open();
        let rows = wt.code_window(10);
        assert_eq!(rows.len(), 10);
        // Step 5-9 opens below 3 context lines: first row is line 2.
        assert_eq!(rows[0].0, 2);
        assert!(!rows[0].2);
        assert!(rows[3].2 && rows[3].0 == 5, "range starts flagged: {rows:?}");
        assert!(rows[7].2 && rows[7].0 == 9, "range ends flagged: {rows:?}");
        assert!(!rows[9].2);
    }

    #[test]
    fn scroll_code_clamps_to_content() {
        let mut wt = open();
        wt.scroll_code(-10_000);
        let rows = wt.code_window(10);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[9].0, 40, "sticks at the file bottom: {rows:?}");
        wt.scroll_code(10_000);
        let rows = wt.code_window(10);
        assert_eq!(rows[0].0, 2, "climbs back to the anchor: {rows:?}");
    }

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn keys_drive_steps_scroll_and_input_modes() {
        let mut wt = open();
        assert_eq!(wt.key(&ch('j')), WalkKey::Moved);
        assert_eq!(wt.index, 1);
        assert_eq!(wt.key(&ch('k')), WalkKey::Moved);
        assert_eq!(wt.index, 0);
        assert_eq!(wt.key(&press(KeyCode::Right)), WalkKey::Moved);
        assert_eq!(wt.index, 1);
        assert_eq!(wt.key(&press(KeyCode::Left)), WalkKey::Moved);
        assert_eq!(wt.index, 0);
        wt.code_scroll = 3;
        assert_eq!(wt.key(&ch('g')), WalkKey::Moved);
        assert_eq!(wt.code_scroll, 2, "g climbs one line");
        let g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(wt.key(&g), WalkKey::Moved);
        assert_eq!(wt.code_scroll, 3, "G descends one line");
        assert_eq!(wt.key(&ch('z')), WalkKey::Ignored);
        assert_eq!(wt.key(&press(KeyCode::Esc)), WalkKey::Closed);
    }

    #[test]
    fn input_mode_types_backspaces_submits_and_cancels() {
        let mut wt = open();
        assert_eq!(wt.key(&press(KeyCode::Enter)), WalkKey::Edited);
        assert_eq!(wt.input.as_deref(), Some(""));
        assert_eq!(wt.key(&ch('h')), WalkKey::Edited);
        assert_eq!(wt.key(&ch('i')), WalkKey::Edited);
        assert_eq!(wt.key(&press(KeyCode::Backspace)), WalkKey::Edited);
        assert_eq!(wt.input.as_deref(), Some("h"));
        // Blank draft stays; the caller takes non-blank drafts itself.
        wt.input = Some("  ".to_string());
        assert_eq!(wt.key(&press(KeyCode::Enter)), WalkKey::Edited);
        assert!(wt.input.is_some());
        assert_eq!(wt.status.as_deref(), Some("type a question first"));
        wt.input = Some("why?".to_string());
        assert_eq!(wt.key(&press(KeyCode::Enter)), WalkKey::Submitted);
        assert_eq!(wt.take_draft().as_deref(), Some("why?"));
        assert_eq!(wt.key(&press(KeyCode::Enter)), WalkKey::Edited);
        assert_eq!(wt.key(&press(KeyCode::Esc)), WalkKey::CancelledInput);
        assert!(wt.input.is_none());
    }

    #[test]
    fn paste_sanitizes_controls_and_caps() {
        let mut wt = open();
        assert!(!wt.push_paste("nope"), "no input mode, no change");
        wt.input = Some(String::new());
        assert!(wt.push_paste("a\x1bb\nc\td"));
        assert_eq!(wt.input.as_deref(), Some("ab c d"));
        wt.input = Some("x".repeat(MAX_INPUT_CHARS));
        wt.push_paste("yz");
        assert_eq!(wt.input.as_ref().unwrap().chars().count(), MAX_INPUT_CHARS);
    }

    fn render_text(wt: &Walkthrough, w: u16, h: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| wt.view(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn rich() -> Walkthrough {
        let steps = vec![
            Step { start: 2, end: 3, explanation: "# Why\nSome **reason**.".to_string() },
            Step { start: 8, end: 9, explanation: "More.".to_string() },
        ];
        let content = (1..=30).map(|i| format!("code line {i}")).collect::<Vec<_>>().join("\n");
        Walkthrough::start("Tour".to_string(), "src/f.rs".to_string(), &content, steps)
            .expect("valid fixture")
    }

    #[test]
    fn view_shows_header_code_explanation_and_keybar() {
        let text = render_text(&rich(), 100, 30);
        assert!(text.contains("Tour"), "title: {text:?}");
        assert!(text.contains("Step 1 of 2"), "counter: {text:?}");
        assert!(text.contains("src/f.rs"), "path: {text:?}");
        assert!(text.contains("code line 2"), "step code: {text:?}");
        assert!(text.contains("▌"), "gutter bar on step rows: {text:?}");
        assert!(text.contains("Why"), "markdown explanation: {text:?}");
        assert!(text.contains("reason"), "markdown body: {text:?}");
        for hint in ["Enter ask", "steps", "scroll", "back"] {
            assert!(text.contains(hint), "key bar {hint:?}: {text:?}");
        }
    }

    fn render_cells(wt: &Walkthrough, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| wt.view(f, f.area())).unwrap();
        terminal.backend().buffer().clone()
    }

    fn rust_tour() -> Walkthrough {
        let content = "fn main() {\n    let x = \"hi\"; // note\n}\n";
        Walkthrough::start(
            "Tour".to_string(),
            "main.rs".to_string(),
            content,
            vec![Step { start: 1, end: 3, explanation: "e".to_string() }],
        )
        .expect("valid fixture")
    }

    #[test]
    fn rust_tokens_take_static_palette() {
        let wt = rust_tour();
        // Code rows start below the two header rows; the gutter plus a
        // two-wide number field precede the source text.
        let buf = render_cells(&wt, 60, 20);
        let fg = |x: u16, y: u16| buf[(x, y)].fg;
        assert_eq!(fg(4, 2), code::KEYWORD, "fn keyword");
        assert_eq!(fg(8, 3), code::KEYWORD, "let keyword");
        assert_eq!(fg(16, 3), code::STRING, "string");
        assert_eq!(fg(22, 3), code::COMMENT, "comment");
        // Step rows sit on the static wash, never the old cyan theme
        // tint; out-of-step rows stay transparent.
        assert_eq!(buf[(4, 2)].bg, code::WASH, "wash behind step code");
        assert_eq!(buf[(4, 5)].bg, ratatui::style::Color::Reset, "no wash off-step");
    }

    #[test]
    fn gutter_bar_marks_only_step_rows() {
        let wt = rich();
        // Step 1 covers lines 2-3; the code window opens at line 1.
        let buf = render_cells(&wt, 100, 30);
        assert_eq!(buf[(0, 2)].symbol(), " ");
        assert_eq!(buf[(0, 3)].symbol(), "▌");
        assert_eq!(buf[(0, 4)].symbol(), "▌");
        assert_eq!(buf[(0, 5)].symbol(), " ");
        assert_eq!(buf[(0, 3)].fg, code::GUTTER, "static bar color");
        assert_eq!(buf[(0, 3)].bg, code::WASH, "bar sits on the wash");
    }

    #[test]
    fn unknown_extension_stays_plain() {
        let content = (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let wt = Walkthrough::start(
            "Tour".to_string(),
            "data.foobarxyz".to_string(),
            &content,
            vec![Step { start: 1, end: 2, explanation: "e".to_string() }],
        )
        .expect("valid fixture");
        assert!(wt.hl.is_empty(), "no grammar, no tokens");
        let text = render_text(&wt, 60, 20);
        assert!(text.contains("line 1"), "plain render: {text:?}");
    }

    #[test]
    fn highlight_covers_every_line_contiguously() {
        let content = (0..3000)
            .map(|i| format!("fn f{i}() {{ let x = {i}; }} // tail"))
            .collect::<Vec<_>>()
            .join("\n");
        let wt = Walkthrough::start(
            "Tour".to_string(),
            "big.rs".to_string(),
            &content,
            vec![Step { start: 1, end: 10, explanation: "e".to_string() }],
        )
        .expect("valid fixture");
        assert_eq!(wt.hl.len(), 3000);
        for (line, spans) in content.lines().zip(wt.hl.iter()) {
            assert!(!spans.is_empty(), "line renders: {line:?}");
            assert_eq!(spans[0].start, 0, "head: {line:?}");
            assert_eq!(spans.last().unwrap().end, line.len(), "tail: {line:?}");
            for w in spans.windows(2) {
                assert_eq!(w[0].end, w[1].start, "gap: {line:?}");
            }
        }
    }

    #[test]
    fn walk_area_keeps_topbar_visible() {
        use ratatui::layout::Rect;
        for (w, h) in [(120u16, 30u16), (200u16, 50u16)] {
            let term = Rect::new(0, 0, w, h);
            let areas = crate::ui::chrome_areas(term);
            let walk = walk_area(term);
            assert!(
                walk.y >= areas.topbar.y + areas.topbar.height,
                "{w}x{h}: tour starts below the strip: {walk:?}"
            );
            assert_eq!((walk.x, walk.width), (areas.main.x, areas.main.width));
            assert!(walk.height > 0, "{w}x{h}: tour keeps body rows");
        }
    }

    #[test]
    fn view_splits_qa_with_waiting_then_answer() {
        let mut wt = rich();
        wt.push_question("why two?".to_string());
        let text = render_text(&wt, 100, 30);
        assert!(text.contains("Q&A"), "panel border title: {text:?}");
        assert!(text.contains("why two?"), "question: {text:?}");
        assert!(text.contains(WAITING_TEXT), "placeholder: {text:?}");
        assert!(text.contains("Enter ask"), "key bar stays in browse mode: {text:?}");
        wt.answer_latest("Because **two**.");
        let text = render_text(&wt, 100, 30);
        assert!(!text.contains(WAITING_TEXT), "placeholder replaced: {text:?}");
        assert!(text.contains("two"), "answer: {text:?}");
    }

    #[test]
    fn view_input_mode_shows_prompt_and_complete_badge() {
        let mut wt = rich();
        wt.input = Some("wh".to_string());
        let text = render_text(&wt, 100, 30);
        assert!(text.contains("> wh"), "prompt draft: {text:?}");
        assert!(!text.contains("Enter ask"), "help cleared while typing: {text:?}");
        wt.input = None;
        wt.end(Some("done".to_string()));
        let text = render_text(&wt, 100, 30);
        assert!(text.contains("Complete"), "badge: {text:?}");
        assert!(text.contains("Summary"), "summary appended: {text:?}");
    }

    #[test]
    fn stylesheet_uses_no_inline_rgb() {
        use ratatui::style::Color;
        let sample = "# H\n`code` [l](http://x) > quote\n| a | b |\n|---|---|\n| 1 | 2 |\n> [!NOTE]\n> note body\n";
        let mut found = 0;
        for line in md_text(sample) {
            for span in &line.spans {
                found += 1;
                for color in [span.style.fg, span.style.bg].into_iter().flatten() {
                    assert!(!matches!(color, Color::Rgb(..)), "span {span:?}");
                }
            }
        }
        assert!(found > 10, "sample must exercise the sheet");
    }

    #[test]
    fn qa_text_keeps_newest_behind_earlier_note() {
        let mut wt = rich();
        for i in 0..5 {
            wt.push_question(format!("q{i}?"));
            wt.answer_latest("yes");
        }
        let text = wt.qa_text(6);
        let flat: String = text.lines.iter().map(|l| l.spans.iter().map(|s| s.content.clone()).collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(flat.contains("q4?"), "newest kept: {flat:?}");
        assert!(flat.contains("earlier"), "note shown: {flat:?}");
        assert!(!flat.contains("q0?"), "oldest shed: {flat:?}");
    }

    #[test]
    fn end_marks_complete_and_keeps_view() {
        let mut wt = open();
        wt.end(Some("toured".to_string()));
        assert!(wt.completed);
        assert_eq!(wt.summary.as_deref(), Some("toured"));
        assert_eq!(wt.step_count(), 2, "steps stay for the Complete view");
        wt.end(None);
        assert!(wt.summary.is_none(), "blank summary stays empty");
    }
}
