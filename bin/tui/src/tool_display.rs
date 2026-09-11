//! Tool result display: the port of `pi-tool-display`
//! (docs/tui-tool-display-port.md) plus the content layer of
//! docs/tui-tool-result-truncation.md.
//!
//! The request in one line: wrap every tool result in a lighter
//! panel (no border lines: the lighter background only, the
//! 2026-09-14 user pass), fold long output to a preview, and one
//! global key expands every collapsed block. The content layer owns
//! what shows and how many lines: `Read` and `Write` results
//! truncate to a preview, `Edit` results render as a diff, and
//! `bash` output folds to a collapsed line count. The panel header
//! row names the tool in the purple `tool_name` accent, bold, with
//! the result status.
//!
//! Reference: `pi-tool-display` (github.com/MasuRii/pi-tool-display,
//! v0.5.0, pinned rev `91cef758`), the config shape and the presets
//! ported here:
//!
//! - Per-tool limits: `previewLines` 8 for read,
//!   `bashCollapsedLines` 10, `diffCollapsedLines` 24.
//! - Output modes: `hidden` / `summary` / `preview` for read and
//!   bash.
//! - Presets: `opencode` (default), `balanced`, `verbose`.
//! - Fold: the preview shows the first lines only; a muted hint
//!   states the remainder and the expand key, like
//!   `... (173 more lines • Ctrl+O to expand)`.
//! - Expand: one global key (`Ctrl+O`) toggles every collapsed
//!   block to the full output; the expanded preview caps at
//!   `expandedPreviewMaxLines` (4000).
//!
//! This module is pure: the render call in render.rs feeds it the
//! event value, the config state, and the pane width. It returns
//! styled segments. No I/O, no decision logic.

use bon::builder;
use ratatui::style::{Color, Modifier, Style};

use crate::highlight::Seg;
use crate::image_render;

use ansi_to_tui::IntoText as _;

/// The tool-result presets of the port
/// (docs/tui-tool-display-port.md section 2). The `opencode` preset
/// is the default: read content shows a syntax-highlighted preview,
/// bash collapses to the first 10 lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// The default preset: read shows a syntax-highlighted
    /// preview, bash collapsed to the first 10 lines.
    OpenCode,
    /// Compact summaries: read line count, bash line count.
    Balanced,
    /// Larger previews: read shows 12 preview lines, bash 20.
    Verbose,
}

/// Parse one preset name from the config. Case-insensitive.
pub fn parse_preset(s: &str) -> Option<Preset> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "opencode" => Preset::OpenCode,
        "balanced" => Preset::Balanced,
        "verbose" => Preset::Verbose,
        _ => return None,
    })
}

/// The output mode of one tool's result body. `Hidden` shows no body
/// lines; `Summary` one muted count line; `Preview` the first lines
/// of the body plus the fold hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Hidden,
    Summary,
    Preview,
}

pub fn parse_output_mode(s: &str) -> Option<OutputMode> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "hidden" => OutputMode::Hidden,
        "summary" => OutputMode::Summary,
        "preview" => OutputMode::Preview,
        _ => return None,
    })
}

/// The diff layout of `Edit` and `Write` results
/// (docs/tui-tool-result-truncation.md: the adaptive diff).
/// `Auto` picks split at wide panes and unified at narrow ones;
/// `Split` and `Unified` force the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffView {
    Auto,
    Split,
    Unified,
}

pub fn parse_diff_view(s: &str) -> Option<DiffView> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "auto" => DiffView::Auto,
        "split" => DiffView::Split,
        "unified" => DiffView::Unified,
        _ => return None,
    })
}

/// Which engine colors the code in tool-result previews
/// (docs/tui-tool-display-fancy.md section 4). `TreeSitter` is the
/// default (the 2026-09-11 request): the `tui-highlight` crate
/// bundles tree-sitter grammars for ~13 languages and carries
/// block-comment and fence state across lines. `Builtin` keeps the
/// hand-rolled tokenizer as an opt-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightEngine {
    Builtin,
    TreeSitter,
}

pub fn parse_highlight_engine(s: &str) -> Option<HighlightEngine> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "builtin" | "hand-rolled" | "builtin-tokenizer" => HighlightEngine::Builtin,
        "tree-sitter" | "treesitter" | "syntect" => HighlightEngine::TreeSitter,
        _ => return None,
    })
}

/// How tool-result blocks expand
/// (docs/tui-tool-display-fancy.md section 6).
/// `Global` keeps the current Ctrl+O behaviour (all blocks expand
/// together). `Focus` auto-expands only the block nearest to the
/// viewport centre (or the tail). `Click` expands a block only when
/// the user clicks on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExpandMode {
    #[default]
    Global,
    Focus,
    Click,
}

pub fn parse_expand_mode(s: &str) -> Option<ExpandMode> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "global" => ExpandMode::Global,
        "focus" => ExpandMode::Focus,
        "click" => ExpandMode::Click,
        _ => return None,
    })
}

/// A stateful, per-file code highlighter chosen by [`HighlightEngine`]
/// (the docs/tui-syntax-highlighting.md section 5 seam: the engine
/// swap touches no caller). Both variants feed one hard line at a
/// time in file order, so `/* … */` block comments and similar
/// multi-line context stay coherent across the lines of one body.
/// `line` returns the segments in source order; plain (unscoped) runs
/// come out as `Style::default()` so the caller's recolor keeps them
/// in the `Code` tone.
pub enum CodeHl {
    Builtin(crate::highlight::CodeHighlighter),
    TreeSitter(tui_highlight::Highlighter),
}

impl CodeHl {
    pub fn new(engine: HighlightEngine) -> Self {
        match engine {
            HighlightEngine::Builtin => Self::Builtin(crate::highlight::CodeHighlighter::new()),
            HighlightEngine::TreeSitter => Self::TreeSitter(tui_highlight::Highlighter::new()),
        }
    }

    /// Highlight one hard line. `lang` is a
    /// [`crate::highlight::language_from_path`] token (or a plain
    /// extension / fence tag); `None` keeps the line plain.
    pub fn line(
        &mut self,
        line: &str,
        lang: Option<&str>,
        palette: &crate::color::Palette,
    ) -> Vec<crate::highlight::Seg> {
        match self {
            Self::Builtin(h) => h.line(line, lang, palette),
            Self::TreeSitter(h) => {
                // tui-highlight returns Catppuccin Macchiato RGB;
                // lower to the terminal capability level.
                h.line(line, lang)
                    .into_iter()
                    .map(|(st, text)| {
                        (crate::color::lower_style(st, palette.level()), text)
                    })
                    .collect()
            }
        }
    }
}

/// The tool-result display config of one TUI run
/// (docs/tui-tool-display-port.md section 2: the config part). The
/// preset value plus per-field overrides; the overrides win.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolDisplay {
    pub read_mode: OutputMode,
    pub bash_mode: OutputMode,
    /// The preview line count of read results (`previewLines`, 8).
    pub preview_lines: usize,
    /// The collapsed line count of bash output
    /// (`bashCollapsedLines`, 10).
    pub bash_collapsed_lines: usize,
    /// The collapsed line count of edit/write diffs
    /// (`diffCollapsedLines`, 24).
    pub diff_collapsed_lines: usize,
    /// The expanded preview cap (`expandedPreviewMaxLines`, 4000).
    pub expanded_preview_max_lines: usize,
    /// The diff layout mode (`auto`, `split`, `unified`).
    pub diff_view: DiffView,
    /// The code-highlight engine for the read / write tool displays and
    /// the thinking-block code render (docs/tui-tool-display-fancy.md
    /// section 4): `tree-sitter` (grammar-based, the default since the
    /// 2026-09-11 request — many languages, stateful) or
    /// `builtin` (the hand-rolled tokenizer, the opt-out).
    pub highlight_engine: HighlightEngine,
    /// How tool-result blocks expand
    /// (docs/tui-tool-display-fancy.md section 6).
    pub expand_mode: ExpandMode,
    /// The animation duration in milliseconds for expand/collapse and
    /// fade-in transitions (docs/tui-tool-display-fancy.md section 6).
    /// `0` disables animation (instant toggle).
    pub anim_ms: u64,
}

