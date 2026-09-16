//! Rendering: event -> terminal lines, and the frame layout.
//!
//! Rendering rules (docs/tui.md section 2.2 and 10.4):
//! - known category -> semantic pretty-print
//! - unknown `type` -> raw JSON with a hint
//! - unsupported `v` -> raw JSON with a "newer log version" hint
//! - malformed line -> raw text with a hint
//!
//! Message content (the `content` field of user and assistant
//! events) wraps across as many lines as it needs: it is displayed
//! in full, never truncated or folded (docs/
//! tui_feature_requests_from_human.md item 1). Message content gets
//! markdown syntax highlighting; tool result bodies get the folded,
//! box-wrapped, syntax-aware render of docs/tui-tool-display-port.md.
//! The log file stays the record.
//!
//! The input area is a multi-line textarea in a rounded-corner border
//! whose color tracks the active model's thinking level, and is
//! customizable by a `frame` extension (docs/ui-extensions design:
//! the input area is not hardwired into the TUI; an external process
//! owns its frame through the `frame_spec` reply, never its content).

use bon::builder;
use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::app::{App, StreamBlockCache};
use crate::event::{Event, EventKind};
use crate::highlight;
use crate::picker::preview::Previewer;

/// Map the frame spec's border choice to a ratatui border type. The
/// default (a `None` border on the spec) is the rounded corners.
fn border_style(b: crate::ext::FrameBorderStyle) -> BorderType {
    match b {
        crate::ext::FrameBorderStyle::Rounded => BorderType::Rounded,
        crate::ext::FrameBorderStyle::Plain => BorderType::Plain,
        crate::ext::FrameBorderStyle::Double => BorderType::Double,
        crate::ext::FrameBorderStyle::Thick => BorderType::Thick,
    }
}

const LABEL: &str = " ";
/// Uniform left gutter in visual columns: label, gap, then content.
/// Continuation lines align under the content of the first line.
const GUTTER: usize = 12;
/// Inner content width of the user message panel.
/// The two border columns and one column of left pad
/// are subtracted. The result is clamped to a minimum
/// of four.
fn user_box_content_w(width: usize) -> usize {
    width.saturating_sub(4).max(4)
}
/// Content width of an assistant body.
/// One cell of left pad is subtracted.
fn assistant_body_content_w(width: usize) -> usize {
    width.saturating_sub(1)
}
/// Body lines a single tool call's `command` argument may occupy.
/// The `content` field of user and assistant messages has no cap:
/// it displays in full (docs/tui_feature_requests_from_human.md item
/// 1). Tool result bodies fold at render time (docs/
/// tui-tool-display-port.md).
const TOOL_CALL_BODY_LINES: usize = 4;
/// Raw JSON lines a fallback event block may show. The fallback is for
/// opaque data the TUI does not model; the log keeps the full text.
const RAW_FALLBACK_MAX_LINES: usize = 6;

fn trunc(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        // The ellipsis owns one column of the budget: the result
        // never exceeds `max` columns.
        out = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
    }
    out
}

/// Clip a styled row to `max` visual columns. Each character is one
/// column (the Nerd Font glyphs the statusline extension ships are
/// one column wide). A status row owns one reserved terminal row,
/// and an overflow would wrap to the next row.
fn clip_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let n = span.content.chars().count();
        if used + n > max {
            let keep = max.saturating_sub(used);
            if keep > 0 {
                let cut: String = span.content.chars().take(keep).collect();
                out.push(Span::styled(cut, span.style));
            }
            break;
        }
        used += n;
        out.push(span);
    }
    out
}

/// Wrap `text` at `wrap_w`, one gutter-prefixed line each, capped at
/// `cap` lines with a hint for the remainder. The fold hint is the
/// palette `Hint` role (the pi `muted` tone), not a hard-coded gray.
fn body(
    text: &str,
    style: Style,
    cap: usize,
    wrap_w: usize,
    gutter: &str,
    hint: Style,
) -> Vec<Line<'static>> {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return Vec::new();
    }
    let wrapped = wrap_styled(vec![(style, text.to_string())], wrap_w);
    let dim = hint.add_modifier(Modifier::DIM);
    let mut out = Vec::with_capacity(cap + 1);
    for l in wrapped.iter().take(cap) {
        // Keep the wrapped line's styled spans (the tool-output tone) and
        // only prefix the gutter: flattening to a raw span would drop the
        // foreground and fall back to the terminal default.
        let mut spans = vec![Span::raw(gutter.to_string())];
        spans.extend(l.spans.iter().cloned());
        out.push(Line::from(spans));
    }
    let dropped = wrapped.len().saturating_sub(cap);
    if dropped > 0 {
        out.push(Line::from(Span::styled(
            format!("{gutter}… +{dropped} more lines (full text: bin/log)"),
            dim,
        )));
    }
    out
}

/// Prefix every line with the content gutter, keeping each line's own
/// styled spans. No cap: the content is displayed in full
/// (docs/tui_feature_requests_from_human.md item 1).
fn guttered(lines: &[Line<'static>], gutter: &str) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|l| {
            let mut spans = vec![Span::raw(gutter.to_string())];
            spans.extend(l.spans.iter().cloned());
            Line::from(spans)
        })
        .collect()
}

/// Per-line raw source ownership for a message body. `prov[k]` is the
/// hard source-line index that wrapped output line `k` came from
/// (`None` for lines derived from ext-transformed blocks). A line owns
/// its raw text only when it is the *first* output line of that source
/// line — wrapped continuations contribute nothing, so a yank range
/// collects each source line at most once (docs/tui-conversation-
/// browsing.md section 11.3).
fn owns_raw_line(k: usize, prov: &[Option<usize>], hard: &[String]) -> Option<String> {
    let p = match prov.get(k) {
        Some(&Some(p)) => p,
        _ => return None,
    };
    if k == 0 || prov.get(k - 1) != Some(&Some(p)) {
        hard.get(p).cloned()
    } else {
        None
    }
}

/// The user-message panel: a rounded, bordered block titled "User".
/// User messages are wrapped like tool results
/// (docs/tui_feature_requests_from_human.md 2026-09-06). The
/// rounded `Accent`-toned border replaces the old `user` marker.
/// There is no background fill and no content gutter. The rows are
/// hand-built styled spans, like `box_rows` for the tool panel.
/// The panel composes into the single scrollable transcript
/// `Paragraph`. A browse-mode cursor or yank still lands on a real
/// transcript line.
///
/// `content` are the already-wrapped, styled message lines.
/// The caller clamps them to the panel inner width. The panel
/// spans the full `width`. An empty message renders a single empty
/// interior row between the two border rows.
fn user_box_rows(
    content: &[Line<'static>],
    width: usize,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    message_box_rows(
        content,
        width,
        palette,
        Some("User"),
        crate::color::Role::Accent,
    )
}

/// The panel for the final idle assistant message of a completed
/// turn. It is the same rounded shape as the user box. It carries no
/// title. Only the `Report`-toned border marks the panel. The tone
/// is distinct from the `Accent` user box. See docs/tui-turn-fold.md
/// "Final message panel".
fn report_box_rows(
    content: &[Line<'static>],
    width: usize,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    message_box_rows(content, width, palette, None, crate::color::Role::Report)
}

/// The rounded message panel with no background fill. The border
/// alone marks the message. An optional title sits in the top
/// border. The `border_role` tones both border and title.
fn message_box_rows(
    content: &[Line<'static>],
    width: usize,
    palette: &crate::color::Palette,
    title: Option<&str>,
    border_role: crate::color::Role,
) -> Vec<Line<'static>> {
    let border = Style::default().fg(palette.color(border_role));
    // One column of left pad, mirroring the tool panel pad.
    let left_pad = 1usize;
    let inner = width.saturating_sub(2);
    let mut rows: Vec<Line<'static>> = Vec::new();
    // Top border. `╭Title───╮` when a title is present. A bare
    // `╭───╮` otherwise. The title sits one column in from the
    // corner, overwriting the top border dashes.
    let title_len = title.map_or(0, |t| t.chars().count());
    let top_fill = width.saturating_sub(2 + title_len);
    let mut top = vec![Span::styled("╭", border)];
    if let Some(t) = title {
        top.push(Span::styled(t.to_string(), border));
    }
    top.push(Span::styled("─".repeat(top_fill), border));
    top.push(Span::styled("╮", border));
    rows.push(Line::from(top));
    // Interior rows: `│ content │` with one column of left pad. An empty
    // message yields a single empty interior row.
    if content.is_empty() {
        rows.push(Line::from(vec![
            Span::styled("│", border),
            Span::raw(" ".repeat(inner)),
            Span::styled("│", border),
        ]));
    } else {
        for line in content {
            let mut cells = vec![Span::styled("│", border), Span::raw(" ".repeat(left_pad))];
            let mut content_width = 0usize;
            for span in &line.spans {
                // Keep the span's own foreground and modifiers.
                let st = span.style;
                cells.push(Span::styled(span.content.clone(), st));
                content_width += span.content.chars().count();
            }
            // Right padding to the panel edge.
            let right_pad = inner.saturating_sub(left_pad).saturating_sub(content_width);
            cells.push(Span::raw(" ".repeat(right_pad)));
            cells.push(Span::styled("│", border));
            rows.push(Line::from(cells));
        }
    }
    // Bottom border: `╰───╯`.
    let bottom_fill = width.saturating_sub(2);
    rows.push(Line::from(vec![
        Span::styled("╰", border),
        Span::styled("─".repeat(bottom_fill), border),
        Span::styled("╯", border),
    ]));
    rows
}

/// Human-readable status text for a tool_result value.
fn result_status(value: Option<&serde_json::Value>, err: bool) -> String {
    let code = value
        .and_then(|v| v.get("exit_code").or_else(|| v.get("exit")))
        .and_then(|c| c.as_i64());
    match (code, err) {
        (Some(c), _) => format!("exit {c}{}", if err { " (error)" } else { "" }),
        (None, true) => "error".to_string(),
        // No exit code and not an error: the panel's success
        // background already signals the outcome, so no status word
        // (the 2026-09-14 user pass: drop the redundant `ok`).
        (None, false) => String::new(),
    }
}

/// The token count as a compact `k` figure: 212992 renders as
/// `212k`, 33000 as `33k`, 999 as `999`.
fn fmt_k(n: i64) -> String {
    if n >= 1000 {
        format!("{}k", (n + 500) / 1000)
    } else {
        n.to_string()
    }
}

/// One visual event block: a header line plus wrapped, capped body
/// lines, each continuation line aligned under the content gutter.
/// `event_id` is the log index of the event; `ext` enables the stage
/// 3 span extraction (transform owners may rewrite the message
/// spans in place). `None` ext renders exactly the built-in path.
/// The render state of one transcript build (docs/tui-tool-display-
/// port.md section 2, docs/tui-color-scheme.md section 3, docs/tui-
/// thinking-block.md section 4, docs/tui-pending-user-messages.md
/// stage 2): the palette, the tool display config, the global fold
/// toggle, and the thinking-block state. The render path is a pure
/// function of the events and this state.
pub struct RenderState<'a> {
    pub palette: &'a crate::color::Palette,
    pub tool_display: &'a crate::tool_display::ToolDisplay,
    /// The global tool fold/expand toggle (Ctrl+O): `true` expands
    /// every collapsed block to the full body.
    pub tool_expanded: bool,
    /// The thinking-block visibility (Ctrl+X): `false` hides every
    /// thinking block.
    pub thinking_shown: bool,
    /// The thinking-block expand state (Ctrl+T): `false` shows the
    /// collapsed one-line label. Blocks start collapsed
    /// (docs/tui-turn-fold.md).
    pub thinking_expanded: bool,
    /// Per-block expand fractions for animation. Keys are tool-result
    /// event IDs. A value in `[0.0, 1.0]` interpolates the body cap
    /// between the collapsed and expanded caps. An empty map means
    /// "no animation: use the `tool_expanded` bool as-is".
    /// (docs/tui-tool-display-fancy.md section 6)
    pub expand_fracs: &'a std::collections::HashMap<String, f64>,
}

#[builder]
fn event_lines<'a>(
    e: &'a Event,
    pending: bool,
    call_details: &'a HashMap<String, (String, serde_json::Value)>,
    result_ids: &'a std::collections::HashSet<String>,
    width: usize,
    event_id: u64,
    ext: Option<&'a ExtRenderData>,
    state: &'a RenderState<'a>,
    loop_running: bool,
    compaction_last_open: bool,
    /// Box this event in the final-message panel when it is the
    /// idle reply of a completed turn (docs/tui-turn-fold.md).
    #[builder(default = false)]
    final_report: bool,
) -> (Vec<Line<'static>>, Vec<Option<String>>) {
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);
    let label_style = |fg: Color| Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let palette = state.palette;
    // The muted tone of the hint rows and the error accents: the
    // palette roles, lowered to the capability level
    // (docs/tui-color-tones.md).
    let dim = palette.style(crate::color::Role::Hint, Modifier::DIM);
    // Command/output bodies: the capability-aware muted tones, distinct
    // from the transcript prose color (docs/tui-color-tones.md: the
    // reference-tone fix). The colors are palette roles.
    let output = palette.style(crate::color::Role::ToolOutput, Modifier::empty());
    // The command text of a tool call: a capability-aware light tone
    // (`tool_command`), lighter than the result so the command and its
    // output read as two different voices.
    let command = palette.style(crate::color::Role::ToolCommand, Modifier::empty());
    // Unstyled transcript prose: the capability-aware plain-text
    // color (`plain_text`), not the terminal default.
    let prose = palette.style(crate::color::Role::PlainText, Modifier::empty());

    let mut out: Vec<Line<'static>> = Vec::new();
    // Per-line raw source ownership, parallel to `out` (section 11.3):
    // `Some(s)` on a line means that line is the shareable source of
    // the text `s`; `None` means the line is UI chrome or a wrapped
    // continuation, and contributes nothing to a yank.
    let mut owns: Vec<Option<String>> = Vec::new();
    match e.kind() {
        EventKind::UserMessage => {
            let content = e
                .get_str("content")
                .unwrap_or("[missing content]")
                .to_string();
            // docs/tui_feature_requests_from_human.md 2026-09-06: the user
            // message is wrapped in a rounded "User" panel on the
            // tool-result panel background, with no `user` marker and no
            // content gutter. The panel spans the transcript width, like
            // the tool-result panel.
            // The panel inner content width: the two border columns plus
            // one column of left padding.
            let box_w = width;
            let box_content_w = user_box_content_w(box_w);
            let (wrapped, prov) =
                render_message_content(&content, event_id, ext, box_content_w, prose, palette);
            // Raw source lines the yank maps onto: the content split into
            // hard lines, parallel to `wrap_markdown_p_provenance`'s split.
            let hard: Vec<String> = content
                .trim_end_matches('\n')
                .split('\n')
                .map(String::from)
                .collect();
            let panel = user_box_rows(&wrapped, box_w, palette);
            out.extend(panel);
            // Ownership: the top and bottom border rows are UI chrome; the
            // interior rows keep the per-source-line yank ownership
            // (docs/tui-conversation-browsing.md section 11.3).
            owns.push(None); // top border
            if wrapped.is_empty() {
                owns.push(None); // the single empty interior row
            } else {
                for k in 0..prov.len() {
                    owns.push(owns_raw_line(k, &prov, &hard));
                }
            }
            owns.push(None); // bottom border
        }
        EventKind::AssistantMessage => {
            // The thinking block first: the model's own reasoning items
            // (docs/tui-thinking-block.md section 4). Rendered above
            // the message body and the tool calls that follow.
            // `Ctrl+T` toggles the block between collapsed and
            // expanded. `Ctrl+X` hides or shows the block entirely.
            // The `thinking` tag uses the `ThinkingTag` role. The
            // block body keeps the `Thinking` role.
            // docs/tui-turn-fold.md "Final message panel": when
            // `final_report`, the block renders inside the report panel.
            // Its rows drop the `LABEL` pad and the content gutter.
            // They wrap to the panel inner width.
            let mut think_rows: Vec<Line<'static>> = Vec::new();
            if state.thinking_shown {
                let reasoning = e.get("reasoning").and_then(|v| v.as_array());
                if let Some(text) = thinking_text(reasoning) {
                    let thinking_style =
                        palette.style(crate::color::Role::Thinking, Modifier::empty());
                    let tag_style =
                        Style::default().fg(palette.thinking_tag(state.thinking_expanded));
                    let prefix = if final_report { "" } else { LABEL };
                    if state.thinking_expanded {
                        let header = vec![Span::styled(format!("{prefix}thinking"), tag_style)];
                        think_rows.push(Line::from(header));
                        let content_w = if final_report {
                            user_box_content_w(width)
                        } else {
                            assistant_body_content_w(width)
                        };
                        let wrapped = wrap_thinking(
                            &text,
                            content_w,
                            palette,
                            thinking_style,
                            state.tool_display.highlight_engine,
                        );
                        if final_report {
                            // Inside the panel there is no content
                            // gutter. The panel supplies its own pad.
                            think_rows.extend(wrapped);
                        } else {
                            // The reasoning body aligns with the tool
                            // result text. One cell of left pad, then
                            // the text wraps to the remaining width.
                            // This matches the `read`/`bash` panel pad.
                            think_rows.extend(guttered(&wrapped, " "));
                        }
                    } else {
                        // The collapsed row: a one-line pi-style label
                        // with the expand hint, not the reasoning text.
                        think_rows.push(Line::from(Span::styled(
                            format!("{prefix}thinking \u{2026} (Ctrl+T to expand)"),
                            tag_style,
                        )));
                    }
                }
            }
            let content = e.get_str("content").unwrap_or("").to_string();
            let tool_calls = e.get("tool_calls").and_then(|v| v.as_array());
            // docs/tui_feature_requests_from_human.md 2026-09-06: the
            // `assistant` marker is gone; the body starts at the left edge
            // (no content gutter). The optional tool-call count still notes
            // the actions that follow.
            let mut header: Vec<Span<'static>> = Vec::new();
            if let Some(calls) = tool_calls {
                if !calls.is_empty() {
                    header.push(Span::styled(
                        format!(
                            "({} tool call{})",
                            calls.len(),
                            if calls.len() == 1 { "" } else { "s" }
                        ),
                        dim,
                    ));
                }
            }
            let hard: Vec<String> = content
                .trim_end_matches('\n')
                .split('\n')
                .map(String::from)
                .collect();
            // One cell of left pad aligns the assistant body with the
            // tool-result text (the `read`/`bash` panel left pad). The
            // body wraps to the remaining width. The final-message
            // panel (docs/tui-turn-fold.md "Final message panel") is
            // narrower: its content wraps to the panel inner width.
            let content_w = if final_report {
                user_box_content_w(width)
            } else {
                assistant_body_content_w(width)
            };
            let (wrapped, prov) = if content.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                render_message_content(&content, event_id, ext, content_w, prose, palette)
            };
            // The first body line: the tool-call count header plus the
            // first wrapped line. The remaining lines are the wrap
            // continuations. `body_rows` stay unpadded. Each output form
            // adds its own left pad.
            let mut first_spans: Vec<Span<'static>> = Vec::new();
            if let Some(first) = wrapped.first() {
                first_spans.extend(header.iter().cloned());
                if !header.is_empty() {
                    first_spans.push(Span::raw("  "));
                }
                first_spans.extend(first.spans.iter().cloned());
            }
            let mut body_rows: Vec<Line<'static>> = vec![Line::from(first_spans)];
            for w in wrapped.iter().skip(1) {
                body_rows.push(Line::from(w.spans.clone()));
            }
            let mut body_raws: Vec<Option<String>> = Vec::new();
            for k in 0..body_rows.len() {
                body_raws.push(owns_raw_line(k, &prov, &hard));
            }
            let n_think = think_rows.len();
            if final_report {
                // The idle reply of a completed turn: the rounded
                // panel with the `Report` border and no title
                // (docs/tui-turn-fold.md "Final message panel"). The
                // thinking block rides inside the panel. The border
                // rows are UI chrome. The interior rows keep the
                // per-source-line yank ownership.
                let mut panel_rows = think_rows;
                panel_rows.extend(body_rows.iter().cloned());
                let panel = report_box_rows(&panel_rows, width, palette);
                out.extend(panel);
                owns.push(None);
                owns.extend(std::iter::repeat_n(None, n_think));
                owns.extend(body_raws);
                owns.push(None);
            } else {
                out.extend(think_rows);
                owns.extend(std::iter::repeat_n(None, n_think));
                // The one-cell left pad: a plain space before the body
                // text. FT-006: the empty-content case is guarded by
                // the `body_rows` build above.
                for br in &body_rows {
                    let mut padded: Vec<Span<'static>> = vec![Span::raw(" ")];
                    padded.extend(br.spans.iter().cloned());
                    out.push(Line::from(padded));
                }
                owns.extend(body_raws);
            }
        }
        EventKind::ToolCall => {
            let name = e.get_str("name").unwrap_or("?");
            let id = e.get_str("id").unwrap_or("");
            // Tools whose result box already shows the key info
            // (command, path, diff). When the result follows, the
            // separate call line would be redundant, so it merges
            // into the box. A call without a result yet keeps its
            // line: it is the only view of a running tool.
            let compact_tools = ["bash", "read", "edit", "write"];
            let merged = compact_tools.contains(&name) && result_ids.contains(id);
            if !merged {
                let args_val = e.get("arguments");
                let args = args_val
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "[missing arguments]".to_string());
                // The call line carries the tool name only: the raw
                // args JSON is never rendered on a call line (the
                // 2026-09-14 user directive). The full args JSON is
                // still the shareable source for yank, and a bash call
                // additionally shows its command as a body line below.
                //
                // Ownership of the shareable raw source (section 11.3):
                // a bash call owns its command body line, so the
                // header owns nothing; other tools' headers own the
                // full args JSON.
                let header_own = if name == "bash" {
                    None
                } else {
                    Some(args.clone())
                };
                out.push(Line::from(vec![Span::styled(
                    format!("{LABEL}{name}"),
                    label_style(palette.color(crate::color::Role::ToolName)),
                )]));
                owns.push(header_own);
                // The command of a bash call is the interesting part;
                // show it as its own dim line instead of raw JSON
                // noise. The full command string is the shareable raw
                // source, assigned to the first body line.
                if name == "bash" {
                    if let Some(cmd) = args_val
                        .and_then(|a| a.get("command"))
                        .and_then(|c| c.as_str())
                    {
                        let body_lines =
                            body(cmd, command, TOOL_CALL_BODY_LINES, wrap_w, &gutter, dim);
                        let n = body_lines.len();
                        out.extend(body_lines);
                        owns.push(Some(cmd.to_string()));
                        if n > 1 {
                            owns.extend(std::iter::repeat_n(None, n - 1));
                        }
                    }
                }
            }
        }
        EventKind::ToolResult => {
            let id = e.get_str("id").unwrap_or("?");
            // The call details hold the tool name and the call
            // arguments. The write diff needs the write `content`
            // argument (docs/tui-tool-result-truncation.md).
            let (name, args) = call_details
                .get(id)
                .cloned()
                .unwrap_or_else(|| (id.to_string(), serde_json::Value::Null));
            let value = e.get("value");
            let err = e.get_bool("is_error").unwrap_or(false);
            let status = result_status(value, err);
            let value_ref = value.unwrap_or(&serde_json::Value::Null);
            let body_w = width.saturating_sub(1);
            // The per-block expand fraction (docs/tui-tool-display-
            // fancy.md section 6). When the animation system has a
            // value for this event ID it interpolates the body cap.
            // An empty map means "use the `tool_expanded` bool".
            let expand_frac = state.expand_fracs.get(id).copied().unwrap_or(-1.0);
            // The panel header names the tool.
            // A `read` result adds a dim label after the name.
            // The label is the read `file_path` or `path` argument.
            let label = if name == "read" {
                args.get("file_path")
                    .or_else(|| args.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            } else {
                ""
            };
            // The turn fold L2/L3 (docs/tui-turn-fold.md). Applies in
            // the main view and the browse view alike. An unopened
            // result renders as a one-line header. Opened results
            // render the full body. The per-result frac is the open
            // state. The `zA`, click, `zM`, and `zR` keys set it.
            // The Ctrl+O toggle is ignored by the L2 default.
            let fold_open = expand_frac >= 0.5;
            let fold_closed = !fold_open;
            if fold_closed {
                // The one-line header. No body, no margin rows.
                // The raw output stays the yankable source of the row
                // (section 11.3).
                let row =
                    crate::tool_display::header_row(&name, label, &status, width, palette, err);
                let spans: Vec<Span<'static>> =
                    row.into_iter().map(|(s, t)| Span::styled(t, s)).collect();
                out.push(Line::from(spans));
                owns.push(Some(raw_event_text(e)));
            } else {
                let expanded_use = state.tool_expanded || fold_open;
                // The result body in its lighter panel (docs/tui-tool-
                // display-port.md section 2). The body is the tool-
                // specific compact output folded to the output mode's
                // lines. In fold mode the per-result open state takes
                // the place of the global toggle.
                let mut body = crate::tool_display::body_rows()
                    .tool(&name)
                    .value(value_ref)
                    .call_args(&args)
                    .err(err)
                    .cfg(state.tool_display)
                    .palette(palette)
                    .expanded(expanded_use)
                    .width(body_w)
                    .expand_frac(expand_frac)
                    .call();
                // The JSON-document body (docs/tui-color-tones.md).
                // A read result with a JSON-document body keeps the
                // JSON token colors. So does an unknown tool whose
                // result is JSON. The plain code tone is otherwise
                // used for the body.
                let body_text = value_ref.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let known = matches!(name.as_str(), "read" | "write" | "edit" | "bash");
                if (name == "read" || !known) && highlight::looks_like_json(body_text) {
                    body = crate::tool_display::json_body_rows(
                        &name,
                        value_ref,
                        state.tool_display,
                        palette,
                        expanded_use,
                        body_w,
                        expand_frac,
                    );
                }
                let rows = crate::tool_display::box_rows(
                    &name, label, &status, &body, width, palette, err,
                );
                // The raw output is the shareable source of a result
                // (section 11.3). The box's first row owns it so a
                // yank returns the full output, not the display.
                let raw_output = raw_event_text(e);
                for (i, row) in rows.into_iter().enumerate() {
                    let spans: Vec<Span<'static>> =
                        row.into_iter().map(|(s, t)| Span::styled(t, s)).collect();
                    out.push(Line::from(spans));
                    owns.push(if i == 0 {
                        Some(raw_output.clone())
                    } else {
                        None
                    });
                }
            }
        }
        EventKind::ApprovalRequest => {
            let id = e.get_str("id").unwrap_or("?");
            let prompt = e.get_str("prompt").unwrap_or("[no prompt]").to_string();
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let mut line = vec![
                Span::styled(format!("[approval {id}] "), st),
                Span::styled(trunc(&prompt, wrap_w.max(20)), output),
            ];
            if pending {
                line.push(Span::styled("  [y allow] [n deny] [e edit]", st));
            }
            out.push(Line::from(line));
            owns.push(None); // UI chrome: the rendered text is the fallback
        }
        EventKind::Approval => {
            let id = e.get_str("id").unwrap_or("?");
            let decision = e.get_str("decision").unwrap_or("?");
            let edited = e.get("arguments").is_some();
            let mut spans = vec![Span::styled(
                format!("[approval {id}] -> {decision}"),
                // The pi `success` accent on an allow, not a
                // hard-coded green.
                Style::default().fg(palette.color(crate::color::Role::Success)),
            )];
            if edited {
                spans.push(Span::styled(" (edited arguments)", dim));
            }
            out.push(Line::from(spans));
            owns.push(None);
        }
        EventKind::Cancel => {
            let target = e.get_str("target").unwrap_or("?");
            out.push(Line::from(Span::styled(
                format!("{LABEL}[cancel] target={target}"),
                dim,
            )));
            owns.push(None);
        }
        EventKind::UserMessageRetract => {
            let target = e.get_str("target").unwrap_or("?");
            let reason = e.get_str("reason").unwrap_or("");
            let reason_part = if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            };
            out.push(Line::from(Span::styled(
                format!("{LABEL}[retracted] target={target}{reason_part}"),
                dim,
            )));
            owns.push(None);
        }
        EventKind::ExtStatus => {
            // Shared UI state: the transcript shows no row for the
            // event (docs/ui-extension.md section 5). The log keeps
            // the event.
        }
        EventKind::ContextExhausted => {
            let msg = e.get_str("message").unwrap_or("").to_string();
            let ns = e.get_str("new_session").unwrap_or("").to_string();
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let mut spans = vec![Span::styled(format!("{LABEL}[context exhausted]"), st)];
            let wrapped = if msg.is_empty() {
                Vec::new()
            } else {
                wrap_styled(vec![(prose, msg)], wrap_w)
            };
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            owns.push(None);
            // The message body follows under the gutter, like the
            // error event. An empty `wrapped` leaves the header alone.
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
                owns.extend(std::iter::repeat_n(None, wrapped.len() - 1));
            }
            // The seeded handoff session. The status row carries the
            // one-key hint; this line names the target.
            if !ns.is_empty() {
                out.push(Line::from(vec![
                    Span::raw(gutter.clone()),
                    Span::styled(format!("handoff session: {ns}"), st),
                ]));
                owns.push(None);
            }
        }
        EventKind::Error => {
            let msg = e
                .get_str("message")
                .unwrap_or("[missing message]")
                .to_string();
            let wrapped = wrap_styled(vec![(prose, msg)], wrap_w);
            let mut spans = vec![Span::styled(
                format!("{LABEL}[error]"),
                // The pi `error` accent (not a hard-coded red).
                palette.style(crate::color::Role::Error, Modifier::BOLD),
            )];
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            owns.push(None);
            // The whole message is displayed, multi-line included.
            // An empty `wrapped` has no body line; the header stands
            // alone (an empty-slice `wrapped[1..]` panics).
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
                owns.extend(std::iter::repeat_n(None, wrapped.len() - 1));
            }
        }
        // The in-session auto-compact markers (docs/auto-compact-plan.md
        // section 4.6). The started line shows the trigger and the
        // scale while the summary call runs. The slot says whether
        // this marker is the last open one: a closed marker, or an
        // open marker superseded by a later one, renders dimmed.
        // The renderer closes the first open marker. The last open
        // marker with the loop process not running renders the
        // interrupted form.
        EventKind::CompactionStarted => {
            let reason = e.get_str("reason").unwrap_or("threshold").to_string();
            let tokens = e.get_i64("tokens_before").unwrap_or(0);
            let style = if compaction_last_open && loop_running {
                // The pi `warning` accent on the live marker.
                palette.style(crate::color::Role::Warning, Modifier::BOLD)
            } else {
                dim
            };
            let text = if compaction_last_open && !loop_running {
                format!("{LABEL}compacting (interrupted)")
            } else {
                format!("{LABEL}compacting ({reason}): {} tokens", fmt_k(tokens))
            };
            out.push(Line::from(Span::styled(text, style)));
            owns.push(None);
        }
        EventKind::CompactionSummary => {
            let reason = e.get_str("reason").unwrap_or("threshold").to_string();
            let before = e.get_i64("tokens_before").unwrap_or(0);
            let after = e.get_i64("tokens_after").unwrap_or(0);
            let first_kept = e.get_i64("first_kept_seq").unwrap_or(0);
            let summary = e.get_str("summary").unwrap_or("").to_string();
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let spans = vec![Span::styled(
                format!(
                    "{LABEL}context compacted ({reason}): {} to {} tokens, keeping events from seq {first_kept}",
                    fmt_k(before),
                    fmt_k(after)
                ),
                st,
            )];
            out.push(Line::from(spans));
            owns.push(None);
            // The summary body rides under the gutter, available in
            // the expand mechanism like the error body.
            if !summary.is_empty() {
                let wrapped = wrap_styled(vec![(prose, summary)], wrap_w);
                if !wrapped.is_empty() {
                    out.extend(guttered(&wrapped, &gutter));
                    owns.extend(std::iter::repeat_n(None, wrapped.len()));
                }
            }
        }
        EventKind::CompactionFailed => {
            let reason = e.get_str("reason").unwrap_or("overflow").to_string();
            let detail = e.get_str("detail").unwrap_or("").to_string();
            // The pi `error` accent (not a hard-coded red).
            let st = palette.style(crate::color::Role::Error, Modifier::BOLD);
            let mut spans = vec![Span::styled(
                format!("{LABEL}compaction failed ({reason})"),
                st,
            )];
            if !detail.is_empty() {
                let wrapped = wrap_styled(vec![(prose, detail)], wrap_w);
                if let Some(first) = wrapped.first() {
                    spans.push(Span::raw("  "));
                    spans.extend(first.spans.iter().cloned());
                }
                out.push(Line::from(spans));
                owns.push(None);
                if !wrapped.is_empty() {
                    out.extend(guttered(&wrapped[1..], &gutter));
                    owns.extend(std::iter::repeat_n(None, wrapped.len() - 1));
                }
            } else {
                out.push(Line::from(spans));
                owns.push(None);
            }
        }
        EventKind::Rewind => {
            // The fork marker (docs/rewind-fork-design.md): a past
            // branch point, rendered dim like a closed marker. The
            // event projects to nothing in the model context; it
            // names the target seq and the mode (`before` restores
            // the target user message to the input box).
            let target = e.get_i64("target_seq").unwrap_or(0);
            let mode = e.get_str("mode").unwrap_or("on");
            out.push(Line::from(Span::styled(
                format!("{LABEL}rewound to seq {target} ({mode})"),
                dim,
            )));
            owns.push(None);
        }
        EventKind::UnknownType => {
            let ty = e.type_name().unwrap_or("?").to_string();
            out.push(Line::from(Span::styled(
                format!("{LABEL}[unknown event type \"{ty}\" — raw JSON]"),
                dim,
            )));
            owns.push(None);
            for l in e.pretty_capped(RAW_FALLBACK_MAX_LINES).lines() {
                out.push(Line::from(Span::styled(format!("{gutter}{l}"), dim)));
                owns.push(None);
            }
        }
        EventKind::UnsupportedVersion => {
            let ty = e.type_name().unwrap_or("?").to_string();
            let v = e
                .version()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into());
            out.push(Line::from(Span::styled(
                format!("{LABEL}[event {ty} v={v}: log version newer than this TUI — raw JSON]"),
                dim,
            )));
            for l in e.pretty_capped(RAW_FALLBACK_MAX_LINES).lines() {
                out.push(Line::from(Span::styled(format!("{gutter}{l}"), dim)));
            }
        }
        EventKind::BadLine => {
            let raw = e.raw_line().unwrap_or("").to_string();
            out.push(Line::from(Span::styled(
                format!(
                    "{LABEL}[malformed log line] {}",
                    trunc(&raw, wrap_w.max(20))
                ),
                // The pi `error` accent, dimmed (not a hard-coded red).
                palette.style(crate::color::Role::Error, Modifier::DIM),
            )));
            owns.push(None);
        }
    }
    (out, owns)
}

