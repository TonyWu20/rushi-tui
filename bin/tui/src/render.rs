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
use std::sync::Arc;

use crate::app::App;
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
/// Body lines a single tool call's `command` argument may occupy.
/// The `content` field of user and assistant messages has no cap:
/// it displays in full (docs/tui_feature_requests_from_human.md item
/// 1). Tool result bodies fold at render time (docs/
/// tui-tool-display-port.md).
const TOOL_CALL_BODY_LINES: usize = 4;
/// Events rendered into the transcript at once. The oldest are dropped
/// to bound memory on huge logs. The log file is the record.
pub const TRANSCRIPT_EVENT_CAP: usize = 2000;
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
        hard.get(p).map(String::clone)
    } else {
        None
    }
}

/// The user-message panel: a rounded, bordered block titled "User" on
/// the tool-result panel background (docs/tui_feature_requests_from_human.md
/// 2026-09-06: user messages are wrapped like tool results — the tool
/// panel's background, a `Block::bordered().border_type(Rounded)` box,
/// and the "User" title in place of the old `user` marker, with no
/// content gutter). The rows are hand-built styled spans, like
/// [`crate::tool_display::box_rows`] for the tool panel, so the panel
/// composes into the single scrollable transcript `Paragraph` and a
/// browse-mode cursor / yank still lands on a real transcript line.
///
/// `content` are the already-wrapped, styled message lines (clamped to
/// the panel inner width by the caller). Every cell carries the panel
/// background so the panel reads as one lighter band; the border and
/// the "User" title read in the accent tone. The panel spans the full
/// `width` (the transcript width, like the tool-result panel). An
/// empty message renders a single empty interior row between the two
/// border rows.
fn user_box_rows(
    content: &[Line<'static>],
    width: usize,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    // The tool-result panel background (the same lighter band the tool
    // panel uses, the 2026-09-14 borderless panel background). The
    // border and the "User" title read in the accent tone.
    let bg = crate::tool_display::box_bg(palette, false);
    let border = Style::default().fg(palette.color(crate::color::Role::Accent)).bg(bg);
    let pad = Style::default().bg(bg);
    let left_pad = 1usize; // one column, mirroring the tool panel pad.
    let inner = width.saturating_sub(2);
    let mut rows: Vec<Line<'static>> = Vec::new();
    // Top border: `╭User───╮`. The "User" title sits one column in from
    // the corner, overwriting the top border's dashes — exactly how a
    // ratatui `Block::bordered().border_type(Rounded).title("User")`
    // draws its left title.
    let title = "User";
    // `saturating_sub` keeps the fill non-negative on very narrow widths.
    let top_fill = width.saturating_sub(1 + title.chars().count() + 1);
    rows.push(Line::from(vec![
        Span::styled("╭", border),
        Span::styled(title, border),
        Span::styled("─".repeat(top_fill), border),
        Span::styled("╮", border),
    ]));
    // Interior rows: `│ content │` with one column of left padding and a
    // background fill to the panel edge. An empty message yields a
    // single empty interior row.
    if content.is_empty() {
        rows.push(Line::from(vec![
            Span::styled("│", border),
            Span::styled(" ".repeat(inner), pad),
            Span::styled("│", border),
        ]));
    } else {
        for line in content {
            let mut cells = vec![
                Span::styled("│", border),
                Span::styled(" ".repeat(left_pad), pad),
            ];
            let mut content_width = 0usize;
            for span in &line.spans {
                // Keep the span's own foreground / modifiers; only fill
                // the panel background (mirrors the tool panel's
                // `box_rows` background fill).
                let st = if span.style.bg.is_some() {
                    span.style
                } else {
                    span.style.bg(bg)
                };
                cells.push(Span::styled(span.content.clone(), st));
                content_width += span.content.chars().count();
            }
            // Right padding to the panel edge (the panel is one band).
            let right_pad = inner.saturating_sub(left_pad).saturating_sub(content_width);
            cells.push(Span::styled(" ".repeat(right_pad), pad));
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
    /// The thinking-block visibility (Ctrl+T): `false` hides every
    /// thinking block.
    pub thinking_shown: bool,
    /// The thinking-block expand state (Ctrl+X): `false` shows the
    /// collapsed header row only.
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
    ext: Option<&'a crate::ext::ExtHost>,
    state: &'a RenderState<'a>,
    loop_running: bool,
    compaction_last_open: bool,
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
            let box_content_w = box_w.saturating_sub(4).max(4);
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
            // The thinking block first: the model's own reasoning items,
            // captured into the log by the loop, render above the
            // message body and the tool calls that follow, so the
            // transcript reads thinking, then the actions (docs/tui-
            // thinking-block.md section 4: the reasoning content shows
            // above the message body).
            // pi-aligned keymap: `Ctrl+T` collapses or expands
            // the block (the pi `app.thinking.toggle`). Collapsed it is a
            // one-line label row; expanded it is the full reasoning text.
            // `Ctrl+X` hides or shows the block entirely. The color is
            // the lighter thinking tone (docs/tui-color-tones.md), not a
            // dimmed gray.
            if state.thinking_shown {
                let reasoning = e.get("reasoning").and_then(|v| v.as_array());
                if let Some(text) = thinking_text(reasoning) {
                    let thinking_style =
                        palette.style(crate::color::Role::Thinking, Modifier::empty());
                    if state.thinking_expanded {
                        let header = vec![Span::styled(format!("{LABEL}thinking"), thinking_style)];
                        out.push(Line::from(header));
                        owns.push(None); // the thinking label: UI chrome, not shareable source
                        let wrapped = wrap_thinking(
                            &text,
                            wrap_w,
                            palette,
                            thinking_style,
                            state.tool_display.highlight_engine,
                        );
                        let n = wrapped.len();
                        out.extend(guttered(&wrapped, &gutter));
                        owns.extend(std::iter::repeat_n(None, n));
                    } else {
                        // The collapsed row: a one-line pi-style label with
                        // the expand hint, not the full reasoning text.
                        out.push(Line::from(Span::styled(
                            format!("{LABEL}thinking \u{2026} (Ctrl+T to expand)"),
                            thinking_style,
                        )));
                        owns.push(None);
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
            let (wrapped, prov) = if content.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                render_message_content(&content, event_id, ext, wrap_w, prose, palette)
            };
            if let Some(first) = wrapped.first() {
                if !header.is_empty() {
                    header.push(Span::raw("  "));
                }
                header.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(header));
            owns.push(owns_raw_line(0, &prov, &hard));
            // An empty content (a model output that carries only tool
            // calls) has no body line; the header stands alone.
            // FT-006: an unguarded `wrapped[1..]` panicked on the
            // first launch draw. No content gutter on the body lines
            // (2026-09-06 request): they start at the left edge.
            if !wrapped.is_empty() {
                out.extend(wrapped[1..].iter().cloned());
                for k in 1..prov.len() {
                    owns.push(owns_raw_line(k, &prov, &hard));
                }
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
            // arguments (the write diff needs the write `content`
            // argument; docs/tui-tool-result-truncation.md).
            let (name, args) = call_details
                .get(id)
                .cloned()
                .unwrap_or_else(|| (id.to_string(), serde_json::Value::Null));
            let value = e.get("value");
            let err = e.get_bool("is_error").unwrap_or(false);
            let status = result_status(value, err);
            // The result body in its lighter panel (docs/tui-tool-
            // display-port.md section 2, the box; the 2026-09-14 user
            // pass dropped the border lines: the panel is the lighter
            // background only). The body is the tool-specific compact
            // output (docs/tui-tool-result-truncation.md section 1,
            // the content layer), folded to the output mode's lines,
            // with the global Ctrl+O expansion to the full body. The
            // panel header row carries the tool name (the purple
            // `tool_name` accent, bold) and the status, so no
            // separate header line above the panel.
            let value_ref = value.unwrap_or(&serde_json::Value::Null);
            // The body content budget: the panel inner width minus the
            // left padding cell. The panel truncates overflow with a
            // trailing ellipsis, so the content fills the panel
            // instead of leaving dead columns (the 2026-09-03 user
            // directive: truncate, never wrap). The borderless panel
            // (2026-09-14) keeps one cell of left padding, so the
            // budget is `width - 1` instead of the old `width - 3`.
            let body_w = width.saturating_sub(1);
            // The per-block expand fraction (docs/tui-tool-display-
            // fancy.md section 6): when the animation system has a
            // value for this event ID, it interpolates the body cap
            // between the collapsed and expanded caps. An empty map
            // means "no animation: use the `tool_expanded` bool".
            let expand_frac = state.expand_fracs.get(id).copied().unwrap_or(-1.0);
            let mut body = crate::tool_display::body_rows()
                .tool(&name)
                .value(value_ref)
                .call_args(&args)
                .err(err)
                .cfg(state.tool_display)
                .palette(palette)
                .expanded(state.tool_expanded)
                .width(body_w)
                .expand_frac(expand_frac)
                .call();
            // The JSON-document body (docs/tui-color-tones.md): a
            // read result whose content is a complete JSON document,
            // or an unknown tool whose result is JSON, keeps the
            // JSON token colors instead of the plain code tone.
            let body_text = value_ref.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let known = matches!(name.as_str(), "read" | "write" | "edit" | "bash");
            if (name == "read" || !known) && highlight::looks_like_json(body_text) {
                body = crate::tool_display::json_body_rows(
                    &name,
                    value_ref,
                    state.tool_display,
                    palette,
                    state.tool_expanded,
                    body_w,
                    expand_frac,
                );
            }
            // The panel header names the tool. A `read` result also
            // carries the file it read as a dim label after the name
            // (the `file_path` of its call arguments; the kernel knows
            // the argument shape of its own read tool).
            let label = if name == "read" {
                args.get("file_path")
                    .or_else(|| args.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            } else {
                ""
            };
            let rows =
                crate::tool_display::box_rows(&name, label, &status, &body, width, palette, err);
            // The shareable raw source of a result is its raw output
            // text (section 11.3); assign it to the box's first row so
            // a yank of the box returns the full output, not the
            // truncated/boxed display.
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
/// section 4): the raw reasoning text in the thinking tone. Two
/// exceptions: a run of consecutive `|` table lines draws as the
/// box-drawing grid (the 2026-09-03 user report: tables inside a
/// thinking block lost their fixed column widths), and a fenced code
/// block (` ``` ` / `~~~`) draws through the active highlight engine
/// (`tree-sitter` by default, the 2026-09-11 request): the fence marker
/// lines take the dimmed `Fence` tone, the language tag after the
/// opening delimiter drives the highlight, and the unscoped runs fall
/// back to the `Code` tone, fg-only, like the tool-result bodies.
fn wrap_thinking(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    engine: crate::tool_display::HighlightEngine,
) -> Vec<Line<'static>> {
    let hard_lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let border_style = palette.style(crate::color::Role::Hint, Modifier::DIM);
    let code_style = palette.style(crate::color::Role::Code, Modifier::empty());
    // One stateful highlighter for the whole thinking text: a block
    // comment that spans code-fence lines stays open across them.
    let mut hl = crate::tool_display::CodeHl::new(engine);
    let mut in_fence = false;
    let mut fence_lang: Option<String> = None;
    let mut i = 0usize;
    while i < hard_lines.len() {
        let t = hard_lines[i].trim_start();
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
            let line = hard_lines[i];
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
        if highlight::is_table_row(hard_lines[i]) {
            let mut block: Vec<String> = Vec::new();
            while i < hard_lines.len() && highlight::is_table_row(hard_lines[i]) {
                block.push(hard_lines[i].to_string());
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
        let line = hard_lines[i];
        i += 1;
        if line.is_empty() {
            out.push(Line::default());
            continue;
        }
        out.extend(wrap_styled(vec![(style, line.to_string())], wrap_w));
    }
    out
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
/// rest ride on, like the capture in commit `61cde02`.
fn thinking_text(reasoning: Option<&Vec<serde_json::Value>>) -> Option<String> {
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
/// - A `fence:mermaid` block: the host requests a transform for the
///   fence body. A finished reply replaces the fence with the
///   extension's lines, guttered like the transcript. A missing
///   owner, a pending or timed-out request, or a dead extension
///   shows the raw fence (G5 fallback).
/// - An `inline:latex` span: the host requests a transform for the
///   span. A finished reply replaces the span text in place (the
///   reply lines join into one line). No owner: the raw span text
///   renders, exactly like the built-in path.
/// - A table block: a run of consecutive `|`-separated lines draws
///   as a box-drawing grid, like the `ext == None` path (the
///   2026-09-03 report: the block path showed the raw pipes).
///
/// `base` is the foreground of the unstyled plain-text runs (the
/// capability-aware prose color); styled runs keep their own
/// styles. The `ext == None` path renders the content with
/// [`wrap_markdown_p`] over the same base.
fn render_message_content(
    content: &str,
    event_id: u64,
    ext: Option<&crate::ext::ExtHost>,
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
                    if highlight::is_table_row(&text) {
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
                            Part::Latex { idx, raw, text } => {
                                let req =
                                    host.request_span(event_id, *idx, "inline:latex", text, wrap_w);
                                let replaced = req
                                    .and_then(|_| host.span_lines(event_id, *idx))
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
            MBlock::Mermaid { idx, raw, text } => {
                let req = host.request_span(event_id, idx, "fence:mermaid", &text, wrap_w);
                // An empty reply erases the block: treat it as no
                // reply and show the raw fence.
                let art = req
                    .and_then(|_| host.span_lines(event_id, idx))
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
/// (docs/tui-model-wait-indicator.md section 2). One of four values,
/// derived from two inputs: the last `loop_phase` value in the log
/// and the loop-running bit. The state is a pure function of the
/// log and the bit, so it survives a TUI restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseState {
    /// The loop is not running. Bit `[idle]`, the working row
    /// stays blank.
    Idle,
    /// The loop runs with no marker, or a value outside
    /// `wait` / `tools`. Bit `[running]`, the row shows
    /// `Working...`.
    RunningUnknown,
    /// The loop runs, the last marker value is `wait`. Bit
    /// `[wait]`, the row shows the wait for the model response.
    Wait,
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
        PhaseState::Tools => " [tools] ",
    }
}

/// Derive the phase state from the last marker value and the running
/// bit (docs/tui-model-wait-indicator.md section 2). The value comes
/// from the O(1) per-id map; a missing marker or a value outside
/// the two known strings maps to `RunningUnknown`.
pub fn phase_state(app: &App, running: bool) -> PhaseState {
    if !running {
        return PhaseState::Idle;
    }
    match app
        .ext_statuses()
        .get(crate::app::LOOP_PHASE_STATUS_ID)
        .and_then(|v| v.as_str())
    {
        Some("wait") => PhaseState::Wait,
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
/// (docs/tui-model-wait-indicator.md section 3): the phase with the
/// wait span from the marker timestamp. The `wait` and `tools`
/// states carry the span; an unparseable timestamp drops the span
/// and keeps the label. The idle state owns no text: the row stays
/// blank.
fn working_row_text(
    state: PhaseState,
    ts: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    match state {
        PhaseState::Idle => None,
        PhaseState::RunningUnknown => Some("Working...".to_string()),
        PhaseState::Wait => {
            let label = "waiting for model";
            match ts.and_then(|t| wait_span_text(t, *now)) {
                Some(span) => Some(format!("{label} · {span}")),
                None => Some(label.to_string()),
            }
        }
        PhaseState::Tools => {
            let label = "tools running";
            match ts.and_then(|t| wait_span_text(t, *now)) {
                Some(span) => Some(format!("{label} · {span}")),
                None => Some(label.to_string()),
            }
        }
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
    Line::from(vec![frame, body])
}

/// The live stream block for an in-progress model response
/// (docs/tui-streaming-response.md §6.3).
///
/// Shows the header with an ellipsis ("…") while the stream is open.
/// Once the done line arrives, the header shows "· done".
///
/// The body shows the tail of the accumulated content. The thinking
/// tail and the response text share one window, the last
/// `max_body_lines` rows, thinking above text. The block grows with
/// the content, and its height never shrinks when the response text
/// starts. The thinking slides out as the text arrives. It never
/// collapses suddenly. Any partial tool-call arguments render when no
/// content has arrived yet. A blinking block cursor marks the end of
/// the live text.
///
/// The block sits right after the existing messages, the transcript.
/// It sits above the model status indicator, the working row. It does
/// not scroll with the transcript. The app clears it when the
/// matching log event lands or the loop stops.
///
/// The block grows with the arriving content. The body rows are
/// bounded by `max_body_lines`, which the caller sets to a fraction of
/// the viewport height. A long response extends the block without
/// stealing the whole screen. The transcript absorbs the rest.
fn stream_block_lines(app: &App, width: usize, max_body_lines: usize) -> Vec<Line<'static>> {
    let buf = match app.stream_buf() {
        Some(b) => b,
        None => return Vec::new(),
    };
    let palette = app.palette();
    let prose = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let dim = palette.style(crate::color::Role::Status, Modifier::DIM);
    let label_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolCommand))
        .add_modifier(Modifier::BOLD);
    // The tool name of a partial call: the purple `tool_name` accent
    // (the 2026-09-14 user pass), bold, standing alone with no
    // `tool:` prefix.
    let tool_name_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolName))
        .add_modifier(Modifier::BOLD);
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);

    let mut out: Vec<Line<'static>> = Vec::new();

    // Header row: the status suffix only — the `assistant` type marker
    // is gone (2026-09-06 request: no message-type markers); the block
    // body below is the live response.
    let suffix = if buf.done { " · done" } else { " …" };
    out.push(Line::from(vec![Span::styled(
        suffix.to_string(),
        label_style,
    )]));

    let mut body_lines: Vec<Line<'static>> = Vec::new();

    let has_thinking = app.thinking_shown() && !buf.reasoning.is_empty();
    let has_text = !buf.text.is_empty();

    // Thinking renders above the response text (the natural order is
    // thinking → response). Thinking and text share one content
    // window: when the response starts, the thinking is not suddenly
    // collapsed — the block keeps its height and the oldest thinking
    // lines slide out as the text arrives, so the view never jumps
    // (no flicker at the thinking → text transition).
    let thinking_style = palette.style(crate::color::Role::Thinking, Modifier::empty());
    let mut thinking_tail: Vec<Line<'static>> = Vec::new();
    let mut shows_thinking_label = false;
    if has_thinking && max_body_lines > 0 {
        let mut ids: Vec<&String> = buf.reasoning.keys().collect();
        ids.sort();
        let thinking_text = ids
            .iter()
            .filter_map(|id| buf.reasoning.get(*id))
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !thinking_text.is_empty() {
            shows_thinking_label = true;
            thinking_tail = wrap_thinking(
                &thinking_text,
                wrap_w,
                palette,
                thinking_style,
                app.tool_display().highlight_engine,
            );
        }
    }

    // Text block: the accumulated response text, wrapped with the same
    // markdown/syntax path used for the settled message.
    let text_tail: Vec<Line<'static>> = if has_text {
        wrap_markdown_p(&buf.text, wrap_w, palette, prose)
    } else {
        Vec::new()
    };

    // The shared content window: the last `max_body_lines` rows (one
    // row reserved for the pinned "thinking" label when thinking is
    // shown). While the content fits, the block grows with it; once
    // full, the oldest rows (the front of the thinking) scroll out as
    // the text grows. The block height never shrinks at the
    // thinking → text transition, so the transcript above it does not
    // lurch and the view does not flicker.
    let window = max_body_lines.saturating_sub(usize::from(shows_thinking_label));
    if window > 0 {
        let combined = thinking_tail.len() + text_tail.len();
        let drop = combined.saturating_sub(window);
        let think_take = thinking_tail.len().saturating_sub(drop);
        let text_drop = drop.saturating_sub(thinking_tail.len());
        let text_take = text_tail.len().saturating_sub(text_drop);
        if shows_thinking_label {
            body_lines.push(Line::from(vec![Span::styled(
                "thinking".to_string(),
                thinking_style,
            )]));
        }
        body_lines.extend(
            thinking_tail[thinking_tail.len() - think_take..]
                .iter()
                .cloned(),
        );
        body_lines.extend(text_tail[text_tail.len() - text_take..].iter().cloned());
    }

    // Partial tool-call arguments: one dim line per call.
    if body_lines.is_empty() {
        let mut ids: Vec<&String> = buf.tool_args.keys().collect();
        ids.sort();
        for id in ids {
            let Some((name, args)) = buf.tool_args.get(id) else {
                continue;
            };
            let name_disp = if name.is_empty() {
                id.as_str()
            } else {
                name.as_str()
            };
            let budget = max_body_lines.saturating_sub(body_lines.len());
            if budget == 0 {
                break;
            }
            let shown = trunc(args, wrap_w.saturating_sub(12));
            body_lines.push(Line::from(vec![
                Span::styled(format!("{gutter}{name_disp}"), tool_name_style),
                Span::styled(format!(" {shown}"), dim),
            ]));
        }
    }

    // Apply the gutter to text/thinking body lines that lack it.
    let mut content: Vec<Line<'static>> = body_lines
        .into_iter()
        .map(|l| {
            // The wrap helpers already prefix the gutter when the wrap
            // width accounts for it. If the first span does not start
            // with the gutter, prepend it.
            let has_gutter = l
                .spans
                .first()
                .map_or(false, |s| s.content.starts_with(&gutter));
            if has_gutter {
                l
            } else {
                let mut spans = vec![Span::styled(gutter.clone(), Style::default())];
                spans.extend(l.spans);
                Line::from(spans)
            }
        })
        .collect();

    // Blinking cursor on the last content line while the stream is open.
    if !buf.done && content.is_empty() {
        content.push(Line::from(vec![
            Span::styled(gutter.clone(), dim),
            Span::styled("▊", dim),
        ]));
    } else if !buf.done && !content.is_empty() {
        let now = chrono::Utc::now();
        let blink = (now.timestamp_millis() / 500) % 2 == 0;
        let cursor = if blink { "▊" } else { " " };
        let last = content.last_mut().unwrap();
        last.spans.push(Span::styled(cursor, dim));
    }

    out.extend(content);
    out
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
    selection: Option<((usize, usize), (usize, usize), bool)>,
    sel_bg: Color,
) -> Vec<Line<'static>> {
    let (cl, cc) = cursor;
    lines
        .iter()
        .skip(start)
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
                l.spans.iter().cloned().collect()
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
    sel: &((usize, usize), (usize, usize), bool),
    abs: usize,
) -> Option<(usize, Option<usize>)> {
    let ((al, ac), (el, ec), linewise) = *sel;
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
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut rest = cc;
    for s in l {
        if rest == 0 {
            out.push(Span::styled(s.content.clone(), patch(s)));
            continue;
        }
        let n = s.content.chars().count();
        if rest < n {
            let chars: Vec<char> = s.content.chars().collect();
            let pre: String = chars[..rest].iter().collect();
            let at = chars[rest];
            let post: String = chars[rest + 1..].iter().collect();
            out.push(Span::styled(pre, patch(s)));
            let mut caret = Style::default()
                .bg(Color::Black)
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED);
            if let Some(o) = fg_override {
                caret = o.patch(caret);
            }
            out.push(Span::styled(at.to_string(), caret));
            out.push(Span::styled(post, patch(s)));
            rest = 0;
        } else {
            out.push(Span::styled(s.content.clone(), patch(s)));
            rest -= n;
        }
    }
    if rest > 0 {
        out.push(Span::styled(
            " ",
            Style::default()
                .bg(Color::Black)
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED),
        ));
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
/// lines. The oldest events beyond `TRANSCRIPT_EVENT_CAP` are dropped.
/// The result is cached per (events version, width, reply version)
/// by the caller.
///
/// Extension replies fold in (ui-extension-plan stage 1): when an
/// extension owns an event kind (the first extension in the composed
/// sequence that lists the kind) and a valid `lines` reply is cached
/// for the event, the extension's styled lines replace the built-in
/// render. A missing, stale, or timed-out reply falls back to the
/// built-in render (per-op G5 fallback).
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

/// Build the transcript: the rendered lines plus the line-to-event
/// map a browse yank needs (docs/tui-conversation-browsing.md
/// section 11.3).
pub fn build_transcript(
    app: &App,
    width: usize,
    ext: Option<&crate::ext::ExtHost>,
) -> TranscriptBuild {
    let details = app.call_details();
    let pending = app.oldest_pending_approval().is_some();
    let events = app.events();
    let start = events.len().saturating_sub(TRANSCRIPT_EVENT_CAP);
    // The tool_result ids of the visible window: a tool_call whose
    // result follows merges into the result box (the call line drops
    // for the tools whose result carries the call info).
    let result_ids: std::collections::HashSet<String> = events[start..]
        .iter()
        .filter(|e| e.kind() == EventKind::ToolResult)
        .filter_map(|e| e.get_str("id").map(String::from))
        .collect();
    // The render state (docs/tui-tool-display-port.md section 2, the
    // config part, plus the fold and thinking toggles): the palette,
    // the tool display config, and the app's toggle states.
    let state = RenderState {
        palette: app.palette(),
        tool_display: app.tool_display(),
        tool_expanded: app.tool_expanded(),
        thinking_shown: app.thinking_shown(),
        thinking_expanded: app.thinking_expanded(),
        expand_fracs: app.expand_fracs(),
    };
    // The loop supervision (docs/auto-compact-plan.md section 4.6):
    // the transcript session's loop process running bit.
    let running = app.active().is_some_and(|s| app.loop_running(s));
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
    for (i, e) in events[start..].iter().enumerate() {
        // ext_status is shared UI state: suppressed from the transcript
        // by default. ext_status events add no rows, and add no blank
        // separators. The log keeps ext_status events
        // (docs/ui-extension.md 5).
        if e.kind() == EventKind::ExtStatus {
            continue;
        }
        if !all.is_empty() {
            all.push(Line::from(""));
            line_raw.push(None);
        }
        let event_id = (start + i) as u64;
        // Record the start of a tool-result block for click hit-testing.
        let tr_start = if e.kind() == EventKind::ToolResult {
            Some(all.len())
        } else {
            None
        };
        let (segs, raws): (Vec<Line<'static>>, Vec<Option<String>>) =
            if let Some(owner) = ext.and_then(|h| h.owner_for_kind(e.kind())) {
                match ext.unwrap().lookup_lines(owner, event_id) {
                    Some(lines) => {
                        let lines = ext_lines_guttered(&lines, width);
                        let raws: Vec<Option<String>> = vec![None; lines.len()];
                        (lines, raws)
                    }
                    // No valid reply for this event: the built-in render
                    // is the fallback.
                    None => {
                        let builder = event_lines()
                            .e(e)
                            .pending(pending)
                            .call_details(&details)
                            .result_ids(&result_ids)
                            .width(width.max(GUTTER + 8))
                            .event_id(event_id)
                            .state(&state)
                            .loop_running(running)
                            .compaction_last_open(last_open.get(i).copied().unwrap_or(false));
                        if let Some(h) = ext {
                            builder.ext(h).call()
                        } else {
                            builder.call()
                        }
                    }
                }
            } else {
                let builder = event_lines()
                    .e(e)
                    .pending(pending)
                    .call_details(&details)
                    .result_ids(&result_ids)
                    .width(width.max(GUTTER + 8))
                    .event_id(event_id)
                    .state(&state)
                    .loop_running(running)
                    .compaction_last_open(last_open.get(i).copied().unwrap_or(false));
                if let Some(h) = ext {
                    builder.ext(h).call()
                } else {
                    builder.call()
                }
            };
        all.extend(segs);
        line_raw.extend(raws);
        // Record the end of the tool-result block span.
        if let Some(s) = tr_start {
            if let Some(id) = e.get_str("id") {
                block_spans.insert(id.to_string(), (s, all.len()));
            }
        }
    }
    TranscriptBuild {
        lines: all,
        line_raw,
        block_spans,
    }
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
    let area = f.area();
    if area.width < 12 || area.height < 6 {
        return;
    }

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
    // The loop-phase bit (docs/tui-model-wait-indicator.md): a
    // running loop names its phase (`[wait]`, `[tools]`); an
    // idle loop or an unknown marker keeps the plain bit.
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
    // The live stream block (docs/tui-streaming-response.md section
    // 6.3): pinned right after the existing messages (the transcript)
    // and above the model status indicator while a model response
    // streams. Empty while no model call is in flight. The block
    // grows with the arriving content: the body is bounded to half
    // the viewport height, so a long response extends the block
    // without stealing the whole screen (the transcript's Min(2)
    // absorbs the rest).
    let stream_max_body = inner.height as usize / 2;
    let stream_lines = stream_block_lines(app, inner.width as usize, stream_max_body);
    let mut constraints: Vec<Constraint> = vec![Constraint::Min(2)];
    // The live stream block owns one layout cell per row it renders
    // (docs/tui-streaming-response.md section 6.3): it sits right
    // after the existing messages, above the model status indicator.
    // No cell when no model response is streaming.
    if !stream_lines.is_empty() {
        constraints.push(Constraint::Length(stream_lines.len() as u16));
    }
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
    // like the `pi` working indicator.
    if running {
        constraints.push(Constraint::Length(1));
    }
    // The host-reserved row above the input box (docs/ui-extension.md
    // section 4, `row` capability): the row owner's last valid
    // `row_spec` lines, one layout cell per line. No cell when no row
    // extension is installed, or when the owner's content is empty
    // (the bare TUI shows no row).
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
    let total = app.transcript_lines(text_w, Some(host)).len();
    // Store block spans and transcript top row for mouse-click hit-testing
    // (docs/tui-tool-display-fancy.md section 6).
    let spans = app.transcript_block_spans(text_w, Some(host));
    app.set_block_spans(spans);
    app.set_transcript_top_row(t_area.y);
    let mut scroll = scroll0;
    if browse_active {
        let grew = app.take_events_grew();
        app.browse().sync(total, h, &mut scroll, grew);
        app.set_scroll(scroll);
    }
    let start = total.saturating_sub(scroll + h);
    app.set_transcript_visible_start(start);
    // The cursor col clamps to the visible cursor line length
    // (section 4.1): a transient lines read, released before the
    // mutation.
    if browse_active {
        let (cl, _) = app.browse_ref().line_col();
        if cl >= start && cl < start + h {
            let len = {
                let ls = app.transcript_lines(text_w, Some(host));
                ls[cl]
                    .spans
                    .iter()
                    .map(|s| s.content.chars().count())
                    .sum::<usize>()
            };
            app.browse().clamp_col(len);
        }
    }
    // The press-path layout: the line texts and the width the
    // browse motions and the search read, plus the raw source texts
    // the browse yank prefers over the rendered lines (section 11.3).
    if browse_active {
        let texts: Vec<String> = app
            .transcript_lines(text_w, Some(host))
            .iter()
            .map(ToString::to_string)
            .collect();
        let line_raw = app.transcript_raw(text_w, Some(host));
        app.set_browse_layout(total, h, text_w, texts, line_raw);
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
    let lines = app.transcript_lines(text_w, Some(host));
    let window = &lines[start..];
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
                lines,
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
    // Live stream block (docs/tui-streaming-response.md section 6.3):
    // the in-progress model response, rendered right after the
    // existing messages, above the model status indicator (working
    // row). It does not scroll with the transcript; the settle event
    // moves the text into the transcript and clears the block.
    if !stream_lines.is_empty() {
        for (i, l) in stream_lines.iter().enumerate() {
            if i as u16 >= rows[row].height {
                break;
            }
            let sub = ratatui::layout::Rect {
                x: rows[row].x,
                y: rows[row].y + i as u16,
                width: rows[row].width,
                height: 1,
            };
            f.render_widget(Paragraph::new(l.clone()), sub);
        }
        row += 1;
    }

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
        let previewer = crate::picker::preview::FilePreviewer::new(50);
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

        let hints = "enter ok · esc keep · ctrl-j/ctrl-k move · ctrl-p preview · ctrl-i scope";
        // Clone the palette so the mutable picker borrow below does not
        // overlap an immutable borrow of the same app.
        let palette = app.palette().clone();
        crate::picker::render::render_picker()
            .f(f)
            .state(app.picker())
            .snapshot(&snap)
            .layout(&layout)
            .previewer(&previewer)
            .hints(hints)
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
        let joined: String = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("goal_complete"), "bare tool name: {joined:?}");
        assert!(!joined.contains("tool:"), "no tool: prefix: {joined:?}");
        assert!(
            !joined.contains('"'),
            "no raw args JSON on the call line: {joined:?}"
        );
    }
}

// ── issue #4: user-message box, no markers, no indent ────────────────

#[cfg(test)]
mod user_box_tests {
    use super::user_box_rows;
    use crate::color::{Level, Palette};
    use ratatui::text::{Line, Span};

    fn joined(lines: &[Line<'static>]) -> String {
        lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")
    }

    /// The user panel: a rounded bordered box titled "User" on the
    /// tool-result panel background — no `user` marker, no content
    /// gutter (docs/tui_feature_requests_from_human.md 2026-09-06).
    #[test]
    fn user_box_has_title_and_background() {
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
        // Every cell of every row carries the tool-result background,
        // so the panel reads as one lighter band.
        let bg = crate::tool_display::box_bg(&palette, false);
        for line in &lines {
            for span in &line.spans {
                assert_eq!(
                    span.style.bg,
                    Some(bg),
                    "every cell on the panel background: {j:?}"
                );
            }
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
        let tool_display = crate::tool_display::ToolDisplay::preset(
            crate::tool_display::Preset::OpenCode,
        );
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
        let joined = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
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
}
