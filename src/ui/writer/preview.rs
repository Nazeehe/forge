//! Writer preview: read-only rendered Markdown (E8) with a Forge
//! stylesheet. Every tui-markdown style maps to a semantic role,
//! so preview follows the terminal palette in light and dark
//! setups; heading markers are omitted (headings read as headings,
//! not source).

use ratatui::style::Modifier;
use ratatui::text::Text;

use crate::ui::theme::{Role, style};

/// tui-markdown stylesheet in Forge roles. Unit struct: the trait
/// needs `Clone + Send + Sync + 'static`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ForgeStyleSheet;

impl tui_markdown::StyleSheet for ForgeStyleSheet {
    fn heading(&self, level: u8) -> ratatui::style::Style {
        if level <= 1 {
            style(Role::MdHeading).add_modifier(Modifier::UNDERLINED)
        } else {
            style(Role::MdHeading)
        }
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> ratatui::style::Style {
        style(Role::MdCode)
    }

    fn link(&self) -> ratatui::style::Style {
        style(Role::MdLink)
    }

    fn blockquote(&self) -> ratatui::style::Style {
        style(Role::Muted)
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
        style(Role::MdCode).add_modifier(Modifier::ITALIC)
    }

    fn math_display(&self) -> ratatui::style::Style {
        style(Role::MdCode)
    }

    fn footnote_ref(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(Modifier::ITALIC)
    }

    fn footnote_def(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn definition_term(&self) -> ratatui::style::Style {
        style(Role::Text).add_modifier(Modifier::BOLD)
    }

    fn alert(&self, kind: tui_markdown::AlertKind) -> ratatui::style::Style {
        use tui_markdown::AlertKind as K;
        match kind {
            K::Note => style(Role::Info),
            K::Tip => style(Role::Success),
            K::Important => style(Role::Brand),
            K::Warning => style(Role::Warning),
            K::Caution => style(Role::Danger),
        }
    }

    fn table_header(&self) -> ratatui::style::Style {
        style(Role::Text).add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn image_alt(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(Modifier::ITALIC)
    }
}

/// Render `text` to wrapped-ready lines in Forge roles.
pub fn preview_text(text: &str) -> Text<'_> {
    let options = tui_markdown::Options::new(ForgeStyleSheet);
    tui_markdown::from_str_with_options(text, &options)
}

/// Visual row count of the rendered preview at `width` cells:
/// each line wraps greedily, empty lines take one row. Scroll
/// clamps to this, so the tail stays reachable.
pub fn preview_height(text: &str, width: usize) -> usize {
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let rendered = preview_text(text);
    rendered
        .lines
        .iter()
        .map(|line| {
            let cells: usize = line.spans.iter().map(|span| span.content.width()).sum();
            cells.div_ceil(width).max(1)
        })
        .sum::<usize>()
        .max(1)
}
