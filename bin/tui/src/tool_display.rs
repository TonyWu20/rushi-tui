//! Tool result display: the port of `pi-tool-display`
//! (docs/tui-tool-display-port.md) plus the content layer of
//! docs/tui-tool-result-truncation.md.
//!
//! The request in one line: wrap every tool result in a lighter
//! box, fold long output to a preview, and one global key expands
//! every collapsed block. The content layer owns what shows and how
//! many lines: `Read` and `Write` results truncate to a preview,
//! `Edit` results render as a diff, and `bash` output folds to a
//! collapsed line count.
//!
//! Reference: `pi-tool-display` (github.com/MasuRii/pi-tool-display,
//! v0.5.0, pinned rev `91cef758`), the config shape and the presets
//! ported here:
//!
//! - Per-tool limits: `previewLines` 8 for read,
//!   `bashCollapsedLines` 10, `diffCollapsedLines` 24.
//! - Output modes: `hidden` / `summary` / `preview` for read and
//!   bash, `hidden` / `count` / `preview` for search.
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
/// search stays hidden, bash collapses to the first 10 lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// The default preset: read shows a syntax-highlighted
    /// preview, search hidden, bash collapsed to the first 10
    /// lines.
    OpenCode,
    /// Compact summaries: read line count, search match total, bash
    /// line count.
    Balanced,
    /// Larger previews: read and search show 12 preview lines, bash
    /// 20.
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

/// The search output mode of the port. `Count` shows the match
/// total instead of a line summary; `Hidden` and `Preview` behave as
/// in [`OutputMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Hidden,
    Count,
    Preview,
}

pub fn parse_search_mode(s: &str) -> Option<SearchMode> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "hidden" => SearchMode::Hidden,
        "count" => SearchMode::Count,
        "preview" => SearchMode::Preview,
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
    pub search_mode: SearchMode,
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
                search_mode: SearchMode::Hidden,
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
                search_mode: SearchMode::Count,
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
                search_mode: SearchMode::Preview,
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
        "list" => search_body(value, cfg, &out, &hint, expanded, width, expand_frac),
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

