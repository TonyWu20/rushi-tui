//! Markdown rendering backed by
//! [`ratatui-markdown`](https://github.com/celestia-island/ratatui-markdown).
//!
//! Replaces the hand-rolled per-line markdown pass in
//! [`crate::highlight`] for transcript message content
//! (docs/NEW-refactor.md, recommended core library #1).
//!
//! The active [`Palette`] is mapped to a [`MdTheme`] (a
//! [`RichTextTheme`] impl) so that every palette role (headings,
//! code, links, JSON tokens, …) flows through the same color
//! pipeline the rest of the TUI uses.

use ratatui::style::Style;
use ratatui::text::Line;
use ratatui_markdown::markdown::MarkdownRenderer;
use ratatui_markdown::theme::{CodeColors, Generation, RichTextTheme};

use crate::color::{Palette, Role};

/// A [`RichTextTheme`] backed by the TUI's active [`Palette`].
///
/// Maps each palette role to the corresponding ratatui-markdown
/// theme slot:
///
/// | Theme slot               | Palette role         |
/// |--------------------------|----------------------|
/// | text (paragraph)         | `PlainText`          |
/// | muted text (fences, …)   | `Hint`               |
/// | primary (H1 headings)    | `Heading`            |
/// | border / fence marker    | `Fence`              |
/// | secondary (H3, quotes)   | `Quote`              |
/// | info (links)             | `Link`               |
/// | accent-yellow (inline code) | `Code`             |
/// | JSON / code tokens       | `Syntax*` roles      |
#[derive(Debug, Clone, Copy)]
pub struct MdTheme<'a> {
    palette: &'a Palette,
}

impl<'a> MdTheme<'a> {
    pub fn new(palette: &'a Palette) -> Self {
        Self { palette }
    }
}

impl RichTextTheme for MdTheme<'_> {
    fn generation(&self) -> Generation {
        Generation(0)
    }
    fn get_text_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::PlainText)
    }
    fn get_muted_text_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Hint)
    }
    fn get_primary_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Heading)
    }
    fn get_popup_selected_background(&self) -> ratatui::style::Color {
        self.palette.color(Role::Selection)
    }
    fn get_border_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Fence)
    }
    fn get_focused_border_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Accent)
    }
    fn get_secondary_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Quote)
    }
    fn get_info_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Link)
    }
    fn get_json_key_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::SyntaxVariable)
    }
    fn get_json_string_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::SyntaxString)
    }
    fn get_json_number_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::SyntaxNumber)
    }
    fn get_json_bool_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::SyntaxNumber)
    }
    fn get_json_null_color(&self) -> ratatui::style::Color {
        self.palette.color(Role::Hint)
    }
    fn get_accent_yellow(&self) -> ratatui::style::Color {
        self.palette.color(Role::Code)
    }
    fn get_code_colors(&self) -> CodeColors {
        CodeColors::builder()
            .comment(self.palette.color(Role::SyntaxComment))
            .keyword(self.palette.color(Role::SyntaxKeyword))
            .string(self.palette.color(Role::SyntaxString))
            .string_escape(self.palette.color(Role::SyntaxString))
            .number(self.palette.color(Role::SyntaxNumber))
            .constant(self.palette.color(Role::SyntaxNumber))
            .function(self.palette.color(Role::SyntaxFunction))
            .r#type(self.palette.color(Role::SyntaxType))
            .variable(self.palette.color(Role::SyntaxVariable))
            .property(self.palette.color(Role::SyntaxVariable))
            .operator(self.palette.color(Role::SyntaxOperator))
            .punctuation(self.palette.color(Role::SyntaxPunctuation))
            .attribute(self.palette.color(Role::SyntaxVariable))
            .tag(self.palette.color(Role::Link))
            .label(self.palette.color(Role::Success))
            .error(self.palette.color(Role::Error))
            .build()
    }
}

/// Render a markdown document into wrapped, styled terminal lines.
///
/// `wrap_w` bounds the word-wrap width. The renderer handles
/// paragraphs, headings, lists, fenced code blocks, blockquotes,
/// tables, and inline formatting. Colors come from `palette`
/// through [`MdTheme`]; unstyled prose runs get the `PlainText`
/// tone.
///
/// `base` is kept for API compatibility with the old
/// `wrap_markdown_p` signature but is unused: the text color
/// comes from the palette's `PlainText` role, which is what every
/// call site already passes as `base`.
pub fn render_markdown_lines(
    text: &str,
    wrap_w: usize,
    palette: &Palette,
    _base: Style,
) -> Vec<Line<'static>> {
    let renderer = MarkdownRenderer::new(wrap_w.max(1));
    let blocks = renderer.parse(text);
    let theme = MdTheme::new(palette);
    renderer.render(&blocks, &theme)
}
