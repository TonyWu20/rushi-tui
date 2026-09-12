//! The palette float body renderer (docs/tui-command-palette.md section 11).
//!
//! `render_palette` draws the palette into a pre-computed [`FloatLayout`].
//! It never decides where the float is placed — the container in
//! `render::draw` owns placement, mirroring the picker float.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use bon::builder;

use crate::float::FloatLayout;
use crate::palette::items::PaletteItem;
use crate::palette::preview::render_preview;
use crate::palette::state::PaletteState;

/// Draw the palette body into a pre-computed float layout.
///
/// Takes the full ranked item list, the palette state machine, and a
/// palette for colors. The hardware cursor is placed on the input bar.
/// Eight parameters, so a `bon` builder (docs/coding-conventions.md).
#[builder]
pub fn render_palette<'frame>(
    f: &mut Frame<'frame>,
    state: &mut PaletteState,
    items: &[PaletteItem],
    layout: &FloatLayout,
    palette: &crate::color::Palette,
    cursor: &mut Option<(u16, u16)>,
) {
    // Clamp the cursor and window to the item count.
    state.sync(items.len());

    // Paint the float region with the terminal default background.
    f.render_widget(Clear, layout.float);

    // The outer border and title.
    let accent = palette.color(crate::color::Role::Border4);
    let border_style = Style::default().fg(accent);
    let n = items.len();
    let list_label = if state.stage == crate::palette::state::PaletteStage::SessionList {
        "sessions"
    } else {
        "commands"
    };
    let title = format!(
        "{list_label} ({}) — {n} item{}",
        layout.orientation.label(),
        if n == 1 { "" } else { "s" }
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    f.render_widget(block, layout.float);

    // The command list.
    render_list(f, state, items, layout, palette, accent);

    // The preview pane, if present.
    if let Some(preview_rect) = layout.preview {
        if let Some(item) = items.get(state.cursor()) {
            render_preview_pane(
                f,
                item,
                state,
                &preview_rect,
                palette,
            );
        }
    }

    // The input bar: the `:query` prompt and key hints.
    let query_display = format!(":{}", state.query);
    let query_len = query_display.chars().count();
    let prose = palette.color(crate::color::Role::PlainText);
    let hint_style = Style::default().fg(palette.color(crate::color::Role::Hint));
    let hints = "j/k move · enter ok · esc close · ctrl-p preview";
    let input_line = Line::from(vec![
        Span::styled(
            query_display.clone(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(prose),
        ),
        Span::styled(format!("  {hints}"), hint_style),
    ]);
    f.render_widget(Paragraph::new(input_line), layout.input);

    // Place the hardware cursor on the input bar, one cell past the
    // query text.
    let caret = layout.input.x + query_len as u16;
    let max_x = layout.input.x + layout.input.width.saturating_sub(1);
    *cursor = Some((caret.min(max_x), layout.input.y));
}

/// Draw the command list in the left (or top) pane.
fn render_list(
    f: &mut Frame,
    state: &PaletteState,
    items: &[PaletteItem],
    layout: &FloatLayout,
    palette: &crate::color::Palette,
    accent: ratatui::style::Color,
) {
    let list_w = layout.list.width as usize;
    let max_chars = list_w.saturating_sub(4);
    let end = (state.top() + state.visible).min(items.len());
    let lines: Vec<Line> = (state.top()..end)
        .map(|i| {
            let item = &items[i];
            let is_cursor = i == state.cursor();
            let marker = if is_cursor { "❯ " } else { "  " };
            let marker_style = if is_cursor {
                Style::default()
                    .bg(accent)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.color(crate::color::Role::Hint))
            };
            let label_style = if is_cursor {
                Style::default()
                    .bg(accent)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.color(crate::color::Role::PlainText))
            };

            // Kind marker: R=Run, S=Set, G=Goto, E=Ext.
            let kind_marker = match item.kind {
                crate::palette::items::CmdKind::Run => "R",
                crate::palette::items::CmdKind::Set => "S",
                crate::palette::items::CmdKind::Goto => "G",
                crate::palette::items::CmdKind::Ext => "E",
            };

            // Truncate the label to fit.
            let label_display = if item.label.chars().count() > max_chars {
                let head: String =
                    item.label.chars().take(max_chars.saturating_sub(1)).collect();
                format!("{head}…")
            } else {
                item.label.clone()
            };

            let mut spans: Vec<Span> = vec![
                Span::styled(marker, marker_style),
                Span::styled(kind_marker.to_string(), label_style),
                Span::styled(" ", label_style),
                Span::styled(label_display, label_style),
            ];

            // Show the hint (current value or keybinding) after the label.
            if !item.hint.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", item.hint),
                    Style::default().fg(palette.color(crate::color::Role::Hint)),
                ));
            }

            // For Set items, show the current value in the hint column.
            if matches!(item.kind, crate::palette::items::CmdKind::Set)
                && item.options.is_empty()
            {
                // Handled above via item.hint
            }

            Line::from(spans)
        })
        .collect();

    let list_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(
            palette.color(crate::color::Role::Status),
        ))
        .title(Line::from(Span::styled(
            "commands",
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(list_block), layout.list);
}

/// Draw the preview pane for the highlighted item.
fn render_preview_pane(
    f: &mut Frame,
    item: &PaletteItem,
    state: &mut PaletteState,
    preview_rect: &ratatui::layout::Rect,
    palette: &crate::color::Palette,
) {
    let header = if item.ext.is_some() {
        format!("{} (ext)", item.label)
    } else {
        item.label.clone()
    };

    let content = render_preview(item, state.option_cursor, palette);
    let pane_h = preview_rect.height as usize;
    // The block border consumes 2 rows (top + bottom).
    let visible_h = pane_h.saturating_sub(2).max(1);

    // Auto-scroll: keep the selected option visible. The selected
    // option is at content-line index `option_cursor`. If it falls
    // outside the visible window, adjust `preview_scroll`.
    if !item.options.is_empty() {
        let sel = state.option_cursor.min(item.options.len().saturating_sub(1));
        let top = state.preview_scroll;
        let bot = top + visible_h;
        if sel < top {
            state.preview_scroll = sel;
        } else if sel >= bot {
            state.preview_scroll = sel - visible_h + 1;
        }
    }

    let start = state.preview_scroll.min(content.len());

    let plain_base = palette.style(
        crate::color::Role::PlainText,
        Modifier::empty(),
    );
    let lines: Vec<Line> = content
        .iter()
        .skip(start)
        .take(visible_h)
        .map(|segs| {
            if segs.is_empty() {
                Line::from("")
            } else {
                let spans: Vec<Span> = segs
                    .iter()
                    .map(|(s, t)| {
                        let style = if *s == Style::default() {
                            plain_base
                        } else {
                            *s
                        };
                        Span::styled(t.clone(), style)
                    })
                    .collect();
                Line::from(spans)
            }
        })
        .collect();

    let preview_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(
            palette.color(crate::color::Role::Status),
        ))
        .title(Line::from(Span::styled(
            header,
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(preview_block), *preview_rect);
}