impl ToolDisplay {
    /// The value table of a preset (docs/tui-tool-display-port.md
    /// section 2: the presets). `Custom` is the `opencode` value
    /// table: a user override of a preset field is a `Custom`
    /// preset, not the default.
    pub fn preset(p: Preset) -> Self {
        match p {
            Preset::OpenCode => Self {
                read_mode: OutputMode::Preview,
                bash_mode: OutputMode::Preview,
                preview_lines: 8,
                bash_collapsed_lines: 10,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
                highlight_engine: HighlightEngine::TreeSitter,
                expand_mode: ExpandMode::Global,
                anim_ms: 300,
            },
            Preset::Balanced => Self {
                read_mode: OutputMode::Summary,
                bash_mode: OutputMode::Summary,
                preview_lines: 8,
                bash_collapsed_lines: 10,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
                highlight_engine: HighlightEngine::TreeSitter,
                expand_mode: ExpandMode::Global,
                anim_ms: 300,
            },
            Preset::Verbose => Self {
                read_mode: OutputMode::Preview,
                bash_mode: OutputMode::Preview,
                preview_lines: 12,
                bash_collapsed_lines: 20,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
                highlight_engine: HighlightEngine::TreeSitter,
                expand_mode: ExpandMode::Global,
                anim_ms: 300,
            },
        }
    }

    /// The split threshold of the `auto` diff layout: split when the
    /// pane is at least this wide, unified below it (the reference
    /// `diffSplitMinWidth` 120, in half the two-pane width).
    /// Lowered to 30 so a typical 80-column terminal triggers the
    /// two-area split view (the user requirement of two separate
    /// areas for before/after, matching pi-tool-display).
    pub const DIFF_SPLIT_MIN_WIDTH: usize = 30;

    /// The layout of the `auto` mode at `width` columns: split when
    /// the pane fits two panes of the diff plus the divider.
    pub fn diff_layout(&self, width: usize) -> DiffView {
        match self.diff_view {
            DiffView::Auto => {
                if width >= Self::DIFF_SPLIT_MIN_WIDTH * 2 {
                    DiffView::Split
                } else {
                    DiffView::Unified
                }
            }
            other => other,
        }
    }
}

// ── the fold/expand render ──────────────────────────────────────

/// One visual row of the tool-result body: one or more styled
/// segments (the split diff row carries the left pane, the divider,
/// and the right pane). One row is one terminal line of the box;
/// the box border lines around it are one row each.
pub type BodyRow = Vec<(Style, String)>;

/// The body rows of one tool result at the display state, as styled
/// segments for the word-wraper. `expanded` is the global fold/
/// expand toggle (the `Ctrl+O` key): collapsed shows the mode's
/// lines, expanded shows the body up to `expanded_preview_max_lines`.
///
/// The body is the tool-specific compact output
/// (docs/tui-tool-result-truncation.md section 1: the content
/// layer). The fold state owns how many lines of it show.
#[builder]
pub fn body_rows(
    tool: &str,
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    err: bool,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
    /// The per-block expand fraction (0.0–1.0) for animation. A value
    /// of `-1.0` (the default) means "use the `expanded` bool as-is".
    /// When set to a value in `[0.0, 1.0]`, it interpolates the body
    /// cap between the collapsed and expanded caps (docs/tui-tool-
    /// display-fancy.md section 6).
    #[builder(default = -1.0)]
    expand_frac: f64,
) -> Vec<BodyRow> {
    let out = Style::default().fg(palette.color(crate::color::Role::ToolOutput));
    let hint = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .add_modifier(Modifier::DIM);
    let diff_added = Style::default().fg(palette.color(crate::color::Role::DiffAdded));
    let diff_removed = Style::default().fg(palette.color(crate::color::Role::DiffRemoved));
    let error = Style::default().fg(palette.color(crate::color::Role::Error));
    let command = Style::default().fg(palette.color(crate::color::Role::ToolCommand));
    let code = Style::default().fg(palette.color(crate::color::Role::Code));
    match tool {
        "read" => {
            let builder = read_body()
                .value(value)
                .cfg(cfg)
                .hint(&hint)
                .code(&code)
                .palette(palette)
                .expanded(expanded)
                .width(width)
                .expand_frac(expand_frac);
            if let Some(ca) = call_args {
                builder.call_args(ca).call()
            } else {
                builder.call()
            }
        }
        "write" => {
            let builder = write_body()
                .value(value)
                .cfg(cfg)
                .out(&out)
                .hint(&hint)
                .diff_added(&diff_added)
                .code(&code)
                .palette(palette)
                .expanded(expanded)
                .width(width)
                .expand_frac(expand_frac);
            if let Some(ca) = call_args {
                builder.call_args(ca).call()
            } else {
                builder.call()
            }
        }
        "edit" => edit_body()
            .value(value)
            .cfg(cfg)
            .out(&out)
            .hint(&hint)
            .diff_added(&diff_added)
            .diff_removed(&diff_removed)
            .code(&code)
            .palette(palette)
            .expanded(expanded)
            .width(width)
            .expand_frac(expand_frac)
            .call(),
        "bash" => bash_body()
            .value(value)
            .cfg(cfg)
            .palette(palette)
            .out(&out)
            .hint(&hint)
            .error(&error)
            .command(&command)
            .err(err)
            .expanded(expanded)
            .width(width)
            .expand_frac(expand_frac)
            .call(),
        _ => generic_body(value, cfg, &out, &hint, expanded, width, expand_frac),
    }
}

/// The body content plan of one output mode at the fold state.
/// The global expand (the `Ctrl+O` key) overrides every mode
/// (the port spec of the module header): a hidden or summary block
/// shows its body lines, capped by the expanded cap. A summary
/// line is the one muted count row; no body keeps the mode's hint
/// row only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyPlan {
    /// No body lines: the mode's hint row only.
    NoBody,
    /// One muted count row.
    SummaryLine,
    /// The body lines, capped by the fold state.
    Lines,
}

fn body_plan(mode: OutputMode, expanded: bool) -> BodyPlan {
    if expanded {
        return BodyPlan::Lines;
    }
    match mode {
        OutputMode::Hidden => BodyPlan::NoBody,
        OutputMode::Summary => BodyPlan::SummaryLine,
        OutputMode::Preview => BodyPlan::Lines,
    }
}

/// The folded-preview hint row: `... (N more lines • Ctrl+O to expand)`.
/// `N` is the hidden line count. When the global expand is on, the
/// key hint drops (the block is already expanded). The hint
/// shortens on narrow panes: the key hint drops first, then the
/// count sentence to the ellipsis run.
fn fold_hint(
    remaining: usize,
    expanded: bool,
    hint: &Style,
    width: usize,
) -> Option<(Style, String)> {
    if remaining == 0 {
        return None;
    }
    let unit = if remaining == 1 { "line" } else { "lines" };
    let full = format!("... ({remaining} more {unit} • Ctrl+O to expand)");
    let short = format!("... ({remaining} more {unit})");
    let text = if expanded || full.chars().count() > width {
        if short.chars().count() <= width {
            short
        } else {
            // The pane is too narrow for the count itself: show the
            // ellipsis run only, never a clipped sentence.
            "...".to_string()
        }
    } else {
        full
    };
    Some((*hint, text))
}