/// Push the accumulated spans as one visual line, clearing `cur`.
fn push_line(cur: &mut Vec<Span<'static>>, out: &mut Vec<Line<'static>>) {
    let line: Vec<Span<'static>> = std::mem::take(cur);
    out.push(Line::from(line));
}

/// Word-wrap styled segments to a fixed width. A newline in the text is
/// a hard break: each hard line word-wraps independently. Width is
/// measured in characters; CJK and combining characters will drift a
/// few columns on non-ASCII lines (phase-1 log content is ASCII).
/// The expanded thinking block text (docs/tui-thinking-block.md
/// section 4). The reasoning body renders as markdown: headings,
/// lists, blockquotes, and inline code, bold, italic, and links get
/// their respective palette styles. Plain runs take the `style`
/// parameter (the thinking tone). Two exceptions remain: a run of
/// consecutive `|` table lines draws as the box-drawing grid (the
/// 2026-09-03 user report: tables inside a thinking block lost their
/// fixed column widths), and a fenced code block draws through the
/// active highlight engine (the 2026-09-11 request): the fence
/// marker lines take the dimmed `Fence` tone, the language tag after
/// the opening delimiter drives the highlight, and the unscoped runs
/// fall back to the `Code` tone, fg-only, like the tool-result
/// bodies.
fn wrap_thinking(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    engine: crate::tool_display::HighlightEngine,
) -> Vec<Line<'static>> {
    let (lines, _in_fence, _fence_lang) = wrap_thinking_full(text, wrap_w, palette, style, engine);
    lines
}

/// Wrap a whole thinking text from a fresh highlighter.
///
/// The settled-transcript path and the live cache's full-rebuild
/// path use this. It creates a stateful `CodeHl`, feeds every hard
/// line, and returns the wrapped lines plus the final fence state.
/// A block comment that spans fence lines stays open across them.
fn wrap_thinking_full(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    engine: crate::tool_display::HighlightEngine,
) -> (Vec<Line<'static>>, bool, Option<String>) {
    let hard_lines: Vec<&str> = text.split('\n').collect();
    let mut hl = crate::tool_display::CodeHl::new(engine);
    let (out, in_fence, fence_lang) =
        wrap_thinking_delta(&hard_lines, wrap_w, palette, style, &mut hl, false, None);
    (out, in_fence, fence_lang)
}

/// Wrap the complete hard lines of an incremental thinking suffix.
///
/// `lines` are the newly-settled complete hard lines. They never
/// include the in-progress trailing partial. `hl`, `in_fence`, and
/// `fence_lang` carry state from the already-wrapped prefix. A code
/// fence or block comment spanning the append boundary stays
/// coherent. The in-progress partial line is not handled here.
/// Wrap it with [`wrap_thinking_held`] instead, since re-wrapping a
/// growing line each frame cannot advance the persistent tree-sitter
/// parser in place.
fn wrap_thinking_delta(
    lines: &[&str],
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    hl: &mut crate::tool_display::CodeHl,
    in_fence: bool,
    fence_lang: Option<String>,
) -> (Vec<Line<'static>>, bool, Option<String>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let border_style = palette.style(crate::color::Role::Hint, Modifier::DIM);
    let code_style = palette.style(crate::color::Role::Code, Modifier::empty());
    let mut fence_lang = fence_lang;
    let mut in_fence = in_fence;
    let mut i = 0usize;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if highlight::is_fence_delim(t) {
            i += 1;
            // The fence marker line: the delimiter and the language
            // tag, dimmed like the markdown fence pass.
            let marker = highlight::fence_line_p(t, palette);
            out.extend(wrap_flow(marker, wrap_w));
            if in_fence {
                in_fence = false;
                fence_lang = None;
            } else {
                in_fence = true;
                let rest = t
                    .strip_prefix("```")
                    .or_else(|| t.strip_prefix("~~~"))
                    .unwrap_or("");
                let tag = rest.trim();
                fence_lang = if tag.is_empty() {
                    None
                } else {
                    Some(tag.to_string())
                };
            }
            continue;
        }
        if in_fence {
            let line = lines[i];
            i += 1;
            // One hard code line through the active engine. The scoped
            // tokens carry the engine's fg colors (no background, the
            // 2026-09-11 color rule); the unscoped runs keep the code
            // tone.
            let segs: Vec<(Style, String)> = hl
                .line(line, fence_lang.as_deref(), palette)
                .into_iter()
                .map(|(st, s)| {
                    (
                        if st == Style::default() {
                            code_style
                        } else {
                            st
                        },
                        s,
                    )
                })
                .collect();
            if line.is_empty() {
                // An empty code line still occupies its row.
                out.push(Line::default());
            } else {
                out.extend(wrap_flow(segs, wrap_w));
            }
            continue;
        }
        if highlight::is_table_block_start(lines, i) {
            let mut block: Vec<String> = Vec::new();
            while i < lines.len() && highlight::is_table_row(lines[i]) {
                block.push(lines[i].to_string());
                i += 1;
            }
            let grid = highlight::table_grid(&block, wrap_w, palette);
            for row in grid {
                let spans: Vec<Span<'static>> = row
                    .into_iter()
                    .map(|(s, t2)| {
                        // The cell text takes the thinking tone; the
                        // border runs keep the hint style.
                        let s = if s == border_style { s } else { style };
                        Span::styled(t2, s)
                    })
                    .collect();
                out.push(Line::from(spans));
            }
            continue;
        }
        let line = lines[i];
        i += 1;
        if line.is_empty() {
            out.push(Line::default());
            continue;
        }
        // Prose renders as markdown: the per-line highlighter recovers
        // headings, lists, blockquotes, and inline code, bold, italic,
        // and links. Plain runs fall back to the thinking tone. Fence
        // and table lines are handled by the branches above, so the
        // fence state here stays `false`.
        let mut md_fence = false;
        let segs = with_plain_base(highlight::md_line(line, &mut md_fence, palette), style);
        out.extend(wrap_flow(segs, wrap_w));
    }
    (out, in_fence, fence_lang)
}

/// Wrap the in-progress (partial) last thinking line.
///
/// A fresh `CodeHl` is seeded with the carried fence language. This
/// keeps a line inside an open code fence highlighted. Block-comment
/// state that spans the boundary is not carried. That is a visual
/// approximation on the single in-progress line only. An empty
/// `held` returns an empty vec.
fn wrap_thinking_held(
    held: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    engine: crate::tool_display::HighlightEngine,
    in_fence: bool,
    fence_lang: Option<&str>,
) -> Vec<Line<'static>> {
    if held.is_empty() {
        return Vec::new();
    }
    let mut hl = crate::tool_display::CodeHl::new(engine);
    let (out, _, _) = wrap_thinking_delta(
        std::slice::from_ref(&held),
        wrap_w,
        palette,
        style,
        &mut hl,
        in_fence,
        fence_lang.map(str::to_string),
    );
    out
}

/// Re-wrap the in-progress held thinking line and store it on the
/// cache (docs/tui-perf-streaming-incremental-plan.md). A join
/// ending in a newline leaves an empty held line, which shows as
/// one blank row (matching the legacy full wrap).
fn refresh_held_lines(
    cache: &mut StreamBlockCache,
    held: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    thinking_style: Style,
) {
    cache.think_held = held.to_string();
    cache.think_held_lines = if held.is_empty() {
        vec![gutter_line(Line::default(), LIVE_GUTTER)]
    } else {
        gutter_lines(
            wrap_thinking_held(
                held,
                wrap_w,
                palette,
                thinking_style,
                cache.engine,
                cache.think_in_fence,
                cache.think_fence_lang.as_deref(),
            ),
            LIVE_GUTTER,
        )
    };
    cache.think_held_wrapped_for = held.len();
}

/// Apply the live-stream gutter to a wrapped line. The live block
/// settles where it lands, so it must not carry the 12-col settled
/// gutter. Instead each body line owns one leading space. Lines that
/// already start with the gutter keep it. Everything else gets it
/// prepended (docs/tui-streaming-simplify.md section 3).
fn gutter_line(line: Line<'static>, gutter: &str) -> Line<'static> {
    let has_gutter = line
        .spans
        .first()
        .is_some_and(|s| s.content.starts_with(gutter));
    if has_gutter {
        line
    } else {
        let mut spans = vec![Span::styled(gutter.to_string(), Style::default())];
        spans.extend(line.spans);
        Line::from(spans)
    }
}

/// Apply the live-stream gutter to a whole set of wrapped lines.
fn gutter_lines(lines: Vec<Line<'static>>, gutter: &str) -> Vec<Line<'static>> {
    lines.into_iter().map(|l| gutter_line(l, gutter)).collect()
}

fn wrap_styled(segs: Vec<(Style, String)>, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for (style, text) in segs {
        // A newline is a hard break, not a space: split into hard
        // lines first, then word-wrap each hard line.
        for hard in text.split('\n') {
            let mut cur: Vec<Span<'static>> = Vec::new();
            let mut cur_w = 0usize;
            for word in hard.split_inclusive(' ') {
                let w = word.chars().count();
                if cur_w + w > width && cur_w > 0 {
                    push_line(&mut cur, &mut out);
                    cur_w = 0;
                }
                if w > width {
                    // Hard-break an overlong word into width-sized
                    // pieces, each full piece its own line.
                    if cur_w > 0 {
                        push_line(&mut cur, &mut out);
                        cur_w = 0;
                    }
                    let mut piece = String::new();
                    for ch in word.chars() {
                        if piece.chars().count() == width {
                            out.push(Line::from(vec![Span::styled(
                                std::mem::take(&mut piece),
                                style,
                            )]));
                        }
                        piece.push(ch);
                    }
                    if !piece.is_empty() {
                        cur.push(Span::styled(piece.clone(), style));
                        cur_w = piece.chars().count();
                    }
                    continue;
                }
                cur_w += w;
                cur.push(Span::styled(word.to_string(), style));
            }
            if !cur.is_empty() || hard.is_empty() {
                push_line(&mut cur, &mut out);
            }
        }
    }
    out
}

/// Wrap a list of hard lines of styled segments to fit `width` display
/// cells, preserving segment styling across wrap points.
///
/// Each input line is a `Vec<Seg>` (one hard line of the previewer
/// output). Segments of one line flow continuously: word-wrapping
/// crosses segment (style) boundaries, and a newline inside a
/// segment's text is a hard break. Each output line is at most
/// `width` cells; a word longer than the width hard-breaks. A hard
/// line whose segments are all empty (a blank line) yields one
/// empty display line, so blank lines stay visible. Re-running this
/// every frame with the pane's current inner width is what makes the
/// preview pane reflow when the terminal resizes instead of
/// clipping (docs/tui-ratatui-ecosystem-audit.md §4.8).
pub fn wrap_hard_lines(
    lines: &[Vec<crate::highlight::Seg>],
    width: usize,
) -> Vec<Vec<Line<'static>>> {
    let width = width.max(1);
    lines
        .iter()
        .map(|segs| {
            if segs.iter().all(|(_, t)| t.is_empty()) {
                // A blank line: keep exactly one empty display row.
                return vec![Line::from("")];
            }
            // One hard line's segments flow as a continuous stream:
            // word-wrapping crosses segment (style) boundaries, and a
            // '\n' inside a segment text stays a hard break.
            wrap_styled_continuous(segs, width)
        })
        .collect()
}

/// The display-row index where hard line `hard` begins in `wrapped`
/// (the output of [`wrap_hard_lines`]). Used to translate the
/// hard-line-unit `preview_scroll` offset into a display-row offset
/// without re-wrapping.
pub fn hard_line_display_start(wrapped: &[Vec<Line>], hard: usize) -> usize {
    let mut acc = 0usize;
    for (i, lines) in wrapped.iter().enumerate() {
        if i == hard {
            return acc;
        }
        acc += lines.len();
    }
    // `hard` past the end: the display row after the last hard line.
    acc
}

/// The largest hard-line index whose display start is at or before
/// `row` (in `wrapped`). Used to back-track the scroll target when
/// the palette auto-scrolls to a selected option that wraps onto
/// several display rows.
pub fn display_row_hard_line(wrapped: &[Vec<Line>], row: usize) -> usize {
    let mut acc = 0usize;
    let mut last = 0usize;
    for (i, lines) in wrapped.iter().enumerate() {
        if acc > row {
            break;
        }
        last = i;
        acc += lines.len();
    }
    last
}

/// Like [`wrap_styled`] but treats every segment as one continuous
/// text stream. All spans from a single ext line flow onto the same
/// visual line (word-wrapping across span boundaries when the width
/// is exceeded). This preserves multi-span layouts such as the
/// side-by-side split-diff rows from `tool_result`.
fn wrap_styled_continuous(segs: &[(Style, String)], width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;

    for (style, text) in segs {
        let chunks: Vec<&str> = text.split('\n').collect();
        for (i, hard) in chunks.iter().enumerate() {
            // A '\n' inside the text is a hard break: flush the
            // current line before starting the next hard line.
            if i > 0 {
                if !cur.is_empty() {
                    out.push(Line::from(std::mem::take(&mut cur)));
                }
                cur_w = 0;
            }

            for word in hard.split_inclusive(' ') {
                let w = word.chars().count();
                if w == 0 {
                    continue;
                }
                if cur_w + w > width && cur_w > 0 {
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                }
                if w > width {
                    // Hard-break an overlong word into width-sized
                    // pieces, each full piece its own line.
                    if cur_w > 0 {
                        out.push(Line::from(std::mem::take(&mut cur)));
                        cur_w = 0;
                    }
                    let mut piece = String::new();
                    for ch in word.chars() {
                        piece.push(ch);
                        if piece.chars().count() == width {
                            out.push(Line::from(vec![Span::styled(
                                std::mem::take(&mut piece),
                                *style,
                            )]));
                        }
                    }
                    if !piece.is_empty() {
                        cur.push(Span::styled(piece.clone(), *style));
                        cur_w = piece.chars().count();
                    }
                    continue;
                }
                cur_w += w;
                cur.push(Span::styled(word.to_string(), *style));
            }
        }
    }
    if !cur.is_empty() {
        out.push(Line::from(cur));
    }
    out
}

/// The marker-free markdown render (docs/tui-markdown-render.md):
/// backed by `ratatui-markdown` (docs/NEW-refactor.md, recommended
/// core library #1). The renderer handles headings, lists, code
/// blocks, blockquotes, tables, and inline formatting. Colors come
/// from the active palette via `crate::markdown::MdTheme`.
/// `base` is the foreground of the unstyled plain-text runs;
/// `palette` the color roles.
fn wrap_markdown_p(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    base: Style,
) -> Vec<Line<'static>> {
    wrap_markdown_p_provenance(text, wrap_w, palette, base).0
}

/// Same as [`wrap_markdown_p`], plus a per-output-line provenance
/// map. With `ratatui-markdown` the parser is a whole-document
/// black box and exposes no source-line mapping, so every entry is
/// `None` — the browse yank falls back to the rendered text
/// (the same fallback the ext path already uses).
fn wrap_markdown_p_provenance(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    base: Style,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let lines = crate::markdown::render_markdown_lines(text, wrap_w, palette, base);
    let prov = vec![None; lines.len()];
    (lines, prov)
}

/// The thinking text of one `assistant_message` reasoning array
/// (docs/tui-thinking-block.md section 4): the item `summary` texts
/// first (the provider's own summary), the `content` reasoning-text
/// entries when the summary is empty. Malformed items drop; the
/// rest ride on, like the capture in commit `61cde02`. Also feeds
/// the tree preview pane for pure-thinking assistant events
/// (docs/tree-ui-design-from-human-phase-2.md, 2026-07-09).
pub(crate) fn thinking_text(reasoning: Option<&Vec<serde_json::Value>>) -> Option<String> {
    let items = reasoning?;
    let mut parts: Vec<String> = Vec::new();
    for item in items.iter().filter(|i| i.is_object()) {
        // The summary first: the provider's own summary of the
        // reasoning, when it carries text.
        let summary_text: String = item
            .get("summary")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if !summary_text.is_empty() {
            parts.push(summary_text);
            continue;
        }
        // The raw reasoning-text entries of the content array.
        // A `reasoning_text` entry and a plain text entry both carry
        // the reasoning text (older session logs store the plain
        // shape; docs/tui-thinking-block.md section 4).
        let content_text: String = item
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| match e.get("type").and_then(|t| t.as_str()) {
                        Some(t) => t == "reasoning_text",
                        None => true,
                    })
                    .filter_map(|e| e.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if !content_text.is_empty() {
            parts.push(content_text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Give the style-free segments of a highlight flow a `base`
/// foreground so plain text never falls back to the terminal's
/// default foreground (the "all gray" defect: the default color
/// depends on the emulator, and on the iPad Blink app it reads as
/// purple). A segment that carries any fg or modifier — a heading,
/// a quote marker, a JSON token — is not the plain text and keeps
/// its own style.
fn with_plain_base(segs: Vec<(Style, String)>, base: Style) -> Vec<(Style, String)> {
    segs.into_iter()
        .map(|(s, t)| (if s == Style::default() { base } else { s }, t))
        .collect()
}

/// Word-wrap styled segments that form one continuous flow: all
/// segments wrap into the same visual lines, unlike [`wrap_styled`],
/// where each segment's hard lines are independent.
/// Overlong words hard-break into width-sized pieces, each piece its
/// own line, like `wrap_styled`.
fn wrap_flow(segs: Vec<(Style, String)>, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for (style, text) in segs {
        for word in text.split_inclusive(' ') {
            let w = word.chars().count();
            if cur_w + w > width && cur_w > 0 {
                push_line(&mut cur, &mut out);
                cur_w = 0;
            }
            if w > width {
                // Hard-break an overlong word into width-sized
                // pieces, each full piece its own line.
                if cur_w > 0 {
                    push_line(&mut cur, &mut out);
                    cur_w = 0;
                }
                let mut piece = String::new();
                for ch in word.chars() {
                    if piece.chars().count() == width {
                        out.push(Line::from(vec![Span::styled(
                            std::mem::take(&mut piece),
                            style,
                        )]));
                    }
                    piece.push(ch);
                }
                if !piece.is_empty() {
                    cur.push(Span::styled(piece.clone(), style));
                    cur_w = piece.chars().count();
                }
                continue;
            }
            cur_w += w;
            cur.push(Span::styled(word.to_string(), style));
        }
    }
    if !cur.is_empty() {
        push_line(&mut cur, &mut out);
    }
    out
}

// ── transform span extraction (ui-extension-plan stage 3) ──────
/// One extracted span of a message, in content order. The `idx`
/// numbering is a pure function of the content, so it is stable
/// across transcript rebuilds: the host dedupes transform requests
/// per (event log index, span index).
#[derive(Debug)]
enum MBlock<'a> {
    /// A run of hard lines with no mermaid fence. Each line is
    /// pre-split into flow parts at split time. Fence-aware: no
    /// span inside a code fence.
    Text { parts: Vec<Vec<Part<'a>>> },
    /// A `fence:mermaid` code fence. `raw` is the whole fence
    /// (backtick lines included) for the raw fallback; `text` is
    /// the body the extension rewrites.
    Mermaid { idx: u32, raw: String, text: String },
}

/// One flow part of a hard line: markdown text, or an extracted
/// span.
#[derive(Debug, Clone)]
enum Part<'a> {
    /// Markdown flow text, highlighted with the shared fence state.
    Text(&'a str),
    /// An `inline:latex` span. `raw` is the source with delimiters;
    /// `text` is what the extension rewrites.
    Latex {
        idx: u32,
        raw: &'a str,
        text: &'a str,
    },
}

/// One marker line of a code fence: the run of leading backticks or
/// tildes (three or more) plus the info string. A closing fence has
/// an empty info string.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    let bytes = t.as_bytes();
    let first = *bytes.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = bytes.iter().take_while(|&&b| b == first).count();
    if run < 3 {
        return None;
    }
    Some((first as char, run, t[run..].trim()))
}

/// Split one hard line into flow parts. Outside a code fence, a
/// `$$...$$` or `$...$` pair becomes a LaTeX span (an
/// `inline:latex` transform target). Inside a fence, on fence
/// marker lines, and for a `$` with no closing partner, the
/// dollars stay literal. The span `idx` numbering continues the
/// counter, in content order.
fn line_parts<'a>(line: &'a str, in_fence: bool, next_idx: &mut u32) -> Vec<Part<'a>> {
    if in_fence || fence_marker(line).is_some() || !line.contains('$') {
        return vec![Part::Text(line)];
    }
    let mut out: Vec<Part<'a>> = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let display = i + 1 < bytes.len() && bytes[i + 1] == b'$';
        if display {
            let rest = &line[i + 2..];
            if let Some(off) = rest.find("$$") {
                let close = i + 2 + off;
                if close > i + 2 {
                    if start < i {
                        out.push(Part::Text(&line[start..i]));
                    }
                    out.push(Part::Latex {
                        idx: *next_idx,
                        raw: &line[i..close + 2],
                        text: &line[i + 2..close],
                    });
                    *next_idx += 1;
                    start = close + 2;
                    i = start;
                    continue;
                }
            }
            // No closing marker: the dollars stay literal.
            i += 2;
            continue;
        }
        let rest = &line[i + 1..];
        if let Some(off) = rest.find('$') {
            let close = i + 1 + off;
            if close > i + 1 {
                if start < i {
                    out.push(Part::Text(&line[start..i]));
                }
                out.push(Part::Latex {
                    idx: *next_idx,
                    raw: &line[i..close + 1],
                    text: &line[i + 1..close],
                });
                *next_idx += 1;
                start = close + 1;
                i = start;
                continue;
            }
        }
        // A lone dollar stays literal.
        i += 1;
    }
    if start < line.len() {
        out.push(Part::Text(&line[start..]));
    }
    if out.is_empty() {
        out.push(Part::Text(line));
    }
    out
}

/// The code-fence state before each hard line of `content`: true
/// inside a code fence. A miniature of the markdown fence rules:
/// three or more leading backticks or tildes opens; a matching run
/// of the same character with an empty info string closes; a fence
/// opens only outside another fence. Known divergence from the
/// highlighter: it closes on any three-backtick line, tagged or
/// not. For a tagged marker inside a fence the two disagree about
/// the inline state; the visual render is identical, and span
/// extraction and highlight only diverge for that pathological
/// content (ui-extension-plan stage 3, LaTeX eligibility note).
fn fence_states(content: &str) -> Vec<bool> {
    let content = content.trim_end_matches('\n');
    let lines: Vec<&str> = content.split('\n').collect();
    let mut states = vec![false; lines.len()];
    let mut open: Option<(char, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        states[i] = open.is_some();
        if let Some((c, run, info)) = fence_marker(line) {
            match open {
                Some((oc, orun)) => {
                    if c == oc && info.is_empty() && run >= orun {
                        open = None;
                    }
                }
                None => open = Some((c, run)),
            }
        }
    }
    states
}

/// Split message content into blocks: `fence:mermaid` code fences
/// become [`MBlock::Mermaid`] blocks; everything else (including
/// other code fences and their bodies) stays in the text flow, with
/// LaTeX spans extracted per line.
///
/// Mermaid fences are recognized by the info string `mermaid`
/// (case-insensitive), outside a code fence. All span indices are
/// numbered in content order at split time: that is what makes
/// them stable across transcript rebuilds.
fn message_blocks(content: &str) -> Vec<MBlock<'_>> {
    let content = content.trim_end_matches('\n');
    let lines: Vec<&str> = content.split('\n').collect();
    let states = fence_states(content);
    let mut blocks: Vec<MBlock> = Vec::new();
    let mut next_idx: u32 = 0;
    let mut i = 0usize;
    let mut run_start = 0usize;
    while i < lines.len() {
        let line = lines[i];
        // Inside a code fence: plain text run.
        if states[i] {
            i += 1;
            continue;
        }
        let marker = fence_marker(line);
        if let Some((c, run, info)) = marker {
            if !info.eq_ignore_ascii_case("mermaid") {
                // A non-mermaid fence: flow text, until its close.
                i += 1;
                continue;
            }
            // A mermaid fence: extract it. The body is the
            // transform target; the whole fence is the raw
            // fallback.
            if run_start < i {
                let parts: Vec<Vec<Part>> = (run_start..i)
                    .map(|j| line_parts(lines[j], states[j], &mut next_idx))
                    .collect();
                if !parts.is_empty() {
                    blocks.push(MBlock::Text { parts });
                }
            }
            let open_run = run;
            let mut raw_lines: Vec<&str> = vec![line];
            let mut text_lines: Vec<&str> = Vec::new();
            i += 1;
            loop {
                let l2 = lines[i];
                if let Some((c2, run2, info2)) = fence_marker(l2) {
                    if c2 == c && info2.is_empty() && run2 >= open_run {
                        raw_lines.push(l2);
                        i += 1;
                        break;
                    }
                }
                raw_lines.push(l2);
                text_lines.push(l2);
                i += 1;
                if i >= lines.len() {
                    // An unterminated fence runs to the end of the
                    // content: standard markdown behavior.
                    break;
                }
            }
            let raw = raw_lines.join("\n");
            let text = text_lines.join("\n");
            blocks.push(MBlock::Mermaid {
                idx: next_idx,
                raw,
                text,
            });
            next_idx += 1;
            run_start = i;
            continue;
        }
        i += 1;
    }
    if run_start < lines.len() {
        let parts: Vec<Vec<Part>> = (run_start..lines.len())
            .map(|j| line_parts(lines[j], states[j], &mut next_idx))
            .collect();
        if !parts.is_empty() {
            blocks.push(MBlock::Text { parts });
        }
    }
    blocks
}

/// The rendered text of one hard line of a text block: the text
/// parts in order, the latex spans by their span text. Used for
/// the grid-table detection: a run of consecutive table lines in
/// one text block draws as a box-drawing grid, the same rule as
/// [`wrap_markdown_p`] (docs/tui-markdown-render.md section 1).
fn block_line_text(parts: &[crate::render::Part]) -> String {
    parts
        .iter()
        .map(|p| match p {
            crate::render::Part::Text(s) => *s,
            crate::render::Part::Latex { text, .. } => *text,
        })
        .collect::<Vec<_>>()
        .concat()
}

