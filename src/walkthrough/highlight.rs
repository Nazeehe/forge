//! Walkthrough highlighting: syntect code colors and the Markdown skin.
//!
//! Sublime grammars map scopes onto the hand-picked `code` palette
//! (OneDark hues; the OS theme is ignored on purpose so tours always
//! contrast). Agent Markdown renders through the theme-skinned
//! `ForgeSheet`, shared with the visual chat footer.

use super::HLSpan;

use crate::theme::{style, Role};

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
pub(super) fn highlight_lines(file_path: &str, lines: &[String]) -> Vec<Vec<HLSpan>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Step, Walkthrough};

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
}