/// The body rows of a `Read` result (the truncation request of
/// docs/tui-tool-result-truncation.md section 1). The log value
/// carries `{text}` (the fuller tool shape adds `path` and
/// `total_lines`); the body is the compact read output: the line
/// count, then the output-mode lines of the text. Collapsed, the
/// preview shows `preview_lines` (8) lines of the text; expanded,
/// up to the expanded cap.
/// The file path for language detection resolves `value.path`
/// first, then the call argument `file_path` (the log stores the
/// path in the call arguments, not the result value).
/// The read tool prefixes each content line with its 1-based file
/// line number (`"12: "`). Split that prefix off so the syntax
/// highlighter only sees file content. Lines without a digit-colon
/// prefix (annotation lines like `(5 lines omitted)` or
/// `(End of file ...)`) come back unsplit with a `None` number.
fn split_line_number(line: &str) -> (Option<&str>, &str) {
    match line.find(':') {
        Some(pos) if pos > 0 && line[..pos].bytes().all(|b| b.is_ascii_digit()) => {
            let rest = line[pos + 1..].strip_prefix(' ').unwrap_or(&line[pos + 1..]);
            (Some(&line[..pos]), rest)
        }
        _ => (None, line),
    }
}

#[builder]
fn read_body(
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    cfg: &ToolDisplay,
    hint: &Style,
    code: &Style,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
    /// Per-block expand fraction for animation. `-1.0` means "use
    /// the `expanded` bool as-is" (the default for callers that do
    /// not animate).
    #[builder(default = -1.0)]
    expand_frac: f64,
) -> Vec<BodyRow> {
    // Image results: the kernel sends `{type:"image", data, mime_type, path}`
    // inside `value.details` (or top-level). Render as half-block text so
    // the image scrolls with the transcript (docs/NEW-refactor.md must-do 3).
    if let Some(rows) = image_render::image_body_rows(value, width) {
        return rows;
    }
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let details = value.get("details").cloned().unwrap_or_default();
    // Read `total_lines` from details (kernel) or top-level (legacy/test).
    let total = details.get("total_lines").and_then(|t| t.as_u64())
        .or_else(|| value.get("total_lines").and_then(|t| t.as_u64()));
    let lines: Vec<&str> = text.lines().collect();
    let count = total.map(|n| n as usize).unwrap_or(lines.len());
    let mut rows: Vec<BodyRow> = Vec::new();
    let eff_expanded = if expand_frac >= 0.0 { expand_frac > 0.0 } else { expanded };
    match body_plan(cfg.read_mode, eff_expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, format!("↳ {count} lines hidden"))]);
        }
        BodyPlan::SummaryLine => {
            let n = if count == 1 { "1 line" } else { "lines" };
            rows.push(vec![(*hint, format!("↳ {count} {n}"))]);
        }
        BodyPlan::Lines => {
            let frac = if expand_frac >= 0.0 {
                expand_frac.clamp(0.0, 1.0)
            } else {
                if expanded { 1.0 } else { 0.0 }
            };
            let collapsed = cfg.preview_lines;
            let cap = collapsed
                + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed)) as f64 * frac)
                    .round() as usize;
            let remaining = lines.len().saturating_sub(cap);
            // The shared syntax-highlight entry point: the language is
            // detected from the read path; unknown types stay plain.
            // This is the same engine the picker preview pane uses
            // (docs/tui-file-picker.md section 9). The log stores the
            // path in the call arguments (`file_path`), not the result
            // value, so resolve value first, then the call arguments.
            let path = value
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    details
                        .get("path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    call_args
                        .and_then(|a| a.get("file_path").or_else(|| a.get("path")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let lang = crate::highlight::language_from_path(&path);
            // The active engine (tree-sitter by default since the
            // 2026-09-11 request), stateful across the body's lines.
            let mut hl = CodeHl::new(cfg.highlight_engine);
            for raw in lines.iter().take(cap) {
                let (num, content) = split_line_number(raw);
                let mut row: BodyRow = Vec::new();
                if let Some(n) = num {
                    row.push((*code, format!("{n}: ")));
                }
                if num.is_some() {
                    for (st, s) in hl.line(content, lang, palette) {
                        let st = if st == Style::default() { *code } else { st };
                        row.push((st, s));
                    }
                } else {
                    row.push((*code, raw.to_string()));
                }
                rows.push(row);
            }
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    rows
}

/// The body of a `Write` result: the adaptive write diff of
/// docs/tui-tool-result-truncation.md. The result value carries the
/// summary (`{text, path, operation, bytes}`); the written content
/// is the call argument `content` (the TUI holds the call
/// arguments through `call_args`). The diff shows the new content
/// as added lines (the pi `toolDiffAdded` color), collapsed to
/// `diff_collapsed_lines`. The summary row names the size, like the
/// reference write summary.
#[builder]
fn write_body(
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    diff_added: &Style,
    code: &Style,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
    #[builder(default = -1.0)]
    expand_frac: f64,
) -> Vec<BodyRow> {
    // Read fields from `details` (kernel-wrapped) or top-level (legacy/test).
    let path = value.get("details").and_then(|d| d.get("path"))
        .or_else(|| value.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let operation = value.get("details").and_then(|d| d.get("operation"))
        .or_else(|| value.get("operation"))
        .and_then(|v| v.as_str())
        .unwrap_or("create");
    let bytes = value.get("details").and_then(|d| d.get("bytes"))
        .or_else(|| value.get("bytes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let content = call_args
        .and_then(|a| a.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let content_lines: Vec<&str> = content.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    // The write summary: the line count and the byte size, inline,
    // like the reference write summary (the adaptive write diff).
    rows.push(vec![(
        *hint,
        format!(
            "↳ {} {} lines, {} bytes",
            match operation {
                "update" => "overwrite",
                _ => "create",
            },
            content_lines.len(),
            bytes
        ),
    )]);
    if !path.is_empty() {
        rows.push(vec![(*out, path.to_string())]);
    }
    if !content_lines.is_empty() {
        let frac = if expand_frac >= 0.0 {
            expand_frac.clamp(0.0, 1.0)
        } else {
            if expanded { 1.0 } else { 0.0 }
        };
        let collapsed = cfg.diff_collapsed_lines;
        let cap = collapsed
            + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed)) as f64 * frac)
                .round() as usize;
        let remaining = content_lines.len().saturating_sub(cap);
        // pi-tool-display write rendering (docs/tui-tool-display-fancy.md
        // section 5.2): each new/overwritten line is a `▌` marker plus a
        // line-number gutter, with a dark green whole-line shade and
        // syntax-highlighted content — the added status lives in the
        // marker + shade, not a solid green `+ text` foreground.
        use crate::color::Role;
        let lang = crate::highlight::language_from_path(&path);
        let num_w = content_lines.len().to_string().len().max(2);
        let shade_bg = palette.color(Role::DiffAddedBg);
        let marker = diff_added.bg(shade_bg);
        // One stateful highlighter for the whole written file (the
        // 2026-09-11 request): `/* … */` block comments stay coherent
        // across the lines instead of restarting at each line.
        let mut hl = CodeHl::new(cfg.highlight_engine);
        for (li, l) in content_lines.iter().take(cap).enumerate() {
            let num = format!("{:>num_w$}", li + 1, num_w = num_w);
            let gutter = format!("▌ {num} │ ");
            let mut row: BodyRow = vec![(marker, gutter)];
            for (st, s) in hl.line(l, lang, palette) {
                let st = if st == Style::default() { *code } else { st };
                row.push((st.bg(shade_bg), s));
            }
            rows.push(row);
        }
        if let Some(h) = fold_hint(remaining, expanded, hint, width) {
            rows.push(vec![h]);
        }
    }
    rows
}

/// The body of an `Edit` result: the adaptive edit diff of
/// docs/tui-tool-result-truncation.md. The value carries `before`
/// (the old string) and `after` (the new string). The unified
/// layout colors the removed lines in the pi `toolDiffRemoved`
/// role and the added lines in the pi `toolDiffAdded` role; the
/// split layout shows the two sides in one wide row. Collapsed to
/// `diff_collapsed_lines`.
#[builder]
fn edit_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    diff_added: &Style,
    diff_removed: &Style,
    code: &Style,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
    #[builder(default = -1.0)]
    expand_frac: f64,
) -> Vec<BodyRow> {
    // Read fields from `details` (kernel-wrapped) or top-level (legacy/test).
    let path = value.get("details").and_then(|d| d.get("path"))
        .or_else(|| value.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let before: Vec<&str> = value.get("details").and_then(|d| d.get("before"))
        .or_else(|| value.get("before"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .lines()
        .collect();
    let after: Vec<&str> = value.get("details").and_then(|d| d.get("after"))
        .or_else(|| value.get("after"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .lines()
        .collect();
    let replace_all = value.get("details").and_then(|d| d.get("replace_all"))
        .or_else(|| value.get("replace_all"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut rows: Vec<BodyRow> = Vec::new();
    // The diff stats row with a proportional green/red progress bar
    // (docs/tui-tool-display-fancy.md section 5.2): the counts come
    // from the positional diff (how many lines are added / removed,
    // not the file sizes), and the `[████]` bar shows the ratio:
    // green `█` runs are added lines, red `█` runs are removed lines,
    // each proportional to its share of the total.
    {
        use crate::color::Role;
        let max_n = before.len().max(after.len());
        let (mut added, mut removed) = (0usize, 0usize);
        for i in 0..max_n {
            let b_ok = i < before.len();
            let a_ok = i < after.len();
            if b_ok && (!a_ok || before[i] != after[i]) {
                removed += 1;
            }
            if a_ok && (!b_ok || after[i] != before[i]) {
                added += 1;
            }
        }
        let total = added + removed;
        let bar_w = 12usize;
        let layout_label = match cfg.diff_layout(width) {
            DiffView::Split => "split",
            _ => "unified",
        };
        let mut stats: Vec<(Style, String)> = Vec::new();
        stats.push((
            *hint,
            format!("↳ diff +{added} -{removed} • {layout_label} "),
        ));
        if total > 0 {
            // Integer split of the bar: green gets `added/total` of the
            // width (floor), red gets the remainder. When one side is
            // zero the whole bar is the other color.
            let green_w = added * bar_w / total;
            let red_w = bar_w - green_w;
            stats.push((*hint, "[".to_string()));
            if green_w > 0 {
                stats.push((
                    Style::default().fg(palette.color(Role::DiffAdded)),
                    "█".repeat(green_w),
                ));
            }
            if red_w > 0 {
                stats.push((
                    Style::default().fg(palette.color(Role::DiffRemoved)),
                    "█".repeat(red_w),
                ));
            }
            stats.push((*hint, "]".to_string()));
        }
        rows.push(stats);
    }
    if !path.is_empty() {
        rows.push(vec![(*out, path.to_string())]);
    }
    if replace_all {
        rows.push(vec![(*hint, "replace_all".to_string())]);
    }
    let frac = if expand_frac >= 0.0 {
        expand_frac.clamp(0.0, 1.0)
    } else {
        if expanded { 1.0 } else { 0.0 }
    };
    let collapsed = cfg.diff_collapsed_lines;
    let cap = collapsed
        + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed)) as f64 * frac)
            .round() as usize;
    let total = before.len() + after.len();
    // The pi-tool-display coloring model (docs/tui-tool-display-fancy.md
    // section 5.2): changed lines are shown by a darker whole-line
    // background shade plus a lighter `▌` marker in the line-number
    // gutter, while the text itself keeps its normal (syntax-highlighted)
    // tone instead of a solid red/green foreground.
    use crate::color::Role;
    // The lighter marker colors reuse the diff accent foregrounds.
    let marker_added = *diff_added;
    let marker_removed = *diff_removed;
    // Language detection drives the syntax highlighter for diff text.
    let lang = crate::highlight::language_from_path(&path);
    let layout = cfg.diff_layout(width);
    let max_n = before.len().max(after.len());
    let num_w = max_n.to_string().len().max(2);
    match layout {
        DiffView::Split => {
            // Two-area split diff (pi-tool-display style):
            //   old          │  new
            //   ─────────────┼─────────────
            // ▌ 12│old line  │▌ 12│new line
            //
            // Each pane owns (width - 8) / 2: the left pane, the
            // ` │ ` divider, and the right pane. Changed panes carry a
            // whole-line background shade and a `▌` gutter marker.
            let pane = width.saturating_sub(8) / 2;

            // Header: labels the two areas so the user sees which
            // side is the old content and which is the new. The label
            // spans the marker + number columns (width `num_w + 1`,
            // matching the body gutter) so the `│` dividers line up
            // with the body's gutter separators on every row.
            let hdr_l = format!("{:<w$} │ ", "old", w = num_w + 1);
            let hdr_r = format!("{:<w$} │ ", "new", w = num_w + 1);
            rows.push(vec![
                (*hint, pad_cell(hdr_l, pane)),
                (*hint, " │ ".to_string()),
                (*hint, pad_cell(hdr_r, pane)),
            ]);
            // Divider line under the header.
            let sep = "─".repeat(pane.min(width / 2));
            rows.push(vec![
                (*hint, pad_cell(sep.clone(), pane)),
                (*hint, " │ ".to_string()),
                (*hint, pad_cell(sep, pane)),
            ]);

            let mut i = 0usize;
            let mut shown = 0usize;
            while (i < before.len() || i < after.len()) && shown < cap {
                let b = before.get(i).copied().unwrap_or("");
                let a = after.get(i).copied().unwrap_or("");
                // Line numbers (1-based) for each side; blank when the
                // side has no line at this index.
                let b_num = if i < before.len() {
                    format!("{:>num_w$}", i + 1, num_w = num_w)
                } else {
                    " ".repeat(num_w)
                };
                let a_num = if i < after.len() {
                    format!("{:>num_w$}", i + 1, num_w = num_w)
                } else {
                    " ".repeat(num_w)
                };
                // A side is "changed" when it has a line that differs
                // from the other side (or the other side is short).
                let left_changed = i < before.len()
                    && (i >= after.len() || before[i] != after[i]);
                let right_changed = i < after.len()
                    && (i >= before.len() || after[i] != before[i]);
                // Each pane is two segments: the gutter (marker + number
                // + separator, in the lighter diff accent so the line
                // number reads as a highlighted bar) and the content
                // (neutral tone, with a dark whole-line shade on changed
                // lines). The gutter owns a fixed width so the `│` stays
                // in the same column on every row.
                let gutter_w = num_w + 4; // marker(1) + num(num_w) + " │ "(3)
                let content_w = pane.saturating_sub(gutter_w).max(1);
                let lgutter =
                    format!("{}{} │ ", if left_changed { "▌" } else { " " }, b_num);
                let rgutter =
                    format!("{}{} │ ", if right_changed { "▌" } else { " " }, a_num);
                // Gutter: lighter diff accent + shade on the number.
                let lg_style = if left_changed {
                    Style::default()
                        .fg(palette.color(Role::DiffRemoved))
                        .bg(palette.color(Role::DiffRemovedBg))
                } else {
                    *out
                };
                let rg_style = if right_changed {
                    Style::default()
                        .fg(palette.color(Role::DiffAdded))
                        .bg(palette.color(Role::DiffAddedBg))
                } else {
                    *out
                };
                // Content: neutral tone; the background shade (not a solid
                // red/green foreground) marks the line as added/removed.
                let lc_style = if left_changed {
                    (*out).bg(palette.color(Role::DiffRemovedBg))
                } else {
                    *out
                };
                let rc_style = if right_changed {
                    (*out).bg(palette.color(Role::DiffAddedBg))
                } else {
                    *out
                };
                let mut row: BodyRow = Vec::new();
                row.push((lg_style, pad_cell(lgutter, gutter_w)));
                row.push((lc_style, pad_cell(b.to_string(), content_w)));
                row.push((out.clone(), " │ ".to_string()));
                row.push((rg_style, pad_cell(rgutter, gutter_w)));
                row.push((rc_style, pad_cell(a.to_string(), content_w)));
                rows.push(row);
                shown += 1;
                i += 1;
            }
            let remaining = total.saturating_sub(shown * 2);
            if let Some(h) = fold_hint(remaining / 2, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
        DiffView::Auto | DiffView::Unified => {
            // The narrow / vertical layout (pi-tool-display): each line
            // carries a line-number gutter with a `▌` marker on changed
            // lines, a whole-line background shade, and syntax-
            // highlighted content. Removed lines (old content) come
            // first at each position, then the added (new) line.
            let max_lines = before.len().max(after.len());
            let mut shown = 0usize;
            for i in 0..max_lines {
                let b = before.get(i).copied().unwrap_or("");
                let a = after.get(i).copied().unwrap_or("");
                let b_exists = i < before.len();
                let a_exists = i < after.len();
                let changed = b_exists
                    && a_exists
                    && before[i] != after[i];
                let ln = i + 1;
                // Removed line (only when it exists and is not context).
                let removed = b_exists && (changed || !a_exists || (a_exists && before[i] != after[i]));
                if removed && shown < cap {
                    let num = format!("{:>num_w$}", ln, num_w = num_w);
                    let gutter =
                        format!("▌ {num} │ ");
                    let mut row: BodyRow =
                        vec![(with_shade(marker_removed, palette.color(Role::DiffRemovedBg)), gutter)];
                    for (st, s) in highlight_diff_line(b, lang, cfg, palette) {
                        let st = if st == Style::default() { *code } else { st };
                        row.push((with_shade(st, palette.color(Role::DiffRemovedBg)), s));
                    }
                    rows.push(row);
                    shown += 1;
                }
                // Added line (only when it exists and is not context).
                let added = a_exists && (changed || !b_exists);
                if added && shown < cap {
                    let num = format!("{:>num_w$}", ln, num_w = num_w);
                    let gutter =
                        format!("▌ {num} │ ");
                    let mut row: BodyRow =
                        vec![(with_shade(marker_added, palette.color(Role::DiffAddedBg)), gutter)];
                    for (st, s) in highlight_diff_line(a, lang, cfg, palette) {
                        let st = if st == Style::default() { *code } else { st };
                        row.push((with_shade(st, palette.color(Role::DiffAddedBg)), s));
                    }
                    rows.push(row);
                    shown += 1;
                }
                // Context line (both sides equal).
                if !removed && !added {
                    let context = before.get(i).copied().unwrap_or("");
                    if shown < cap {
                        let num = format!("{:>num_w$}", ln, num_w = num_w);
                        let gutter = format!(" {num} │ ");
                        let mut row: BodyRow = vec![(
                            (*out),
                            gutter,
                        )];
                        for (st, s) in highlight_diff_line(&context, lang, cfg, palette) {
                            let st = if st == Style::default() { *code } else { st };
                            row.push((st, s));
                        }
                        rows.push(row);
                        shown += 1;
                    }
                }
            }
            let remaining = total.saturating_sub(shown);
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    rows
}

/// Highlight one diff content line through the active engine. A fresh
/// highlighter is used per line (cheap: the built-in holds two booleans,
/// the tree-sitter handle wraps the per-crate parser). Plain
/// (unscoped) runs come out as `Style::default()` so the caller can
/// recolor them to the `Code` role, matching `read_body`.
fn highlight_diff_line(
    line: &str,
    lang: Option<&str>,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
) -> Vec<crate::highlight::Seg> {
    match cfg.highlight_engine {
        HighlightEngine::Builtin => {
            let mut hl = crate::highlight::CodeHighlighter::new();
            hl.line(line, lang, palette)
        }
        HighlightEngine::TreeSitter => {
            let mut hl = tui_highlight::Highlighter::new();
            hl.line(line, lang)
                .into_iter()
                .map(|(st, text)| {
                    (crate::color::lower_style(st, palette.level()), text)
                })
                .collect()
        }
    }
}

/// Apply an optional background shade to a style. A `None` shade keeps
/// the style unchanged (the box background is applied later by
/// `box_rows`); a set shade becomes the whole-line diff tint.
fn with_shade(st: Style, bg: impl Into<Option<ratatui::style::Color>>) -> Style {
    match bg.into() {
        Some(c) => st.bg(c),
        None => st,
    }
}

/// The body of a `bash` result: the collapsed bash output of the
/// port. The value is `{text, exit_code, stdout, stderr, ...}`; the
/// body is the model-facing `text` (the command line, the output,
/// the exit line), collapsed to `bash_collapsed_lines` (10).
/// `Hidden` shows no body; `Summary` one line-count line;
/// Convert text containing ANSI SGR escape sequences into styled
/// `BodyRow`s.  Returns `None` when the text has no `\x1b` so the
/// caller can keep its plain-text path.
fn ansi_to_body_rows(text: &str, fallback: &Style) -> Option<Vec<BodyRow>> {
    if !text.contains('\x1b') {
        return None;
    }
    let parsed = text.as_bytes().into_text().ok()?;
    let mut rows: Vec<BodyRow> = Vec::with_capacity(parsed.lines.len());
    for line in &parsed.lines {
        let mut row: BodyRow = Vec::new();
        for span in &line.spans {
            let content = span.content.to_string();
            if content.is_empty() {
                continue;
            }
            let style = if span.style == Style::default() {
                *fallback
            } else {
                span.style
            };
            row.push((style, content));
        }
        if row.is_empty() {
            row.push((*fallback, String::new()));
        }
        rows.push(row);
    }
    Some(rows)
}

#[builder]
fn bash_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    out: &Style,
    hint: &Style,
    error: &Style,
    command: &Style,
    err: bool,
    expanded: bool,
    width: usize,
    #[builder(default = -1.0)]
    expand_frac: f64,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let so = value.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
    let se = value.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
    let out_count = so.lines().count() + se.lines().count();
    let eff_expanded = if expand_frac >= 0.0 { expand_frac > 0.0 } else { expanded };
    let mut rows: Vec<BodyRow> = Vec::new();
    match body_plan(cfg.bash_mode, eff_expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, "↳ output hidden".to_string())]);
        }
        BodyPlan::SummaryLine => {
            let n = if out_count == 1 { "1 line" } else { "lines" };
            rows.push(vec![(*hint, format!("↳ {out_count} {n} returned"))]);
            if err {
                rows.push(vec![(*error, "↳ command failed".to_string())]);
            }
        }
        BodyPlan::Lines => {
            let frac = if expand_frac >= 0.0 {
                expand_frac.clamp(0.0, 1.0)
            } else {
                if expanded { 1.0 } else { 0.0 }
            };
            let collapsed = cfg.bash_collapsed_lines;
            let cap = collapsed
                + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed)) as f64 * frac)
                    .round() as usize;

            let cmd_line = lines.first().copied().unwrap_or("");
            let output_text = if lines.len() > 1 {
                lines[1..].join("\n")
            } else {
                String::new()
            };

            let ansi_rows = ansi_to_body_rows(&output_text, out);

            if let Some(mut out_rows) = ansi_rows {
                // ANSI-aware rendering: the command line keeps its
                // wrapping and style; output lines carry the ANSI-
                // derived colours. A failed command paints every line
                // in the error accent.
                let out_cap = cap.saturating_sub(1);
                let remaining = out_rows.len().saturating_sub(out_cap);
                let cmd_st = if err { *error } else { *command };
                for piece in wrap_hard_line(cmd_line, width) {
                    rows.push(vec![(cmd_st, piece)]);
                }
                if err {
                    for row in out_rows.iter_mut() {
                        for (st, _) in row.iter_mut() {
                            *st = *error;
                        }
                    }
                }
                rows.extend(out_rows.into_iter().take(out_cap));
                if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                    rows.push(vec![h]);
                }
            } else {
                // Plain-text path (no ANSI escape codes present).
                let remaining = lines.len().saturating_sub(cap);
                for (i, l) in lines.iter().enumerate().take(cap) {
                    let st = if err {
                        *error
                    } else if i == 0 {
                        *command
                    } else {
                        *out
                    };
                    if i == 0 {
                        for piece in wrap_hard_line(l, width) {
                            rows.push(vec![(st, piece)]);
                        }
                    } else {
                        rows.push(vec![(st, l.to_string())]);
                    }
                }
                if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                    rows.push(vec![h]);
                }
            }
        }
    }
    let _ = palette;
    rows
}