/// Render one user/assistant message with the stage 3 span
/// extraction.
///
/// - A `fence:mermaid` block: a finished transform reply replaces the
///   fence with the extension's lines, guttered like the transcript.
///   The reply is read from the pre-resolved span map. A missing
///   entry, a pending or timed-out request, or a dead extension
///   shows the raw fence (G5 fallback).
/// - An `inline:latex` span: a finished transform reply replaces the
///   span text in place (the
///   reply lines join into one line). No entry: the raw span text
///   renders, exactly like the built-in path.
/// - A table block: a run of consecutive `|`-separated lines draws
///   as a box-drawing grid, like the `ext == None` path (the
///   2026-09-03 report: the block path showed the raw pipes).
///
/// `base` is the foreground of the unstyled plain-text runs (the
/// capability-aware prose color); styled runs keep their own
/// styles. The `ext == None` path renders the content with
/// [`wrap_markdown_p`] over the same base.
///
/// The transform requests are not sent here.
/// The host is not `Send`.
/// The main thread sends them up front.
/// It uses [`resolve_ext_spans`] and hands the finished
/// replies through [`ExtRenderData`]
/// (docs/tui-perf-background-build-plan.md, stage 1).
fn render_message_content(
    content: &str,
    event_id: u64,
    ext: Option<&ExtRenderData>,
    wrap_w: usize,
    base: Style,
    palette: &crate::color::Palette,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    if ext.is_none() {
        let (lines, prov) = wrap_markdown_p_provenance(content, wrap_w, palette, base);
        // ratatui-markdown renders the whole document at once and
        // exposes no per-source-line provenance; the browse yank falls
        // back to the rendered text for these lines (same fallback as
        // the ext path below).
        return (lines, prov);
    }
    let host = ext.expect("checked above");
    let blocks = message_blocks(content);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence = false;
    for block in blocks {
        match block {
            MBlock::Text { parts } => {
                let mut li = 0usize;
                while li < parts.len() {
                    // The grid table: a run of consecutive
                    // table lines, clamped to the pane width,
                    // like wrap_markdown_p.
                    let text = block_line_text(&parts[li]);
                    let next_is_sep = parts
                        .get(li + 1)
                        .map(|p| highlight::is_table_separator(&block_line_text(p)))
                        .unwrap_or(false);
                    if highlight::is_table_row(&text) && next_is_sep {
                        let mut block: Vec<String> = Vec::new();
                        while li < parts.len() {
                            let t = block_line_text(&parts[li]);
                            if !highlight::is_table_row(&t) {
                                break;
                            }
                            block.push(t);
                            li += 1;
                        }
                        let grid = highlight::table_grid(&block, wrap_w, palette);
                        for row in grid {
                            let spans: Vec<Span<'static>> =
                                row.into_iter().map(|(s, t)| Span::styled(t, s)).collect();
                            out.push(Line::from(spans));
                        }
                        continue;
                    }
                    let line_parts = &parts[li];
                    li += 1;
                    let mut segs: Vec<(Style, String)> = Vec::new();
                    for part in line_parts {
                        match part {
                            Part::Text(s) => {
                                segs.extend(with_plain_base(
                                    highlight::md_line(s, &mut fence, palette),
                                    base,
                                ));
                            }
                            Part::Latex { idx, raw, .. } => {
                                // The transform reply is pre-resolved
                                // on the main thread.
                                // A missing entry means no finished
                                // reply yet. The raw span shows then
                                // (G5 fallback).
                                let replaced = host
                                    .span_lines
                                    .get(&(event_id, *idx))
                                    .map(|ls| {
                                        ls.iter()
                                            .map(|l| l.text.clone())
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .unwrap_or_else(|| raw.to_string());
                                segs.push((base, replaced));
                            }
                        }
                    }
                    // One hard line always renders at least one
                    // visual line (the pre-stage-3 invariant the
                    // event header slices `wrapped[1..]` on).
                    if segs.is_empty() {
                        out.push(Line::default());
                    } else {
                        out.extend(wrap_flow(segs, wrap_w));
                    }
                }
            }
            MBlock::Mermaid { idx, raw, .. } => {
                // The transform reply is pre-resolved on the main
                // thread. An empty or missing entry erases the
                // block: show the raw fence (G5 fallback).
                let art = host
                    .span_lines
                    .get(&(event_id, idx))
                    .cloned()
                    .filter(|ls| !ls.is_empty());
                match art {
                    Some(lines) => {
                        // The reply lines are width-independent; the
                        // host wraps them to the pane width with the
                        // transcript gutter, like every extension
                        // reply.
                        out.extend(ext_lines_guttered(&lines, wrap_w + GUTTER));
                    }
                    None => {
                        // The raw fence shows. A private fence state
                        // highlights the fence lines; the shared
                        // state is untouched, because the block is a
                        // balanced fence.
                        let mut private = fence;
                        for hard in raw.split('\n') {
                            let segs = with_plain_base(
                                highlight::md_line(hard, &mut private, palette),
                                base,
                            );
                            if segs.is_empty() {
                                out.push(Line::default());
                            } else {
                                out.extend(wrap_flow(segs, wrap_w));
                            }
                        }
                    }
                }
            }
        }
    }
    // The ext path transforms content (mermaid → diagram, latex →
    // image), so the rendered lines do not map 1:1 to the source
    // hard lines. Provenance is `None` for every line; the browse
    // yank falls back to the rendered text for ext-transformed
    // content (the ext case is rare; the plain path above carries
    // full provenance).
    let n = out.len();
    (out, vec![None; n])
}

/// The static help row. The quit hint comes first: it is the safety
/// relevant one, and terminal clipping always eats the right end.
pub fn help_line(running: bool) -> String {
    let run_key = if running { "Ctrl+C stop" } else { "Ctrl+R run" };
    format!(
        " q×2 quit · {run_key} · Ctrl+E edit · Enter send · Ctrl-J ⏎ · vim · y/n/e · Tab · PgUp/Dn"
    )
}

/// The loop-phase display state of the active session
/// (docs/tui-model-wait-indicator.md section 2, the `working` row
/// from docs/tui-working-status.md section 4). One of five values,
/// derived from three inputs: the last `loop_phase` value in the log,
/// the loop-running bit, and the session stream buffer. The state is
/// a pure function of the log, the bit, and the buffer, so it
/// survives a TUI restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseState {
    /// The loop is not running. Bit `[idle]`, the working row
    /// stays blank.
    Idle,
    /// The loop runs with no marker, or a value outside
    /// `wait` / `tools`. Bit `[running]`, the row shows
    /// `Working...`.
    RunningUnknown,
    /// The loop runs, the last marker value is `wait`, and the
    /// stream buffer is closed. The request is pending on the
    /// server. Bit `[wait]`, the row shows the wait for the
    /// model response.
    Wait,
    /// The loop runs, the last marker value is `wait`, and the
    /// session stream buffer is open (at least one
    /// `.model-stream` delta line was read). The response is
    /// streaming back. Bit `[working]`, the row shows
    /// `model working · Ns` (docs/tui-working-status.md). The
    /// timer keeps the `wait` marker timestamp.
    Working,
    /// The loop runs, the last marker value is `tools`. Bit
    /// `[tools]`, the row shows the tool run.
    Tools,
}

/// The title bit text per phase state (docs/tui-model-wait-indicator.md
/// section 2). A running loop names its phase; an idle loop or an
/// unknown marker keeps the plain bit.
pub fn phase_bit(state: PhaseState) -> &'static str {
    match state {
        PhaseState::Idle => " [idle] ",
        PhaseState::RunningUnknown => " [running] ",
        PhaseState::Wait => " [wait] ",
        PhaseState::Working => " [working] ",
        PhaseState::Tools => " [tools] ",
    }
}

/// Derive the phase state from the last marker value, the running
/// bit, and the session stream buffer (docs/tui-model-wait-indicator.md
/// section 2; the `working` row from docs/tui-working-status.md
/// section 4). The value comes from the O(1) per-id map; a missing
/// marker or a value outside the two known strings maps to
/// `RunningUnknown` regardless of the stream buffer. Inside `wait`,
/// an open stream buffer (at least one `.model-stream` delta line
/// read) upgrades the state to `Working`: the response is
/// streaming back instead of the request still pending.
pub fn phase_state(app: &App, running: bool) -> PhaseState {
    if !running {
        return PhaseState::Idle;
    }
    match app
        .ext_statuses()
        .get(crate::app::LOOP_PHASE_STATUS_ID)
        .and_then(|v| v.as_str())
    {
        Some("wait") => {
            if app.stream_buf().is_some() {
                PhaseState::Working
            } else {
                PhaseState::Wait
            }
        }
        Some("tools") => PhaseState::Tools,
        _ => PhaseState::RunningUnknown,
    }
}

/// The wait span between the marker timestamp and `now`, in whole
/// seconds: `Ns` under 60 s, `Mm SSs` at 60 s and up. A negative
/// span (the loop host clock runs behind the TUI) clamps to `0s`.
/// An unparseable timestamp yields `None`: the working row keeps
/// the label and drops the span (docs/tui-model-wait-indicator.md
/// section 4).
pub fn wait_span_text(ts: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let marked = chrono::DateTime::parse_from_rfc3339(ts)
        .ok()?
        .with_timezone(&chrono::Utc);
    let secs = now.signed_duration_since(marked).num_seconds().max(0);
    if secs < 60 {
        Some(format!("{secs}s"))
    } else {
        Some(format!("{m}m {s}s", m = secs / 60, s = secs % 60))
    }
}

/// The braille spinner frames of the working row, in cycle order.
/// The row shows one frame per redraw; the main loop redraws about
/// every 100 ms, so the cycle runs at 100 ms per frame.
const WORKING_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner frame index for `now`: the wall-clock milliseconds
/// over the frame interval. The index is a pure function of time, so
/// no animation state lives in the App.
fn spinner_frame(now: &chrono::DateTime<chrono::Utc>) -> &'static str {
    let idx = (now.timestamp_millis() / 100) % WORKING_SPINNER_FRAMES.len() as i64;
    WORKING_SPINNER_FRAMES[idx as usize]
}

/// The working row text above the input box
/// (docs/tui-model-wait-indicator.md section 3; the `working` row
/// from docs/tui-working-status.md section 4): the phase with the
/// span from the marker timestamp. The `wait`, `working`, and
/// `tools` states carry the span; an unparseable timestamp drops
/// the span and keeps the label. The `working` state keeps the
/// `wait` marker timestamp: `N` counts the whole model call from
/// the marker, not from the first delta. The idle state owns no
/// text: the row stays blank.
fn working_row_text(
    state: PhaseState,
    ts: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let label = match state {
        PhaseState::Idle => return None,
        PhaseState::RunningUnknown => return Some("Working...".to_string()),
        PhaseState::Wait => "waiting for model",
        PhaseState::Working => "model working",
        PhaseState::Tools => "tools running",
    };
    match ts.and_then(|t| wait_span_text(t, *now)) {
        Some(span) => Some(format!("{label} · {span}")),
        None => Some(label.to_string()),
    }
}

/// The working row above the input box
/// (docs/tui-model-wait-indicator.md section 3, after the `pi`
/// working indicator): the spinner frame in the thinking-level
/// border color, then the phase text in dim. The caller draws the
/// row only while the loop runs. The idle state yields no text:
/// no row, and the transcript absorbs its place.
fn working_row(app: &App, running: bool, now: &chrono::DateTime<chrono::Utc>) -> Line<'static> {
    let state = phase_state(app, running);
    let Some(text) = working_row_text(state, app.loop_phase_ts(), now) else {
        return Line::default();
    };
    let frame = Span::styled(
        format!("{} ", spinner_frame(now)),
        Style::default().fg(app.palette().thinking_border(app.thinking_level())),
    );
    // The phase text in the pi `dim` tone (not a hard-coded gray).
    let body = Span::styled(
        format!(" {text}"),
        app.palette()
            .style(crate::color::Role::Status, Modifier::empty()),
    );
    let mut spans = vec![frame, body];
    // The in-progress turn's live tally, merged into this row instead
    // of a separate in-transcript line (docs/tui-turn-fold.md).
    if let Some(tally) = app.live_fold_tally() {
        spans.push(Span::styled(
            format!(" · {tally}"),
            app.palette()
                .style(crate::color::Role::Status, Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// The transcript-building row, shown while a background build is in
/// flight (docs/tui-perf-background-build-plan.md, stage 2).
/// It reuses the working-row spinner shape: the braille frame in the
/// thinking-border color, then the label in the pi `dim` tone.
fn rebuilding_row(app: &App, now: &chrono::DateTime<chrono::Utc>) -> Line<'static> {
    let frame = Span::styled(
        format!("{} ", spinner_frame(now)),
        Style::default().fg(app.palette().thinking_border(app.thinking_level())),
    );
    let body = Span::styled(
        " building transcript…",
        app.palette()
            .style(crate::color::Role::Status, Modifier::empty()),
    );
    Line::from(vec![frame, body])
}

/// The one-cell left pad of the live stream body
/// (docs/tui-streaming-simplify.md section 3). The live body
/// settles where it lands, so it must not carry the old 12-column
/// settled gutter.
const LIVE_GUTTER: &str = " ";

/// Split the joined reasoning text into the settled prefix and the
/// in-progress held line. The settled half ends at the last `\n`
/// (inclusive). The held half has no trailing `\n`. An empty input
/// yields two empty halves. Text without any newline is held
/// entirely.
fn split_settled_held(full: &str) -> (&str, &str) {
    match full.rfind('\n') {
        Some(p) => (&full[..=p], &full[p + 1..]),
        None => ("", full),
    }
}

/// The frame's config fields that invalidate the live stream block
/// cache (docs/tui-perf-streaming-incremental-plan.md). A change to
/// any of these forces a full rebuild.
#[derive(Clone, Copy)]
struct ConfigSnap {
    width: usize,
    palette_level: crate::color::Level,
    engine: crate::tool_display::HighlightEngine,
    thinking_expanded: bool,
}

/// Owned snapshot of the live buffer and config. It is captured
/// under shared borrows of [`App`] before the mutable cache update
/// (docs/tui-perf-streaming-incremental-plan.md).
struct StreamPlan {
    /// Whether the stream's `done` line has been read.
    done: bool,
    /// Partial tool-call arguments, cloned (a handful at most).
    tool_args: HashMap<String, (String, String)>,
    /// The sorted reasoning id set plus total value byte length.
    /// This is the join-skip fingerprint.
    cur_keys: Vec<String>,
    cur_len: usize,
    /// The joined reasoning text. It is joined only when a
    /// re-wrap is needed. Idle frames keep it `None`.
    thinking_joined: Option<String>,
    /// The new response text. Set only when it changed, when the
    /// cache was invalidated, or on the first build.
    text_new: Option<String>,
    /// A width, palette, or engine change. Both sections fully
    /// rebuild.
    cfg_invalid_full: bool,
    /// The `thinking_expanded` toggle flipped. The thinking section
    /// rebuilds. The text section is unaffected.
    think_toggle_invalid: bool,
    cfg: ConfigSnap,
    /// The owned palette. Wrap calls below run without any `App`
    /// borrow.
    palette: crate::color::Palette,
}

/// One frame's view of the live stream block
/// (docs/tui-perf-streaming-incremental-plan.md).
///
/// The two big sections (settled thinking and response text) share
/// their wrapped lines with the app's
/// [`StreamBlockCache`] through `Rc`. A cache hit costs two
/// refcount bumps and no line copies. The small pieces (header,
/// thinking label, in-progress held lines, partial tool args,
/// cursor) are owned and rebuilt each frame.
///
/// `len`, `Index`, and `iter` walk the sections in display order:
/// header, label, visible thinking, held lines, visible text, tool
/// args. The blinking cursor overlays the last body line. When the
/// body is empty it is its own trailing row. This matches the
/// legacy flat-vec render byte for byte.
pub(crate) struct StreamBlockView {
    /// The pinned status row ("…" open, "· done" settled).
    header: Vec<Line<'static>>,
    /// The "thinking …" label row, when thinking is shown.
    label: Option<Line<'static>>,
    /// Settled thinking lines shared with the cache.
    think: Rc<Vec<Line<'static>>>,
    /// Offset into `think` where the visible window starts.
    think_start: usize,
    /// Wrapped in-progress held thinking lines, fresh each frame
    /// they change.
    think_held: Vec<Line<'static>>,
    /// Response text lines shared with the cache.
    text: Rc<Vec<Line<'static>>>,
    /// Offset into `text` where the visible window starts.
    text_start: usize,
    /// Partial tool-call argument rows.
    tool_args: Vec<Line<'static>>,
    /// The last body line with the cursor span applied, or the
    /// standalone cursor row when the body is empty. `None` when
    /// the stream is done.
    cursor_line: Option<Line<'static>>,
    /// True when `cursor_line` is its own trailing row instead of an
    /// overlay on the last body line.
    cursor_standalone: bool,
}

impl StreamBlockView {
    /// The empty view used when there is no live stream buffer.
    pub(crate) fn empty() -> Self {
        Self {
            header: Vec::new(),
            label: None,
            think: Rc::new(Vec::new()),
            think_start: 0,
            think_held: Vec::new(),
            text: Rc::new(Vec::new()),
            text_start: 0,
            tool_args: Vec::new(),
            cursor_line: None,
            cursor_standalone: false,
        }
    }

    /// The total visible line count.
    pub fn len(&self) -> usize {
        let mut n = self.header.len();
        if self.label.is_some() {
            n += 1;
        }
        n += self.think.len() - self.think_start;
        n += self.think_held.len();
        n += self.text.len() - self.text_start;
        n += self.tool_args.len();
        if self.cursor_standalone {
            n += 1;
        }
        n
    }

    /// The visible line at flat index `i`, or `None` past the end.
    /// The last body line comes back with the cursor span applied
    /// while the stream is open.
    fn line_at(&self, i: usize) -> Option<&Line<'static>> {
        // The cursor row (overlay or standalone) always occupies
        // the final visible row.
        if self.cursor_line.is_some() && self.len() > 0 && i == self.len() - 1 {
            return self.cursor_line.as_ref();
        }
        let mut rem = i;
        if rem < self.header.len() {
            return self.header.get(rem);
        }
        rem -= self.header.len();
        if let Some(l) = &self.label {
            if rem == 0 {
                return Some(l);
            }
            rem -= 1;
        }
        let think = &self.think[self.think_start..];
        if rem < think.len() {
            return Some(&think[rem]);
        }
        rem -= think.len();
        if rem < self.think_held.len() {
            return Some(&self.think_held[rem]);
        }
        rem -= self.think_held.len();
        let text = &self.text[self.text_start..];
        if rem < text.len() {
            return Some(&text[rem]);
        }
        rem -= text.len();
        self.tool_args.get(rem)
    }

    /// The visible lines in display order. The last body line
    /// carries the cursor overlay. A standalone cursor is a
    /// trailing row.
    pub fn iter(&self) -> impl Iterator<Item = &Line<'static>> + '_ {
        (0..self.len()).map(move |i| self.line_at(i).expect("in-bounds view index"))
    }
}

impl std::ops::Index<usize> for StreamBlockView {
    type Output = Line<'static>;

    fn index(&self, i: usize) -> &Line<'static> {
        self.line_at(i)
            .expect("StreamBlockView index out of bounds")
    }
}

/// The in-progress model response lines, rendered inside the
/// transcript (docs/tui-streaming-simplify.md section 3). The caller
/// appends the returned view's lines to the settled transcript. The
/// live body settles where it lands, and the user can scroll up
/// through the whole body.
///
/// Shows the header with an ellipsis ("…") while the stream is open.
/// Once the done line arrives, the header shows "· done".
///
/// The body shows the full accumulated content. There is no sliding-
/// window cap when `max_body_lines` is `usize::MAX`. Thinking renders
/// above text, with partial tool-call arguments when no content has
/// arrived yet. A blinking block cursor marks the end of the live
/// text.
///
/// The thinking block honors the same global toggles as settled
/// blocks (docs/tui-streaming-simplify.md section 3). The
/// `thinking_shown` toggle (Ctrl+X) controls visibility and the
/// `thinking_expanded` toggle (Ctrl+T) controls collapse/expand.
/// When collapsed the live thinking shows only the one-line
/// "thinking …" label.
///
/// The wrapped thinking and response-text lines come from the app's
/// `StreamBlockCache` (docs/tui-perf-streaming-incremental-plan.md).
/// Only a newly appended thinking suffix is re-wrapped. The response
/// text re-parses only when it changed. Idle frames skip all wrap
/// work. The returned view shares the cached lines through `Rc`, so
/// producing it copies nothing large.
fn stream_block_lines(app: &mut App, width: usize, max_body_lines: usize) -> StreamBlockView {
    // Phase 1 (shared borrows only): snapshot the live buffer, the
    // cache fingerprints, and the config. No shared borrow of `App`
    // may outlive the mutable cache update below.
    let plan = {
        let buf = match app.stream_buf() {
            Some(b) => b,
            None => return StreamBlockView::empty(),
        };
        let cached = app.stream_block_cache_ref().as_ref();

        // Join-skip fingerprint: the sorted reasoning id set plus the
        // total value byte length. Within a stream the id set only
        // grows. The values only grow via `push_str`. A match means
        // the joined text is byte-identical, so the O(T) join is
        // skipped.
        let mut cur_keys: Vec<String> = buf.reasoning.keys().cloned().collect();
        cur_keys.sort();
        let cur_len: usize = buf.reasoning.values().map(|s| s.len()).sum();
        let fp_match = cached.is_some_and(|c| {
            c.think_reasoning_keys == cur_keys && c.think_reasoning_len == cur_len
        });
        // The response text only grows within a stream. clear_stream
        // drops the cache on settle and session switch. Equal length
        // therefore means identical content, so the markdown re-parse
        // is skipped.
        let text_unchanged = cached.is_some_and(|c| c.text_src.len() == buf.text.len());

        let cfg = ConfigSnap {
            width,
            palette_level: app.palette().level(),
            engine: app.tool_display().highlight_engine,
            thinking_expanded: app.thinking_expanded(),
        };
        let cfg_invalid_full = cached.is_none_or(|c| {
            c.width != cfg.width || c.palette_level != cfg.palette_level || c.engine != cfg.engine
        });
        let think_toggle_invalid =
            cached.is_none_or(|c| c.thinking_expanded != cfg.thinking_expanded);
        // A thinking re-wrap is needed when the fingerprint moved,
        // the expand toggle flipped, or the config invalidated the
        // cache.
        let need_think_work = !fp_match || think_toggle_invalid || cfg_invalid_full;

        StreamPlan {
            done: buf.done,
            tool_args: buf.tool_args.clone(),
            cur_keys,
            cur_len,
            thinking_joined: if need_think_work {
                Some(buf.reasoning_text())
            } else {
                None
            },
            text_new: if text_unchanged {
                None
            } else {
                Some(buf.text.clone())
            },
            cfg_invalid_full,
            think_toggle_invalid,
            cfg,
            palette: app.palette().clone(),
        }
    };
    // All shared borrows of `App` end here.

    // Phase 2 (mutable): update the cache in place. Idle frames do
    // nothing here.
    {
        let cache_slot = app.stream_block_cache_mut();
        if cache_slot.is_none() {
            *cache_slot = Some(StreamBlockCache::new(plan.cfg.engine));
        }
        let cache = cache_slot.as_mut().expect("cache initialized above");

        if plan.cfg_invalid_full {
            // Width, palette, or engine changed: full rebuild of both
            // sections with a fresh highlighter.
            *cache = StreamBlockCache::new(plan.cfg.engine);
        }
        // Record the config snapshot after the rebuild decision so the
        // next frame's check reads the current values. Without this
        // the `new()` sentinels would re-trigger a rebuild every frame.
        cache.width = plan.cfg.width;
        cache.palette_level = plan.cfg.palette_level;
        cache.engine = plan.cfg.engine;
        cache.thinking_expanded = plan.cfg.thinking_expanded;

        if plan.think_toggle_invalid {
            // The `thinking_expanded` toggle rebuilds the thinking
            // section only. The text section is unaffected.
            cache.think_hl = crate::tool_display::CodeHl::new(plan.cfg.engine);
            cache.think_src.clear();
            cache.think_lines = Rc::new(Vec::new());
            cache.think_in_fence = false;
            cache.think_fence_lang = None;
            cache.think_held.clear();
            cache.think_held_lines.clear();
            cache.think_held_wrapped_for = 0;
            cache.think_reasoning_keys.clear();
            cache.think_reasoning_len = 0;
        }

        // ── thinking section ─────────────────────────────────
        if let Some(full) = &plan.thinking_joined {
            if !plan.cur_keys.is_empty() && !full.is_empty() {
                let thinking_style = plan
                    .palette
                    .style(crate::color::Role::Thinking, Modifier::empty());
                let wrap_w = plan.cfg.width.saturating_sub(1).max(4);
                // Split the joined text into the settled prefix and the
                // in-progress held line. `think_lines` covers every
                // hard line except the held one, which is rewrapped
                // whenever it changes.
                let (_, held) = split_settled_held(full);
                // Append-only growth keeps the prefix intact. A broken
                // prefix (an earlier reasoning id grew) forces a full
                // re-wrap from a fresh highlighter.
                if full.starts_with(cache.think_src.as_str()) {
                    let delta = &full[cache.think_src.len()..];
                    if !delta.is_empty() {
                        let frags: Vec<&str> = delta.split('\n').collect();
                        if frags.len() >= 2 {
                            // Completed hard lines: the old held line
                            // plus the delta's first fragment, then the
                            // delta's middle fragments. The final delta
                            // fragment is the new held line.
                            let mut to_wrap: Vec<String> = Vec::with_capacity(frags.len() - 1);
                            to_wrap.push(cache.think_held.clone() + frags[0]);
                            for f in &frags[1..frags.len() - 1] {
                                to_wrap.push(f.to_string());
                            }
                            let refs: Vec<&str> = to_wrap.iter().map(|s| s.as_str()).collect();
                            let (new_lines, in_fence, fence_lang) = wrap_thinking_delta(
                                &refs,
                                wrap_w,
                                &plan.palette,
                                thinking_style,
                                &mut cache.think_hl,
                                cache.think_in_fence,
                                cache.think_fence_lang.clone(),
                            );
                            // The append copies the old lines into the
                            // new `Rc` (O(T)). A persistent deque
                            // would remove it. That is out of scope
                            // for this pass.
                            let mut merged: Vec<Line<'static>> =
                                cache.think_lines.iter().cloned().collect();
                            merged.extend(new_lines);
                            cache.think_lines = Rc::new(gutter_lines(merged, LIVE_GUTTER));
                            cache.think_in_fence = in_fence;
                            cache.think_fence_lang = fence_lang;
                        }
                    }
                } else {
                    // Non-suffix change (an earlier reasoning id
                    // grew): full re-wrap of the settled prefix from
                    // a fresh highlighter.
                    let settled = split_settled_held(full).0;
                    let hard: Vec<&str> = if settled.is_empty() {
                        Vec::new()
                    } else {
                        // `settled` ends in the held line's boundary
                        // `\n`. Drop it before splitting, else a
                        // spurious empty line appears.
                        settled
                            .strip_suffix('\n')
                            .unwrap_or(settled)
                            .split('\n')
                            .collect()
                    };
                    cache.think_hl = crate::tool_display::CodeHl::new(plan.cfg.engine);
                    let (lines, in_fence, fence_lang) = wrap_thinking_delta(
                        &hard,
                        wrap_w,
                        &plan.palette,
                        thinking_style,
                        &mut cache.think_hl,
                        false,
                        None,
                    );
                    cache.think_lines = Rc::new(gutter_lines(lines, LIVE_GUTTER));
                    cache.think_in_fence = in_fence;
                    cache.think_fence_lang = fence_lang;
                }
                cache.think_src = full.to_string();
                // Re-wrap the held line whenever the join ran. The
                // fence state or a reset path may have moved even
                // when the held text is unchanged. Idle frames skip
                // the whole section, so the cached held lines stay
                // O(1) with no highlight work.
                refresh_held_lines(cache, held, wrap_w, &plan.palette, thinking_style);
                cache.think_reasoning_keys = plan.cur_keys.clone();
                cache.think_reasoning_len = plan.cur_len;
            }
        }

        // ── response-text section ────────────────────────────
        if let Some(text) = plan.text_new {
            if text.is_empty() {
                // Mirror the legacy path. An empty text contributes no
                // lines.
                cache.text_src.clear();
                cache.text_lines = Rc::new(Vec::new());
            } else {
                let prose = plan
                    .palette
                    .style(crate::color::Role::PlainText, Modifier::empty());
                let wrap_w = plan.cfg.width.saturating_sub(1).max(4);
                let lines = wrap_markdown_p(&text, wrap_w, &plan.palette, prose);
                cache.text_lines = Rc::new(gutter_lines(lines, LIVE_GUTTER));
                cache.text_src = text;
            }
        }
    }
    // The mutable cache borrow ends here.

    // Phase 3 (shared): assemble the view. The big sections are
    // shared by `Rc` clone, so a cache hit copies nothing.
    let cache = app
        .stream_block_cache_ref()
        .as_ref()
        .expect("cache built above");
    let cfg = plan.cfg;
    let palette = &plan.palette;
    let wrap_w = cfg.width.saturating_sub(1).max(4);

    let dim = palette.style(crate::color::Role::Status, Modifier::DIM);
    let label_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolCommand))
        .add_modifier(Modifier::BOLD);
    let tool_name_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolName))
        .add_modifier(Modifier::BOLD);
    let thinking_tag_style = Style::default().fg(palette.thinking_tag(cfg.thinking_expanded));

    // Header row: the status suffix only. The `assistant` type
    // marker is gone (2026-09-06 request: no message-type markers).
    let suffix = if plan.done { " · done" } else { " …" };
    let header = vec![Line::from(vec![Span::styled(
        suffix.to_string(),
        label_style,
    )])];

    // The label row, shown when thinking content exists. Collapsed
    // shows "thinking …" and expanded shows "thinking"
    // (docs/tui-streaming-simplify.md section 3).
    let join_nonempty = plan.cur_len + plan.cur_keys.len().saturating_sub(1) > 0;
    let shows_thinking_label =
        app.thinking_shown() && !plan.cur_keys.is_empty() && join_nonempty && max_body_lines > 0;
    let label = shows_thinking_label.then(|| {
        let t = if cfg.thinking_expanded {
            "thinking".to_string()
        } else {
            "thinking \u{2026}".to_string()
        };
        Line::from(vec![
            Span::styled(LIVE_GUTTER.to_string(), Style::default()),
            Span::styled(t, thinking_tag_style),
        ])
    });

    // The shared content window: the last `max_body_lines` rows. One
    // row is reserved for the pinned thinking label when thinking is
    // shown. While the content fits, the block grows with it. Once
    // full, the oldest thinking rows scroll out as the text grows.
    let settled_total = cache.think_lines.len();
    let held_total = cache.think_held_lines.len();
    // The thinking lines show only when the label row shows (which
    // folds in the global `thinking_shown` toggle) and the block is
    // expanded. Collapsed or hidden shows no thinking content lines.
    let think_total = if shows_thinking_label && cfg.thinking_expanded {
        settled_total + held_total
    } else {
        0
    };
    let text_total = cache.text_lines.len();
    let window = max_body_lines.saturating_sub(usize::from(shows_thinking_label));
    let combined = think_total + text_total;
    let drop = combined.saturating_sub(window);
    let think_take = think_total.saturating_sub(drop);
    let text_drop = drop.saturating_sub(think_total);
    let text_take = text_total.saturating_sub(text_drop);

    // The window drops from the front. The visible thinking is the
    // tail of the settled lines followed by the tail of the held
    // lines. Collapsed shows no thinking lines at all.
    let held_take = think_take.min(held_total);
    let settled_take = think_take.saturating_sub(held_total);
    let think_start = if cfg.thinking_expanded {
        settled_total - settled_take
    } else {
        settled_total
    };
    let held_visible: Vec<Line<'static>> = if cfg.thinking_expanded {
        cache.think_held_lines[held_total - held_take..].to_vec()
    } else {
        Vec::new()
    };
    let text_start = text_total - text_take;

    // Partial tool-call arguments: one dim line per call. They show
    // only when no other body content is present.
    let body_has_content = shows_thinking_label || think_take > 0 || text_take > 0;
    let mut tool_args: Vec<Line<'static>> = Vec::new();
    if !body_has_content {
        let mut ids: Vec<&String> = plan.tool_args.keys().collect();
        ids.sort();
        for id in ids {
            if tool_args.len() >= max_body_lines {
                break;
            }
            let Some((name, args)) = plan.tool_args.get(id) else {
                continue;
            };
            let name_disp = if name.is_empty() {
                id.as_str()
            } else {
                name.as_str()
            };
            let shown = trunc(args, wrap_w.saturating_sub(12));
            tool_args.push(Line::from(vec![
                Span::styled(format!("{LIVE_GUTTER}{name_disp}"), tool_name_style),
                Span::styled(format!(" {shown}"), dim),
            ]));
        }
    }

    // The blinking cursor overlays the last body line while the
    // stream is open. When the body is empty it is its own row.
    let (cursor_line, cursor_standalone) = if plan.done {
        (None, false)
    } else {
        let now = chrono::Utc::now();
        let blink = (now.timestamp_millis() / 500) % 2 == 0;
        let cursor = if blink { "▊" } else { " " };
        let last_body: Option<Line<'static>> = if !tool_args.is_empty() {
            tool_args.last().cloned()
        } else if text_take > 0 {
            Some(cache.text_lines[cache.text_lines.len() - 1].clone())
        } else if held_take > 0 {
            Some(cache.think_held_lines[held_total - 1].clone())
        } else if settled_take > 0 {
            Some(cache.think_lines[settled_total - 1].clone())
        } else {
            label.clone()
        };
        match last_body {
            Some(mut l) => {
                l.spans.push(Span::styled(cursor, dim));
                (Some(l), false)
            }
            None => (
                Some(Line::from(vec![
                    Span::styled(LIVE_GUTTER.to_string(), dim),
                    Span::styled("▊", dim),
                ])),
                true,
            ),
        }
    };

    StreamBlockView {
        header,
        label,
        think: Rc::clone(&cache.think_lines),
        think_start,
        think_held: held_visible,
        text: Rc::clone(&cache.text_lines),
        text_start,
        tool_args,
        cursor_line,
        cursor_standalone,
    }
}
/// The status/help row content as terminal lines (one per row).
///
/// The TUI flash wins; then the status extension row (its lines or
/// the dead hint); then the built-in content. A status reply may
/// carry up to two lines (the narrow two-line layout,
/// ui-extension-plan stage 2); more than two is capped at two.
fn status_rows(
    app: &App,
    host: &crate::ext::ExtHost,
    running: bool,
    row_width: usize,
) -> Vec<Line<'static>> {
    // The built-in status text in the pi `dim` tone (not a
    // hard-coded gray).
    let dim = app
        .palette()
        .style(crate::color::Role::Status, Modifier::empty());
    if let Some(msg) = app.status() {
        // The pi flash line: a neutral confirmation tone, not a hard
        // coded cyan. The scheme `Hint` role (the pi `muted` value)
        // carries it, bold for the emphasis (docs/tui-color-pi-
        // alignment.md).
        let hint = app
            .palette()
            .style(crate::color::Role::Hint, Modifier::BOLD);
        return vec![Line::from(Span::styled(format!(" {msg}"), hint))];
    }
    if app.pending_name().is_some() {
        return vec![Line::from(Span::styled(
            " Enter confirm · Esc cancel · q×2 quit",
            dim,
        ))];
    }
    let last_line = app
        .active()
        .and_then(|s| app.loop_state(s))
        .and_then(|l| l.last_line.clone());
    match host.status_row() {
        crate::ext::StatusRow::Lines(lines) => {
            let rows: Vec<Line<'static>> = lines
                .iter()
                .take(2)
                .map(|l| {
                    let spans: Vec<Span<'static>> = l
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.text.clone(), s.style))
                        .collect();
                    // A status row owns one reserved terminal row. A
                    // too-wide row would wrap, so the host clips it
                    // to the row width (the extension's own width is
                    // the last tick's; the terminal may have resized
                    // since).
                    Line::from(clip_spans(spans, row_width))
                })
                .collect();
            // An empty reply must not erase the row slot (the help
            // content shares it): reserve one blank row.
            if rows.is_empty() {
                vec![Line::default()]
            } else {
                rows
            }
        }
        crate::ext::StatusRow::DeadHint(hint) => vec![Line::from(Span::styled(
            format!(" {hint}"),
            // The pi `error` accent, dimmed (not a hard-coded red).
            app.palette()
                .style(crate::color::Role::Error, Modifier::DIM),
        ))],
        crate::ext::StatusRow::Builtin => {
            // The pending handoff hint wins the built-in slot: it is
            // the action that unblocks the session. The flash still
            // wins over it (it names the result of the user's own
            // key press).
            if let Some(name) = app.pending_handoff() {
                return vec![Line::from(Span::styled(
                    format!(" context exhausted — press h to hand off to {name} · q×2 quit "),
                    // The pi `warning` accent (not a hard-coded yellow).
                    app.palette()
                        .style(crate::color::Role::Warning, Modifier::BOLD),
                ))];
            }
            // The running loop names its phase in the reserved
            // working row above the input box, not here
            // (docs/tui-model-wait-indicator.md section 3): the
            // statusline extension owns this slot, and the built-in
            // fallback shows the handoff hint, the last loop line,
            // or the help row.
            match last_line {
                Some(l) => vec![Line::from(Span::styled(
                    format!(" » {}", trunc(&l, row_width.saturating_sub(4))),
                    dim,
                ))],
                None => vec![Line::from(Span::styled(help_line(running), dim))],
            }
        }
    }
}

