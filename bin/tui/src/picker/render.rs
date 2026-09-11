//! The picker body renderer (section 4.4 and 6).
//!
//! `render_picker` draws the picker body into a given `Rect`. It never
//! decides where the `Rect` is: the container (the floating window in
//! section 6) owns placement. This keeps the body reusable for a
//! future inline container (section 6, Option A).
//!
//! The float layout lives in `crate::float`, shared with the command
//! palette (docs/tui-command-palette.md section 11).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use bon::builder;

use super::fuzzy::Snapshot;
use super::preview::Previewer;
use super::state::PickerState;

// Re-export the shared float layout so existing imports keep working.
pub use crate::float::{compute_float_layout, FloatLayout};

/// The result count below which the preview pane auto-hides so a short
/// list keeps the full width (a `telescope` `preview_cutoff`
/// behavior).
pub const PREVIEW_CUTOFF: usize = 4;
/// The default height of the preview scroll page for `Ctrl+U` /
/// `Ctrl+D`.
pub const PREVIEW_PAGE: usize = 5;

/// Draw the picker body into a pre-computed float layout. This
/// function never decides where the float is: the container (the
/// floating window in section 6, or an inline row in a later
/// revision) computes the regions and passes them in. The hardware
/// cursor is placed on the input bar. Eight parameters, so a `bon`
/// builder (docs/coding-conventions.md).
#[builder]
pub fn render_picker<'frame>(
    f: &mut Frame<'frame>,
    state: &mut PickerState,
    snapshot: &Snapshot,
    layout: &FloatLayout,
    previewer: &dyn Previewer,
    hints: &str,
    palette: &crate::color::Palette,
    cursor: &mut Option<(u16, u16)>,
) {
    // Clamp the cursor and window to the fresh snapshot before
    // drawing (the snapshot may have shrunk since the last move).
    state.sync(snapshot.items.len());

    // Paint the entire float region with the terminal default background
    // so the transcript / input rows underneath are hidden.
    f.render_widget(Clear, layout.float);

    // The outer float border and title. An unsettled ranking shows a
    // trailing ellipsis so the user sees a fresh pass is in flight.
    let accent = palette.color(crate::color::Role::Border4);
    let border_style = Style::default().fg(accent);
    let n = snapshot.items.len();
    let pending = if snapshot.settled { "" } else { " ·" };
    // The cycled file scope (docs/tui-file-picker.md P9) tags the
    // title so the widened set is visible at a glance.
    let scope_tag = state
        .scope
        .tag()
        .map(|t| format!(" · {t}"))
        .unwrap_or_default();
    let title = format!(
        "files ({}) — @ {} — {} match{}{}{}",
        layout.orientation.label(),
        snapshot.query,
        n,
        if n == 1 { "" } else { "es" },
        pending,
        scope_tag
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    f.render_widget(block, layout.float);

    // The result list.
    render_list(f, state, snapshot, layout, palette, accent);

    // The preview pane, if present.
    if let Some(preview_rect) = layout.preview {
        if let Some(item) = snapshot.items.get(state.cursor()) {
            render_preview(f, item, state, previewer, &preview_rect, palette);
        }
    }

    // The input bar: the `@query` prompt and the key hints, as a
    // plain line at the bottom of the float interior.
    let query_display = format!("@{}", snapshot.query);
    let query_len = query_display.chars().count();
    let prose = palette.color(crate::color::Role::PlainText);
    let hint_style = Style::default().fg(palette.color(crate::color::Role::Hint));
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

    // The hardware cursor on the input bar, one cell past the query text.
    let caret = layout.input.x + query_len as u16;
    let max_x = layout.input.x + layout.input.width.saturating_sub(1);
    *cursor = Some((caret.min(max_x), layout.input.y));
}

/// Shorten a result-list label to fit `max_chars` characters (P10 in
/// `docs/tui-file-picker.md`).
///
/// For a path label that has directory levels, the leading levels are
/// collapsed into a `...` prefix so the tail of the path stays
/// visible (`.../a/b/src/app.rs`): the largest suffix of path
/// components that fits with the `.../` prefix is kept. A label that
/// fits is returned unchanged. A label without directory levels, or
/// one still too wide with only `.../` plus the file name, falls
/// back to the plain head truncation with a trailing `…`.
pub fn abbreviate_path(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    // Empty components (a leading `/` on absolute labels) carry no
    // directory, so they never count as a level to collapse.
    let comps: Vec<&str> = label.split('/').filter(|c| !c.is_empty()).collect();
    if comps.len() >= 2 {
        // Growing suffixes: the first (smallest start index) that
        // fits with the `.../` prefix is the largest tail that fits.
        for start in 0..comps.len() {
            let candidate = format!(".../{}", comps[start..].join("/"));
            if candidate.chars().count() <= max_chars {
                return candidate;
            }
        }
    }
    let head: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

fn render_list(
    f: &mut Frame,
    state: &PickerState,
    snapshot: &Snapshot,
    layout: &FloatLayout,
    palette: &crate::color::Palette,
    accent: ratatui::style::Color,
) {
    let list_w = layout.list.width as usize;
    let max_chars = list_w.saturating_sub(4);
    let end = (state.top() + state.visible).min(snapshot.items.len());
    let lines: Vec<Line> = (state.top()..end)
        .map(|i| {
            let item = &snapshot.items[i];
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
            // P10: when the path label is too wide for the list
            // column, collapse the leading directory levels into a
            // `...` prefix so the tail of the path stays visible
            // (docs/tui-file-picker.md P10). The budget follows the
            // column, which differs with the preview pane on or off.
            let truncated = abbreviate_path(&item.label, max_chars);
            Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(truncated, label_style),
            ])
        })
        .collect();
    let list_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.color(crate::color::Role::Status)))
        .title(Line::from(Span::styled(
            "results",
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(list_block), layout.list);
}

fn render_preview(
    f: &mut Frame,
    item: &crate::picker::items::PickerItem,
    state: &PickerState,
    previewer: &dyn Previewer,
    preview_rect: &Rect,
    palette: &crate::color::Palette,
) {
    let header = previewer.header(item).unwrap_or_else(|| item.label.clone());
    let content = previewer.content(item, palette);
    let pane_h = preview_rect.height as usize;
    let start = state.preview_scroll.min(content.len());
    // Plain (default-style) segments get the pane's plain-text tone;
    // highlighted segments keep their palette syntax styles.
    let plain_base = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let lines: Vec<Line> = content
        .iter()
        .skip(start)
        .take(pane_h)
        .map(|segs| {
            if segs.is_empty() {
                Line::from("")
            } else {
                let spans: Vec<Span> = segs
                    .iter()
                    .map(|(s, t)| {
                        let style = if *s == Style::default() { plain_base } else { *s };
                        Span::styled(t.clone(), style)
                    })
                    .collect();
                Line::from(spans)
            }
        })
        .collect();
    let preview_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.color(crate::color::Role::Status)))
        .title(Line::from(Span::styled(
            header,
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(preview_block), *preview_rect);
}

// ── tests ───────────────────────────────────────────────────────────