/// The body of a tool outside the known set: the compact generic
/// output. Collapsed shows the first lines (the `preview` cap of
/// the reference `generic` rendering); expanded, the body up to the
/// expanded cap. The body is the result text (the tool's `text`
/// field, the reference body precedence of docs/tui.md 13.1), the
/// pi `toolOutput` tone.
fn generic_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    expanded: bool,
    width: usize,
    expand_frac: f64,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    let frac = if expand_frac >= 0.0 {
        expand_frac.clamp(0.0, 1.0)
    } else {
        if expanded { 1.0 } else { 0.0 }
    };
    let collapsed = cfg.preview_lines;
    let cap = collapsed
        + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed)) as f64 * frac)
            .round() as usize;
    let remaining = lines.len().saturating_sub(cap);
    for l in lines.iter().take(cap) {
        rows.push(vec![(*out, l.to_string())]);
    }
    if let Some(h) = fold_hint(remaining, expanded, hint, width) {
        rows.push(vec![h]);
    }
    rows
}

/// The JSON-document body (docs/tui-color-tones.md): the result
/// text is a complete JSON document. The body lines keep the JSON
/// token colors (`highlight::json_line_p`), and the same fold caps
/// as the tool's output mode: the read preview cap for `read`, the
/// generic preview cap otherwise. The fold hint states the
/// remainder.
pub fn json_body_rows(
    tool: &str,
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
    expand_frac: f64,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let hint = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .add_modifier(Modifier::DIM);
    let frac = if expand_frac >= 0.0 {
        expand_frac.clamp(0.0, 1.0)
    } else {
        if expanded { 1.0 } else { 0.0 }
    };
    // The cap: the read preview cap for read results, the generic
    // preview cap otherwise; the expanded state lifts both to the
    // expanded cap. A hidden or summary mode shows no body lines.
    let collapsed_cap = if tool == "read" {
        match cfg.read_mode {
            OutputMode::Preview => cfg.preview_lines,
            _ => 0,
        }
    } else {
        cfg.preview_lines
    };
    let cap = collapsed_cap
        + ((cfg.expanded_preview_max_lines.saturating_sub(collapsed_cap)) as f64 * frac)
            .round() as usize;
    let mut rows: Vec<BodyRow> = Vec::new();
    let remaining = lines.len().saturating_sub(cap);
    for l in lines.iter().take(cap) {
        rows.push(crate::highlight::json_line_p(l, palette));
    }
    if let Some(h) = fold_hint(remaining, expanded, &hint, width) {
        rows.push(vec![h]);
    }
    rows
}