/// Resolve the transcript text width with the bar and the browse
/// gutter reserved (sections 3 and 4.3). The width drives the wrap,
/// the wrap the total, the total the gutter width: the loop runs to
/// the fixpoint, capped at three passes.
fn resolve_transcript_width(
    app: &mut crate::app::App,
    host: &crate::ext::ExtHost,
    t_width: usize,
    bar_w: usize,
    browse_active: bool,
) -> (usize, usize) {
    // (text_w, gutter_w). The gutter hint seeds from the last
    // rendered total; a missing layout reads as zero.
    let mut gutter_w = if browse_active {
        crate::browse::gutter_width(app.browse_layout_total())
    } else {
        0
    };
    let mut text_w = t_width.saturating_sub(bar_w).saturating_sub(gutter_w);
    let mut total = app.transcript_lines(text_w, Some(host)).len();
    if browse_active {
        for _ in 0..2 {
            let g = crate::browse::gutter_width(total);
            if g == gutter_w {
                break;
            }
            gutter_w = g;
            text_w = t_width.saturating_sub(bar_w).saturating_sub(gutter_w);
            total = app.transcript_lines(text_w, Some(host)).len();
        }
    }
    (text_w, gutter_w)
}

/// The browse-mode window lines (sections 4.3, 7.3, and 11.4): the
/// gutter prefix on every row, the cursorline highlight and the
/// caret block on the cursor row, the search highlight on the match
/// rows, and the visual-selection shading on the selected span
/// (docs/tui-conversation-browsing.md section 11.4: the
/// `Role::Selection` background, distinct from the search-highlight
/// tone). The styles are owned values: the caller precomputes them
/// so no palette borrow crosses the lines borrow.
#[allow(clippy::too_many_arguments)]
fn browse_window_lines(
    lines: &[Line<'static>],
    start: usize,
    h: usize,
    gutter_w: usize,
    cursor: (usize, usize),
    hl: &std::collections::HashSet<usize>,
    active_match: Option<(usize, usize)>,
    dim: Style,
    accent: Style,
    cursor_bg: Color,
    match_style: Style,
    active_style: Style,
    selection: Option<crate::browse::VisualSelection>,
    sel_bg: Color,
) -> Vec<Line<'static>> {
    let (cl, cc) = cursor;
    lines
        .iter()
        .take(h)
        .enumerate()
        .map(|(i, l)| {
            let abs = start + i;
            let is_cursor = abs == cl;
            let line_matched = hl.contains(&abs);
            let active_line = active_match.is_some_and(|m| m.0 == abs);
            // The gutter: the absolute number at the cursor, the
            // relative distance elsewhere (section 4.3), right-
            // aligned in `gutter_w` cells (the digit count plus one
            // trailing space).
            let num = crate::browse::gutter_number(cl, abs);
            let gutter_style = if is_cursor { accent } else { dim };
            let gutter_span =
                Span::styled(format!("{num:>width$}", width = gutter_w), gutter_style);
            // The selected span of this row (section 11.4): the
            // char range, or the whole row for a linewise selection.
            let sel = selection.and_then(|s| selection_row_range(&s, abs));
            // The base text spans: the match rows flatten to the
            // highlight tone (section 7.3); the cursor row keeps
            // its spans under the caret block.
            let base: Vec<Span<'static>> = if !is_cursor && (active_line || line_matched) {
                let style = if active_line {
                    active_style
                } else {
                    match_style
                };
                let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                vec![Span::styled(text, style)]
            } else {
                l.spans.to_vec()
            };
            // The visual-selection shading over the base spans: the
            // `Role::Selection` background on the selected chars,
            // distinct from the search-highlight tone. On the cursor
            // row the unselected part keeps the low-contrast
            // cursorline background; the selected span keeps the
            // stronger selection tone so it stays visible on top of
            // it (a cursorline bg would otherwise shadow the
            // selection, docs section 11.4).
            let rest_bg = if is_cursor { Some(cursor_bg) } else { None };
            let shaded = match sel {
                Some((s, e)) => shade_spans(&base, s, e, sel_bg, rest_bg),
                None if rest_bg.is_some() => shade_spans(&base, 0, Some(0), sel_bg, rest_bg),
                None => base,
            };
            if is_cursor {
                // The cursor row: the low-contrast background across
                // the row (section 4.3, applied by the shading above,
                // not on the selected span), the caret block at the
                // col. A matched cursor row keeps the accent tone.
                let fg_override = if active_line {
                    Some(active_style)
                } else if line_matched {
                    Some(match_style)
                } else {
                    None
                };
                let mut spans = vec![gutter_span];
                spans.extend(caret_spans(&shaded, cc, fg_override));
                Line::from(spans)
            } else {
                let mut spans = vec![gutter_span];
                spans.extend(shaded);
                Line::from(spans)
            }
        })
        .collect()
}

/// The selected char range of row `abs` (section 11.4):
/// `(start_char, end_char_inclusive)`; a `None` end runs to the row
/// end. Linewise selections shade whole rows; char-visual shades the
/// span between the anchor and the active end (the end rows from the
/// edge to the col, the middle rows whole). `None` outside the
/// selection.
fn selection_row_range(
    sel: &crate::browse::VisualSelection,
    abs: usize,
) -> Option<(usize, Option<usize>)> {
    use crate::browse::VisualSelection;
    let VisualSelection {
        anchor: (al, ac),
        active: (el, ec),
        linewise,
    } = *sel;
    if linewise {
        let (lo, hi) = if al <= el { (al, el) } else { (el, al) };
        return (abs >= lo && abs <= hi).then_some((0, None));
    }
    if al == el {
        if abs != al {
            return None;
        }
        let (lo, hi) = if ac <= ec { (ac, ec) } else { (ec, ac) };
        return Some((lo, Some(hi)));
    }
    let (first, second) = if al < el { (al, el) } else { (el, al) };
    let (first_c, second_c) = if al < el { (ac, ec) } else { (ec, ac) };
    match abs {
        a if a == first => Some((first_c, None)),
        a if a == second => Some((0, Some(second_c))),
        a if a > first && a < second => Some((0, None)),
        _ => None,
    }
}

/// The selection background over the char range `[start, end]` of a
/// row's spans (a `None` end runs to the row end, section 11.4):
/// the selected span takes the `Role::Selection` background, the
/// rest keeps its style (or the `rest_bg` background, the cursorline
/// tone on the cursor row, so the selection stays visible on top of
/// it); a span straddling an edge splits.
fn shade_spans(
    spans: &[Span<'static>],
    start: usize,
    end: Option<usize>,
    bg: Color,
    rest_bg: Option<Color>,
) -> Vec<Span<'static>> {
    let end = end.unwrap_or(usize::MAX);
    let rest_style = |s: &Span| match rest_bg {
        Some(c) => s.style.patch(Style::default().bg(c)),
        None => s.style,
    };
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut off = 0usize;
    for s in spans {
        let n = s.content.chars().count();
        let span_end = off + n;
        let lo = off.max(start);
        let hi = span_end.min(end);
        if hi > lo {
            // The span overlaps the range: split into the unshaded
            // head, the shaded middle, and the unshaded tail.
            let chars: Vec<char> = s.content.chars().collect();
            let pre_end = lo - off;
            let in_end = hi - off;
            if pre_end > 0 {
                let pre: String = chars[..pre_end].iter().collect();
                out.push(Span::styled(pre, rest_style(s)));
            }
            if in_end > pre_end {
                let mid: String = chars[pre_end..in_end].iter().collect();
                let st = s.style.patch(Style::default().bg(bg));
                out.push(Span::styled(mid, st));
            }
            if in_end < n {
                let post: String = chars[in_end..].iter().collect();
                out.push(Span::styled(post, rest_style(s)));
            }
        } else {
            out.push(Span::styled(s.content.clone(), rest_style(s)));
        }
        off = span_end;
    }
    out
}

/// The caret block at col `cc` over the row's text spans: the split
/// span keeps its styling (the background is set by the shading
/// pass, which keeps the selection tone on the selected span), the
/// cell inverts, the tail keeps its styling. A col past the rendered
/// line draws the block on the line-end blank (section 4.1).
fn caret_spans(l: &[Span<'static>], cc: usize, fg_override: Option<Style>) -> Vec<Span<'static>> {
    let patch = |s: &Span| {
        let mut st = s.style;
        if let Some(o) = fg_override {
            st = o.patch(st);
        }
        st
    };
    // The caret block: a reversed black-on-white cell. An `fg_override`
    // (a matched or active-match cursor row) retints it so the caret
    // stays visible on top of the highlight tone.
    let caret_style = || {
        let mut c = Style::default()
            .bg(Color::Black)
            .fg(Color::White)
            .add_modifier(Modifier::REVERSED);
        if let Some(o) = fg_override {
            c = o.patch(c);
        }
        c
    };
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut rest = cc;
    // The caret was drawn on an earlier span: every later span runs
    // past it and is pushed verbatim.
    let mut drawn = false;
    for s in l {
        if drawn {
            out.push(Span::styled(s.content.clone(), patch(s)));
            continue;
        }
        let n = s.content.chars().count();
        if n == 0 {
            // An empty span occupies no cell: the caret still waits
            // for the next non-empty span (or the end-of-line cell).
            out.push(Span::styled(s.content.clone(), patch(s)));
            continue;
        }
        if rest == 0 {
            // The caret lands on this span's first character: a span
            // boundary or col 0. This is the start-of-word case
            // (section 4.1) — `w` / `b` / `e` often leave the cursor
            // exactly at a span start, and the caret must paint there.
            let chars: Vec<char> = s.content.chars().collect();
            let at = chars[0];
            let post: String = chars[1..].iter().collect();
            out.push(Span::styled(at.to_string(), caret_style()));
            out.push(Span::styled(post, patch(s)));
            drawn = true;
        } else if rest < n {
            // The caret is strictly inside this span, at local offset
            // `rest`: split around it.
            let chars: Vec<char> = s.content.chars().collect();
            let pre: String = chars[..rest].iter().collect();
            let at = chars[rest];
            let post: String = chars[rest + 1..].iter().collect();
            out.push(Span::styled(pre, patch(s)));
            out.push(Span::styled(at.to_string(), caret_style()));
            out.push(Span::styled(post, patch(s)));
            drawn = true;
        } else {
            // The caret sits past this span: keep the rest running.
            out.push(Span::styled(s.content.clone(), patch(s)));
            rest -= n;
        }
    }
    if !drawn {
        // The caret sits one past the last character of the line
        // (section 4.1: the col may rest at the line end): the block
        // on a space cell.
        out.push(Span::styled(" ", caret_style()));
    }
    out
}

/// The position bar (section 3): one column at the right edge of the
/// transcript. The track draws in the dim tone, the thumb in the
/// normal transcript tone, the tail and cursor markers in the accent
/// tone (the `Border4` role, docs/tui-color-scheme.md section 6).
#[allow(clippy::too_many_arguments)]
fn draw_position_bar(
    f: &mut Frame,
    t_area: &ratatui::layout::Rect,
    total: usize,
    scroll: usize,
    cursor_line: Option<usize>,
    track_c: Color,
    thumb_c: Color,
    mark_c: Color,
) {
    let h = t_area.height as usize;
    let g = match crate::browse::bar_geometry(total, h, scroll, cursor_line) {
        Some(g) => g,
        None => return,
    };
    for row in 0..h {
        let mut ch = '·';
        let mut color = track_c;
        if row >= g.thumb_top && row < g.thumb_top + g.thumb_h {
            ch = '█';
            color = thumb_c;
        }
        if row == g.tail_cell {
            ch = '▼';
            color = mark_c;
        }
        if g.cursor_cell == Some(row) {
            ch = '▶';
            color = mark_c;
        }
        let rect = ratatui::layout::Rect {
            x: t_area.x + t_area.width - 1,
            y: t_area.y + row as u16,
            width: 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ch.to_string(),
                Style::default().fg(color),
            ))),
            rect,
        );
    }
}

/// Render the transcript into wrapped visual lines, separated by blank
/// lines. All in-memory events render: the log file is the record.
/// The result is cached per (events version, width, reply version)
/// by the caller.
///
/// Extension replies fold in (ui-extension-plan stage 1): when an
/// extension owns an event kind (the first extension in the composed
/// sequence that lists the kind) and a valid `lines` reply is cached
/// for the event, the extension's styled lines replace the built-in
/// render. A missing, stale, or timed-out reply falls back to the
/// built-in render (per-op G5 fallback).
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptBuild {
    /// The rendered transcript lines, oldest first.
    pub lines: Vec<Line<'static>>,
    /// The shareable raw source text that each rendered line maps to
    /// (`None` on blank separators, UI-chrome lines, and wrapped
    /// continuations), parallel to `lines` (docs/tui-conversation-
    /// browsing.md section 11.3: the per-line raw map a browse yank
    /// uses to reach the source text). A yank range joins the
    /// non-`None` entries it covers.
    pub line_raw: Vec<Option<String>>,
    /// The screen-line spans of tool-result boxes: maps event ID to
    /// `(start_line, end_line)` (exclusive end) in the transcript.
    /// Used for mouse-click hit-testing
    /// (docs/tui-tool-display-fancy.md section 6).
    pub block_spans: std::collections::HashMap<String, (usize, usize)>,
    /// First screen-line index of each event, indexed by the event's
    /// position in the in-memory log window (docs/tree-ui-design-from-
    /// human.md view-only scroll). `None` for events the transcript
    /// does not render (suppressed ext_status, or outside the render
    /// window).
    pub event_line_starts: Vec<Option<usize>>,
    /// The per-line display text, oldest first, parallel to `lines`.
    /// Cacheable: the browse layout stores this instead of rebuilding
    /// the string form of every line on each frame
    /// (docs/tui-conversation-browsing.md section 4.6).
    pub texts: Vec<String>,
}