/// The body plan of one search mode at the fold state: the search
/// `count` mode maps to the summary line, `hidden` to no body,
/// `preview` to the lines. The global expand overrides, like
/// [`body_plan`].
fn search_body_plan(mode: SearchMode, expanded: bool) -> BodyPlan {
    if expanded {
        return BodyPlan::Lines;
    }
    match mode {
        SearchMode::Hidden => BodyPlan::NoBody,
        SearchMode::Count => BodyPlan::SummaryLine,
        SearchMode::Preview => BodyPlan::Lines,
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

/// The body of a `list` (search) result. The value is
/// `{text, path, type: "directory", count}`. The modes: `hidden`
/// shows no body, `count` the entry total, `preview` the first
/// lines of the listing.
fn search_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    expanded: bool,
    width: usize,
    expand_frac: f64,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let count = value
        .get("count")
        .and_then(|c| c.as_u64())
        .map(|n| n as usize)
        .unwrap_or_else(|| text.lines().count());
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    let eff_expanded = if expand_frac >= 0.0 { expand_frac > 0.0 } else { expanded };
    match search_body_plan(cfg.search_mode, eff_expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, format!("↳ {count} entries hidden"))]);
        }
        BodyPlan::SummaryLine => {
            let n = if count == 1 { "1 entry" } else { "entries" };
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
            for l in lines.iter().take(cap) {
                // The listing lines are plain text, the pi
                // `toolOutput` tone, not code.
                rows.push(vec![(*out, l.to_string())]);
            }
            if let Some(h) = fold_hint(remaining, eff_expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
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

/// The background of a tool-result box, per the pi box-state role
/// (docs/tui-color-pi-alignment.md): the `toolBoxBgSuccess` /
/// `toolBoxBgError` palette role, lowered to the active capability
/// level. One cell of padding, the rounded corners.
pub fn box_bg(palette: &crate::color::Palette, err: bool) -> Color {
    if err {
        palette.color(crate::color::Role::ToolBoxBgError)
    } else {
        palette.color(crate::color::Role::ToolBoxBgSuccess)
    }
}

/// The rows of the rounded tool-result box: the top border with the
/// title text, the body rows (one cell of left padding, the border
/// cells of the row), and the bottom border. Every segment carries
/// the box-state background (`err` picks the pi `toolErrorBg` role,
/// otherwise the `toolSuccessBg` role) so the box reads as one
/// lighter panel on the transcript. `width` is the box width in
/// columns.
///
/// `title_style` restyles the title run of the top border (the
/// result status accent, like the red bold of an error status);
/// `None` keeps the border style.
///
/// One box row is one terminal line: the top border row is three
/// segments (the left border cell, the title, the close run); a
/// body row is one or three segments (the left border cell, the
/// body segments, the right border cell), or a pad when the body
/// row is empty.
pub fn box_rows(
    title: &str,
    body: &[BodyRow],
    width: usize,
    palette: &crate::color::Palette,
    title_style: Option<&Style>,
    err: bool,
) -> Vec<BodyRow> {
    let bg = box_bg(palette, err);
    let border = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .bg(bg);
    let inner_w = width.saturating_sub(2).max(1);
    // The title row: `┌` + one-space padding + the title + the
    // `─` run to the close. The title clamps to the inner width
    // and keeps its own style (the error accent) when given.
    let title_chars: Vec<char> = title.chars().take(inner_w.saturating_sub(1)).collect();
    let title_text: String = title_chars.iter().collect();
    let pad_run = inner_w.saturating_sub(title_text.chars().count() + 1);
    let title_style = title_style
        .cloned()
        .map(|s| s.bg(bg))
        .unwrap_or(border);
    let top: Vec<(Style, String)> = vec![
        (border, "┌ ".to_string()),
        (title_style, title_text.clone()),
        (
            border,
            std::iter::repeat_n('─', pad_run)
                .chain(std::iter::once('┐'))
                .collect(),
        ),
    ];
    let mut out: Vec<BodyRow> = Vec::new();
    out.push(top);
    // The body rows: `│` + one-space padding + the row, padded to
    // the inner width. A body row may hold several segments (the
    // split diff panes). Each segment keeps its own clamped width:
    // the segments share the inner width left to right, and the
    // last segment pads to the right border. A segment that still
    // overflows is truncated with a trailing ellipsis, never pushed
    // past the border, and never wrapped to the next line (the
    // 2026-09-03 user directive).
    for row in body {
        let mut cells: Vec<(Style, String)> = Vec::new();
        cells.push((border, "│".to_string()));
        let mut used = 0usize;
        let n = row.len();
        for (idx, (st, text)) in row.iter().enumerate() {
            // Preserve an explicit per-segment bg (the diff-line shade)
            // over the box panel bg; segments without a bg get the
            // box background as before.
            let st = if st.bg.is_some() { *st } else { (*st).bg(bg) };
            let is_last = idx + 1 == n;
            // One leading space plus the segment text, clamped to
            // the inner columns still free.
            let avail = inner_w.saturating_sub(used);
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
                // The last segment owns the padding to the right
                // border: the box rows stay one terminal line.
                cell.push_str(&" ".repeat(inner_w.saturating_sub(used)));
            }
            cells.push((st, cell));
        }
        // An empty body row still fills the panel: pad to the
        // inner width so the borders stay one cell apart.
        if row.is_empty() {
            cells.push((Style::default().bg(bg), " ".repeat(inner_w)));
        }
        cells.push((border, "│".to_string()));
        out.push(cells);
    }
    let bottom: String = std::iter::once('└')
        .chain(std::iter::repeat_n('─', inner_w))
        .chain(std::iter::once('┘'))
        .collect();
    out.push(vec![(border, bottom)]);
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The joined text of one body row: the segments in order.
    fn row_text(row: &BodyRow) -> String {
        row.iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    /// The joined text of every body row.
    fn rows_text(rows: &[BodyRow]) -> Vec<String> {
        rows.iter().map(row_text).collect()
    }

    #[test]
    fn preset_value_tables() {
        let o = ToolDisplay::preset(Preset::OpenCode);
        assert_eq!(o.read_mode, OutputMode::Preview);
        assert_eq!(o.search_mode, SearchMode::Hidden);
        assert_eq!(o.bash_mode, OutputMode::Preview);
        assert_eq!(o.preview_lines, 8);
        assert_eq!(o.bash_collapsed_lines, 10);
        assert_eq!(o.diff_collapsed_lines, 24);
        assert_eq!(o.expanded_preview_max_lines, 4000);

        let b = ToolDisplay::preset(Preset::Balanced);
        assert_eq!(b.read_mode, OutputMode::Summary);
        assert_eq!(b.search_mode, SearchMode::Count);
        assert_eq!(b.bash_mode, OutputMode::Summary);

        let v = ToolDisplay::preset(Preset::Verbose);
        assert_eq!(v.read_mode, OutputMode::Preview);
        assert_eq!(v.search_mode, SearchMode::Preview);
        assert_eq!(v.bash_mode, OutputMode::Preview);
        assert_eq!(v.preview_lines, 12);
        assert_eq!(v.bash_collapsed_lines, 20);
    }

    #[test]
    fn preset_and_mode_parsing() {
        assert_eq!(parse_preset("opencode"), Some(Preset::OpenCode));
        assert_eq!(parse_preset("BALANCED"), Some(Preset::Balanced));
        assert_eq!(parse_preset("verbose"), Some(Preset::Verbose));
        assert_eq!(parse_preset("weird"), None);
        assert_eq!(parse_output_mode("hidden"), Some(OutputMode::Hidden));
        assert_eq!(parse_output_mode("summary"), Some(OutputMode::Summary));
        assert_eq!(parse_output_mode("preview"), Some(OutputMode::Preview));
        assert_eq!(parse_output_mode("nope"), None);
        assert_eq!(parse_search_mode("count"), Some(SearchMode::Count));
        assert_eq!(parse_diff_view("auto"), Some(DiffView::Auto));
        assert_eq!(parse_diff_view("split"), Some(DiffView::Split));
        assert_eq!(parse_diff_view("unified"), Some(DiffView::Unified));
    }

    #[test]
    fn fold_hint_format_and_clamp() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let hint = Style::default().fg(p.color(crate::color::Role::Hint));
        let h = fold_hint(173, false, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (173 more lines \u{2022} Ctrl+O to expand)");
        // The singular unit.
        let h = fold_hint(1, false, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (1 more line \u{2022} Ctrl+O to expand)");
        // The expanded state drops the key hint.
        let h = fold_hint(173, true, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (173 more lines)");
        // A narrow pane shortens the hint: the key drops first.
        let h = fold_hint(173, false, &hint, 30).expect("a hint");
        assert!(!h.1.contains("Ctrl+O"), "{h:?}");
        assert!(h.1.chars().count() <= 30, "{h:?}");
        // Zero remaining: no hint.
        assert!(fold_hint(0, false, &hint, 80).is_none());
    }

    #[test]
    fn read_summary_shows_the_line_count() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let mut cfg = ToolDisplay::preset(Preset::Balanced);
        cfg.read_mode = OutputMode::Summary;
        let value = serde_json::json!({"text": "a\nb\nc", "total_lines": 342});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(texts.iter().any(|t| t.contains("342 lines")), "{texts:?}");
    }

    #[test]
    fn read_preview_folds_to_the_preview_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let text = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let value = serde_json::json!({"text": text, "total_lines": 50});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // The opencode preset now previews read content (8 lines)
        // with syntax highlighting; the fold hint states the rest.
        let texts = rows_text(&rows);
        let shown = texts
            .iter()
            .filter(|l| l.starts_with("line "))
            .count();
        assert_eq!(shown, 8, "the preview shows 8 lines: {texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("42 more lines")),
            "the fold hint states the remainder: {texts:?}"
        );
        // The preview mode of the balanced preset shows 8 lines.
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert_eq!(
            shown.iter().filter(|l| l.starts_with("line ")).count(),
            8,
            "the preview shows 8 lines: {shown:?}"
        );
        assert!(
            shown.iter().any(|l| l.contains("42 more lines")),
            "the fold hint states the remainder: {shown:?}"
        );
        // The expanded state shows up to the cap, no hint.
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert_eq!(
            shown.iter().filter(|l| l.starts_with("line ")).count(),
            50,
            "the expanded state shows the body: {shown:?}"
        );
        assert!(!shown.iter().any(|l| l.contains("more lines")));
    }

    #[test]
    fn read_preview_highlights_known_language() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            // The palette-role token colors are the built-in engine's
            // output; pin the engine (the default is `TreeSitter` since
            // the 2026-09-11 request).
            highlight_engine: HighlightEngine::Builtin,
            ..cfg
        };
        let text = "1: let x = 42; // note\n2: ";
        let value = serde_json::json!({
            "text": text,
            "path": "/tmp/sample.rs",
            "total_lines": 2,
        });
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("let x = 42")),
            "the first body row shows the rust line: {shown:?}"
        );
        // The keyword "let" gets the palette syntax-keyword color.
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_kw = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "let" && *st == kw)
        });
        assert!(found_kw, "the 'let' keyword is colored: {rows:?}");
        // The number "42" gets the palette syntax-number color.
        let num = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        let found_num = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "42" && *st == num)
        });
        assert!(found_num, "the literal 42 is colored: {rows:?}");
        // The comment "// note" gets the palette syntax-comment color.
        let cmt = p.style(crate::color::Role::SyntaxComment, Modifier::DIM);
        let found_cmt = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t.contains("note") && *st == cmt)
        });
        assert!(found_cmt, "the comment is colored: {rows:?}");
    }

    #[test]
    fn read_preview_highlights_from_call_args() {
        // The log stores the read path in the call arguments, not the
        // result value (the value carries only `text`). The language
        // must still resolve from `call_args.file_path`.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            // The palette-role token colors are the built-in engine's
            // output; pin the engine (the default is `TreeSitter` since
            // the 2026-09-11 request).
            highlight_engine: HighlightEngine::Builtin,
            ..cfg
        };
        let value = serde_json::json!({"text": "1: let x = 42; // note\n2: "});
        let args =
            serde_json::json!({"file_path": "/tmp/sample.rs"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("let x = 42")),
            "the first body row shows the rust line: {shown:?}"
        );
        // The keyword "let" gets the palette syntax-keyword color even
        // though the value carries no `path` (the call args supply it).
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_kw = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "let" && *st == kw)
        });
        assert!(found_kw, "the 'let' keyword is colored from call args: {rows:?}");
        let cmt = p.style(crate::color::Role::SyntaxComment, Modifier::DIM);
        let found_cmt = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t.contains("note") && *st == cmt)
        });
        assert!(found_cmt, "the comment is colored from call args: {rows:?}");
    }

    #[test]
    fn read_value_path_wins_over_call_args() {
        // When the result value carries a `path`, it wins over the
        // call argument: the value is the record for the read.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            // The palette-role token colors are the built-in engine's
            // output; pin the engine (the default is `TreeSitter` since
            // the 2026-09-11 request).
            highlight_engine: HighlightEngine::Builtin,
            ..cfg
        };
        let value = serde_json::json!({
            "text": "1: def f():\n2:     pass",
            "path": "/tmp/a.py",
        });
        let args = serde_json::json!({"file_path": "/tmp/b.rs"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // The python keyword "def" colors through the python family,
        // not the rust keyword set: proves the value path won.
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_def = rows.iter().any(|row| {
            row.iter().any(|(st, t)| t == "def" && *st == kw)
        });
        assert!(
            found_def,
            "python 'def' is a keyword: {rows:?}"
        );
    }

    /// The 2026-09-11 request: every preset defaults to the
    /// `TreeSitter` engine, so the shipped read/write displays are
    /// tree-sitter-powered out of the box.
    #[test]
    fn presets_default_to_the_treesitter_engine() {
        for preset in [Preset::OpenCode, Preset::Balanced, Preset::Verbose] {
            assert_eq!(
                ToolDisplay::preset(preset).highlight_engine,
                HighlightEngine::TreeSitter,
                "{preset:?} defaults to tree-sitter"
            );
        }
    }

    /// Default (tree-sitter) read body: the tokens carry the
    /// macchiato theme foregrounds — not the palette role colors —
    /// and no highlighted token carries a background (the 2026-09-11
    /// fg-only rule).
    #[test]
    fn read_preview_default_treesitter_highlights_fg_only() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..ToolDisplay::preset(Preset::Balanced)
        };
        let text = "1: let x = 42; // note\n2: ";
        let value = serde_json::json!({
            "text": text,
            "path": "/tmp/sample.rs",
            "total_lines": 2,
        });
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // The keyword "let" carries the tree-sitter theme color (not the
        // palette `SyntaxKeyword` role color).
        let role_kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let let_seg = rows
            .iter()
            .flat_map(|r| r.iter())
            .find(|(_, t)| t == "let")
            .expect("the `let` token is present");
        assert!(
            let_seg.0.fg.is_some() && let_seg.0 != role_kw,
            "the keyword is a theme color, fg-only: {let_seg:?}"
        );
        // No segment of the body carries a background.
        for row in &rows {
            for (st, _) in row.iter() {
                assert!(st.bg.is_none(), "no bg on highlighted text: {st:?}");
            }
        }
    }

    /// Default (tree-sitter) write body: the written content is
    /// tokenized by the engine (the keyword is a theme color), and
    /// the whole-line `DiffAddedBg` shade is kept — the shade marks
    /// the diff line, it is not a token background.
    #[test]
    fn write_body_default_treesitter_highlights_the_content() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let value = serde_json::json!({
            "text": "Successfully wrote 14 bytes to /tmp/a.py.",
            "path": "/tmp/a.py",
            "operation": "create",
            "bytes": 14
        });
        let args = serde_json::json!({"file_path": "/tmp/a.py", "content": "def f():\n    return 1\n"});
        let rows = body_rows()
            .tool("write")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let code = p.style(crate::color::Role::Code, Modifier::empty());
        let kw_seg = rows
            .iter()
            .flat_map(|r| r.iter())
            .find(|(_, t)| t == "def")
            .expect("the `def` token is present");
        assert!(
            kw_seg.0.fg.is_some() && kw_seg.0 != code,
            "the keyword is a theme color: {kw_seg:?}"
        );
        // The added-line shade is still on the content runs.
        let shade = p.color(crate::color::Role::DiffAddedBg);
        assert!(
            rows.iter().any(|r| r.iter().any(|(st, t)| t.contains("def") && st.bg == Some(shade))),
            "the added-line shade spans the content: {rows:?}"
        );
    }

    /// The write body runs one stateful highlighter over the whole
    /// written file: a block comment opened on line 1 stays open on
    /// line 2, so line 2 (whole line, the comment never closes)
    /// keeps the comment color instead of re-highlighting as code.
    #[test]
    fn write_body_block_comment_state_spans_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let content = "let a = 1; /* open\nlet b = 2; // still inside";
        let value = serde_json::json!({
            "text": "wrote", "path": "/tmp/f.rs", "operation": "create", "bytes": 44
        });
        let args = serde_json::json!({"file_path": "/tmp/f.rs", "content": content});
        let rows = body_rows()
            .tool("write")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // Line 1's `let` is a keyword: a theme foreground, not the
        // palette role color (that would mean the builtin engine ran).
        let role_kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let kw = rows
            .iter()
            .flat_map(|r| r.iter())
            .find(|(_, t)| t == "let")
            .expect("line 1's `let` token");
        assert!(
            kw.0.fg.is_some() && kw.0 != role_kw,
            "line-1 `let` is a theme-color keyword: {kw:?}"
        );
        // The run containing `open` is inside the block comment on
        // line 1. The run containing `b = 2` on line 2 carries the
        // same color: the comment stayed open across lines. A
        // per-line highlighter would color line 2 as code instead.
        let open = rows
            .iter()
            .flat_map(|r| r.iter())
            .find(|(_, t)| t.contains("open"))
            .expect("line 1's comment run");
        let rest = rows
            .iter()
            .flat_map(|r| r.iter())
            .find(|(_, t)| t.contains("b = 2"))
            .expect("line 2's run");
        assert_eq!(
            open.0, rest.0,
            "line 2 stays inside the block comment: open={open:?} rest={rest:?}"
        );
        assert!(
            open.0 != kw.0,
            "comment text is not the keyword color: open={open:?} kw={kw:?}"
        );
    }

    #[test]
    fn read_preview_no_path_stays_plain() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let value = serde_json::json!({"text": "1: hello\n2: world"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("1: hello")),
            "line 1 is shown: {shown:?}"
        );
        // Without a path there is no language, so every run stays in
        // the code tone (no syntax colors applied).
        let code = p.style(crate::color::Role::Code, Modifier::empty());
        for row in &rows {
            for (st, _) in row.iter() {
                assert!(
                    *st == code || *st == hint_style(&p),
                    "unexpected style in plain read body: {st:?}"
                );
            }
        }
    }

    /// The muted hint style, used by the plain-read assertion.
    fn hint_style(p: &crate::color::Palette) -> Style {
        Style::default()
            .fg(p.color(crate::color::Role::Hint))
            .add_modifier(Modifier::DIM)
    }

    #[test]
    fn edit_diff_shows_removed_and_added() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let value = serde_json::json!({
            "text": "The file f has been updated successfully.",
            "path": "f",
            "before": "old one\nold two",
            "after": "new one\nnew two\nnew three",
            "replace_all": false
        });
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        // The stats line, like the reference diff presentation.
        assert!(
            texts.iter().any(|t| t.contains("+3 -2")),
            "the stats line shows the added and removed counts: {texts:?}"
        );
        // Split layout: removed lines appear in the left pane and added
        // lines in the right pane. The pi-tool-display style carries the
        // +/- status in the `▌` gutter marker + background shade, not in
        // a `-`/`+` text prefix, so the raw content is shown bare.
        assert!(texts.iter().any(|t| t.contains("old one")), "left pane shows removed lines: {texts:?}");
        assert!(texts.iter().any(|t| t.contains("new three")), "right pane shows added lines: {texts:?}");
        // Changed lines carry the `▌` gutter marker.
        assert!(texts.iter().any(|t| t.contains('▌')), "changed lines carry the ▌ marker: {texts:?}");
    }

    /// The pi-tool-display coloring model (docs/tui-tool-display-fancy.md
    /// section 5.2): changed lines are signalled by a whole-line background
    /// shade plus a lighter `▌` gutter marker, while the content text keeps
    /// a neutral tone instead of a solid red/green foreground.
    #[test]
    fn diff_changed_lines_use_a_shade_and_marker_not_solid_fg() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let mut split = ToolDisplay::preset(Preset::OpenCode);
        split.diff_view = DiffView::Split;
        let value = serde_json::json!({
            "path": "f",
            "before": "alpha\nbeta",
            "after":  "alpha\nbeta-mod",
        });
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&split)
            .palette(&p)
            .expanded(true)
            .width(120)
            .call();

        use crate::color::Role;
        let removed_bg = p.color(Role::DiffRemovedBg);
        let added_bg = p.color(Role::DiffAddedBg);
        let out_fg = p.color(Role::ToolOutput);
        let mut saw_removed_shade = false;
        let mut saw_added_shade = false;
        let mut marker_on_changed_line = false;
        for row in &rows {
            let text: String = row.iter().map(|(_, t)| t.as_str()).collect();
            for (st, _t) in row.iter() {
                if st.bg == Some(removed_bg) {
                    saw_removed_shade = true;
                }
                if st.bg == Some(added_bg) {
                    saw_added_shade = true;
                }
            }
            // A changed pane carries the `▌` marker and a shade; its content
            // segment uses the neutral `ToolOutput` tone, not the diff accent
            // foreground.
            if text.contains('▌') {
                marker_on_changed_line = true;
                for (st, t) in row.iter() {
                    if st.bg == Some(removed_bg) || st.bg == Some(added_bg) {
                        // The content segment (not the gutter marker) must be
                        // neutral-colored: its foreground is ToolOutput.
                        if t.chars().any(|c| c.is_alphanumeric())
                            && st.fg == Some(out_fg)
                        {
                            marker_on_changed_line &= true;
                        }
                    }
                }
            }
        }
        assert!(saw_removed_shade, "a removed pane carries the DiffRemovedBg shade");
        assert!(saw_added_shade, "an added pane carries the DiffAddedBg shade");
        assert!(marker_on_changed_line, "changed lines carry the ▌ marker on a shaded pane");
    }

    /// The diff stats row shows a proportional green/red progress bar
    /// (docs/tui-tool-display-fancy.md section 5.2). Green segments are
    /// proportional to added lines, red to removed lines.
    #[test]
    fn diff_stats_bar_is_proportional_to_added_and_removed() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        // before has 2 lines, after has 4 lines:
        //   i=0: differ -> removed+1, added+1
        //   i=1: differ -> removed+1, added+1
        //   i=2: only after -> added+1
        //   i=3: only after -> added+1
        // => added=4, removed=2, total=6
        let value = serde_json::json!({
            "path": "f",
            "before": "old one\nold two",
            "after": "new one\nnew two\nnew three\nnew four"
        });
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call();

        use crate::color::Role;
        let green = p.color(Role::DiffAdded);
        let red = p.color(Role::DiffRemoved);

        // The stats row is the first body row.
        let stats = &rows[0];
        let joined: String = stats.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            joined.contains("+4 -2"),
            "stats row shows added/removed counts: {joined:?}"
        );
        // Bar segments: green █ × (4*12/6=8) and red █ × (12-8=4).
        let mut green_count = 0usize;
        let mut red_count = 0usize;
        for (st, t) in stats.iter() {
            if st.fg == Some(green) {
                green_count += t.chars().filter(|c| *c == '█').count();
            }
            if st.fg == Some(red) {
                red_count += t.chars().filter(|c| *c == '█').count();
            }
        }
        assert_eq!(
            green_count, 8,
            "green bar should be 8 chars for 4 added of 6 total: {stats:?}"
        );
        assert_eq!(
            red_count, 4,
            "red bar should be 4 chars for 2 removed of 6 total: {stats:?}"
        );
    }

    #[test]
    fn write_diff_shows_the_content_as_added_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let value = serde_json::json!({
            "text": "Successfully wrote 5 bytes to f.",
            "path": "f",
            "operation": "create",
            "bytes": 5
        });
        let args = serde_json::json!({"file_path": "f", "content": "abc\ndef\n"});
        let rows = body_rows()
            .tool("write")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(
            texts.iter().any(|t| t.contains("2 lines, 5 bytes")),
            "the write summary names the size: {texts:?}"
        );
        // pi-tool-display style: new lines carry a `▌` marker and a
        // line-number gutter, not a `+` text prefix.
        assert!(
            texts.iter().any(|t| t.contains("│ abc")),
            "the written content line is shown with a gutter marker: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("│ def")),
            "the second written line is shown: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains('▌')),
            "added lines carry the ▌ marker: {texts:?}"
        );
    }

    #[test]
    fn bash_collapsed_preview() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let text = format!(
            "$ ls\n{}",
            (0..30)
                .map(|i| format!("file{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let value = serde_json::json!({
            "text": text,
            "exit_code": 0,
            "stdout": text,
            "stderr": "",
            "timed_out": false,
            "truncated": false
        });
        let rows = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        // The first line is the command line; the preview holds the
        // next 9 of the 10 collapsed lines.
        assert_eq!(
            texts.iter().filter(|l| l.starts_with("file")).count(),
            9,
            "the collapsed bash preview shows the first 10 lines (the command line plus 9 output lines): {texts:?}"
        );
        assert!(
            texts.iter().any(|l| l.contains("21 more lines")),
            "{texts:?}"
        );
    }

    #[test]
    fn search_count_mode() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let mut cfg = ToolDisplay::preset(Preset::Balanced);
        cfg.search_mode = SearchMode::Count;
        let value =
            serde_json::json!({"text": "a\nb\nc\nd", "path": "d", "type": "directory", "count": 4});
        let rows = body_rows()
            .tool("list")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(
            texts.iter().any(|t| t.contains("4 entries")),
            "the count mode names the total: {texts:?}"
        );
    }

    #[test]
    fn expand_overrides_the_hidden_and_summary_modes() {
        // The port spec of the module header: one global key (the
        // Ctrl+O toggle) expands every collapsed block to the full
        // output. A hidden or summary mode shows its body lines
        // when the global expand is on.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        // read previews by default in opencode: collapsed shows the
        // body lines (up to the cap), expanded shows the full body.
        let value = serde_json::json!({"text": "a\nb\nc\nd", "total_lines": 4});
        let collapsed = rows_text(&body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call());
        let shown = collapsed
            .iter()
            .filter(|l| l.starts_with('a') || l.starts_with('b')
                || l.starts_with('c') || l.starts_with('d'))
            .count();
        assert_eq!(shown, 4, "all four lines fit in the preview: {collapsed:?}");
        let expanded = rows_text(&body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert_eq!(
            expanded
                .iter()
                .filter(|l| l.starts_with("a") || l.starts_with("b"))
                .count(),
            2,
            "expanded hidden mode shows the body lines: {expanded:?}"
        );
        // bash summary by default in balanced: expanded shows lines.
        let mut bcfg = ToolDisplay::preset(Preset::Balanced);
        bcfg.bash_mode = OutputMode::Summary;
        let bvalue =
            serde_json::json!({"text": "$ ls\nf1\nf2", "exit_code": 0, "stdout": "f1\nf2\n"});
        let collapsed = rows_text(&body_rows()
            .tool("bash")
            .value(&bvalue)
            .err(false)
            .cfg(&bcfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call());
        assert!(
            collapsed.iter().any(|l| l.contains("2 lines returned")),
            "collapsed summary mode keeps the count: {collapsed:?}"
        );
        let expanded = rows_text(&body_rows()
            .tool("bash")
            .value(&bvalue)
            .err(false)
            .cfg(&bcfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert!(
            expanded.iter().any(|l| l == "f1"),
            "expanded summary mode shows the body lines: {expanded:?}"
        );
        // search count by default in balanced: expanded shows lines.
        let mut scfg = ToolDisplay::preset(Preset::Balanced);
        scfg.search_mode = SearchMode::Count;
        let svalue =
            serde_json::json!({"text": "f1\nf2", "path": "d", "type": "directory", "count": 2});
        let expanded = rows_text(&body_rows()
            .tool("list")
            .value(&svalue)
            .err(false)
            .cfg(&scfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert!(
            expanded.iter().any(|l| l == "f1"),
            "expanded count mode shows the listing: {expanded:?}"
        );
    }

    #[test]
    fn box_rows_never_exceed_the_width() {
        // The narrow-pane width clamp of the port: no box row may
        // wrap past the reserved terminal width. The three-segment
        // split diff row is the worst case: two panes, the divider,
        // and the borders must fit the box width exactly.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let value = serde_json::json!({"before": "a\nb\nc", "after": "a\nx\ny"});
        let mut split = ToolDisplay::preset(Preset::OpenCode);
        split.diff_view = DiffView::Split;
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&split)
            .palette(&p)
            .expanded(false)
            .width(120)
            .call();
        let boxed = box_rows("tool:edit  ok", &rows, 120, &p, None, false);
        for r in &boxed {
            let w: usize = r.iter().map(|(_, t)| t.chars().count()).sum();
            assert_eq!(w, 120, "a box row overflows the wide pane: {w}");
        }
        // The narrow pane: the auto layout switches to unified, the
        // rows stay single-segment, nothing wraps.
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(60)
            .call();
        let boxed = box_rows("tool:edit  ok", &rows, 60, &p, None, false);
        for r in &boxed {
            let w: usize = r.iter().map(|(_, t)| t.chars().count()).sum();
            assert!(w <= 60, "a box row overflows the narrow pane: {w}");
        }
    }

    /// Regression test: the `│` dividers of the split-diff layout must
    /// sit in the same column on every row (header, separator, and
    /// content rows). Each 3-segment row is `[left, " │ ", right]`;
    /// `pad_cell` keeps every left cell exactly `pane` wide, so the
    /// middle `│` lands at the same column regardless of content
    /// length.
    #[test]
    fn split_diff_dividers_align_across_rows() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let value = serde_json::json!({
            "before": "alpha\nbravo\ncarlos",
            "after": "alpha\nbravo-mod\ncharlie"
        });
        let mut split = ToolDisplay::preset(Preset::OpenCode);
        split.diff_view = DiffView::Split;
        let width = 120;
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&split)
            .palette(&p)
            .expanded(false)
            .width(width)
            .call();
        // Collect the starting column of the middle ` │ ` separator on
        // every row. Header/separator rows are 3 segments; content rows
        // are 5 (left gutter, left content, sep, right gutter, right
        // content). In both cases the left half is padded to a fixed
        // `pane` width, so the separator must start at the same column.
        let mut cols: Vec<usize> = Vec::new();
        for r in &rows {
            let mut col = 0usize;
            for (_, t) in r.iter() {
                if t == " │ " {
                    cols.push(col);
                    break;
                }
                col += t.chars().count();
            }
        }
        // The stats and path rows carry no separator; the header, the
        // separator row, and every content row do.
        assert!(
            cols.len() >= 3,
            "expected header + separator + content rows, got {cols:?}"
        );
        let first = cols[0];
        for c in &cols[1..] {
            assert_eq!(
                *c, first,
                "split-diff `│` divider drifted: {cols:?}"
            );
        }
    }

    /// Every `│` in a split diff — the two gutter separators and the
    /// middle ` │ ` between the panes — must sit in the same columns on
    /// the header row, the separator row, and every content row. The
    /// header used to render its gutter label without the leading space
    /// of the body gutter, so its `│` sat one column left of the body's;
    /// this regression test pins the alignment.
    #[test]
    fn split_diff_every_divider_lines_up_across_rows() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let value = serde_json::json!({
            "before": "alpha\nbravo\ncarlos",
            "after": "alpha\nbravo-mod\ncharlie"
        });
        let mut split = ToolDisplay::preset(Preset::OpenCode);
        split.diff_view = DiffView::Split;
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&split)
            .palette(&p)
            .expanded(true)
            .width(120)
            .call();

        // The column of every `│` in a row, in reading order.
        let divider_cols = |row: &BodyRow| -> Vec<usize> {
            let mut pos = 0usize;
            let mut cols = Vec::new();
            for (_, t) in row.iter() {
                let mut i = 0;
                for ch in t.chars() {
                    if ch == '│' {
                        cols.push(pos + i);
                    }
                    i += 1;
                }
                pos += t.chars().count();
            }
            cols
        };

        // The header and content rows carry the two gutter `│`s plus the
        // middle ` │ `; the separator row carries only the middle one
        // (its panes are solid dashes); the stats and path rows carry
        // none. Every divider in every row must land in a column used
        // by a full content row, so the grid never drifts.
        let dividered: Vec<Vec<usize>> = rows
            .iter()
            .map(divider_cols)
            .filter(|cols| !cols.is_empty())
            .collect();
        assert!(dividered.len() >= 3, "header + separator + content: {rows:?}");
        // A content row has all three dividers: left gutter, middle,
        // right gutter.
        let content = dividered
            .iter()
            .find(|cols| cols.len() == 3)
            .expect("a content row carries three dividers");
        assert_eq!(
            content.windows(2).all(|w| w[0] < w[1]),
            true,
            "content dividers are in increasing order: {content:?}"
        );
        let in_content = |cols: &[usize]| {
            let mut i = 0usize;
            for &c in content {
                if i < cols.len() && cols[i] == c {
                    i += 1;
                }
            }
            i == cols.len()
        };
        for cols in &dividered {
            assert!(
                in_content(cols),
                "a `│` sits in a column unused by content rows: {dividered:?}"
            );
        }
    }

    #[test]
    fn wrap_hard_line_fits_and_wraps_and_splits() {
        // A line that fits returns as one piece.
        assert_eq!(wrap_hard_line("hi there", 40), vec!["hi there"]);
        // A space wrap: the pieces stay within the width, the words
        // stay whole.
        let pieces = wrap_hard_line("alpha beta gamma delta", 11);
        assert!(pieces.iter().all(|p| p.chars().count() <= 11),
            "a piece overflows: {pieces:?}");
        assert_eq!(pieces.join(" "), "alpha beta gamma delta",
            "the wrap keeps the words: {pieces:?}");
        // A word wider than the pane splits at the width.
        let big = wrap_hard_line(&"x".repeat(50), 20);
        assert_eq!(big.len(), 3, "50 over 20 is three pieces: {big:?}");
        assert_eq!(big[0].chars().count(), 20);
        assert_eq!(big[2].chars().count(), 10);
        // Zero width returns the line untouched: the box border
        // handles the empty pane.
        assert_eq!(wrap_hard_line("abc", 0), vec!["abc"]);
    }

    #[test]
    fn bash_body_wraps_the_command_line_on_a_narrow_pane() {
        // The user request of the 2026-09-04 pass: the bash command
        // text wraps to the next line on a narrow terminal, not a
        // truncated row. The output lines keep the hard-line
        // behavior (the box truncates them).
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let cmd = "make -C /home/dev/project build --jobs 8 --target release";
        let value = serde_json::json!({
            "text": format!("$ {cmd}\nline one\nline two"),
            "exit_code": 0, "stdout": "", "stderr": "",
        });
        // A wide pane: the command fits, one row.
        let wide = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(120)
            .call();
        assert_eq!(wide.len(), 3, "command plus two lines: {wide:?}");
        assert_eq!(row_text(&wide[0]), format!("$ {cmd}"));
        // A narrow pane: the command wraps to several rows; the
        // output rows stay single.
        let narrow = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(24)
            .call();
        let head: Vec<String> = narrow.iter().take(4).map(row_text).collect();
        let joined: String = head.join("\n");
        assert!(
            joined.contains("$ make -C\n/home/dev/project build"),
            "the command wraps at a word boundary: {joined:?}"
        );
        for r in &narrow[..4] {
            assert!(row_text(r).chars().count() <= 24,
                "a wrapped row overflows the pane: {narrow:?}");
        }
    }

    #[test]
    fn box_rows_mark_the_truncated_overflow_with_an_ellipsis() {
        // The 2026-09-03 user directive: overflow is truncated with
        // a trailing ellipsis, never wrapped to the next line. The
        // box row keeps the box width exactly.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let long = "a".repeat(500);
        let rows: Vec<BodyRow> = vec![vec![(Style::default(), long.clone())]];
        let boxed = box_rows("tool:read  ok", &rows, 80, &p, None, false);
        // The body row sits between the top and the bottom border.
        assert_eq!(boxed.len(), 3, "the box has three rows: {boxed:?}");
        let body = &boxed[1];
        let w: usize = body.iter().map(|(_, t)| t.chars().count()).sum();
        assert_eq!(w, 80, "the body row keeps the box width: {w}");
        let text = row_text(body);
        // The row is `│` + 78 inner columns + `│`. The cut sits at
        // the last inner column: one space, 76 content columns, the
        // ellipsis. The right border closes the row.
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[78], '…', "the cut marks the overflow: {text:?}");
        assert_eq!(chars[79], '│', "the border closes the row: {text:?}");
        // The three-segment split row: the middle segment that
        // overflows the remaining columns carries the ellipsis,
        // the row still fits the width.
        let seg = "b".repeat(200);
        let rows3: Vec<BodyRow> =
            vec![vec![
                (Style::default(), "left".to_string()),
                (Style::default(), "│".to_string()),
                (Style::default(), seg),
            ]];
        let boxed3 = box_rows("tool:edit  ok", &rows3, 60, &p, None, false);
        let b3 = &boxed3[1];
        let w3: usize = b3.iter().map(|(_, t)| t.chars().count()).sum();
        assert_eq!(w3, 60, "the split row keeps the box width: {w3}");
        let t3 = row_text(b3);
        assert!(
            t3.contains("…"),
            "the overflow marks the cut with the ellipsis: {t3:?}"
        );
    }

    #[test]
    fn box_rows_draw_the_rounded_panel() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let body: Vec<BodyRow> = vec![
            vec![(Style::default(), "line one".to_string())],
            vec![(Style::default(), "line two".to_string())],
        ];
        let rows = box_rows("tool:read  ok", &body, 30, &p, None, false);
        // One terminal line per row: the top border row, then the
        // body rows (border + body + border cells), then the bottom.
        assert_eq!(rows.len(), 4, "top + 2 body rows + bottom");
        let top = rows_text(&[rows[0].clone()]);
        assert!(
            top[0].starts_with('\u{250c}') && top[0].ends_with('\u{2510}'),
            "{top:?}"
        );
        let bottom = rows_text(&[rows[3].clone()]);
        assert!(
            bottom[0].starts_with('\u{2514}') && bottom[0].ends_with('\u{2518}'),
            "{bottom:?}"
        );
        let r1 = rows_text(&[rows[1].clone()]);
        assert!(r1[0].contains("line one"), "{r1:?}");
        assert!(
            r1[0].starts_with('\u{2502}') && r1[0].ends_with('\u{2502}'),
            "{r1:?}"
        );
        // Every cell of the box carries the light background.
        for row in &rows {
            for (st, _) in row {
                assert!(st.bg.is_some(), "the box cells keep the background: {st:?}");
            }
        }
    }

    #[test]
    fn diff_layout_auto_switches_at_the_threshold() {
        let cfg = ToolDisplay::preset(Preset::Balanced);
        // Split triggers at width >= DIFF_SPLIT_MIN_WIDTH * 2 = 60.
        assert_eq!(cfg.diff_layout(60), DiffView::Split, "wide: split");
        assert_eq!(cfg.diff_layout(59), DiffView::Unified, "narrow: unified");
        let mut forced = ToolDisplay::preset(Preset::Balanced);
        forced.diff_view = DiffView::Unified;
        assert_eq!(
            forced.diff_layout(200),
            DiffView::Unified,
            "forced: unified"
        );
    }

    #[test]
    fn clamp_col_elides_the_overflow() {
        let s = "x".repeat(50);
        let c = clamp_col(s, 10);
        assert_eq!(c.chars().count(), 10, "{c:?}");
        assert!(c.ends_with('\u{2026}'), "{c:?}");
        assert_eq!(clamp_col(String::new(), 10), "");
    }

    /// P2/P3: the word-level diff highlights only the changed middle
    /// portion and leaves the unchanged prefix and suffix in their
    /// respective tones. (docs/tui-tool-display-fancy.md section 5)
    #[test]
    fn delta_word_diff_highlights_only_the_change() {
        let plain = Style::default();
        let changed = Style::default().add_modifier(Modifier::BOLD);

        let (old_segs, new_segs) =
            inline_word_diff("the quick brown fox", "the quick red fox", &plain, &plain, &changed)
                .expect("should produce a diff");

        // Verify the full text is preserved on both sides.
        let old_text: String = old_segs.iter().map(|(_, t)| t.as_str()).collect();
        let new_text: String = new_segs.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(old_text, "the quick brown fox");
        assert_eq!(new_text, "the quick red fox");

        // The "brown"/"red" word is the only changed word and must be
        // bold; every other token keeps the plain style.
        assert!(
            old_segs.iter().any(|(s, t)| t.as_str() == "brown" && s.has_modifier(Modifier::BOLD)),
            "'brown' segment should be bold: {old_segs:?}"
        );
        assert!(
            new_segs.iter().any(|(s, t)| t.as_str() == "red" && s.has_modifier(Modifier::BOLD)),
            "'red' segment should be bold: {new_segs:?}"
        );
        // Unchanged words must NOT be bold.
        for (s, t) in &old_segs {
            if t.as_str() != "brown" {
                assert!(!s.has_modifier(Modifier::BOLD), "unchanged segment should not be bold: {t:?}");
            }
        }
        for (s, t) in &new_segs {
            if t.as_str() != "red" {
                assert!(!s.has_modifier(Modifier::BOLD), "unchanged segment should not be bold: {t:?}");
            }
        }
    }

    /// The word-diff returns `None` when both lines are identical
    /// (no changed middle to highlight).
    #[test]
    fn delta_word_diff_none_on_identical_lines() {
        let s = Style::default();
        assert!(inline_word_diff("same line", "same line", &s, &s, &s).is_none());
        // Two empty lines: no tokens at all → no changed middle.
        assert!(inline_word_diff("", "", &s, &s, &s).is_none());
    }

    /// P2: `edit_body` in the unified layout applies the word-level
    /// diff to paired differing lines, so the changed token is bold
    /// while the rest of the line keeps the row tint.
    #[test]
    fn diff_lines_produces_the_changed_hunk() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let before = "line one\nline two\nline three";
        let after = "line one\nline two changed\nline three";
        let val = serde_json::json!({
            "path": "/tmp/f.rs",
            "before": before,
            "after": after,
        });

        // Use width 50 (< DIFF_SPLIT_MIN_WIDTH * 2 = 60) to force
        // the unified layout, which emits separate `-` / `+` lines.
        let rows = body_rows()
            .tool("edit")
            .value(&val)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(50)
            .call();
        let all_text: Vec<String> = rows.iter().map(row_text).collect();

        // The stats row.
        assert!(
            all_text.first().map_or(false, |t| t.contains("diff")),
            "stats line: {all_text:?}"
        );

        // In the pi-tool-display unified layout the `▌` gutter marker
        // (not a `-`/`+` text prefix) flags changed lines. A changed
        // position emits a removed line (old text) and an added line
        // (new text), both marked; context lines are unmarked.
        let marked: Vec<&String> = all_text.iter().filter(|l| l.contains('▌')).collect();
        assert_eq!(
            marked.len(),
            2,
            "a changed position emits one removed + one added marked line: {all_text:?}"
        );
        // The added line carries the new text.
        assert!(
            marked.iter().any(|l| l.contains("line two changed")),
            "an added marked line holds the new text: {all_text:?}"
        );
        // The removed line carries the old text.
        let removed = marked
            .iter()
            .find(|l| l.contains("line two") && !l.contains("changed"))
            .expect("a marked removed line with old 'line two'");
        assert!(removed.contains("line two"), "{removed}");
        // Context lines carry no marker.
        let ctx: Vec<&String> = all_text
            .iter()
            .filter(|l| l.contains("line one") || l.contains("line three"))
            .collect();
        assert!(
            !ctx.is_empty() && ctx.iter().all(|l| !l.contains('▌')),
            "context lines must not carry the ▌ marker: {ctx:?}"
        );
    }

    /// P3: when `expand_frac` is 0.0 (collapsed), `edit_body` caps
    /// the number of lines shown to `diff_collapsed_lines`, eliding
    /// the rest with a fold hint.
    #[test]
    fn delta_diff_elides_unchanged_runs() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);

        // 50 lines of before/after, differing in the middle.
        let before: Vec<String> = (0..50).map(|i| format!("old line {i}")).collect();
        let after: Vec<String> = (0..50)
            .map(|i| {
                if i == 25 {
                    "new line 25".to_string()
                } else {
                    format!("old line {i}")
                }
            })
            .collect();

        let val = serde_json::json!({
            "path": "/tmp/f.rs",
            "before": before.join("\n"),
            "after": after.join("\n"),
        });

        // Collapsed: frac = 0.0 → cap = diff_collapsed_lines.
        let collapsed_rows = body_rows()
            .tool("edit")
            .value(&val)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .expand_frac(0.0)
            .call();
        // Expanded: frac = 1.0 → cap = expanded_preview_max_lines.
        let expanded_rows = body_rows()
            .tool("edit")
            .value(&val)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .expand_frac(1.0)
            .call();

        assert!(
            collapsed_rows.len() < expanded_rows.len(),
            "collapsed {} should be < expanded {}",
            collapsed_rows.len(),
            expanded_rows.len()
        );
        // The collapsed view carries a fold hint.
        let last_text = row_text(collapsed_rows.last().unwrap());
        assert!(last_text.contains("more lines"), "{last_text}");
    }

    // ── ansi_to_body_rows tests ──────────────────────────────────────────

    #[test]
    fn ansi_plain_text_returns_none() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let fallback = Style::default().fg(p.color(crate::color::Role::ToolOutput));
        assert!(ansi_to_body_rows("hello world", &fallback).is_none());
        assert!(ansi_to_body_rows("", &fallback).is_none());
    }

    #[test]
    fn ansi_colored_text_produces_styled_rows() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let fallback = Style::default().fg(p.color(crate::color::Role::ToolOutput));
        // \x1b[31m = red foreground, \x1b[0m = reset
        let text = "\u{1b}[31mERROR: something\u{1b}[0m\nplain line";
        let rows = ansi_to_body_rows(text, &fallback).expect("should parse ANSI text");
        assert_eq!(rows.len(), 2);
        // First line: "ERROR: something" with red foreground.
        let row0_text = row_text(&rows[0]);
        assert_eq!(row0_text, "ERROR: something");
        let (style, _) = &rows[0][0];
        assert!(matches!(style.fg, Some(Color::Red)), "expected Red fg, got {style:?}");
        // Second line: "plain line" with fallback style (no ANSI codes).
        let row1_text = row_text(&rows[1]);
        assert_eq!(row1_text, "plain line");
    }

    #[test]
    fn ansi_empty_lines_produce_fallback_style() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let fallback = Style::default().fg(p.color(crate::color::Role::ToolOutput));
        // A line that is just an SGR reset produces an empty span; the
        // helper substitutes an empty string with the fallback style.
        let text = "\u{1b}[0m";
        let rows = ansi_to_body_rows(text, &fallback).expect("should parse");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].len() == 1, "single empty-span row: {rows:?}");
    }
}