/// Inline word-level diff (delta-inspired, docs/tui-tool-display-fancy.md
/// section 5). Given one removed line and one added line, find the
/// common word-level prefix and suffix so that only the "changed"
/// middle portion is highlighted in bold. This mirrors `delta`'s
/// inline word-diff behaviour.
///
/// Returns `(removed_segments, added_segments)` where each segment is
/// `(style, text)`. Unchanged words use `unchanged_style`; the
/// changed middle uses `changed_style` (e.g. bold diff color).
///
/// Returns `None` when either line is empty (nothing to inline-diff).
#[allow(dead_code)]
fn inline_word_diff(
    old_line: &str,
    new_line: &str,
    old_unchanged: &Style,
    new_unchanged: &Style,
    changed: &Style,
) -> Option<(Vec<Seg>, Vec<Seg>)> {
    // Tokenize into words + whitespace runs so the tokens can be
    // joined back into the original line.
    fn tokenize(s: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() {
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                tokens.push(chars[i..j].iter().collect());
                i = j;
            } else {
                let mut j = i;
                while j < chars.len() && !chars[j].is_whitespace() {
                    j += 1;
                }
                tokens.push(chars[i..j].iter().collect());
                i = j;
            }
        }
        tokens
    }

    let a = tokenize(old_line);
    let b = tokenize(new_line);

    // Common prefix.
    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    // Common suffix (not overlapping the prefix).
    let mut suffix = 0usize;
    while suffix + prefix < a.len() && suffix + prefix < b.len() {
        let ai = a.len() - 1 - suffix;
        let bi = b.len() - 1 - suffix;
        if a[ai] == b[bi] {
            suffix += 1;
        } else {
            break;
        }
    }

    // Rebuild each side: prefix (unchanged) + middle (changed) + suffix (unchanged).
    let mut old_segs: Vec<Seg> = Vec::new();
    let mut new_segs: Vec<Seg> = Vec::new();

    // Prefix tokens.
    for t in &a[..prefix] {
        old_segs.push((old_unchanged.clone(), t.clone()));
        new_segs.push((new_unchanged.clone(), t.clone()));
    }
    // Changed middle (old).
    let old_mid = &a[prefix..a.len() - suffix];
    if !old_mid.is_empty() {
        let mid_text: String = old_mid.iter().cloned().collect();
        old_segs.push((changed.clone(), mid_text));
    }
    // Changed middle (new).
    let new_mid = &b[prefix..b.len() - suffix];
    if !new_mid.is_empty() {
        let mid_text: String = new_mid.iter().cloned().collect();
        new_segs.push((changed.clone(), mid_text));
    }
    // Suffix tokens.
    for t in &a[a.len() - suffix..] {
        old_segs.push((old_unchanged.clone(), t.clone()));
        new_segs.push((new_unchanged.clone(), t.clone()));
    }

    // If nothing changed, just return plain segments.
    if old_mid.is_empty() && new_mid.is_empty() {
        return None;
    }
    Some((old_segs, new_segs))
}