/// The shareable source text of one event (docs/tui-conversation-
/// browsing.md section 11.3): the original markdown / command /
/// output, not the rendered display lines, so a browse yank can be
/// dropped into a `.md` file or re-sent to the agent. Events with no
/// shareable body yield the empty string.
pub fn raw_event_text(e: &crate::event::Event) -> String {
    match e.kind() {
        EventKind::UserMessage | EventKind::AssistantMessage => {
            e.get_str("content").unwrap_or("").to_string()
        }
        EventKind::ToolCall => {
            // The bash command is the interesting part; any other
            // tool carries its raw arguments JSON.
            if e.get_str("name") == Some("bash") {
                e.get("arguments")
                    .and_then(|a| a.get("command"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                e.get("arguments")
                    .map(|v| v.to_string())
                    .filter(|s| s != "null")
                    .unwrap_or_default()
            }
        }
        EventKind::ToolResult => {
            let value = e.get("value");
            match value {
                Some(v) => {
                    if let Some(t) = v.get("text") {
                        t.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| t.to_string())
                    } else if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                }
                None => String::new(),
            }
        }
        EventKind::ApprovalRequest => e.get_str("prompt").unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// The input snapshot of one transcript build
/// (docs/tui-perf-background-build-plan.md, stage 1).
///
/// It carries every input the build reads from the app.
/// The width is part of the snapshot.
/// Ext replies are pre-resolved on the main thread.
///
/// Every field is `Clone` and `Send`.
/// The snapshot is a cheap move, not a borrow.
/// Stage 2 sends it to the background worker.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptBuildInput {
    /// Events of the active session, oldest first.
    /// A tail-window build holds only the newest suffix of the log.
    pub events: Vec<Event>,
    /// 1-based log seq of the first in-memory event.
    /// For a tail-window build this is the full-log base seq shifted
    /// forward by the events dropped, so log seq math stays exact.
    pub events_base_seq: usize,
    /// First in-memory event index this input covers.
    /// `0` for a full build.
    /// A tail-window build holds the newest suffix of the log,
    /// and this field is that suffix's first in-memory index.
    /// (docs/tui-perf-background-build-plan.md, stage 2.)
    pub event_offset: usize,
    /// Tool-call details: call id to (name, arguments).
    pub call_details: std::collections::HashMap<String, (String, serde_json::Value)>,
    /// The oldest pending approval, if any.
    pub pending: Option<crate::app::PendingApproval>,
    /// The palette the built-in styles lower to.
    pub palette: crate::color::Palette,
    /// The tool-result display config.
    pub tool_display: crate::tool_display::ToolDisplay,
    /// The global tool fold/expand toggle.
    pub tool_expanded: bool,
    /// Thinking-block visibility.
    pub thinking_shown: bool,
    /// Thinking-block expand state.
    pub thinking_expanded: bool,
    /// Per-block expand fractions for animations.
    pub expand_fracs: std::collections::HashMap<String, f64>,
    /// Active-path ranges of the log.
    /// `None` means no rewind marker.
    pub rewind_active_ranges: Option<Vec<(usize, usize)>>,
    /// The active session, if any.
    pub active: Option<crate::port::SessionId>,
    /// Whether the active session loop is running.
    pub loop_running: bool,
    /// The render width for this build.
    pub width: usize,
    /// Ext render state. `None` means no ext host was supplied and
    /// the plain markdown engine renders the message content.
    /// `Some` activates the block markdown engine and carries the
    /// pre-resolved transform replies.
    pub ext_data: Option<ExtRenderData>,
    /// Pre-resolved ext reply lines, keyed by event id.
    /// The key is the in-memory event index.
    pub ext_lines: std::collections::HashMap<u64, Vec<crate::ext::ExtLine>>,
    /// The open turns of the turn fold (docs/tui-turn-fold.md).
    /// A turn in the set renders its full body. A turn outside the
    /// set is collapsed to its user box and tally. The fold applies
    /// in the main view and the browse view alike.
    pub turn_fold: std::collections::HashSet<u64>,
}

/// The ext-side inputs of a transcript build, decoupled from the
/// ext host (docs/tui-perf-background-build-plan.md, stage 1).
///
/// The host holds an `mpsc` receiver and is not `Send`.
/// This owned value carries everything the build reads from it.
/// The main thread pre-resolves the fields before dispatching
/// the snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtRenderData {
    /// Pre-resolved transform replies for the mermaid and latex
    /// spans, keyed by (event id, span index). A missing entry
    /// renders the raw span (G5 fallback).
    pub span_lines: std::collections::HashMap<(u64, u32), Vec<crate::ext::ExtLine>>,
}

impl TranscriptBuildInput {
    /// Snapshot every input the build reads from the app.
    ///
    /// The ext lines are pre-resolved against the host.
    /// The result is usable without the host.
    pub fn from_app(
        app: &crate::app::App,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> Self {
        Self::from_app_tail(app, width, ext, app.events().len())
    }

    /// Snapshot the newest `tail_events` events only.
    /// The first-build fast path (docs/tui-perf-background-build-plan.md,
    /// stage 2) renders the visible tail window on the main thread.
    /// The result is a partial build until the full build lands.
    /// Event ids and log seqs stay aligned with the full log.
    pub fn from_app_tail(
        app: &crate::app::App,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
        tail_events: usize,
    ) -> Self {
        let all = app.events();
        let offset = all.len().saturating_sub(tail_events.max(1));
        let events = &all[offset..];
        Self {
            events: events.to_vec(),
            events_base_seq: app.events_base_seq() + offset,
            event_offset: offset,
            call_details: app.call_details(),
            pending: app.oldest_pending_approval(),
            palette: app.palette().clone(),
            tool_display: *app.tool_display(),
            tool_expanded: app.tool_expanded(),
            thinking_shown: app.thinking_shown(),
            thinking_expanded: app.thinking_expanded(),
            expand_fracs: app.expand_fracs().clone(),
            rewind_active_ranges: app.rewind_active_ranges(),
            active: app.active().cloned(),
            loop_running: app.active().is_some_and(|s| app.loop_running(s)),
            width,
            ext_data: ext.as_ref().map(|host| ExtRenderData {
                span_lines: resolve_ext_spans_at(host, events, width, offset),
            }),
            ext_lines: resolve_ext_lines_at(ext, events, offset),
            turn_fold: app.fold_input(),
        }
    }
}

/// Pre-resolve ext reply lines for a set of events.
///
/// The ext host is not `Send`.
/// It cannot cross to the background worker.
/// The main thread resolves cached replies up front.
/// Each read is a synchronous cache lookup.
/// It returns an owned `Vec<ExtLine>`.
/// Events without a valid reply add no entry.
/// The build falls back to the built-in render.
#[allow(dead_code)]
pub fn resolve_ext_lines(
    ext: Option<&crate::ext::ExtHost>,
    events: &[Event],
) -> std::collections::HashMap<u64, Vec<crate::ext::ExtLine>> {
    resolve_ext_lines_at(ext, events, 0)
}

/// `resolve_ext_lines` with an in-memory index offset.
/// A tail-window build passes its suffix start so the keys are the
/// full in-memory event indices (docs/tui-perf-background-build-plan.md,
/// stage 2).
pub fn resolve_ext_lines_at(
    ext: Option<&crate::ext::ExtHost>,
    events: &[Event],
    offset: usize,
) -> std::collections::HashMap<u64, Vec<crate::ext::ExtLine>> {
    let mut out: std::collections::HashMap<u64, Vec<crate::ext::ExtLine>> =
        std::collections::HashMap::new();
    if let Some(host) = ext {
        for (i, e) in events.iter().enumerate() {
            let event_id = (offset + i) as u64;
            if let Some(owner) = host.owner_for_kind(e.kind()) {
                if let Some(lines) = host.lookup_lines(owner, event_id) {
                    out.insert(event_id, lines);
                }
            }
        }
    }
    out
}

/// Pre-resolve the transform replies of the message spans
/// (docs/tui-perf-background-build-plan.md, stage 1).
///
/// The host's `request_span` is a request to the extension process.
/// It must fire on the main thread.
/// Each span's finished reply is cached in the host.
/// This helper fires the requests and copies the finished replies
/// into an owned map the pure build can read.
/// Spans without a finished reply add no entry.
/// The build shows the raw span then (G5 fallback).
#[allow(dead_code)]
pub fn resolve_ext_spans(
    host: &crate::ext::ExtHost,
    events: &[Event],
    width: usize,
) -> std::collections::HashMap<(u64, u32), Vec<crate::ext::ExtLine>> {
    resolve_ext_spans_at(host, events, width, 0)
}

/// `resolve_ext_spans` with an in-memory index offset.
/// A tail-window build passes its suffix start so the span keys are
/// the full in-memory event indices (docs/tui-perf-background-build-plan.md,
/// stage 2).
pub fn resolve_ext_spans_at(
    host: &crate::ext::ExtHost,
    events: &[Event],
    width: usize,
    offset: usize,
) -> std::collections::HashMap<(u64, u32), Vec<crate::ext::ExtLine>> {
    use std::collections::HashMap;
    let mut out: HashMap<(u64, u32), Vec<crate::ext::ExtLine>> = HashMap::new();
    // The same width the build passes to `event_lines`.
    let ew = width.max(GUTTER + 8);
    for (i, e) in events.iter().enumerate() {
        let event_id = (offset + i) as u64;
        let (content, wrap_w) = match e.kind() {
            EventKind::UserMessage => (
                e.get_str("content")
                    .unwrap_or("[missing content]")
                    .to_string(),
                user_box_content_w(ew),
            ),
            EventKind::AssistantMessage => (
                e.get_str("content").unwrap_or("").to_string(),
                assistant_body_content_w(ew),
            ),
            _ => continue,
        };
        for block in message_blocks(&content) {
            match block {
                MBlock::Mermaid { idx, text, .. } => {
                    if host
                        .request_span(event_id, idx, "fence:mermaid", &text, wrap_w)
                        .is_some()
                    {
                        if let Some(lines) = host.span_lines(event_id, idx) {
                            out.insert((event_id, idx), lines);
                        }
                    }
                }
                MBlock::Text { parts, .. } => {
                    for line_parts in parts {
                        for part in line_parts {
                            if let Part::Latex { idx, text, .. } = part {
                                if host
                                    .request_span(event_id, idx, "inline:latex", text, wrap_w)
                                    .is_some()
                                {
                                    if let Some(lines) = host.span_lines(event_id, idx) {
                                        out.insert((event_id, idx), lines);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Build the transcript from an input snapshot.
///
/// This is a pure function of the snapshot.
/// The per-event loop of [`event_lines`] is unchanged.
/// No app or ext-host state is read.
/// The build can run on the background worker.
/// (docs/tui-perf-background-build-plan.md, stage 1.)
fn fold_summary_line(
    sl: &crate::fold::SummaryLine,
    state: &RenderState,
    width: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ".repeat(width.min(GUTTER)))];
    if !sl.text.is_empty() {
        let dim = state.palette.style(crate::color::Role::Hint, Modifier::DIM);
        // The `⎿` leader art marks the collapsed-turn tally
        // (docs/tui-turn-fold.md "Summary line").
        spans.push(Span::styled("⎿ ", dim));
        spans.push(Span::styled(sl.text.clone(), dim));
    }
    Line::from(spans)
}

pub fn build_transcript_input(input: &TranscriptBuildInput) -> TranscriptBuild {
    let events = &input.events;
    let details = &input.call_details;
    let pending = input.pending.is_some();
    // No render cap: every in-memory event renders so the whole
    // session history stays reachable (docs/tui-conversation-
    // browsing.md section 4.6). The log file is the record.
    let start = 0;
    // The in-memory index of the first event this input covers.
    // A tail-window input holds a suffix of the log, and its
    // event ids and log seqs shift by this offset.
    let offset = input.event_offset;
    // The tool_result ids of the visible window: a tool_call whose
    // result follows merges into the result box (the call line drops
    // for the tools whose result carries the call info).
    let result_ids: std::collections::HashSet<String> = events[start..]
        .iter()
        .filter(|e| e.kind() == EventKind::ToolResult)
        .filter_map(|e| e.get_str("id").map(String::from))
        .collect();
    // The render state (docs/tui-tool-display-port.md section 2, the
    // config part, plus the fold and thinking toggles).
    let state = RenderState {
        palette: &input.palette,
        tool_display: &input.tool_display,
        tool_expanded: input.tool_expanded,
        thinking_shown: input.thinking_shown,
        thinking_expanded: input.thinking_expanded,
        expand_fracs: &input.expand_fracs,
    };
    // The loop running bit, precomputed on the main thread.
    let running = input.loop_running;
    // The last open compaction marker: a `compaction_started` with
    // no later `compaction_summary` or `compaction_failed` in the
    // visible window. A restarted loop may add more open markers;
    // the renderer closes the first one, and only the last open
    // marker renders the live or interrupted form.
    let mut last_open: Vec<bool> = vec![false; events.len() - start];
    let open_started: Vec<usize> = (0..events.len() - start)
        .filter(|&i| events[start + i].kind() == EventKind::CompactionStarted)
        .filter(|&i| {
            !(start + i + 1..events.len()).any(|j| {
                matches!(
                    events[j].kind(),
                    EventKind::CompactionSummary | EventKind::CompactionFailed
                )
            })
        })
        .collect();
    if let Some(&i) = open_started.last() {
        last_open[i] = true;
    }
    let mut all: Vec<Line<'static>> = Vec::new();
    let mut line_raw: Vec<Option<String>> = Vec::new();
    let mut block_spans: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    // First screen-line index of each in-memory event, indexed by its
    // position in the full log window. `None` for events outside the
    // build window (the tail-window fast build keeps the head `None`,
    // docs/tui-perf-background-build-plan.md stage 2) and for events
    // dropped by the active-path mask (rewind, see below).
    let mut event_line_starts: Vec<Option<usize>> = vec![None; offset + events.len()];
    // The active-path ranges of the current log (docs/rewind-fork-
    // design.md section 3, docs/tree-ui-design-from-human.md). When a
    // rewind marker exists, off-path events are dropped from the
    // transcript; only the active path renders. `None` when there is
    // no marker: the full log is active.
    let active_ranges = &input.rewind_active_ranges;
    let base = input.events_base_seq;
    // The ext-side state for the built-in fallback path. `None` keeps
    // the plain markdown engine.
    let ext_data: Option<&ExtRenderData> = input.ext_data.as_ref();
    // The turn fold is active in every build (docs/tui-turn-fold.md):
    // the main view and the browse view share the same open-turn set.
    let fold = crate::fold::FoldState::new(
        events,
        input.events_base_seq,
        running,
        input.turn_fold.clone(),
    );
    for (i, e) in events[start..].iter().enumerate() {
        // ext_status is shared UI state: suppressed from the transcript
        // by default. ext_status events add no rows, and add no blank
        // separators. The log keeps ext_status events
        // (docs/ui-extension.md 5).
        if e.kind() == EventKind::ExtStatus {
            continue;
        }
        let w = start + i;
        // Active-path mask (docs/rewind-fork-design.md section 3,
        // docs/tree-ui-design-from-human.md): with a rewind marker in
        // the log, off-path events are dropped from the transcript.
        // The transcript shows the active path only. Branch visibility
        // is owned by the `tree` palette and its preview pane. Rewind
        // markers still render: they mark the fork boundary. The log
        // seq of this in-memory event is `base + (start + i)` (base is
        // the 1-based seq of the first in-memory event).
        let gseq = base + start + i;
        let off_path = active_ranges
            .as_ref()
            .is_some_and(|r| !rushi_common::rewind::seq_in_ranges(gseq, r));
        // A rewind marker is a structural fork boundary, not
        // conversation content. It always renders: even when it sits
        // off the active path, and even when a collapsed turn's span
        // would fold it away. Every other off-path event is dropped.
        let is_rewind = e.kind() == EventKind::Rewind;
        if off_path && !is_rewind {
            continue;
        }
        if !is_rewind && !fold.visible(w) {
            continue;
        }
        let turn_start_summary: Option<crate::fold::SummaryLine> = fold
            .turn_for_event(w)
            .filter(|t| w == t.start)
            .and_then(|t| fold.collapsed_summary(events, t));
        if !all.is_empty() {
            all.push(Line::from(""));
            line_raw.push(None);
        }
        let event_id = (offset + start + i) as u64;
        // The idle reply of a completed turn renders in the `Report`
        // panel. `final_msg` names it. The live tail of a running turn
        // is never boxed.
        let final_report = e.kind() == EventKind::AssistantMessage
            && fold
                .turn_for_event(w)
                .is_some_and(|t| Some(w) == t.final_msg && !t.in_progress);
        // Record the start of a tool-result block for click hit-testing.
        let tr_start = if e.kind() == EventKind::ToolResult {
            Some(all.len())
        } else {
            None
        };
        let (segs, raws): (Vec<Line<'static>>, Vec<Option<String>>) =
            if let Some(lines) = input.ext_lines.get(&event_id) {
                // Pre-resolved ext reply: the extension's styled lines
                // replace the built-in render.
                let lines = ext_lines_guttered(lines, input.width);
                let raws: Vec<Option<String>> = vec![None; lines.len()];
                (lines, raws)
            } else {
                // No valid reply for this event: the built-in render
                // is the fallback (per-op G5 fallback).
                let builder = event_lines()
                    .e(e)
                    .pending(pending)
                    .call_details(details)
                    .result_ids(&result_ids)
                    .width(input.width.max(GUTTER + 8))
                    .event_id(event_id)
                    .state(&state)
                    .loop_running(running)
                    .compaction_last_open(last_open.get(i).copied().unwrap_or(false))
                    .final_report(final_report);
                match ext_data {
                    Some(data) => builder.ext(data).call(),
                    None => builder.call(),
                }
            };
        event_line_starts[offset + start + i] = Some(all.len());
        all.extend(segs);
        line_raw.extend(raws);
        // Record the end of the tool-result block span.
        if let Some(s) = tr_start {
            if let Some(id) = e.get_str("id") {
                block_spans.insert(id.to_string(), (s, all.len()));
            }
        }
        if let Some(sl) = turn_start_summary {
            all.push(fold_summary_line(&sl, &state, input.width));
            line_raw.push(None);
        }
    }
    // The display text of each line, for the browse layout. Computed
    // once per build here, not per frame.
    let texts: Vec<String> = all.iter().map(|l| l.to_string()).collect();
    TranscriptBuild {
        lines: all,
        line_raw,
        block_spans,
        event_line_starts,
        texts,
    }
}

/// Build the transcript from the app: the rendered lines plus the
/// line-to-event map a browse yank needs
/// (docs/tui-conversation-browsing.md section 11.3).
///
/// The `&App` form kept for tests and the main-thread fallback.
/// It snapshots the app on the spot, then runs the pure build.
/// The background worker calls [`build_transcript_input`] with a
/// dispatched [`TranscriptBuildInput`] instead.
pub fn build_transcript(
    app: &App,
    width: usize,
    ext: Option<&crate::ext::ExtHost>,
) -> TranscriptBuild {
    let input = TranscriptBuildInput::from_app(app, width, ext);
    build_transcript_input(&input)
}

/// Fast tail-window build (docs/tui-perf-background-build-plan.md, stage 2).
///
/// Renders only the newest `tail_events` events, so a first build or a
/// transient re-build stays O(viewport). The `event_line_starts` output
/// is padded back to the full event length with `None`, so event-index
/// addressing stays valid. The full background build follows and
/// replaces this partial result.
pub fn build_transcript_tail(
    app: &App,
    width: usize,
    ext: Option<&crate::ext::ExtHost>,
    tail_events: usize,
) -> TranscriptBuild {
    let input = TranscriptBuildInput::from_app_tail(app, width, ext, tail_events);
    build_transcript_input(&input)
}

/// The transcript lines, oldest first (the legacy signature: the map
/// of [`build_transcript`] is dropped). Test convenience: production
/// builds use [`build_transcript`].
#[cfg(test)]
pub fn build_transcript_lines(
    app: &App,
    width: usize,
    ext: Option<&crate::ext::ExtHost>,
) -> Vec<Line<'static>> {
    build_transcript(app, width, ext).lines
}

/// Extension reply lines into the transcript: the extension returns
/// width-independent styled lines; the host wraps them to the pane
/// width with the same gutter as the built-in render (docs/ui-
/// extension.md section 4).
///
/// Each ext line is wrapped independently to preserve line
/// boundaries. This is essential for multi-span lines (such as
/// split diff rows) that rely on their spans staying on the
/// same terminal row.
fn ext_lines_guttered(lines: &[crate::ext::ExtLine], width: usize) -> Vec<Line<'static>> {
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.spans.len() > 1 {
            // Multi-span line (e.g. split-diff row): the ext pre-formats
            // the layout. Do not word-wrap; emit the spans as-is so the
            // two-column structure stays intact. The terminal clips any
            // overflow.
            let mut spans = vec![Span::raw(gutter.clone())];
            for s in &line.spans {
                spans.push(Span::styled(s.text.clone(), s.style));
            }
            // Pad to full width so the card background fills the row.
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            if used < width {
                let pad_style = line.spans.last().map(|s| s.style).unwrap_or_default();
                spans.push(Span::styled(" ".repeat(width - used), pad_style));
            }
            out.push(Line::from(spans));
        } else {
            // Single-span line: word-wrap to the pane width.
            let segs: Vec<(Style, String)> = line
                .spans
                .iter()
                .map(|s| (s.style, s.text.clone()))
                .collect();
            let wrapped = wrap_styled_continuous(&segs, wrap_w);
            for wl in &wrapped {
                let mut spans = vec![Span::raw(gutter.clone())];
                spans.extend(wl.spans.iter().cloned());
                // Pad to full width so the card background fills the row.
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                if used < width {
                    let pad_style = wl.spans.last().map(|s| s.style).unwrap_or_default();
                    spans.push(Span::styled(" ".repeat(width - used), pad_style));
                }
                out.push(Line::from(spans));
            }
        }
    }
    out
}

/// The waiting-message cap of one pending block: the header row
/// plus the listed rows stay inside five terminal rows. The rest
/// counts in the `+N more` row. The full content renders in the
/// transcript when the loop answers the message.
const PENDING_MESSAGE_CAP: usize = 3;

/// The pending-message blocks of the active session (docs/tui-
/// pending-user-messages.md stage 2): the two delivery queues, each
/// with its count and its previews. The stage-1 steering block is
/// replaced by the pair: the steer queue lists the messages that
/// inject at the next step (or wait for a loop start when stopped),
/// and the follow queue lists the messages that run after the loop
/// would stop, as new turns. Each preview row owns one terminal
/// row: previews truncate to the width.
pub fn pending_message_lines(app: &App, running: bool, row_width: usize) -> Vec<Line<'static>> {
    let steer = app.pending_steering();
    let follow = app.pending_follows();
    if steer.is_empty() && follow.is_empty() {
        return Vec::new();
    }
    let prose = app
        .palette()
        .style(crate::color::Role::PlainText, Modifier::empty());
    let mut out: Vec<Line<'static>> = Vec::new();
    // The steer block: the stage-1 label states the delivery the
    // loop actually performs. Running: injected at the next step.
    // Stopped: the messages wait for a loop start.
    if !steer.is_empty() {
        let (label, dim) = if running {
            ("steering — injected at the next step", false)
        } else {
            ("no loop running — waiting for Ctrl+R run", true)
        };
        out.extend(pending_block_lines(
            &steer,
            label,
            dim,
            running,
            row_width,
            &prose,
            app.palette(),
        ));
    }
    // The follow block: the messages run after the loop would stop,
    // as new turns.
    if !follow.is_empty() {
        out.extend(pending_block_lines(
            &follow,
            "follow-up — run after the loop stops",
            false,
            running,
            row_width,
            &prose,
            app.palette(),
        ));
    }
    out
}

/// One pending block: the header row (the count and the delivery
/// hint), the preview rows capped at [`PENDING_MESSAGE_CAP`], and
/// the `+N more` row for the rest. `waiting` dims the header of a
/// stopped-loop steer block: the messages wait, nothing injects.
fn pending_block_lines(
    pending: &[&crate::event::Event],
    label: &str,
    waiting: bool,
    running: bool,
    row_width: usize,
    prose: &Style,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    let n = pending.len();
    let what = if n == 1 {
        "1 message".to_string()
    } else {
        format!("{n} messages")
    };
    // The header is ` {what} waiting — {label}`. The label owns its
    // full width; the what part gets the rest. A zero budget drops
    // the what part (the ellipsis would own a column the row does
    // not have).
    let label_w = label.chars().count();
    let what_budget = row_width.saturating_sub(label_w + 4);
    let what_cell = if what_budget == 0 {
        String::new()
    } else {
        trunc(&format!("{what} waiting"), what_budget)
    };
    let mut out: Vec<Line<'static>> = vec![Line::from(Span::styled(
        format!(" {what_cell} — {label}"),
        if waiting {
            palette.style(crate::color::Role::Hint, Modifier::BOLD | Modifier::DIM)
        } else {
            palette.style(crate::color::Role::Status, Modifier::BOLD)
        },
    ))];
    for (i, ev) in pending.iter().enumerate().take(PENDING_MESSAGE_CAP) {
        let content = ev
            .get_str("content")
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        let prefix = format!("  {}. ", i + 1);
        let max = row_width
            .saturating_sub(prefix.chars().count())
            .saturating_sub(1);
        out.push(Line::from(Span::styled(
            format!("{prefix}{}", trunc(content, max)),
            *prose,
        )));
    }
    if n > PENDING_MESSAGE_CAP {
        out.push(Line::from(Span::styled(
            format!("  +{} more", n - PENDING_MESSAGE_CAP),
            palette.style(crate::color::Role::Hint, Modifier::DIM),
        )));
    }
    let _ = running;
    out
}

/// The input box title (the box top border text).
///
/// The search prompt wins: in command-line mode the host renders the
/// prompt (`/pat█`) even when a frame extension labels the frame, so
/// the typed pattern stays visible (the host keeps its modal-state
/// render, docs/ui-extension.md section 10 and docs/vim-editor-design.md
/// section 7). Otherwise a frame label replaces the built-in mode
/// label. Without a frame label, the mode label shows
/// (`[NORMAL]`, `[d-PENDING]`, ...).
fn input_box_title(
    editor: &crate::vim_editor::Editor,
    mode_label: &str,
    frame: &Option<crate::ext::FrameSpec>,
    border_color: Color,
    browse_prompt: Option<&str>,
) -> Line<'static> {
    // The browse command line owns the title while it is open
    // (docs/tui-conversation-browsing.md sections 4.4 and 7.3):
    // the `:N` goto and the `/` / `?` pattern, like the editor's
    // search prompt.
    if let Some(p) = browse_prompt {
        return Line::from(Span::styled(
            format!("{p}█"),
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(prompt) = editor.command_line_label() {
        Line::from(Span::styled(
            prompt,
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
    } else if let Some((flabel, _)) = frame.as_ref().and_then(|f| f.label.as_ref()) {
        // Each label line is a list of styled spans; the title is the
        // first line's spans, in order.
        Line::from(
            flabel
                .first()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| Span::styled(s.text.clone(), s.style))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
    } else {
        Line::from(Span::styled(
            mode_label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
    }
}

/// The whole frame: bordered panel with session title, transcript,
/// optional approval banner, the reserved working row, the input
/// line, and the status/help row.
/// When a status extension exists, its row owns that last line
/// (ui-extension-plan stage 1 layout); otherwise the built-in
/// help/status content shows there.
pub fn draw(
    f: &mut Frame,
    app: &mut App,
    cursor: &mut Option<(u16, u16)>,
    host: &crate::ext::ExtHost,
) {
    *cursor = None;
    let full = f.area();
    if full.width < 12 || full.height < 6 {
        return;
    }
    // One column of left/right margin (user directive): the main content
    // does not touch the terminal's left/right edge. The inset is a layout
    // concern — it reserves one column on each side for the whole content
    // region (title rule, transcript, input box, status rows).
    let area = if full.width >= 2 {
        ratatui::layout::Rect {
            x: full.x + 1,
            y: full.y,
            width: full.width - 2,
            height: full.height,
        }
    } else {
        full
    };

    // The active session id and the status bits need no live borrow:
    // the cache rebuild below takes a mutable borrow of `app`.
    let active = app.active().cloned();
    let session_label = match &active {
        Some(s) => s.to_string(),
        None => match app.pending_name() {
            Some(n) => format!("new — {n}"),
            None => "none".to_string(),
        },
    };
    let running = active
        .as_ref()
        .map(|s| app.loop_running(s))
        .unwrap_or(false);
    // The transcript build bit (docs/tui-perf-background-build-plan.md,
    // stage 2): a rebuild is recorded or in flight on the worker.
    let rebuilding = app.transcript_rebuilding();
    // The loop-phase bit (docs/tui-model-wait-indicator.md): a
    // running loop names its phase (`[wait]`, `[working]`,
    // `[tools]`); an idle loop or an unknown marker keeps the plain
    // bit.
    let phase = phase_state(app, running);
    // The palette colors as owned values: the palette borrow must
    // end before the mutable app borrows below (the viewport and
    // transcript lines).
    let success_fg = app.palette().color(crate::color::Role::Success);
    let status_fg = app.palette().color(crate::color::Role::Status);
    let warning_fg = app.palette().color(crate::color::Role::Warning);
    // The loop-phase bit in the pi accents: `success` while the loop
    // runs, the `dim` tone when idle (not hard-coded swatches).
    let mut status_bits: Vec<Span<'static>> = vec![Span::styled(
        phase_bit(phase),
        Style::default()
            .fg(if running { success_fg } else { status_fg })
            .add_modifier(Modifier::BOLD),
    )];
    if app.other_running_loops() > 0 {
        status_bits.push(Span::styled(
            format!(" +{} loop", app.other_running_loops()),
            // The pi `warning` accent for the extra running loops.
            Style::default().fg(warning_fg),
        ));
    }
    let title_left = Line::from(vec![Span::styled(
        format!("Session: {session_label}"),
        Style::default().add_modifier(Modifier::BOLD),
    )]);
    let block = Block::new()
        .borders(Borders::TOP)
        .title(title_left)
        .title(Line::from(status_bits).right_aligned())
        .border_style(Style::default());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Rows inside the border:
    //   transcript (fill)
    //   waiting messages (0..=4, only when messages wait)
    //   approval banner (1, only when pending)
    //   working row (1, while the loop runs: spinner + phase)
    //   input line (1)
    //   help/status row (1)
    let banner = app.oldest_pending_approval().is_some();
    // The input-area frame: the `frame` extension's last valid spec,
    // or the host built-in (rounded border, thinking-level color).
    // The frame owns the border style, label, and interior height;
    // it never owns the draft content (docs/ui-extensions design:
    // the input area is customizable, not hardwired).
    let frame = host.frame_spec();
    // The interior width a draft line wraps to: the input box spans the
    // full main-interior width, its border takes two columns. Every
    // wrapping / sizing / scroll below keys off this so a long line
    // breaks at the box edge instead of running off it.
    let input_wrap_w = inner.width.saturating_sub(2) as usize;
    // Default interior: fit the draft (2..=6 display rows) so a
    // multi-line message is shown in full. A `frame` extension's
    // explicit height still overrides (the input area is customizable,
    // not hardwired).
    let input_interior = frame
        .as_ref()
        .and_then(|f| f.height)
        .unwrap_or_else(|| app.draft_lines(input_wrap_w).clamp(2, 6));
    // The bordered box is the interior rows plus a top and bottom
    // border row each.
    let input_area_h = (input_interior + 2) as u16;
    // Status/help row content, computed before the layout: the layout
    // reserves one terminal row per status line. A status extension
    // reply may carry two lines (the narrow two-line layout,
    // ui-extension-plan stage 2).
    let status_lines = status_rows(app, host, running, inner.width as usize);
    let status_n = status_lines.len() as u16;
    // The waiting-message block owns one layout cell for its rows
    // (docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    let pending_rows = pending_message_lines(app, running, inner.width as usize);
    // The in-progress model response now renders inside the transcript
    // (docs/tui-streaming-simplify.md section 3) instead of a separate
    // pinned layout cell between the transcript and the working row.
    // No dedicated layout cell is allocated for it: the transcript is
    // the fill cell, so the layout no longer shifts when a response
    // starts or completes. The stream lines are computed later, once
    // the transcript text width is known, and appended to the tail of
    // the transcript so the user can scroll up through the whole body.
    let mut constraints: Vec<Constraint> = vec![Constraint::Min(2)];
    if !pending_rows.is_empty() {
        constraints.push(Constraint::Length(pending_rows.len() as u16));
    }
    if banner {
        constraints.push(Constraint::Length(1));
    }
    // The working row above the input box: the loop-phase spinner
    // and text while the loop runs (docs/tui-model-wait-indicator.md
    // section 3). No row when idle: the transcript absorbs it. The
    // input box shifts one row at the loop start/stop transition,
    // like the `pi` working indicator. The transcript rebuild
    // indicator (docs/tui-perf-background-build-plan.md, stage 2)
    // reserves the same row while a build is in flight.
    if running || rebuilding {
        constraints.push(Constraint::Length(1));
    }
    // The host-reserved row above the input box (docs/ui-extension.md
    // section 4, `row` capability): the row owners' last valid
    // `row_spec` lines stacked in sequence order, one layout cell
    // per line. No cell when no row extension is installed, or when
    // every owner's content is empty (the bare TUI shows no row).
    let row_lines = host.row_spec().unwrap_or_default();
    let row_cells: Vec<Line<'static>> = row_lines
        .iter()
        .map(|l| {
            Line::from(
                l.spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), s.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    for _ in &row_cells {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(input_area_h));
    constraints.push(Constraint::Length(status_n));
    let rows = ratatui::layout::Layout::vertical(constraints).split(inner);

    // transcript
    let t_area = rows[0];
    // The transcript spans the full width of its layout cell. The
    // borderless tool panel (the 2026-09-14 user pass) reaches the
    // right edge of the cell, so only the position bar and the browse
    // gutter reserve their own columns (the 2026-09-14 follow-up:
    // span the tool result box full width, no dead column on the right).
    let t_width = t_area.width as usize;
    let h = t_area.height as usize;
    let scroll0 = app.scroll();
    app.set_viewport_height(h);
    // The position bar show decision (section 3): browse shows it
    // always; normal mode shows it when the view left the tail.
    // While shown, the bar owns the rightmost transcript column.
    let browse_active = app.browse_ref().active();
    let bar_shown = browse_active || scroll0 > 0;
    let bar_w = if bar_shown { 1 } else { 0 };
    // The text width with the bar and the browse gutter reserved
    // (sections 3 and 4.3): the width drives the wrap, the wrap the
    // total, the total the gutter width: iterate to the fixpoint.
    let (text_w, gutter_w) = resolve_transcript_width(app, host, t_width, bar_w, browse_active);
    // The total and the browse layout sync run before the lines
    // borrow: the cache holds the lines, so no app borrow may stay
    // live while the app mutates (the palette owned values above,
    // same pattern).
    // The in-progress model response renders at the tail of the
    // transcript (docs/tui-streaming-simplify.md section 3). `usize::MAX`
    // disables the sliding-window cap so the whole streamed body
    // (thinking + text + partial tool calls) stays reachable by
    // scrolling; the stream file and the settled log are unchanged.
    let stream_lines = if browse_active {
        StreamBlockView::empty()
    } else {
        stream_block_lines(app, text_w, usize::MAX)
    };
    let stream_len = stream_lines.len();
    // The settled lines come from the cached transcript (a `&[Line]`
    // borrow); the live stream tail is a small owned Vec. We do not
    // clone the full settled transcript each frame: the visible
    // window (at most `h` lines) is built on demand below, so a long
    // session costs O(visible + stream) per frame, not O(all lines).
    // `total` still spans the settled lines plus the live tail, so the
    // cursor clamp and the browse layout account for the live lines.
    let settled_len = app.transcript_lines(text_w, Some(host)).len();
    let total = settled_len + stream_len;
    // Store block spans and transcript top row for mouse-click hit-testing
    // (docs/tui-tool-display-fancy.md section 6). The spans cover only the
    // settled tool-result blocks; the live stream tail carries no block span.
    let spans = app.transcript_block_spans(text_w, Some(host));
    app.set_block_spans(spans);
    app.set_transcript_top_row(t_area.y);
    // No fixed scroll cap: the offset is bounded by the transcript.
    // Clamp to `total - h` so a shrunk tail (a settled stream) or a
    // long user scroll cannot overflow `scroll + h` below.
    let mut scroll = scroll0.min(total.saturating_sub(h));
    if browse_active {
        // A settled event landing, or a live stream tail change
        // (growth or a settle shrink), both count as a model-driven
        // tail change. The browse view pins on either instead of
        // re-centering (docs/tui-conversation-browsing.md section
        // 4.6).
        let grew = app.take_events_grew() || app.note_stream_changed(stream_len);
        app.browse().sync(total, h, &mut scroll, grew);
        app.set_scroll(scroll);
    }
    // View-only scroll (docs/tree-ui-design-from-human.md): the tree
    // picker set a one-shot target, the in-memory event index. Resolve
    // it to a transcript line and pin it at the top of the viewport.
    // The target is consumed by this frame; the sticky scroll then
    // holds until the user scrolls.
    if let Some(ev_idx) = app.take_view_only_target() {
        if let Some(line) = app
            .transcript_event_line_starts(text_w, Some(host))
            .get(ev_idx)
            .copied()
            .flatten()
        {
            if line < total {
                scroll = total.saturating_sub(line + h);
                app.set_scroll(scroll);
            }
        }
    }
    let start = total.saturating_sub(scroll + h);
    app.set_transcript_visible_start(start);
    // Build the visible window only: at most `h` lines, each read from
    // the settled cache slice or the stream tail. This is the draw
    // path's whole cost for a long transcript (no full-vector copy).
    let end = (start + h).min(total);
    let view_lines: Vec<Line<'static>> = {
        let settled = app.transcript_lines(text_w, Some(host));
        let mut v = Vec::with_capacity(end.saturating_sub(start));
        for g in start..end {
            let l: &Line<'static> = if g < settled_len {
                &settled[g]
            } else {
                &stream_lines[g - settled_len]
            };
            v.push(l.clone());
        }
        v
    };
    // The cursor col clamps to the visible cursor line length
    // (section 4.1): read from the visible window.
    if browse_active {
        let (cl, _) = app.browse_ref().line_col();
        if cl >= start && cl < end {
            let len = view_lines[cl - start]
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>();
            app.browse().clamp_col(len);
        }
    }
    // The press-path layout: the line texts and the width the
    // browse motions and the search read, plus the raw source texts
    // the browse yank prefers over the rendered lines (section 11.3).
    // The raw map extends the settled lines with one `None` per live
    // stream line (the live body is not yet a settled, shareable
    // event; yanks over it fall back to the rendered text).
    if browse_active {
        // The settled display texts come from the transcript cache
        // (built once per build, not per frame); the live tail is
        // stringified here (a small Vec). The combined texts keep the
        // browse search, motions, and yank over the full transcript.
        let settled_texts: Vec<String> = app.transcript_texts(text_w, Some(host)).clone();
        let stream_texts: Vec<String> = stream_lines.iter().map(|l| l.to_string()).collect();
        let mut texts = settled_texts;
        texts.extend(stream_texts);
        let mut line_raw: Vec<Option<String>> = app.transcript_raw(text_w, Some(host));
        line_raw.extend(std::iter::repeat_n(None, stream_len));
        app.set_browse_layout(total, h, texts, line_raw);
        let starts = app.transcript_event_line_starts(text_w, Some(host));
        app.set_event_line_starts(starts);
        app.apply_fold_cursor_target();
    }
    // The owned browse draw inputs: the cursor, the match-line
    // cache, the highlight styles. The cache clone is one pass per
    // frame; the styles own their colors, so the palette borrow
    // ends before the lines borrow.
    let cursor_pos = app.browse_ref().line_col();
    let (hl, active_match): (std::collections::HashSet<usize>, Option<(usize, usize)>) =
        if browse_active {
            app.browse_highlight(total)
        } else {
            (std::collections::HashSet::new(), None)
        };
    let pl = app.palette();
    let dim_style = pl.style(crate::color::Role::Status, Modifier::empty());
    let accent_style = pl.style(crate::color::Role::Border4, Modifier::empty());
    let cursor_bg = pl.color(crate::color::Role::CursorLine);
    let match_style = pl.style(crate::color::Role::Hint, Modifier::BOLD);
    let active_style = pl.style(crate::color::Role::Border4, Modifier::BOLD);
    let track_c = pl.color(crate::color::Role::Status);
    let thumb_c = pl.color(crate::color::Role::PlainText);
    let mark_c = pl.color(crate::color::Role::Border4);
    // The visual-selection shading (section 11.4): the selected span,
    // read through the browse accessor, and the `Role::Selection`
    // background tone.
    let selection = if browse_active {
        app.browse_ref().visual_selection()
    } else {
        None
    };
    let sel_bg = pl.color(crate::color::Role::Selection);
    // The visible window, at most `h` lines, built above from the
    // settled cache slice and the stream tail. `start` is the global
    // index of the first window row.
    let window = view_lines;
    if window.is_empty() {
        let placeholder = match app.active() {
            // The placeholder in the pi `dim` tone (not a hard-coded gray).
            Some(_) => Line::from(Span::styled(
                " (no events yet — type a message below, then Ctrl+R to run the loop)",
                Style::default().fg(status_fg),
            )),
            None => Line::from(Span::styled(
                " (no session yet — type the new session name below, Enter confirms)",
                Style::default().fg(status_fg),
            )),
        };
        let p = Paragraph::new(vec![placeholder]);
        f.render_widget(p, t_area);
    } else {
        let mut draw_lines = if browse_active {
            browse_window_lines(
                &window,
                start,
                h,
                gutter_w,
                cursor_pos,
                &hl,
                active_match,
                dim_style,
                accent_style,
                cursor_bg,
                match_style,
                active_style,
                selection,
                sel_bg,
            )
        } else {
            window.to_vec()
        };
        // Apply fade-in dimming to tool-result blocks that are fading in
        // (docs/tui-tool-display-fancy.md section 7). For each visible
        // line, check if it falls within a block span whose fade alpha
        // is below 1.0; if so, add `Modifier::DIM` to all spans.
        {
            let spans = app.block_spans();
            let fading: std::collections::HashSet<&String> = spans
                .keys()
                .filter(|id| app.fade_alpha(id) < 0.999)
                .collect();
            if !fading.is_empty() {
                for (offset, line) in draw_lines.iter_mut().enumerate() {
                    let idx = start.saturating_add(offset);
                    if spans
                        .iter()
                        .any(|(id, &(s, e))| fading.contains(id) && idx >= s && idx < e)
                    {
                        for span in &mut line.spans {
                            span.style = span.style.add_modifier(Modifier::DIM);
                        }
                    }
                }
            }
        }
        let p = Paragraph::new(draw_lines);
        f.render_widget(p, t_area);
        // The position bar: one column at the right edge (section 3).
        if bar_shown {
            let cursor_line = if browse_active {
                let (cl, _) = cursor_pos;
                if cl >= start && cl < start + h {
                    Some(cl)
                } else {
                    None
                }
            } else {
                None
            };
            draw_position_bar(
                f,
                &t_area,
                total,
                scroll,
                cursor_line,
                track_c,
                thumb_c,
                mark_c,
            );
        }
    }

    let mut row = 1usize;
    // The in-progress stream content now renders inside the transcript
    // (docs/tui-streaming-simplify.md section 3): no dedicated layout
    // cell for it here. The `row` counter starts at 1 and tracks the
    // first post-transcript cell (pending rows, banner, working row,
    // host rows, input, status).

    // waiting messages: the steering list of the active session
    // (docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    // One layout cell for the block; one row per line, like the
    // status rows.
    if !pending_rows.is_empty() {
        let p_area = rows[row];
        for (i, l) in pending_rows.iter().enumerate() {
            if i as u16 >= p_area.height {
                break;
            }
            let sub = ratatui::layout::Rect {
                x: p_area.x,
                y: p_area.y + i as u16,
                width: p_area.width,
                height: 1,
            };
            f.render_widget(Paragraph::new(l.clone()), sub);
        }
        row += 1;
    }

    // approval banner
    if banner {
        let p = app.oldest_pending_approval().unwrap();
        let text = format!(
            "approval {} : {}   [y allow]  [n deny]  [e edit]",
            p.request_id,
            p.prompt
                .clone()
                .unwrap_or_else(|| "(no prompt)".to_string())
        );
        // The banner background is the pi `warning` accent (not a
        // hard-coded yellow); the text keeps the black-on-color chip
        // style.
        let l = Line::from(vec![Span::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(warning_fg)
                .add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(Paragraph::new(l), rows[row]);
        row += 1;
    }

    // working row: shown while the loop runs. The spinner and the
    // phase text. No row when idle (docs/tui-model-wait-indicator.md
    // section 3).
    if running {
        let now = chrono::Utc::now();
        f.render_widget(Paragraph::new(working_row(app, running, &now)), rows[row]);
        row += 1;
    } else if rebuilding {
        // The transcript build indicator: the working-row spinner
        // shape with the build label
        // (docs/tui-perf-background-build-plan.md, stage 2).
        let now = chrono::Utc::now();
        f.render_widget(Paragraph::new(rebuilding_row(app, &now)), rows[row]);
        row += 1;
    }

    // The host-reserved row above the input box (docs/ui-extension.md
    // section 4, `row` capability): the row owner's content, one cell
    // per line. No cell when no row extension is installed, or when
    // the owner's content is empty (the bare TUI shows no row).
    for l in &row_cells {
        f.render_widget(Paragraph::new(l.clone()), rows[row]);
        row += 1;
    }

    // input area: a bordered, rounded-corner box showing two (or the
    // frame spec's) editor lines, colored by the thinking level. The
    // frame extension may override the border style, label, and
    // interior height; the draft content and cursor stay host-owned.
    let i_area = rows[row];
    let naming = app.pending_name().is_some();
    let border_type = frame
        .as_ref()
        .and_then(|f| f.border)
        .map(border_style)
        .unwrap_or(BorderType::Rounded);
    let border_color = frame
        .as_ref()
        .and_then(|f| f.label.as_ref())
        .and_then(|(_, s)| s.fg)
        .unwrap_or_else(|| app.palette().thinking_border(app.thinking_level()));
    // The draft text's own color: the capability-aware prose color,
    // not the terminal default (color.rs: the "all gray by default"
    // complaint). The inverted caret cell is unchanged.
    let prose = app
        .palette()
        .style(crate::color::Role::PlainText, Modifier::empty());
    // The box border: the search prompt wins over the frame label
    // (the host keeps its modal-state render when a frame extension
    // owns the chrome, docs/ui-extension.md section 10); a frame
    // label otherwise replaces the built-in mode label.
    // The mode label first: it is a value, so the call below keeps
    // no second borrow of `app` (the editor borrow is the only one).
    let mode_label = app.editor_mode_label();
    // The browse command line prompt, when the overlay owns the
    // input (docs/tui-conversation-browsing.md section 4.4).
    let browse_prompt = if app.browse_ref().active() {
        app.browse_ref().prompt()
    } else {
        None
    };
    let title = input_box_title(
        &*app.editor(),
        &mode_label,
        &frame,
        border_color,
        browse_prompt.as_deref(),
    );
    let input_block = Block::bordered()
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner_i = input_block.inner(i_area);
    f.render_widget(input_block, i_area);

    // The editor lines. `naming` shows the single-line name input
    // (the session-name bar, pre-active-session). Otherwise the
    // multi-line editor, scrolled by `edit_scroll`. Long lines wrap at
    // the box edge (`input_wrap_w`); the box shows a window of
    // `input_interior` display rows, and the cursor stays in view.
    if !naming {
        app.editor_scroll_to_cursor(input_interior, input_wrap_w);
    }
    let scroll = app.edit_scroll();
    let ed_lines: Vec<String> = if naming {
        vec![app.pending_name().unwrap_or_default().to_string()]
    } else {
        app.editor()
            .display_rows(scroll, input_interior, input_wrap_w)
            .into_iter()
            .map(|r| r.text)
            .collect()
    };
    // One paragraph per editor line. No line-level highlight; the
    // cursor row shows a single inverted block cell at the caret
    // column so the position is always visible, and the hardware
    // cursor sits just after it. In wrapped mode the cursor row/col
    // are display-row based: the cursor may sit on a wrapped fragment
    // of a long line.
    let cursor_row = if naming {
        0
    } else {
        app.editor()
            .cursor_display(input_wrap_w)
            .0
            .saturating_sub(app.edit_scroll())
    };
    let cursor_col = if naming {
        // One past the rendered trailing `_`: the `> ` prefix plus
        // the name plus the underscore.
        app.pending_name().map_or(3, |n| 3 + n.chars().count())
    } else {
        app.editor().cursor_display(input_wrap_w).1
    };
    // The search command line owns the input: the prompt renders in
    // the box title and the text-area cursor block stays off. The
    // browse command line (docs/tui-conversation-browsing.md
    // section 4.4) does the same: the caret parks on the prompt.
    let in_command_line = app.editor().command_line_label().is_some()
        || (app.browse_ref().active() && app.browse_ref().typing());
    let top = inner_i.y;
    for (j, l) in ed_lines.iter().enumerate() {
        let y = top + j as u16;
        if y >= top + inner_i.height {
            break;
        }
        let sub = ratatui::layout::Rect {
            x: inner_i.x,
            y,
            width: inner_i.width,
            height: 1,
        };
        let line = if naming && j == 0 {
            Line::from(vec![
                Span::styled("> ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(l.clone(), prose),
                Span::styled("_", Style::default().add_modifier(Modifier::BOLD)),
            ])
        } else if j == cursor_row && !in_command_line {
            // The cursor row: render the characters up to the caret
            // in normal style, then one inverted block cell. Only
            // this one cell is inverted so the caret is always
            // visible even when the hardware cursor is not blinking.
            // The rest of the line stays plain. In the on-char
            // modes (normal, replace, visual) the block covers the
            // char under the cursor, so that char is drawn exactly
            // once (the highlighted cell); the insert caret is a
            // blank block cell that keeps the char under it.
            let (before, caret, after) =
                cursor_line_spans(l, cursor_col, app.editor().mode().cursor_on_char());
            let style = Style::default().bg(Color::White).fg(Color::Black);
            let mut spans = Vec::new();
            if !before.is_empty() {
                spans.push(Span::styled(before.iter().collect::<String>(), prose));
            }
            spans.push(Span::styled(caret.to_string(), style));
            if !after.is_empty() {
                spans.push(Span::styled(after.iter().collect::<String>(), prose));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(l.clone(), prose))
        };
        f.render_widget(Paragraph::new(line), sub);
    }
    // The cursor position: on the cursor line, on the block cell.
    // In command-line mode it lands on the prompt in the box title.
    if !app.should_quit() {
        if let Some(prompt) = app.editor().command_line_label() {
            let prompt_w = prompt.chars().count().saturating_sub(1);
            let cx = inner_i.x + 1 + prompt_w as u16;
            let cy = inner_i.y.saturating_sub(1);
            *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
        } else {
            let cy = inner_i.y + cursor_row as u16;
            // The hardware cursor lands on the block cell (the
            // visible caret). At end-of-line, `cursor_col` points
            // at the blank block cell, which is still inside the
            // row.
            let cx = inner_i.x + cursor_col as u16;
            if cy < inner_i.y + inner_i.height {
                *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
            }
        }
        // The browse caret (docs/tui-conversation-browsing.md
        // section 4.1): the block cell over the transcript row, or
        // the box title while the browse command line types.
        if app.browse_ref().active() {
            if let Some(prompt) = app.browse_ref().prompt() {
                let prompt_w = prompt.chars().count() + 1;
                let cx = inner_i.x + 1 + prompt_w as u16;
                let cy = inner_i.y.saturating_sub(1);
                *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
            } else {
                let (cl, cc) = app.browse_ref().line_col();
                if cl >= start && cl < start + h {
                    let cx = t_area.x + gutter_w as u16 + cc as u16;
                    let cy = t_area.y + (cl - start) as u16;
                    *cursor = Some((cx, cy));
                }
            }
        }
    }
    row += 1;

    // status/help row: the TUI flash wins; then the status extension
    // row (its lines or the dead hint); then the built-in content.
    // The status slot is one layout cell of `status_n` rows; split it
    // into single-row rects and render one line each.
    let s_area = rows[row];
    for (i, l) in status_lines.iter().enumerate() {
        if i as u16 >= s_area.height {
            break;
        }
        let sub = ratatui::layout::Rect {
            x: s_area.x,
            y: s_area.y + i as u16,
            width: s_area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(l.clone()), sub);
    }

    // The @ picker float (docs/tui-file-picker.md). Drawn last so it
    // overlays the transcript and input rows. The picker and the
    // browse overlay never coexist (section 5.3).
    let picker_open = app.picker_ref().open;
    if picker_open {
        let snap: Arc<crate::picker::fuzzy::Snapshot> = app
            .picker_matcher_ref()
            .as_ref()
            .map(|m| m.snapshot())
            .unwrap_or_else(|| {
                Arc::new(crate::picker::fuzzy::Snapshot {
                    items: Vec::new(),
                    query: String::new(),
                    settled: false,
                })
            });
        // The windowed preview model (docs/tui-preview-pane-plan.md):
        // no line cap. The pane highlights the visible window of the
        // settled background load, through the shared window LRU.
        let previewer = crate::picker::preview::FilePreviewer::new(app.preview_cache().clone());
        let show_preview = previewer.enabled()
            && app
                .picker_ref()
                .preview_visible(snap.items.len(), crate::picker::render::PREVIEW_CUTOFF);
        let layout = crate::picker::render::compute_float_layout(f.area(), show_preview);

        // Keep the visible window in sync with the pane height so
        // Ctrl-J/Ctrl-K paging stays within the rendered list area
        // (the list block's two border rows are not visible content).
        let rows = layout.list.height.saturating_sub(2).max(1) as usize;
        app.picker().visible = rows;

        // The picker input-bar hints. `Tab` completes the
        // highlighted item into the draft; `Ctrl+T` cycles the file
        // scope (docs/tree-ui-design-from-human-phase-2.md item 5);
        // `Ctrl+Shift+P` toggles list/preview focus (item 4).
        let mut hints = String::from(
            "enter ok · esc keep · ctrl-j/k move · ctrl-p preview · ctrl-shift-p focus · tab complete · ctrl-t scope",
        );
        if app.picker_ref().focus == crate::float::Focus::Preview {
            hints.push_str(" · preview focus");
        }
        // Clone the palette so the mutable picker borrow below does not
        // overlap an immutable borrow of the same app.
        let palette = app.palette().clone();
        crate::picker::render::render_picker()
            .f(f)
            .state(app.picker())
            .snapshot(&snap)
            .layout(&layout)
            .previewer(&previewer)
            .hints(&hints)
            .palette(&palette)
            .cursor(cursor)
            .call();
    }

    // The `:` command-palette float (docs/tui-command-palette.md).
    // Drawn last, over the transcript and input rows, just like the
    // picker. The palette and the picker never coexist (section 12).
    if app.palette_state().open {
        let items = app.palette_ranked();
        let show_preview = app
            .palette_state()
            .preview_visible(items.len(), crate::picker::render::PREVIEW_CUTOFF);
        let layout = crate::float::compute_float_layout(f.area(), show_preview);
        let rows = layout.list.height.saturating_sub(2).max(1) as usize;
        let palette = app.palette().clone();
        // Clone the tool-display config so the mutable palette-state
        // borrow below does not overlap an immutable borrow of the
        // same app.
        let tool_display = app.tool_display().clone();
        let pstate = app.palette_state_mut();
        pstate.visible = rows;
        pstate.sync(items.len());
        crate::palette::render::render_palette()
            .f(f)
            .state(pstate)
            .items(&items)
            .layout(&layout)
            .palette(&palette)
            .cursor(cursor)
            .tool_display(&tool_display)
            .call();
    }
}

/// The spans of the editor cursor row: `(before, caret, after)`.
/// `on_char` is true when the mode rests on the character at the
/// caret (normal / replace / visual): the block covers that char,
/// so it is drawn once and the line continues from the next char.
/// In insert the caret is a blank block cell and the char under it
/// stays drawn.
fn cursor_line_spans(line: &str, col: usize, on_char: bool) -> (Vec<char>, char, Vec<char>) {
    let chars: Vec<char> = line.chars().collect();
    let cc = col.min(chars.len());
    if on_char && cc < chars.len() {
        (chars[..cc].to_vec(), chars[cc], chars[cc + 1..].to_vec())
    } else {
        (chars[..cc].to_vec(), ' ', chars[cc..].to_vec())
    }
}
#[cfg(test)]
mod cursor_span_tests {
    use super::cursor_line_spans;
    use super::thinking_text;
    use super::wrap_thinking;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    use serde_json::json;

    fn text(before: &[char], caret: char, after: &[char]) -> String {
        let mut s: String = before.iter().collect();
        s.push(caret);
        s.extend(after.iter());
        s
    }

    /// Bug: the on-char cursor used to draw the covered char a
    /// second time (`[t]this`). The covered char must appear
    /// exactly once.
    #[test]
    fn on_char_cursor_does_not_duplicate_the_covered_char() {
        // Cursor on `t` of "this" (the reported `[t]this` case).
        let (b, c, a) = cursor_line_spans("this", 0, true);
        assert_eq!(text(&b, c, &a), "this", "the covered char is drawn once");
        // Cursor on `i`: `th[i]is` must stay `this`.
        let (b, c, a) = cursor_line_spans("this", 2, true);
        assert_eq!(text(&b, c, &a), "this");
        // Last char of the line.
        let (b, c, a) = cursor_line_spans("this", 3, true);
        assert_eq!(text(&b, c, &a), "this");
    }

    #[test]
    fn insert_caret_keeps_the_char_under_it() {
        // Insert mode, caret between `b` and `c` of "abc": a blank
        // block at the caret, the char under it stays.
        let (b, c, a) = cursor_line_spans("abc", 2, false);
        assert_eq!(c, ' ');
        assert_eq!(text(&b, c, &a), "ab c");
        // Caret past the end of the line.
        let (b, c, a) = cursor_line_spans("ab", 2, false);
        assert_eq!(text(&b, c, &a), "ab ");
    }

    #[test]
    fn empty_line_caret() {
        let (_b, c, _a) = cursor_line_spans("", 0, false);
        assert_eq!(c, ' ');
        // On-char on an empty line degrades to the blank cell.
        let (_b, c, _a) = cursor_line_spans("", 0, true);
        assert_eq!(c, ' ');
    }

    #[test]
    fn thinking_text_accepts_typed_and_typeless_entries() {
        // The typed `reasoning_text` entries and the plain text
        // entries of older logs both carry the reasoning text.
        let typed: Vec<serde_json::Value> = vec![json!({
            "content": [
                {"type": "reasoning_text", "text": "typed"},
                {"type": "image", "text": "nope"}
            ]
        })];
        assert_eq!(thinking_text(Some(&typed)).as_deref(), Some("typed"));
        let typeless: Vec<serde_json::Value> = vec![json!({"content": [{"text": "plain"}]})];
        assert_eq!(thinking_text(Some(&typeless)).as_deref(), Some("plain"));
        // The summary wins over the content entries.
        let summary: Vec<serde_json::Value> = vec![json!({
            "summary": [{"text": "summed"}],
            "content": [{"text": "plain"}]
        })];
        assert_eq!(thinking_text(Some(&summary)).as_deref(), Some("summed"));
        // A text-less item renders nothing.
        let empty: Vec<serde_json::Value> = vec![json!({"content": [{"type": "image"}]})];
        assert_eq!(thinking_text(Some(&empty)), None);
    }

    #[test]
    fn thinking_block_tables_draw_as_a_fixed_width_grid() {
        // The 2026-09-03 user report: a table inside an expanded
        // thinking block kept its raw `|` pipes and lost the fixed
        // column widths. The grid replaces the run of table lines.
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let text = "preamble\n| Level | Border |\n|---|---|\n| 0 | gray |\n| 1 | blue |\nafter";
        let lines = wrap_thinking(
            text,
            60,
            &palette,
            style,
            crate::tool_display::HighlightEngine::TreeSitter,
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let joined = text.join("\n");
        assert!(joined.contains('┌'), "the grid border: {joined:?}");
        assert!(joined.contains("Level"), "{joined:?}");
        assert!(joined.contains("gray"), "{joined:?}");
        // Every grid row has the same width: no clipped column.
        let widths: Vec<usize> = text
            .iter()
            .filter(|l| l.contains('│') || l.contains('─'))
            .map(|l| l.chars().count())
            .collect();
        let first = widths.first().copied().unwrap_or(0);
        assert!(
            !widths.is_empty() && widths.iter().all(|w| *w == first),
            "grid rows keep one width: {widths:?}"
        );
        // The prose lines survive around the grid.
        assert!(text.iter().any(|l| l.contains("preamble")), "{joined:?}");
        assert!(text.iter().any(|l| l.contains("after")), "{joined:?}");
        // The cell text uses the thinking tone, not plain text.
        let cell_line = lines
            .iter()
            .find(|l| l.to_string().contains("gray"))
            .unwrap();
        let spans: Vec<&Span<'static>> = cell_line.iter().collect();
        let cell = spans.iter().find(|s| s.content.contains("gray")).unwrap();
        assert_eq!(cell.style, style, "the cell keeps the thinking tone");
    }

    /// The 2026-09-11 request: fenced code blocks in thinking content
    /// are highlighted by the active engine (tree-sitter by default).
    /// Tokens carry the theme's foreground color and no background;
    /// the fence markers dim in the `Fence` tone; the prose keeps the
    /// thinking tone.
    #[test]
    fn thinking_fence_code_is_highlighted_by_the_engine() {
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let code = palette.style(crate::color::Role::Code, Modifier::empty());
        let fence_style = palette.style(crate::color::Role::Fence, Modifier::DIM);
        let text = "try the fix\n```rust\nfn main() { let x = 42; }\n```\ndone";
        let lines = wrap_thinking(
            text,
            60,
            &palette,
            style,
            crate::tool_display::HighlightEngine::TreeSitter,
        );
        let joined: String = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("fn main"),
            "the code line survives: {joined:?}"
        );
        // The fence marker line is dimmed in the `Fence` tone.
        assert!(
            lines.iter().any(|l| l
                .iter()
                .any(|s| s.content == "```" && s.style == fence_style)),
            "the fence marker is dimmed: {joined:?}"
        );
        // The keyword `fn` carries a theme foreground distinct from
        // both the thinking tone and the code tone; no segment carries
        // a background (the 2026-09-11 fg-only rule).
        let mut saw_kw = false;
        for l in &lines {
            for s in l.iter() {
                assert!(s.style.bg.is_none(), "no bg on highlighted text: {s:?}");
                if s.content == "fn" {
                    saw_kw = s.style.fg.is_some()
                        && s.style != style
                        && s.style != code
                        && s.style != Style::default();
                }
            }
        }
        assert!(saw_kw, "the `fn` keyword carries a theme fg: {joined:?}");
        // The prose around the fence keeps the thinking tone.
        let prose = lines
            .iter()
            .find(|l| l.to_string().contains("try the fix"))
            .unwrap();
        assert!(
            prose
                .iter()
                .any(|s| s.content.contains("try") && s.style == style),
            "prose keeps the thinking tone: {joined:?}"
        );
    }

    /// An unlabeled fence has no grammar: the code line keeps the
    /// `Code` tone, fg-only, with no syntax colors.
    #[test]
    fn thinking_unlabeled_fence_falls_back_to_the_code_tone() {
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let code = palette.style(crate::color::Role::Code, Modifier::empty());
        let text = "```\nlet x = 1\n```";
        let lines = wrap_thinking(
            text,
            60,
            &palette,
            style,
            crate::tool_display::HighlightEngine::TreeSitter,
        );
        let code_line = lines
            .iter()
            .find(|l| l.to_string().contains("let x = 1"))
            .unwrap();
        let spans: Vec<&Span<'static>> = code_line.iter().collect();
        assert!(
            spans.iter().all(|s| s.style == code),
            "unlabeled fence code keeps the code tone: {code_line:?}"
        );
    }

    /// A `|`-row inside a code fence is code, not a table: no grid
    /// border, the pipes stay literal.
    #[test]
    fn thinking_table_rows_inside_a_fence_are_not_a_grid() {
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let text = "```csv\n| a | b |\n| 1 | 2 |\n```";
        let lines = wrap_thinking(
            text,
            60,
            &palette,
            style,
            crate::tool_display::HighlightEngine::TreeSitter,
        );
        let joined: String = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains('┌'),
            "no grid inside a code fence: {joined:?}"
        );
        assert!(
            joined.contains("| a | b |"),
            "the pipe line stays literal: {joined:?}"
        );
    }

    /// Markdown markup in the thinking block is rendered, not shown raw.
    /// The 2026-09-12 user request: markdown in thinking is not lost.
    #[test]
    fn thinking_block_renders_markdown() {
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let heading_style = palette.style(crate::color::Role::Heading, Modifier::BOLD);
        let inline_code = palette.style(crate::color::Role::InlineCode, Modifier::empty());
        let text = "# Plan\nRun **cargo build** and check `main.rs`";
        let lines = wrap_thinking(
            text,
            60,
            &palette,
            style,
            crate::tool_display::HighlightEngine::TreeSitter,
        );
        let joined: String = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // The heading marker `#` is dropped; the text "Plan" is kept.
        assert!(joined.contains("Plan"), "heading text survives: {joined:?}");
        assert!(
            !joined.contains("# Plan"),
            "the `#` marker is dropped: {joined:?}"
        );
        // Inline code renders without backticks.
        assert!(joined.contains("main.rs"), "inline code text: {joined:?}");
        assert!(
            !joined.contains("`main.rs`"),
            "backticks are stripped: {joined:?}"
        );
        // The heading line carries the Heading style, not the thinking tone.
        let heading_line = lines
            .iter()
            .find(|l| l.to_string().contains("Plan"))
            .expect("heading line present");
        assert!(
            heading_line.iter().any(|s| s.style == heading_style),
            "heading uses the Heading style: {joined:?}"
        );
        // Inline-code span keeps the InlineCode role.
        let code_line = lines
            .iter()
            .find(|l| l.to_string().contains("main.rs"))
            .expect("inline code line present");
        assert!(
            code_line
                .iter()
                .any(|s| s.content == "main.rs" && s.style == inline_code),
            "inline code carries the InlineCode style: {code_line:?}"
        );
    }
}

#[cfg(test)]
mod caret_span_tests {
    use super::caret_spans;
    use ratatui::style::Modifier;
    use ratatui::text::Span;

    fn spans_of(texts: &[&str]) -> Vec<Span<'static>> {
        texts.iter().map(|t| Span::raw(t.to_string())).collect()
    }

    fn out_text(out: &[Span<'static>]) -> String {
        out.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The index of the span carrying the reversed caret cell.
    fn caret_index(out: &[Span<'static>]) -> Option<usize> {
        out.iter()
            .position(|s| s.style.add_modifier.contains(Modifier::REVERSED))
    }

    /// The caret must paint at col 0 (the start of the first span).
    /// The old code swallowed the caret here: no caret at all.
    #[test]
    fn caret_paints_at_col_zero() {
        let out = caret_spans(&spans_of(&["foo bar", "baz"]), 0, None);
        assert_eq!(out_text(&out), "foo barbaz");
        let i = caret_index(&out).expect("a caret is drawn");
        assert_eq!(out[i].content.as_ref(), "f");
    }

    /// The caret must paint when it lands exactly on a span boundary
    /// (the first char of a later span): the start-of-word case.
    /// `w` / `b` often leave the cursor at a span start.
    #[test]
    fn caret_paints_on_a_span_boundary() {
        // Spans "foo" + "bar" + "baz". cc = 3 is the start of "bar".
        let out = caret_spans(&spans_of(&["foo", "bar", "baz"]), 3, None);
        assert_eq!(out_text(&out), "foobarbaz");
        let i = caret_index(&out).expect("a caret is drawn");
        assert_eq!(out[i].content.as_ref(), "b");
    }

    /// Regression: the caret strictly inside a span still paints on
    /// that character.
    #[test]
    fn caret_paints_inside_a_span() {
        let out = caret_spans(&spans_of(&["foo", "bar"]), 4, None);
        let i = caret_index(&out).expect("a caret is drawn");
        assert_eq!(out[i].content.as_ref(), "a");
    }

    /// The caret at the line end (one past the last char) paints a
    /// block on a space cell (section 4.1).
    #[test]
    fn caret_paints_at_line_end() {
        let out = caret_spans(&spans_of(&["foo", "bar"]), 6, None);
        let i = caret_index(&out).expect("a caret is drawn");
        assert_eq!(out[i].content.as_ref(), " ");
    }

    /// An empty span never eats the caret: it still paints on the
    /// first char of the next non-empty span.
    #[test]
    fn empty_span_does_not_eat_the_caret() {
        let out = caret_spans(&spans_of(&["", "bar"]), 0, None);
        let i = caret_index(&out).expect("a caret is drawn");
        assert_eq!(out[i].content.as_ref(), "b");
    }
}

#[cfg(test)]
mod tool_call_line_tests {
    use super::{event_lines, RenderState};
    use crate::event::Event;
    use std::collections::{HashMap, HashSet};

    // The call line carries the tool name only (the 2026-09-14
    // user directive): no `tool:` prefix, no tool-specific label,
    // no raw args JSON. Extension (goal-app) tools are not
    // special-cased by the kernel and get the same bare-name line.
    #[test]
    fn extension_tool_call_line_is_bare_name() {
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let tool_display =
            crate::tool_display::ToolDisplay::preset(crate::tool_display::Preset::OpenCode);
        let fracs: HashMap<String, f64> = HashMap::new();
        let state = RenderState {
            palette: &palette,
            tool_display: &tool_display,
            tool_expanded: false,
            thinking_shown: false,
            thinking_expanded: false,
            expand_fracs: &fracs,
        };
        let details: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        let result_ids: HashSet<String> = HashSet::new();
        let ev = Event::parse_line(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"x1","name":"goal_complete","arguments":{"goal_id":"g-1","summary":"done"}}"#,
        )
        .unwrap();
        let (lines, _) = event_lines()
            .e(&ev)
            .pending(false)
            .call_details(&details)
            .result_ids(&result_ids)
            .width(80)
            .event_id(0)
            .state(&state)
            .loop_running(false)
            .compaction_last_open(false)
            .call();
        let joined: String = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("goal_complete"),
            "bare tool name: {joined:?}"
        );
        assert!(!joined.contains("tool:"), "no tool: prefix: {joined:?}");
        assert!(
            !joined.contains('"'),
            "no raw args JSON on the call line: {joined:?}"
        );
    }
}

#[cfg(test)]
// ── issue #4: user-message box, no markers, no indent ────────────────
#[cfg(test)]
mod user_box_tests {
    use super::user_box_rows;
    use crate::color::{Level, Palette};
    use ratatui::text::{Line, Span};

    fn joined(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The user panel: rounded border, "User" title, no background
    /// fill. No `user` marker, no content gutter.
    #[test]
    fn user_box_has_title_no_background() {
        let palette = Palette::builtin(Level::Rgb);
        let content = vec![
            Line::from(Span::raw("Hello, world")),
            Line::from(Span::raw("second line")),
        ];
        let lines = user_box_rows(&content, 40, &palette);
        // Two content lines plus the top and bottom border rows.
        assert_eq!(lines.len(), 4, "got: {}", joined(&lines));
        let top = lines[0].to_string();
        let bottom = lines[lines.len() - 1].to_string();
        assert!(top.starts_with('╭'), "rounded top-left: {top:?}");
        assert!(top.contains("User"), "the box title: {top:?}");
        assert!(top.ends_with('╮'), "rounded top-right: {top:?}");
        assert!(bottom.starts_with('╰'), "rounded bottom-left: {bottom:?}");
        assert!(bottom.ends_with('╯'), "rounded bottom-right: {bottom:?}");
        // No `user` marker and no 12-space content gutter.
        let j = joined(&lines);
        assert!(!j.contains("user "), "no user marker: {j:?}");
        assert!(!j.contains("\n            "), "no gutter indent: {j:?}");
        // No background fill: every span of every row is transparent.
        for line in &lines {
            for span in &line.spans {
                assert!(span.style.bg.is_none(), "no panel background fill: {j:?}");
            }
        }
    }

    /// The report panel: the final idle assistant message.
    /// A rounded `Report`-toned box with no title and no background.
    #[test]
    fn report_box_has_report_border_no_title() {
        use super::report_box_rows;
        let palette = Palette::builtin(Level::Rgb);
        let content = vec![Line::from(Span::raw("All done."))];
        let lines = report_box_rows(&content, 40, &palette);
        assert_eq!(lines.len(), 3, "got: {}", joined(&lines));
        let top = lines[0].to_string();
        assert!(top.starts_with('╭'), "rounded top-left: {top:?}");
        assert!(top.ends_with('╮'), "rounded top-right: {top:?}");
        // No title: the top border is corner, dashes, corner only.
        assert_eq!(lines[0].spans.len(), 3, "no title span: {top:?}");
        assert!(!top.contains("Assistant"), "no title: {top:?}");
        let border_fg = palette.color(crate::color::Role::Report);
        // No background fill anywhere in the panel.
        for line in &lines {
            for span in &line.spans {
                assert!(span.style.bg.is_none(), "no background fill");
            }
        }
        // The border rows are fully in the Report tone.
        for span in &lines[0].spans {
            assert_eq!(span.style.fg, Some(border_fg), "top: {top:?}");
        }
        for span in &lines[lines.len() - 1].spans {
            assert_eq!(span.style.fg, Some(border_fg), "bottom border");
        }
    }

    #[test]
    fn empty_user_box_is_three_rows() {
        let palette = Palette::builtin(Level::Rgb);
        let lines = user_box_rows(&[], 40, &palette);
        assert_eq!(lines.len(), 3, "top + empty interior + bottom: {lines:?}");
    }

    /// An assistant message carries no `assistant` marker and no content
    /// gutter: the body lines start at the left edge (the tool-call count
    /// still notes the actions that follow).
    #[test]
    fn assistant_message_has_no_marker_or_gutter() {
        use super::{event_lines, RenderState};
        use crate::event::Event;
        use std::collections::{HashMap, HashSet};

        let palette = Palette::builtin(Level::Rgb);
        let tool_display =
            crate::tool_display::ToolDisplay::preset(crate::tool_display::Preset::OpenCode);
        let fracs: HashMap<String, f64> = HashMap::new();
        let state = RenderState {
            palette: &palette,
            tool_display: &tool_display,
            tool_expanded: false,
            thinking_shown: false,
            thinking_expanded: false,
            expand_fracs: &fracs,
        };
        let details: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        let result_ids: HashSet<String> = HashSet::new();
        let ev = Event::parse_line(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"line one\nline two","tool_calls":[{"id":"c1","name":"bash"}],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#,
        )
        .unwrap();
        let (lines, _) = event_lines()
            .e(&ev)
            .pending(false)
            .call_details(&details)
            .result_ids(&result_ids)
            .width(80)
            .event_id(0)
            .state(&state)
            .loop_running(false)
            .compaction_last_open(false)
            .call();
        let joined = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains("assistant"),
            "no assistant marker: {joined:?}"
        );
        assert!(joined.contains("1 tool call"), "the call count: {joined:?}");
        // The continuation line starts at the left edge: no gutter.
        let l2 = lines
            .iter()
            .find(|l| l.to_string().contains("line two"))
            .expect("the continuation line renders")
            .to_string();
        assert!(
            !l2.starts_with("            "),
            "no 12-space gutter: {l2:?}"
        );
        assert!(l2.trim_start().starts_with("line two"), "{l2:?}");
    }

    /// The expanded thinking body carries a one-cell left pad, aligning
    /// it with the tool-result text (the `read`/`bash` panel left pad).
    /// The old 12-space content gutter is gone.
    #[test]
    fn thinking_body_one_cell_pad() {
        use super::{event_lines, RenderState};
        use crate::event::Event;
        use std::collections::{HashMap, HashSet};

        let palette = Palette::builtin(Level::Rgb);
        let tool_display =
            crate::tool_display::ToolDisplay::preset(crate::tool_display::Preset::OpenCode);
        let fracs: HashMap<String, f64> = HashMap::new();
        let state = RenderState {
            palette: &palette,
            tool_display: &tool_display,
            tool_expanded: false,
            thinking_shown: true,
            thinking_expanded: true,
            expand_fracs: &fracs,
        };
        let details: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        let result_ids: HashSet<String> = HashSet::new();
        let ev = Event::parse_line(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"answer","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":[{"content":[{"type":"reasoning_text","text":"step one\nstep two"}]}]}"#,
        )
        .unwrap();
        let (lines, _) = event_lines()
            .e(&ev)
            .pending(false)
            .call_details(&details)
            .result_ids(&result_ids)
            .width(80)
            .event_id(0)
            .state(&state)
            .loop_running(false)
            .compaction_last_open(false)
            .call();
        let joined = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("thinking"),
            "the thinking label: {joined:?}"
        );
        // Each reasoning line carries exactly one cell of left pad, not
        // the old 12-space gutter and not the bare left edge.
        for text in ["step one", "step two"] {
            let l = lines
                .iter()
                .find(|l| l.to_string().contains(text))
                .unwrap_or_else(|| panic!("missing {text:?}: {joined:?}"))
                .to_string();
            assert!(
                l.starts_with(' ') && !l.starts_with("  "),
                "one-cell left pad: {l:?}"
            );
            assert!(!l.starts_with("            "), "no 12-space gutter: {l:?}");
            assert!(l.trim_start().starts_with(text), "{l:?}");
        }
    }
}

#[cfg(test)]
// ── §4.1 table rendering fixes ──────────────────────────────────────────
#[cfg(test)]
mod table_fix_tests {
    use crate::color::{Level, Palette};
    use crate::highlight::{is_table_block_start, is_table_row, table_grid};

    /// A lone `|`-prefixed line with no following separator must NOT
    /// be treated as the start of a table block.
    #[test]
    fn lone_pipe_line_is_not_a_table_block() {
        let lines: Vec<&str> = vec!["Some prose", "|x| y => x", "more prose"];
        assert!(
            !is_table_block_start(&lines, 1),
            "lone pipe line must not start a table block"
        );
        // It IS a table row (two pipes), but not a block start.
        assert!(is_table_row("|x| y => x"));
    }

    /// A proper GFM table (header + separator) IS detected.
    #[test]
    fn gfm_table_is_detected() {
        let lines: Vec<&str> = vec!["| Name | Value |", "|------|-------|", "| a    | 1     |"];
        assert!(
            is_table_block_start(&lines, 0),
            "header + separator must be detected"
        );
    }

    /// A single `|`-prefixed line with no separator is not a block.
    #[test]
    fn single_pipe_row_without_separator_is_not_a_block() {
        let lines: Vec<&str> = vec!["|a| b|", "some prose"];
        assert!(!is_table_block_start(&lines, 0));
    }

    /// Wide cells are wrapped onto multiple visual lines, not
    /// truncated with a trailing `…`.
    #[test]
    fn wide_cell_wraps_not_truncates() {
        let palette = Palette::builtin(Level::Rgb);
        let rows = vec![
            "| A | B                          |".to_string(),
            "|---|--------------------------|".to_string(),
            "| x | a very long value that exceeds the column width comfortably and should wrap to a second visual line inside the box |".to_string(),
        ].into_iter().collect::<Vec<_>>();
        let grid = table_grid(&rows, 30, &palette);
        let joined: String = grid
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(_, t)| t.as_str())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Every word of the original cell content must be present
        // (no truncation).
        for word in [
            "very",
            "long",
            "value",
            "that",
            "exceeds",
            "column",
            "width",
            "comfortably",
            "wrap",
            "visual",
            "line",
            "inside",
            "the",
            "box",
        ] {
            assert!(
                joined.contains(word),
                "word `{word}` must appear in output:\n{joined}"
            );
        }
        // No ellipsis character from truncation.
        assert!(
            !joined.contains('…'),
            "no truncation ellipsis expected:\n{joined}"
        );
        // The cell must span more than one visual line (it wrapped).
        let data_line_count = grid
            .iter()
            .filter(|row| {
                row.iter()
                    .any(|(_, t)| t.contains("very") || t.contains("box"))
            })
            .count();
        assert!(
            data_line_count >= 2,
            "the long cell should wrap to ≥2 visual lines, got {data_line_count}:\n{joined}"
        );
    }
}

#[cfg(test)]
// ── §4.8 preview-pane wrapping helpers ─────────────────────────────
#[cfg(test)]
mod wrap_hard_lines_tests {
    use crate::highlight::Seg;
    use crate::render::{display_row_hard_line, hard_line_display_start, wrap_hard_lines};
    use ratatui::style::Style;

    fn segs(texts: &[&str]) -> Vec<Seg> {
        let s = Style::default();
        texts.iter().map(|t| (s, t.to_string())).collect()
    }

    /// Join one hard line's display lines with a marker so wrap
    /// points are visible.
    fn joined(display_lines: &[ratatui::text::Line]) -> String {
        display_lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Short lines pass through unchanged: one display row each.
    #[test]
    fn short_lines_pass_through() {
        let lines = vec![
            segs(&["fn main() {"]),
            segs(&["    println!(\"hi\");"]),
            segs(&["}"]),
        ];
        let wrapped = wrap_hard_lines(&lines, 80);
        assert_eq!(wrapped.len(), 3);
        assert_eq!(wrapped[0].len(), 1);
        assert_eq!(wrapped[1].len(), 1);
        assert_eq!(wrapped[2].len(), 1);
        let j = joined(&wrapped[1]);
        assert!(j.contains("println!"), "indent must be preserved: {j}");
    }

    /// A word longer than the width hard-breaks at the boundary.
    #[test]
    fn overlong_word_hard_breaks() {
        let lines = vec![segs(&["abcdef"])];
        let wrapped = wrap_hard_lines(&lines, 4);
        let j = joined(&wrapped[0]);
        assert_eq!(j, "abcd | ef", "expected 4-char hard break, got: {j}");
    }

    /// A word longer than a narrow width hard-breaks repeatedly.
    #[test]
    fn overlong_word_repeated_breaks() {
        let lines = vec![segs(&["abcdefgh"])];
        let wrapped = wrap_hard_lines(&lines, 3);
        let j = joined(&wrapped[0]);
        assert_eq!(j, "abc | def | gh", "expected repeated breaks, got: {j}");
    }

    /// Words wrap at spaces; a trailing space moves to the next line
    /// but is not visible.
    #[test]
    fn words_wrap_at_spaces() {
        let lines = vec![segs(&["hello world"])];
        let wrapped = wrap_hard_lines(&lines, 6);
        let j = joined(&wrapped[0]);
        assert!(
            j.contains("hello") && j.contains("world"),
            "both words must survive: {j}"
        );
        assert_eq!(wrapped[0].len(), 2, "expected two display rows: {j}");
    }

    /// A blank hard line stays a single empty display row.
    #[test]
    fn blank_line_stays_visible() {
        let lines = vec![segs(&[""]), segs(&["x"])];
        let wrapped = wrap_hard_lines(&lines, 80);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].len(), 1);
        assert!(joined(&wrapped[0]).is_empty());
    }

    /// Width 0 is floored to 1, so no infinite loop or panic.
    #[test]
    fn width_zero_is_floored() {
        let lines = vec![segs(&["ab"])];
        let wrapped = wrap_hard_lines(&lines, 0);
        assert!(!wrapped.is_empty());
    }

    /// `hard_line_display_start` maps a hard-line index to the
    /// display-row index where that hard line's first row begins.
    #[test]
    fn hard_line_display_start_maps_cumulative() {
        // width 4: "abcd" fits in one row; "w1 w2 w3" wraps to
        // three rows ("w1 ", "w2 ", "w3"); "z" is one row.
        let lines = vec![segs(&["abcd"]), segs(&["w1 w2 w3"]), segs(&["z"])];
        let wrapped = wrap_hard_lines(&lines, 4);
        assert_eq!(wrapped[0].len(), 1);
        assert_eq!(wrapped[1].len(), 3);
        assert_eq!(wrapped[2].len(), 1);
        assert_eq!(hard_line_display_start(&wrapped, 0), 0);
        assert_eq!(hard_line_display_start(&wrapped, 1), 1);
        assert_eq!(hard_line_display_start(&wrapped, 2), 4);
        // Past the end: the total display-row count.
        assert_eq!(hard_line_display_start(&wrapped, 99), 5);
    }

    /// `display_row_hard_line` maps a display row back to the largest
    /// hard line that starts at or before it.
    #[test]
    fn display_row_hard_line_back_tracks() {
        let lines = vec![segs(&["abcd"]), segs(&["w1 w2 w3"]), segs(&["z"])];
        let wrapped = wrap_hard_lines(&lines, 4);
        // display rows: 0 -> hard 0; 1..3 -> hard 1; 4 -> hard 2
        assert_eq!(display_row_hard_line(&wrapped, 0), 0);
        assert_eq!(display_row_hard_line(&wrapped, 1), 1);
        assert_eq!(display_row_hard_line(&wrapped, 3), 1);
        assert_eq!(display_row_hard_line(&wrapped, 4), 2);
        // Past the end clamps to the last hard line.
        assert_eq!(display_row_hard_line(&wrapped, 99), 2);
    }
}

// ── stage 1: snapshot plus pure build ───────────────────────────────

/// Tests for the transcript snapshot of
/// docs/tui-perf-background-build-plan.md stage 1. The pure build
/// must equal the direct `&App` build.
#[cfg(test)]
mod transcript_snapshot_tests {
    use ratatui::style::Style;

    use crate::app::App;
    use crate::event::Event;
    use crate::ext::ExtLine;
    use crate::port::SessionId;
    use crate::render::{
        build_transcript, build_transcript_input, resolve_ext_lines, TranscriptBuild,
        TranscriptBuildInput,
    };

    fn ev(json: &str) -> Event {
        Event::parse_line(json).expect("test event parses")
    }

    /// A session that exercises the main render paths.
    /// It covers a user message, an assistant reply,
    /// a tool call with result, and an open approval.
    fn rich_events() -> Vec<Event> {
        vec![
            ev(r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"Hello, world"}"#),
            ev(
                r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"Here is the output.","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5},"reasoning":[{"content":[{"type":"reasoning_text","text":"Step one: think about it."}]}]}"#,
            ),
            ev(
                r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"make"}}"#,
            ),
            ev(
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"Compiling tui v0.1.0\nFinished in 3.2s\n","exit_code":0,"stdout":"Compiling tui v0.1.0\nFinished in 3.2s\n","stderr":"","timed_out":false,"truncated":false},"is_error":false}"#,
            ),
            ev(
                r#"{"v":1,"type":"approval_request","ts":"t","id":"appr-1","call_id":"c2","prompt":"Allow the command?"}"#,
            ),
        ]
    }

    fn app_with_rich_session() -> App {
        let mut app = App::new();
        let events = rich_events();
        let log_lines = events.len() as u64;
        app.set_active(SessionId::new("s1"), events, log_lines);
        app
    }

    /// Stage 1 test gate: the snapshot build equals the direct
    /// `&App` build. The insta snapshot is the golden reference
    /// for the rendered transcript.
    #[test]
    fn snapshot_build_equals_direct_build() {
        let app = app_with_rich_session();
        let width = 100;
        let direct = build_transcript(&app, width, None);
        let input = TranscriptBuildInput::from_app(&app, width, None);
        let pure = build_transcript_input(&input);
        assert_eq!(
            pure, direct,
            "the snapshot build must equal the direct &App build"
        );
        let joined: Vec<&str> = pure.texts.iter().map(String::as_str).collect();
        insta::assert_snapshot!(joined.join("\n"));
    }

    /// `from_app` captures every field the build reads.
    #[test]
    fn from_app_captures_app_state() {
        let mut app = app_with_rich_session();
        app.toggle_block_expand("c1");
        let width = 80;
        let input = TranscriptBuildInput::from_app(&app, width, None);
        assert_eq!(input.events.as_slice(), app.events());
        assert_eq!(input.events_base_seq, app.events_base_seq());
        assert_eq!(input.call_details, app.call_details());
        assert!(
            input.pending.is_some(),
            "the open approval request must be captured as pending"
        );
        assert_eq!(input.palette, app.palette().clone());
        assert_eq!(input.tool_display, *app.tool_display());
        assert!(!input.tool_expanded);
        assert_eq!(input.thinking_shown, app.thinking_shown());
        assert!(
            !input.thinking_expanded,
            "thinking blocks start collapsed (docs/tui-turn-fold.md)"
        );
        assert_eq!(input.expand_fracs, app.expand_fracs().clone());
        assert_eq!(input.rewind_active_ranges, app.rewind_active_ranges());
        assert_eq!(input.active, app.active().cloned());
        assert!(
            !input.loop_running,
            "no loop is attached, so the running bit is false"
        );
        assert_eq!(input.width, width);
        assert!(
            input.ext_lines.is_empty(),
            "no ext host means no pre-resolved lines"
        );
    }

    /// The running-loop bit is captured and reaches the build.
    /// With the bit set, the snapshot build still equals the
    /// direct build on the same app.
    #[test]
    fn running_loop_bit_flows_through_snapshot() {
        let mut app = app_with_rich_session();
        let sid = app
            .active()
            .cloned()
            .expect("the helper sets an active session");
        app.attach_external_loop(sid);
        let direct = build_transcript(&app, 100, None);
        let input = TranscriptBuildInput::from_app(&app, 100, None);
        assert!(input.loop_running, "the running bit must be captured");
        assert_eq!(
            build_transcript_input(&input),
            direct,
            "the snapshot build must equal the direct &App build"
        );
    }

    /// Without an ext host the pre-resolution yields an empty map.
    #[test]
    fn resolve_ext_lines_without_host_is_empty() {
        let events = rich_events();
        let got = resolve_ext_lines(None, &events);
        assert!(got.is_empty());
    }

    /// A pre-resolved ext line replaces the built-in render of the
    /// event it is keyed by.
    /// A missing key keeps the built-in render.
    #[test]
    fn pure_build_uses_pre_resolved_ext_lines() {
        let app = app_with_rich_session();
        let mut input = TranscriptBuildInput::from_app(&app, 80, None);
        input
            .ext_lines
            .insert(0, vec![ExtLine::styled("EXT RENDERED", Style::default())]);
        let build = build_transcript_input(&input);
        let joined = build.texts.join("\n");
        assert!(
            joined.contains("EXT RENDERED"),
            "the ext line must replace the built-in render: {joined:?}"
        );
        // Drop the entry: the built-in render of event 0 comes back.
        let bare = TranscriptBuildInput::from_app(&app, 80, None);
        let bare_build = build_transcript_input(&bare);
        assert!(
            !bare_build.texts.join("\n").contains("EXT RENDERED"),
            "without the ext entry the built-in render must render"
        );
    }

    /// Stage 2 sends the snapshot and the finished build across a
    /// thread boundary. Both must be `Send`.
    #[test]
    fn snapshot_and_build_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<TranscriptBuildInput>();
        assert_send::<TranscriptBuild>();
    }
}

#[cfg(test)]
mod stream_cache_tests {
    //! The stream block cache tests
    //! (docs/tui-perf-streaming-incremental-plan.md).

    use std::rc::Rc;
    use std::time::Instant;

    use ratatui::style::Modifier;
    use ratatui::text::Line;

    use crate::app::{App, StreamBlockCache, StreamBuf};
    use crate::color::{Level, Palette, Role};
    use crate::tool_display::HighlightEngine;

    use super::{gutter_lines, stream_block_lines, wrap_thinking_full, LIVE_GUTTER};

    /// The frame width the frame uses.
    const W: usize = 80;
    /// The wrap width: `W.saturating_sub(1).max(4)`.
    const WRAP_W: usize = 79;

    /// A fresh app with a fixed palette and the given highlight engine.
    fn make_app(engine: HighlightEngine) -> App {
        let mut app = App::new();
        app.set_palette(Palette::builtin(Level::Rgb));
        let mut td = *app.tool_display();
        td.highlight_engine = engine;
        app.set_tool_display(td);
        app
    }

    /// Build a live stream buffer from reasoning pairs and response text.
    fn stream_buf(text: &str, reasoning: &[(&str, &str)], done: bool) -> StreamBuf {
        let mut buf = StreamBuf {
            text: text.to_string(),
            done,
            ..Default::default()
        };
        for (k, v) in reasoning {
            buf.reasoning.insert((*k).to_string(), (*v).to_string());
        }
        buf
    }

    /// The live stream cache, once a frame has built it.
    fn cache_of(app: &App) -> &StreamBlockCache {
        app.stream_block_cache_ref().as_ref().expect("cache built")
    }

    /// The O(T) reasoning join count on this thread.
    fn join_calls() -> u32 {
        crate::app::REASONING_JOIN_CALLS.with(|c| c.get())
    }

    /// The whole-document markdown parse count on this thread.
    fn parse_calls() -> u32 {
        crate::markdown::markdown_parse_calls()
    }

    /// The rendered text of a set of lines, for byte-identity checks.
    fn lines_text<'a>(lines: impl IntoIterator<Item = &'a Line<'static>>) -> Vec<String> {
        lines.into_iter().map(|l| l.to_string()).collect()
    }

    /// A thinking corpus of about `n_kb` kilobytes: prose lines plus
    /// a fenced rust block each, so fences cross chunk boundaries.
    fn thinking_corpus(n_kb: usize) -> String {
        let want = n_kb * 1024;
        let mut s: String = (0..(n_kb * 12))
            .map(|i| {
                format!(
                    "Step {i}: weigh the design against the cost. ```rust\nfn step_{i}() {{ let v: u32 = {i}; v }}\n```\n"
                )
            })
            .collect();
        s.truncate(want);
        s
    }

    /// 1. Feeding 10 KB of thinking in ten 1 KB chunks must match a
    ///    fresh full rebuild of each prefix.
    #[test]
    fn incremental_thinking_matches_full_rebuild() {
        let mut app = make_app(HighlightEngine::Builtin);
        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());
        let corpus = thinking_corpus(10);
        let total = corpus.len();
        assert!(total >= 10 * 1024, "corpus is only {total} bytes");
        let step = total / 10;
        for i in 1..=10 {
            let prefix = &corpus[..step * i];
            app.set_stream_buf(stream_buf("", &[("r1", prefix)], false));
            let _ = stream_block_lines(&mut app, W, usize::MAX);
            let cache = cache_of(&app);
            let actual = lines_text(
                cache
                    .think_lines
                    .iter()
                    .chain(cache.think_held_lines.iter()),
            );
            let (full, _, _) =
                wrap_thinking_full(prefix, WRAP_W, &palette, style, HighlightEngine::Builtin);
            let expected = lines_text(gutter_lines(full, LIVE_GUTTER).iter());
            assert_eq!(actual, expected, "chunk {i}/10 must equal the full rebuild");
        }
    }

    /// 2. A code fence open in chunk 1 and closed in chunk 2 keeps the
    ///    fence state coherent, and the line after it is prose.
    #[test]
    fn thinking_code_fence_spans_delta_boundary() {
        let mut app = make_app(HighlightEngine::Builtin);
        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());

        // Chunk 1: open a fence, one code line inside, no close.
        let c1 = "```rust\nlet a: u32 = 1;\n";
        app.set_stream_buf(stream_buf("", &[("r1", c1)], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        {
            let cache = cache_of(&app);
            assert!(cache.think_in_fence, "the fence is open after chunk 1");
            assert_eq!(cache.think_fence_lang.as_deref(), Some("rust"));
        }

        // Chunk 2: close the fence. The line after it is prose.
        let c2 = format!("{c1}```\nand that is why it works\n");
        app.set_stream_buf(stream_buf("", &[("r1", &c2)], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let cache = cache_of(&app);
        assert!(!cache.think_in_fence, "the fence is closed after chunk 2");
        assert_eq!(cache.think_fence_lang, None);
        let (full, _, _) =
            wrap_thinking_full(&c2, WRAP_W, &palette, style, HighlightEngine::Builtin);
        let expected = lines_text(gutter_lines(full, LIVE_GUTTER).iter());
        let actual = lines_text(
            cache
                .think_lines
                .iter()
                .chain(cache.think_held_lines.iter()),
        );
        assert_eq!(
            actual, expected,
            "the post-fence line is prose, matching the full rebuild"
        );
    }

    /// 3. A width change invalidates the cache, a fresh build of both
    ///    the thinking and text sections.
    #[test]
    fn width_change_invalidates_cache() {
        let mut app = make_app(HighlightEngine::Builtin);
        let reasoning = "w ".repeat(200);
        let text = "x ".repeat(150);
        app.set_stream_buf(stream_buf(&text, &[("r1", &reasoning)], false));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let before = cache_of(&app).think_lines.clone();
        let before_text = cache_of(&app).text_lines.clone();

        let _ = stream_block_lines(&mut app, 120, usize::MAX);
        let cache = cache_of(&app);
        let after = cache.think_lines.clone();
        let after_text = cache.text_lines.clone();
        assert!(
            !Rc::ptr_eq(&before, &after),
            "a width change must rebuild the thinking lines"
        );
        assert!(
            !Rc::ptr_eq(&before_text, &after_text),
            "a width change must rebuild the text lines"
        );
        assert_eq!(
            cache.width, 120,
            "the config snapshot records the new width"
        );
    }

    /// 4. An unchanged response text returns the cached lines with no
    ///    markdown re-parse. A changed one re-parses.
    #[test]
    fn text_unchanged_returns_cache() {
        let mut app = make_app(HighlightEngine::Builtin);
        let text = "Hello **world**\n\n- alpha\n- beta\n";
        app.set_stream_buf(stream_buf(text, &[], false));

        let p0 = parse_calls();
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let p1 = parse_calls();
        assert!(p1 > p0, "a new text body must parse the markdown");

        // Idle frame: same text, no re-parse, lines shared.
        let rc0 = cache_of(&app).text_lines.clone();
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let rc1 = cache_of(&app).text_lines.clone();
        let p2 = parse_calls();
        assert_eq!(p2, p1, "an idle frame must not re-parse the markdown");
        assert!(
            Rc::ptr_eq(&rc0, &rc1),
            "idle frame reuses the cached text lines"
        );

        // Growth: a new parse runs.
        let text2 = format!("{text}more body\n");
        app.set_stream_buf(stream_buf(&text2, &[], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let p3 = parse_calls();
        assert!(p3 > p2, "a changed text body must re-parse the markdown");
    }

    /// 5. Growing the earliest reasoning id changes the join at a
    ///    non-suffix position, so a full rebuild runs and matches a
    ///    fresh full rebuild.
    #[test]
    fn reasoning_reorder_triggers_full_rebuild() {
        let mut app = make_app(HighlightEngine::Builtin);
        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());

        // Grow the last id in sort order (b), a suffix append.
        app.set_stream_buf(stream_buf("", &[("a", "alpha"), ("b", "beta")], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        app.set_stream_buf(stream_buf(
            "",
            &[("a", "alpha"), ("b", "beta\ngamma")],
            false,
        ));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        assert_eq!(cache_of(&app).think_src, "alpha\nbeta\ngamma");

        // Grow the earlier id (a) by appending. The value only grows,
        // so the fingerprint moves and the join runs. But the joined
        // text now changes at a non-suffix position, so the prefix
        // check fails and a full rebuild runs.
        app.set_stream_buf(stream_buf(
            "",
            &[("a", "alpha\nalpha2"), ("b", "beta\ngamma")],
            false,
        ));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let joined = "alpha\nalpha2\nbeta\ngamma";
        assert_eq!(cache_of(&app).think_src, joined);
        let (full, _, _) =
            wrap_thinking_full(joined, WRAP_W, &palette, style, HighlightEngine::Builtin);
        let expected = lines_text(gutter_lines(full, LIVE_GUTTER).iter());
        let cache = cache_of(&app);
        let actual = lines_text(
            cache
                .think_lines
                .iter()
                .chain(cache.think_held_lines.iter()),
        );
        assert_eq!(
            actual, expected,
            "the reorder rebuild matches a fresh full rebuild"
        );
    }

    /// 6. `clear_stream` drops the live cache and the stream buffer.
    #[test]
    fn clear_stream_drops_cache() {
        let mut app = make_app(HighlightEngine::Builtin);
        app.set_stream_buf(stream_buf("hi", &[("r1", "think")], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        assert!(
            app.stream_block_cache_ref().is_some(),
            "the cache is built in a frame"
        );
        app.clear_stream();
        assert!(
            app.stream_block_cache_ref().is_none(),
            "clear_stream drops the cache"
        );
        assert!(app.stream_buf().is_none(), "clear_stream clears the buffer");
    }

    /// 7. An unchanged reasoning map skips the O(T) join on idle
    ///    frames.
    #[test]
    fn join_skip_on_unchanged_reasoning() {
        let mut app = make_app(HighlightEngine::Builtin);
        app.set_stream_buf(stream_buf("", &[("r1", "some reasoning")], false));

        let j0 = join_calls();
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let j1 = join_calls();
        assert_eq!(j1, j0 + 1, "a fresh cache joins the reasoning once");

        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let j2 = join_calls();
        assert_eq!(j2, j1, "an idle frame skips the O(T) join");
    }

    /// 8. A changed reasoning map re-joins, and the incremental path
    ///    picks up the new suffix.
    #[test]
    fn join_triggers_on_reasoning_change() {
        let mut app = make_app(HighlightEngine::Builtin);
        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());

        app.set_stream_buf(stream_buf("", &[("r1", "first part")], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let j1 = join_calls();

        // Grow the single reasoning value, a new suffix.
        app.set_stream_buf(stream_buf("", &[("r1", "first part\nsecond part")], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let j2 = join_calls();
        assert!(j2 > j1, "a changed reasoning map re-joins");

        let cache = cache_of(&app);
        assert_eq!(cache.think_src, "first part\nsecond part");
        let (full, _, _) = wrap_thinking_full(
            "first part\nsecond part",
            WRAP_W,
            &palette,
            style,
            HighlightEngine::Builtin,
        );
        let expected = lines_text(gutter_lines(full, LIVE_GUTTER).iter());
        let actual = lines_text(
            cache
                .think_lines
                .iter()
                .chain(cache.think_held_lines.iter()),
        );
        assert_eq!(
            actual, expected,
            "the new suffix is wrapped and matches the rebuild"
        );
    }

    /// 9. Perf gate. 200 KB of thinking fed in 60 frames of about 3
    ///    KB each. Every frame stays under 100 ms and the average under
    ///    10 ms. The Builtin engine isolates the cache mechanism. The
    ///    tree-sitter highlighter internal re-parse is out of scope.
    #[test]
    fn streaming_thinking_per_frame_stays_under_budget() {
        let mut app = make_app(HighlightEngine::Builtin);
        let total = 200 * 1024;
        let frames = 60;
        let per = total / frames;
        let corpus = thinking_corpus(200);
        assert_eq!(corpus.len(), total, "the corpus must be 200 KB");

        let ms: Vec<u128> = (0..frames)
            .map(|i| {
                let upto = ((i + 1) * per).min(corpus.len());
                app.set_stream_buf(stream_buf("", &[("r1", &corpus[..upto])], false));
                let t0 = Instant::now();
                let _ = stream_block_lines(&mut app, W, usize::MAX);
                t0.elapsed().as_millis()
            })
            .collect();
        let avg = ms.iter().sum::<u128>() / frames as u128;
        let max = *ms.iter().max().unwrap();
        assert!(max < 100, "a frame hit {max} ms, the budget is 100 ms");
        assert!(avg < 10, "the average frame {avg} ms must stay under 10 ms");
    }

    /// 10. Perf gate. A warm cache with no new delta is an idle frame.
    ///     It skips the join, the highlight, and the markdown re-parse,
    ///     and stays under 2 ms.
    #[test]
    fn idle_frame_is_cached() {
        let mut app = make_app(HighlightEngine::Builtin);
        let corpus = thinking_corpus(200);
        app.set_stream_buf(stream_buf("", &[("r1", &corpus)], false));
        let _ = stream_block_lines(&mut app, W, usize::MAX);

        let t0 = Instant::now();
        let _ = stream_block_lines(&mut app, W, usize::MAX);
        let idle_ms = t0.elapsed().as_millis();
        assert!(
            idle_ms < 2,
            "an idle frame took {idle_ms} ms, the budget is 2 ms"
        );
    }
}

#[cfg(test)]
mod stream_cache_independent_tests {
    //! Independent verification of the live-stream-block incremental
    //! cache (docs/tui-perf-streaming-incremental-plan.md).
    //!
    //! These tests are written **independently** of the sibling
    //! `stream_cache_tests` module, which implements the plan's own
    //! test plan. They:
    //!
    //!   * drive the mechanism through the public `App` API
    //!     (`set_stream_buf`, `press`, `clear_stream`) rather than
    //!     poking cache fields,
    //!   * check the plan's **invariant** (the cached lines must stay
    //!     byte-identical to a from-scratch full rebuild of the same
    //!     source prefix) at *irregular* chunk boundaries the plan's
    //!     tests do not exercise, and
    //!   * add an **A/B performance test** that runs the pre-plan
    //!     full-rebuild mechanism and the post-plan incremental
    //!     mechanism over the same streaming workload and asserts the
    //!     incremental one is faster.
    //!
    //! The plan's "Before" cost model is: every draw ran
    //! `wrap_thinking` (a fresh `CodeHl`, O(total thinking chars))
    //! plus `render_markdown_lines` (a fresh parser, O(total response
    //! chars)). The "After" model is: a cache hit reuses the wrapped
    //! lines, a delta re-wraps only the appended suffix, and the
    //! persistent `CodeHl` keeps code-fence state coherent.

    use std::rc::Rc;
    use std::time::Instant;

    use ratatui::style::Modifier;
    use ratatui::text::Line;

    use crate::app::{App, Key, StreamBlockCache, StreamBuf};
    use crate::color::{Level, Palette, Role};
    use crate::tool_display::HighlightEngine;

    use super::{
        gutter_lines, stream_block_lines, wrap_markdown_p, wrap_thinking_full, LIVE_GUTTER,
    };

    // A fresh app with a fixed palette and the Builtin engine (the
    // engine the plan's perf gates run, isolating the cache mechanism
    // from tree-sitter's own full-buffer re-parse).
    fn make_app() -> App {
        let mut app = App::new();
        app.set_palette(Palette::builtin(Level::Rgb));
        let mut td = *app.tool_display();
        td.highlight_engine = HighlightEngine::Builtin;
        app.set_tool_display(td);
        app
    }

    /// Build a live-stream buffer from a response-text string and a
    /// list of (id, value) reasoning pairs.
    fn buf(text: &str, reasoning: &[(&str, &str)]) -> StreamBuf {
        let mut b = StreamBuf {
            text: text.to_string(),
            ..Default::default()
        };
        for (k, v) in reasoning {
            b.reasoning.insert((*k).to_string(), (*v).to_string());
        }
        b
    }

    fn cache_of(app: &App) -> &StreamBlockCache {
        app.stream_block_cache_ref().as_ref().expect("cache built")
    }

    /// The O(T) reasoning-join count on this thread.
    fn join_calls() -> u32 {
        crate::app::REASONING_JOIN_CALLS.with(|c| c.get())
    }

    /// The whole-document markdown parse count on this thread.
    fn parse_calls() -> u32 {
        crate::markdown::markdown_parse_calls()
    }

    /// The rendered text of a line iterable, for byte-identity checks.
    fn lines_text<'a>(lines: impl IntoIterator<Item = &'a Line<'static>>) -> Vec<String> {
        lines.into_iter().map(|l| l.to_string()).collect()
    }

    /// The legacy (pre-plan) full-rebuild baseline for a thinking
    /// prefix: a fresh highlighter over the whole prefix, gutter
    /// applied. This is exactly what the old `stream_block_lines`
    /// paid on every frame.
    fn legacy_full_think(app: &App, prefix: &str) -> Vec<String> {
        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());
        let (lines, _, _) =
            wrap_thinking_full(prefix, 79, &palette, style, HighlightEngine::Builtin);
        lines_text(gutter_lines(lines, LIVE_GUTTER).iter())
    }

    /// The incremental cache's thinking output: settled lines plus the
    /// in-progress held lines.
    fn cache_think(app: &App) -> Vec<String> {
        let c = cache_of(app);
        lines_text(c.think_lines.iter().chain(c.think_held_lines.iter()))
    }

    /// A thinking corpus with code fences and prose, ~`n_kb` KB. The
    /// fence delimiters sit on their own hard lines so fence state
    /// actually opens and closes.
    fn fence_corpus(n_kb: usize) -> String {
        let want = n_kb * 1024;
        let mut s: String = (0..(n_kb * 12))
            .map(|i| {
                format!(
                    "Step {i}: weigh the design. ```rust\nfn step_{i}() {{ let v: u32 = {i}; v }}\n```\n"
                )
            })
            .collect();
        s.truncate(want);
        s
    }

    // ── correctness: the invariant holds at odd boundaries ─────

    /// Feeding the same thinking prefix in *irregular* chunks (splitting
    /// fence lines, landing on newline boundaries, multi-line suffixes)
    /// keeps the cached thinking byte-identical to a fresh full rebuild
    /// of that prefix at every step.
    #[test]
    fn irregular_chunks_stay_byte_identical() {
        let mut app = make_app();
        let corpus = fence_corpus(8);
        let total = corpus.len();
        let bounds: Vec<usize> = [0.0f64, 0.01, 0.13, 0.29, 0.5, 0.61, 0.87, 1.0]
            .iter()
            .map(|f| (*f * total as f64) as usize)
            .collect();
        for &bound in bounds.iter().skip(1) {
            app.set_stream_buf(buf("", &[("r1", &corpus[..bound])]));
            let _ = stream_block_lines(&mut app, 80, usize::MAX);
            assert_eq!(
                cache_think(&app),
                legacy_full_think(&app, &corpus[..bound]),
                "prefix {bound} of {total} must equal the full rebuild"
            );
        }
    }

    /// A code fence that opens in chunk 1 and closes in chunk 2 stays
    /// coherent: after the close the following line is
    /// prose-highlighted, byte-identical to a full rebuild.
    #[test]
    fn fence_state_spans_delta_boundary() {
        let mut app = make_app();
        let c1 = "```rust\nlet x = 1;\n";
        app.set_stream_buf(buf("", &[("r1", c1)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let c1cache = cache_of(&app);
        assert!(c1cache.think_in_fence, "the fence is open after c1");
        assert_eq!(c1cache.think_fence_lang.as_deref(), Some("rust"));

        let c2 = format!("{c1}{}\nafter the fence", "```");
        app.set_stream_buf(buf("", &[("r1", &c2)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let c2cache = cache_of(&app);
        assert!(!c2cache.think_in_fence, "the fence is closed after c2");
        assert_eq!(
            cache_think(&app),
            legacy_full_think(&app, &c2),
            "the post-fence line is prose, matching the full rebuild"
        );
    }

    /// A delta that only grows the in-progress held line must not
    /// merge the settled lines (no `Rc` re-allocation); a delta that
    /// completes a hard line must merge into a new `Rc`. This is the
    /// build deviation "the held line is rewrapped, not cached".
    #[test]
    fn held_line_grows_without_merging_settled_lines() {
        let mut app = make_app();
        let s1 = "alpha\nbeta\n";
        app.set_stream_buf(buf("", &[("r1", s1)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let settled_a = cache_of(&app).think_lines.clone();

        // Grow the held line only (no new newline in the suffix).
        let s2 = "alpha\nbeta\ngamma";
        app.set_stream_buf(buf("", &[("r1", s2)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let settled_b = cache_of(&app).think_lines.clone();
        assert!(
            Rc::ptr_eq(&settled_a, &settled_b),
            "held-line-only growth must not re-allocate the settled lines"
        );
        assert_eq!(
            cache_think(&app),
            legacy_full_think(&app, s2),
            "settled plus rewrapped held must match a full rebuild of the prefix"
        );

        // Now complete the line: a newline lands, so a merge runs.
        let s3 = "alpha\nbeta\ngamma\ndelta";
        app.set_stream_buf(buf("", &[("r1", s3)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let settled_c = cache_of(&app).think_lines.clone();
        assert!(
            !Rc::ptr_eq(&settled_b, &settled_c),
            "completing a hard line must merge into a new settled Rc"
        );
        assert_eq!(
            cache_think(&app),
            legacy_full_think(&app, s3),
            "after the merge, settled plus held must match a full rebuild"
        );
    }

    /// Growing the *earliest* reasoning id changes the join at a
    /// non-suffix position, forcing a full rebuild that still matches
    /// a fresh full rebuild.
    #[test]
    fn reasoning_reorder_triggers_full_rebuild() {
        let mut app = make_app();
        app.set_stream_buf(buf("", &[("a", "alpha"), ("b", "beta")]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let j1 = join_calls();

        // Grow the earlier id "a": the join now changes at a
        // non-suffix position.
        app.set_stream_buf(buf("", &[("a", "alpha\nalpha2"), ("b", "beta")]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let joined = "alpha\nalpha2\nbeta";
        assert_eq!(cache_of(&app).think_src, joined);
        assert!(join_calls() > j1, "the reordered join must re-join");
        assert_eq!(
            cache_think(&app),
            legacy_full_think(&app, joined),
            "the reorder rebuild must match a fresh full rebuild"
        );
    }

    /// A config change (width or palette level) forces a full rebuild
    /// of both sections.
    #[test]
    fn config_change_invalidates_cache() {
        let mut app = make_app();
        let think = "w ".repeat(200);
        let text = "x ".repeat(150);
        app.set_stream_buf(buf(&text, &[("r1", &think)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let t_before = cache_of(&app).think_lines.clone();
        let x_before = cache_of(&app).text_lines.clone();

        // Width change -> full rebuild of both sections.
        let _ = stream_block_lines(&mut app, 120, usize::MAX);
        assert!(
            !Rc::ptr_eq(&t_before, &cache_of(&app).think_lines),
            "a width change must rebuild the thinking section"
        );
        assert!(
            !Rc::ptr_eq(&x_before, &cache_of(&app).text_lines),
            "a width change must rebuild the text section"
        );
        assert_eq!(cache_of(&app).width, 120);

        // Palette level change -> full rebuild again.
        let t_pal = cache_of(&app).think_lines.clone();
        app.set_palette(Palette::builtin(Level::C256));
        let _ = stream_block_lines(&mut app, 120, usize::MAX);
        assert!(
            !Rc::ptr_eq(&t_pal, &cache_of(&app).think_lines),
            "a palette level change must rebuild the thinking section"
        );
        assert_eq!(cache_of(&app).palette_level, Level::C256);
    }

    /// Toggling `thinking_expanded` (Ctrl+T) rebuilds the thinking
    /// section only; the text section is unaffected. Toggling
    /// `thinking_shown` (Ctrl+X) rebuilds nothing.
    #[test]
    fn thinking_toggles_rebuild_only_their_section() {
        let mut app = make_app();
        let think = "alpha\nbeta\n";
        let text = "# Answer\n\n- a\n- b\n";
        app.set_stream_buf(buf(text, &[("r1", think)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let think_a = cache_of(&app).think_lines.clone();
        let text_a = cache_of(&app).text_lines.clone();

        // Ctrl+T: thinking_expanded flips. Thinking section rebuilds,
        // text section keeps its Rc.
        let _ = app.press(Key::CtrlT);
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let think_b = cache_of(&app).think_lines.clone();
        let text_b = cache_of(&app).text_lines.clone();
        assert!(
            !Rc::ptr_eq(&think_a, &think_b),
            "the thinking-expanded toggle must rebuild the thinking section"
        );
        assert!(
            Rc::ptr_eq(&text_a, &text_b),
            "the thinking-expanded toggle must leave the text section alone"
        );
        assert!(cache_of(&app).thinking_expanded);

        // Ctrl+X: thinking_shown flips. No section rebuilds.
        let _ = app.press(Key::CtrlX);
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let think_c = cache_of(&app).think_lines.clone();
        let text_c = cache_of(&app).text_lines.clone();
        assert!(
            Rc::ptr_eq(&think_b, &think_c),
            "the thinking-shown toggle must not rebuild the thinking section"
        );
        assert!(
            Rc::ptr_eq(&text_b, &text_c),
            "the thinking-shown toggle must not rebuild the text section"
        );
    }

    /// `clear_stream` drops the live cache (and the buffer), so memory
    /// returns to baseline.
    #[test]
    fn clear_stream_drops_cache() {
        let mut app = make_app();
        app.set_stream_buf(buf("hi", &[("r1", "think")]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        assert!(app.stream_block_cache_ref().is_some());
        app.clear_stream();
        assert!(
            app.stream_block_cache_ref().is_none(),
            "clear_stream must drop the cache"
        );
        assert!(app.stream_buf().is_none());
    }

    /// A warm idle frame (no reasoning or text delta) does no join, no
    /// markdown re-parse, and reuses the cached lines.
    #[test]
    fn idle_frame_does_no_work() {
        let mut app = make_app();
        let think = "step one\nstep two\n";
        let text = "Answer\n\n- a\n- b\n";
        app.set_stream_buf(buf(text, &[("r1", think)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let t_warm = cache_of(&app).think_lines.clone();
        let x_warm = cache_of(&app).text_lines.clone();
        let j_warm = join_calls();
        let p_warm = parse_calls();

        for _ in 0..2 {
            let _ = stream_block_lines(&mut app, 80, usize::MAX);
        }
        assert_eq!(join_calls(), j_warm, "idle frames must skip the O(T) join");
        assert_eq!(
            parse_calls(),
            p_warm,
            "idle frames must skip the markdown re-parse"
        );
        assert!(
            Rc::ptr_eq(&t_warm, &cache_of(&app).think_lines),
            "idle frames must reuse the cached thinking lines"
        );
        assert!(
            Rc::ptr_eq(&x_warm, &cache_of(&app).text_lines),
            "idle frames must reuse the cached text lines"
        );
    }

    // ── performance: the incremental path beats full rebuild ─────

    /// A/B gate. The pre-plan path re-ran a full O(T) thinking rebuild
    /// and a full O(W) markdown parse on every frame. The post-plan
    /// incremental cache re-wraps only the appended suffix and skips
    /// the markdown parse when the text is unchanged. Over a 60-frame
    /// stream of ~800 KB of thinking, the incremental path must win.
    #[test]
    fn streaming_frames_incremental_beats_full_rebuild() {
        let corpus = fence_corpus(800);
        let text = "## Answer\n\n".to_string() + "x ".repeat(20_000).as_str();
        let frames = 60;
        let per = corpus.len() / frames;

        let app0 = make_app();
        let palette = app0.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());
        let prose = palette.style(Role::PlainText, Modifier::empty());

        // OLD mechanism: full thinking rebuild plus full markdown
        // parse on every frame.
        let mut old_ms: Vec<u128> = Vec::with_capacity(frames);
        for i in 1..=frames {
            let think_prefix = corpus[..i * per].to_string();
            let text_prefix = text[..i * text.len() / frames].to_string();
            let t0 = Instant::now();
            let _ =
                wrap_thinking_full(&think_prefix, 79, &palette, style, HighlightEngine::Builtin);
            let _ = wrap_markdown_p(&text_prefix, 79, &palette, prose);
            old_ms.push(t0.elapsed().as_millis());
        }
        let old_total = old_ms.iter().sum::<u128>();

        // NEW mechanism: the incremental cache.
        let mut app = make_app();
        let mut new_ms: Vec<u128> = Vec::with_capacity(frames);
        for i in 1..=frames {
            let think_prefix = corpus[..i * per].to_string();
            let text_prefix = text[..i * text.len() / frames].to_string();
            app.set_stream_buf(buf(&text_prefix, &[("r1", &think_prefix)]));
            let t0 = Instant::now();
            let _ = stream_block_lines(&mut app, 80, usize::MAX);
            new_ms.push(t0.elapsed().as_millis());
        }
        let new_total = new_ms.iter().sum::<u128>();

        let old_avg = old_total / frames as u128;
        let new_avg = new_total / frames as u128;
        eprintln!(
            "indep_streaming old_total={} ms old_avg={} ms \
             new_total={} ms new_avg={} ms ratio={:.2}",
            old_total,
            old_avg,
            new_total,
            new_avg,
            old_total as f64 / new_total.max(1) as f64
        );
        assert!(
            old_total >= 2 * new_total,
            "the incremental path must be at least 2x faster in total \
             (old {old_total} ms vs new {new_total} ms)"
        );
        assert!(
            new_avg < 50,
            "an incremental frame hit {new_avg} ms, the budget is 50 ms"
        );
    }

    /// A/B gate. A warm idle frame in the new mechanism reuses the
    /// cache and does no wrap or parse work. The pre-plan idle frame
    /// re-ran the full thinking rebuild and the full markdown parse.
    /// The new idle frame must be far cheaper.
    #[test]
    fn idle_frame_incremental_beats_full_rebuild() {
        let think = fence_corpus(500);
        let text = "## Answer\n\n".to_string() + "x ".repeat(60_000).as_str();

        let mut app = make_app();
        app.set_stream_buf(buf(&text, &[("r1", &think)]));
        let _ = stream_block_lines(&mut app, 80, usize::MAX);

        let palette = app.palette().clone();
        let style = palette.style(Role::Thinking, Modifier::empty());
        let prose = palette.style(Role::PlainText, Modifier::empty());

        // OLD idle frame: full thinking rebuild plus full markdown
        // parse.
        let t_old = Instant::now();
        let _ = wrap_thinking_full(&think, 79, &palette, style, HighlightEngine::Builtin);
        let _ = wrap_markdown_p(&text, 79, &palette, prose);
        let old_ms = t_old.elapsed().as_millis();

        // NEW idle frame: a cache hit.
        let t_new = Instant::now();
        let _ = stream_block_lines(&mut app, 80, usize::MAX);
        let new_ms = t_new.elapsed().as_millis();

        eprintln!("indep_idle old={} ms new={} ms", old_ms, new_ms);
        assert!(
            old_ms > 0 && new_ms <= old_ms / 10,
            "the idle frame must be at least 10x cheaper than a full \
             rebuild (old {old_ms} ms, new {new_ms} ms)"
        );
    }
}

#[cfg(test)]
mod working_status_tests {
    //! The TUI-derived `working` state (docs/tui-working-status.md).
    //! `wait` is the sent request pending on the server; `working`
    //! is the response streaming back through the session's
    //! `.model-stream`. The derivation is a rendering rule over
    //! three inputs — the loop-running bit, the last `loop_phase`
    //! value, and the session stream buffer — with no kernel
    //! change (section 2, marker source).

    use crate::app::{App, StreamBuf};
    use crate::event::Event;
    use crate::port::{TailCursor, WatchItem};

    use super::{phase_bit, phase_state, working_row_text, PhaseState};

    /// One `loop_phase` marker event, delivered the way the log
    /// tailer delivers it (app.rs `on_watch_item`, the ext_status
    /// arm records it in the O(1) map and the ts side map).
    fn marker_event(value: &str, ts: &str) -> Event {
        let line = format!(
            r#"{{"v":1,"type":"ext_status","ts":"{ts}","id":"loop_phase","value":"{value}"}}"#
        );
        Event::parse_line(&line).expect("a valid ext_status marker line")
    }

    /// Push one event through the watch path the main loop uses.
    fn watch(app: &mut App, event: Event) {
        app.on_watch_item(WatchItem::Event {
            event,
            cursor: TailCursor::end(),
        });
    }

    /// A settle event: the authoritative `assistant_message` clears
    /// the live stream buffer (app.rs `on_watch_item`).
    fn settle_event() -> Event {
        Event::parse_line(
            r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"hi","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#,
        )
        .expect("a valid settle event")
    }

    /// An open stream buffer: at least one `.model-stream` delta
    /// line was read. `done` mirrors the channel's `done` line.
    fn open_buf(done: bool) -> StreamBuf {
        StreamBuf {
            text: "so far ".to_string(),
            done,
            ..Default::default()
        }
    }

    /// A fixed clock for the span tests.
    fn now_fixed() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-16T12:00:00Z")
            .expect("a valid rfc3339 instant")
            .with_timezone(&chrono::Utc)
    }

    /// The marker `ts` string `secs` before `now` (the marker
    /// timestamp the loop host recorded at one-second resolution).
    fn marker_ts(now: &chrono::DateTime<chrono::Utc>, secs: u64) -> String {
        (*now - chrono::Duration::seconds(secs as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// P1 working-state: given the loop is running, the last
    /// `loop_phase` value is `wait`, and the stream buffer is open,
    /// observe the title bit reads `[working]` and the working row
    /// shows `model working · Ns`.
    #[test]
    fn working_state_shows_the_bit_and_the_row() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        app.set_stream_buf(open_buf(false));

        let state = phase_state(&app, true);
        assert!(
            matches!(state, PhaseState::Working),
            "wait marker plus an open buffer is working, got {state:?}"
        );
        assert_eq!(phase_bit(state), " [working] ");

        let row = working_row_text(state, app.loop_phase_ts(), &now)
            .expect("the working row has text while the loop runs");
        assert_eq!(row, "model working · 30s");
    }

    /// P2 wait-pending: given the loop is running, the last value is
    /// `wait`, and no stream file was read (the buffer is closed),
    /// observe the state stays `wait` and the row keeps
    /// `waiting for model · Ns`.
    #[test]
    fn wait_marker_without_a_stream_buffer_keeps_the_wait_row() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        assert!(
            app.stream_buf().is_none(),
            "no delta read: the buffer is closed"
        );

        let state = phase_state(&app, true);
        assert!(
            matches!(state, PhaseState::Wait),
            "a closed buffer under wait stays wait, got {state:?}"
        );
        assert_eq!(phase_bit(state), " [wait] ");
        let row =
            working_row_text(state, app.loop_phase_ts(), &now).expect("the wait row has text");
        assert_eq!(row, "waiting for model · 30s");
    }

    /// The same `wait` row through the file path: a missing
    /// `.model-stream` file settles the buffer, so the derivation
    /// stays `wait`.
    #[test]
    fn a_missing_stream_file_keeps_the_wait_state() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        let dir = std::env::temp_dir().join(format!(
            "rushi-tui-working-status-{}-missing",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join(".model-stream");
        // The loop created and deleted the file: it is gone. The
        // poll settles any open buffer.
        app.set_stream_buf(open_buf(false));
        app.refresh_stream(&file);
        assert!(
            app.stream_buf().is_none(),
            "a missing stream file settles the buffer"
        );
        assert!(matches!(phase_state(&app, true), PhaseState::Wait));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3 done-window: given the `done` line is read but the
    /// `assistant_message` has not landed, observe the state stays
    /// `working`.
    #[test]
    fn the_done_line_keeps_the_state_working() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        // The channel is complete but the log event has not landed:
        // the buffer is still open (`done` set).
        app.set_stream_buf(open_buf(true));

        let state = phase_state(&app, true);
        assert!(
            matches!(state, PhaseState::Working),
            "the done window stays working, got {state:?}"
        );
        assert_eq!(
            working_row_text(state, app.loop_phase_ts(), &now).unwrap(),
            "model working · 30s"
        );
    }

    /// P4 settle-fallback: given the settle event clears the stream
    /// buffer, observe the state falls back to the marker-derived
    /// one: `wait` while the last marker is `wait`, `tools` after a
    /// `tools` marker, and `idle` when the running bit falls.
    #[test]
    fn settle_clears_the_buffer_and_falls_back_to_the_marker() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        app.set_stream_buf(open_buf(true));
        assert!(matches!(phase_state(&app, true), PhaseState::Working));

        // The authoritative event lands: the buffer clears and the
        // state falls back to the last marker, which is still wait.
        watch(&mut app, settle_event());
        assert!(app.stream_buf().is_none(), "the settle clears the buffer");
        let state = phase_state(&app, true);
        assert!(
            matches!(state, PhaseState::Wait),
            "after settle the marker-derived state is wait, got {state:?}"
        );

        // The next marker is tools: the state follows the marker
        // with the buffer closed.
        watch(&mut app, marker_event("tools", &marker_ts(&now, 5)));
        assert!(matches!(phase_state(&app, true), PhaseState::Tools));

        // The loop stops: the running bit falls and the state is
        // idle, even with a stale buffer.
        app.set_stream_buf(open_buf(true));
        assert!(matches!(phase_state(&app, false), PhaseState::Idle));
    }

    /// P5 unknown-marker: given the loop is running with no marker,
    /// or a value outside `wait`/`tools`, and an open stream
    /// buffer, observe the state stays `running-unknown` — the
    /// derivation applies only inside `wait`.
    #[test]
    fn an_unknown_marker_stays_unknown_with_an_open_buffer() {
        let now = now_fixed();
        // No marker at all, buffer open.
        let mut app = App::new();
        app.set_stream_buf(open_buf(false));
        let state = phase_state(&app, true);
        assert!(
            matches!(state, PhaseState::RunningUnknown),
            "no marker with an open buffer is running-unknown, got {state:?}"
        );
        assert_eq!(phase_bit(state), " [running] ");
        assert_eq!(
            working_row_text(state, app.loop_phase_ts(), &now).unwrap(),
            "Working..."
        );

        // A value outside the two known strings: still unknown.
        watch(&mut app, marker_event("model", &marker_ts(&now, 30)));
        assert!(
            matches!(phase_state(&app, true), PhaseState::RunningUnknown),
            "a value outside wait/tools is running-unknown, got {state:?}"
        );
    }

    /// P6 stale-file: given the running bit is clear and a stale
    /// (undeleted) stream buffer exists, observe the state is
    /// `idle` and no working row draws.
    #[test]
    fn a_stale_stream_buffer_is_idle_when_the_loop_stops() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        app.set_stream_buf(open_buf(true));

        let state = phase_state(&app, false);
        assert!(
            matches!(state, PhaseState::Idle),
            "a dead loop with an undeleted stream file is idle, got {state:?}"
        );
        assert_eq!(phase_bit(state), " [idle] ");
        assert!(
            working_row_text(state, app.loop_phase_ts(), &now).is_none(),
            "no working row when idle"
        );
    }

    /// Restart onto a running session with a non-empty stream file:
    /// the start read rebuilds the marker from the log and the
    /// first `refresh_stream` read opens the buffer — `working` on
    /// the first draw.
    #[test]
    fn a_restart_onto_a_running_session_shows_working() {
        let mut app = App::new();
        let now = now_fixed();
        // The restart rebuilt the per-id values from the log.
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        // The first frame reads the non-empty file from byte 0: the
        // delta line opens the live buffer.
        let dir = std::env::temp_dir().join(format!(
            "rushi-tui-working-status-{}-restart",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join(".model-stream");
        std::fs::write(&file, "{\"kind\":\"text\",\"delta\":\"so far \"}\n").expect("stream file");
        app.refresh_stream(&file);
        assert!(
            app.stream_buf().is_some(),
            "a non-empty file opens the buffer on the first read"
        );
        assert!(
            matches!(phase_state(&app, true), PhaseState::Working),
            "the first draw after the restart shows working"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P7 timer-continuity: given the `wait` marker is 30 s old and
    /// the state is `working`, observe the row shows the span from
    /// the marker, not from the first delta; and the P5
    /// span-format rules of the base contract hold at 90 s.
    #[test]
    fn the_working_timer_counts_from_the_wait_marker() {
        let mut app = App::new();
        let now = now_fixed();
        watch(&mut app, marker_event("wait", &marker_ts(&now, 30)));
        app.set_stream_buf(open_buf(false));

        let state = phase_state(&app, true);
        assert!(matches!(state, PhaseState::Working));
        assert_eq!(
            working_row_text(state, app.loop_phase_ts(), &now).unwrap(),
            "model working · 30s",
            "the span counts from the marker, not the first delta"
        );

        // The base contract's P5 format at 60 s and above: `Mm SSs`.
        watch(&mut app, marker_event("wait", &marker_ts(&now, 90)));
        let row = working_row_text(PhaseState::Working, app.loop_phase_ts(), &now).unwrap();
        assert_eq!(
            row, "model working · 1m 30s",
            "the P5 format holds for working"
        );
    }
}