/// Clamp one split-diff column to `pane` columns: the overflow
/// drops with a trailing ellipsis. An empty column stays empty.
fn clamp_col(text: String, pane: usize) -> String {
    if pane == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= pane {
        return text;
    }
    let mut out: String = chars[..pane.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// Clamp to `pane` columns and right-pad with spaces to exactly
/// `pane` width. Unlike `clamp_col` (truncates but leaves short
/// cells short), this guarantees a fixed rendered width so that
/// the split-diff `│` dividers stay in the same column on every
/// row, regardless of content length. The header cell (`"old │ "`)
/// is much shorter than a content cell (`"12 │ - old line"`), so
/// without padding the middle divider shifts left on the header row.
fn pad_cell(text: String, pane: usize) -> String {
    if pane == 0 {
        return String::new();
    }
    let clamped = clamp_col(text, pane);
    let w = clamped.chars().count();
    if w < pane {
        format!("{clamped}{}", " ".repeat(pane - w))
    } else {
        clamped
    }
}

/// Word-wrap one hard line to `width` columns. The wrap breaks on
/// spaces. A word longer than the width splits at the width. A line
/// that already fits returns as one piece. The command line of a
/// bash result wraps on a narrow pane instead of truncating
/// (the user request of the 2026-09-04 pass).
fn wrap_hard_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![line.to_string()];
    }
    if line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur: String = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    for word in line.split(' ') {
        let wlen = word.chars().count();
        if wlen > width {
            // A word wider than the pane: hard-split it, and move
            // to a fresh piece per split.
            if !cur.is_empty() {
                flush(&mut cur, &mut out);
            }
            let chars: Vec<char> = word.chars().collect();
            let mut i = 0usize;
            while i < chars.len() {
                let end = (i + width).min(chars.len());
                out.push(chars[i..end].iter().collect());
                i = end;
            }
            continue;
        }
        let need = if cur.is_empty() { wlen } else { cur.chars().count() + 1 + wlen };
        if need > width {
            flush(&mut cur, &mut out);
        }
        if cur.is_empty() {
            cur.push_str(word);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    flush(&mut cur, &mut out);
    out
}

// ── the box (docs/tui-tool-display-port.md section 2: the box) ──

/// The background of a tool-result panel, per the pi box-state role
/// (docs/tui-color-pi-alignment.md): the `toolBoxBgSuccess` /
/// `toolBoxBgError` palette role, lowered to the active capability
/// level. One cell of left padding; no border lines (the 2026-09-14
/// user pass: the panel is the lighter background only).
pub fn box_bg(palette: &crate::color::Palette, err: bool) -> Color {
    if err {
        palette.color(crate::color::Role::ToolBoxBgError)
    } else {
        palette.color(crate::color::Role::ToolBoxBgSuccess)
    }
}

/// The rows of the tool-result panel: a top margin row, the header
/// row with the tool name and the result status, the body rows, and
/// a bottom margin row. No border lines (the 2026-09-14 user pass):
/// the panel is the lighter background only, so there are no white
/// border runs around it. The two freed border rows are kept as one
/// cell of top and bottom margin, so the content sits inside the
/// lighter band with breathing room. The tool name reads in the
/// `tool_name` role (the purple accent), bold; the status reads in
/// the error accent on a failure, the muted hint on a success.
/// `width` is the panel width in columns.
///
/// Every segment carries the panel background (`err` picks the pi
/// `toolErrorBg` role, otherwise the `toolSuccessBg` role) so the
/// panel reads as one lighter band on the transcript. The margin
/// rows are background-filled too, so the band extends one row above
/// the header and one row below the last body row.
///
/// One panel row is one terminal line: the margin rows are the
/// background pad to the full width; the header row is the name
/// segment, the optional dim label segment, the status segment, and
/// the background pad; a body row is one or more segments plus the
/// background pad.
pub fn box_rows(
    name: &str,
    label: &str,
    status: &str,
    body: &[BodyRow],
    width: usize,
    palette: &crate::color::Palette,
    err: bool,
) -> Vec<BodyRow> {
    let bg = box_bg(palette, err);
    // The tool name in the purple accent, bold (the user pass of
    // 2026-09-14: the name stood in the top border before, now it is
    // the header row of the borderless panel).
    let name_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolName))
        .add_modifier(Modifier::BOLD)
        .bg(bg);
    // The status tone: the pi `error` accent (not a hard-coded red)
    // on a failed result; the muted hint on a success.
    let status_style = if err {
        palette.style(crate::color::Role::Error, Modifier::BOLD).bg(bg)
    } else {
        Style::default()
            .fg(palette.color(crate::color::Role::Hint))
            .add_modifier(Modifier::DIM)
            .bg(bg)
    };
    // One cell of left padding: the panel content starts one column
    // in, the panel spans the full `width`.
    let inner_w = width.saturating_sub(1).max(1);
    let mut out: Vec<BodyRow> = Vec::new();
    // One row of top margin: a background-filled empty row so the
    // content sits inside the lighter band with breathing room above
    // it (the 2026-09-14 user pass: the two freed border rows become
    // one top and one bottom margin).
    out.push(vec![(Style::default().bg(bg), " ".repeat(width))]);
    // The header row: one cell of padding + the tool name + two
    // spaces + the status + the background pad to the panel width.
    // The old `tool:<name>` title prefix is gone: the name stands
    // alone in the purple accent.
    {
        let mut cells: Vec<(Style, String)> = Vec::new();
        let mut used = 0usize;
        let mut name_cell = format!(" {name}");
        let avail = inner_w.saturating_sub(used);
        if name_cell.chars().count() > avail {
            let keep = avail.saturating_sub(1);
            let cut: String = name_cell.chars().take(keep).collect();
            name_cell = format!("{cut}…");
        }
        used = used.saturating_add(name_cell.chars().count());
        cells.push((name_style, name_cell.clone()));
        // The optional dim label (for a read result, the file it
        // read) sits between the name and the status.
        if !label.is_empty() {
            let label_style = Style::default()
                .fg(palette.color(crate::color::Role::Hint))
                .add_modifier(Modifier::DIM)
                .bg(bg);
            let mut label_cell = format!(" {label}");
            let avail = inner_w.saturating_sub(used);
            if label_cell.chars().count() > avail {
                let keep = avail.saturating_sub(1);
                let cut: String = label_cell.chars().take(keep).collect();
                label_cell = format!("{cut}…");
            }
            used = used.saturating_add(label_cell.chars().count());
            cells.push((label_style, label_cell));
        }
        let mut st_cell = format!("  {status}");
        let avail = inner_w.saturating_sub(used);
        if st_cell.chars().count() > avail {
            let keep = avail.saturating_sub(1);
            let cut: String = st_cell.chars().take(keep).collect();
            st_cell = format!("{cut}…");
        }
        used = used.saturating_add(st_cell.chars().count());
        cells.push((status_style, st_cell));
        // The background pad to the panel width: every panel row is
        // exactly `width` columns so the band fills the transcript
        // row edge to edge.
        let pad = width.saturating_sub(used);
        cells.push((Style::default().bg(bg), " ".repeat(pad)));
        out.push(cells);
    }
    // The body rows: one cell of left padding (the leading space of
    // the first segment) and no border cells. A body row may hold
    // several segments (the split diff panes). Each segment keeps its
    // own clamped width: the segments share the row width left to
    // right, and the last segment pads the background to the panel
    // edge. A segment that still overflows is truncated with a
    // trailing ellipsis, never pushed past the panel edge, and never
    // wrapped to the next line (the 2026-09-03 user directive).
    for row in body {
        let mut cells: Vec<(Style, String)> = Vec::new();
        let mut used = 0usize;
        let n = row.len();
        for (idx, (st, text)) in row.iter().enumerate() {
            // Preserve an explicit per-segment bg (the diff-line shade)
            // over the panel bg; segments without a bg get the
            // panel background.
            let st = if st.bg.is_some() { *st } else { (*st).bg(bg) };
            let is_last = idx + 1 == n;
            // One leading space plus the segment text, clamped to
            // the columns still free on the row.
            let avail = width.saturating_sub(used);
            let mut cell = format!(" {text}");
            let want = cell.chars().count();
            if want > avail {
                if avail == 0 {
                    cell = String::new();
                } else if avail == 1 {
                    cell = "…".to_string();
                } else {
                    // Keep `avail - 1` columns and mark the cut
                    // with the ellipsis (one column).
                    let cut: String = cell.chars().take(avail - 1).collect();
                    cell = format!("{cut}…");
                }
            }
            used = used.saturating_add(cell.chars().count());
            if is_last {
                // The last segment owns the background padding to the
                // panel edge: the panel rows stay one terminal line.
                cell.push_str(&" ".repeat(width.saturating_sub(used)));
            }
            cells.push((st, cell));
        }
        // An empty body row still fills the panel: pad the full row
        // with the panel background.
        if row.is_empty() {
            cells.push((Style::default().bg(bg), " ".repeat(width)));
        }
        out.push(cells);
    }
    // One row of bottom margin: a background-filled empty row so the
    // content sits inside the lighter band with breathing room below
    // it (matching the top margin).
    out.push(vec![(Style::default().bg(bg), " ".repeat(width))]);
    out
}

// ── the borderless panel (2026-09-14 user pass) ──────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Level, Palette};

    fn pal() -> Palette {
        Palette::builtin(Level::Rgb)
    }

    #[test]
    fn panel_has_no_border_lines() {
        // The 2026-09-14 user pass: the box's white border lines
        // (`┌─┐`, `│`, `└─┘`) are gone. No box-drawing char may
        // appear anywhere in the panel rows.
        let body: Vec<BodyRow> = vec![
            vec![(Style::default(), "line1".to_string())],
            vec![
                (Style::default().bg(Color::Red), "left".to_string()),
                (Style::default(), "right".to_string()),
            ],
        ];
        let rows = box_rows("bash", "", "exit 0", &body, 40, &pal(), false);
        assert_eq!(
            rows.len(),
            2 + 1 + body.len(),
            "top margin + header + one row per body row + bottom margin"
        );
        for row in &rows {
            for (_, text) in row {
                for ch in text.chars() {
                    assert!(
                        !matches!(ch, '┌' | '┐' | '└' | '┘' | '│' | '─'),
                        "a box border char {ch:?} survived in the panel"
                    );
                }
            }
        }
    }

    #[test]
    fn header_names_the_tool_in_purple_bold() {
        let rows = box_rows("bash", "", "exit 0", &[], 40, &pal(), false);
        // rows: [top margin, header, bottom margin]; the header is row 1.
        let name_cell = &rows[1][0];
        // The name stands alone: no `tool:` prefix, one cell of
        // left padding.
        assert_eq!(name_cell.1, " bash");
        assert!(!name_cell.1.contains("tool:"));
        // Purple, through the `tool_name` role, and bold.
        assert_eq!(
            name_cell.0.fg,
            Some(Color::Rgb(0xc6, 0xa0, 0xf6)),
            "the tool name must carry the `tool_name` purple role"
        );
        assert!(
            name_cell.0.add_modifier.contains(Modifier::BOLD),
            "the tool name must be bold"
        );
        assert_eq!(
            name_cell.0.bg,
            Some(pal().color(crate::color::Role::ToolBoxBgSuccess)),
            "the name sits on the panel background"
        );
    }

    #[test]
    fn header_status_is_muted_on_success_and_error_bold_on_failure() {
        let ok = box_rows("bash", "", "exit 0", &[], 40, &pal(), false);
        // rows: [top margin, header, bottom margin]; the header is row 1.
        let st = &ok[1][1];
        assert_eq!(st.1, "  exit 0");
        assert_eq!(
            st.0.fg,
            Some(pal().color(crate::color::Role::Hint)),
            "a success status keeps the muted hint tone"
        );
        assert!(st.0.add_modifier.contains(Modifier::DIM));
        assert!(
            !st.0.add_modifier.contains(Modifier::BOLD),
            "a success status is not bold"
        );

        let err = box_rows("bash", "", "error", &[], 40, &pal(), true);
        let st = &err[1][1];
        assert_eq!(st.0.fg, Some(pal().color(crate::color::Role::Error)));
        assert!(
            st.0.add_modifier.contains(Modifier::BOLD),
            "a failed result keeps the bold error accent"
        );
        assert_eq!(
            st.0.bg,
            Some(pal().color(crate::color::Role::ToolBoxBgError)),
            "the status sits on the error panel background"
        );
    }

    #[test]
    fn panel_rows_span_the_full_width_and_keep_their_bg() {
        let body: Vec<BodyRow> = vec![
            vec![(Style::default(), "line".to_string())],
            vec![],
        ];
        let rows = box_rows("bash", "", "exit 0", &body, 40, &pal(), false);
        let bg = pal().color(crate::color::Role::ToolBoxBgSuccess);
        for row in &rows {
            let w: usize = row.iter().map(|(_, t)| t.chars().count()).sum();
            assert_eq!(w, 40, "every panel row is exactly `width` columns");
            for (st, _) in row {
                assert_eq!(
                    st.bg,
                    Some(bg),
                    "every panel cell carries the panel background"
                );
            }
        }
    }

    #[test]
    fn panel_has_a_top_and_bottom_margin_row() {
        // The 2026-09-14 user pass: one background-filled margin row
        // on the top and bottom of the panel, so the content sits
        // inside the lighter band with breathing room.
        let body: Vec<BodyRow> = vec![vec![(Style::default(), "line".to_string())]];
        let rows = box_rows("bash", "", "exit 0", &body, 40, &pal(), false);
        let bg = pal().color(crate::color::Role::ToolBoxBgSuccess);
        // top margin + header + 1 body + bottom margin = 4 rows.
        assert_eq!(rows.len(), 4, "top margin, header, body, bottom margin");
        // The top and bottom rows are background-filled and empty
        // (no visible content).
        for margin in [&rows[0], &rows[rows.len() - 1]] {
            let text: String = margin.iter().map(|(_, t)| t.as_str()).collect();
            assert!(
                text.chars().all(|c| c == ' '),
                "the margin row is empty: {text:?}"
            );
            for (st, _) in margin {
                assert_eq!(st.bg, Some(bg), "the margin row fills the panel bg");
            }
        }
        // The header is now the second row.
        let name_cell = &rows[1][0];
        assert_eq!(name_cell.1, " bash");
    }
}
