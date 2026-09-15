//! The `CommandPreviewer`: renders the preview-pane content for a
//! highlighted palette item (docs/tui-command-palette.md sections 11,
//! 12).
//!
//! - `Run` items show their help text.
//! - `Set` items list their options, marking the current value with
//!   `*` and the user's selection with `>`.
//! - `Goto` items show help text; the sub-list replaces the left pane.
//! - `Ext` items show the extension's help text; settings list their
//!   options the same way `Set` items do.
//!
//! Session-buffer items (the `b` Goto sub-list) carry their metadata
//! in `help` as multi-line text the previewer renders verbatim.
//!
//! Tree-pane pipeline (docs/tree-ui-design-from-human-phase-2.md
//! item 2): items with a `Json` or `Markdown` preview kind route
//! their `help` source through the tree-pane pipeline. JSON parses
//! with `jaq-json` and pretty-prints with a two-space indent.
//! Then it highlights with the tree-sitter `tui-highlight` engine.
//! That is the same engine the transcript uses. A parse failure
//! shows the raw text and never crashes. The highlighted body is
//! cached per event seq in an LRU bound
//! (docs/tui-preview-pane-plan.md, windowed highlighting).

use crate::highlight::Seg;
use crate::palette::items::{CmdKind, PaletteItem, PreviewKind};
use crate::palette::state::PaletteStage;
use ratatui::style::{Style, Modifier};

/// The plain-line suffix shown after a tree event's highlighted body
/// in the `TreeList` stage (docs/tree-ui-design-from-human.md
/// "On selection, shows hint of 4 options").
pub const TREE_ENTER_OPTIONS_HINT: &str = "Enter offers the four options (View-only, Rewind without summary, Summarize the branch, Summarize with custom prompt).";

/// Whether the pane footer should show for the given stage. The
/// "Enter offers the four options" suffix is a `TreeList` thing
/// (docs/tree-ui-design-from-human-phase-2.md item 2).
pub fn footer_for_stage(stage: &PaletteStage) -> Option<&'static str> {
    if *stage == PaletteStage::TreeList {
        Some(TREE_ENTER_OPTIONS_HINT)
    } else {
        None
    }
}

/// Render the preview-pane content for one highlighted item.
///
/// Returns a list of lines; each line is a list of `(style, text)`
/// segments. The caller places the pane and applies scroll.
///
/// `footer` is an optional plain line shown after the content.
/// The tree stage passes the "Enter offers the four options" suffix
/// (docs/tree-ui-design-from-human-phase-2.md item 2). `cache` is
/// the LRU bound of highlighted tree-pane bodies. The key is the
/// item id, the event's 1-based log seq.
pub fn render_preview(
    item: &PaletteItem,
    option_cursor: usize,
    palette: &crate::color::Palette,
    footer: Option<&str>,
    cache: &mut TreePreviewCache,
) -> Vec<Vec<Seg>> {
    match item.preview_kind {
        PreviewKind::Json => tree_pane_lines(item, "json", palette, footer, cache),
        PreviewKind::Markdown => tree_pane_lines(item, "markdown", palette, footer, cache),
        PreviewKind::Plain => {
            match item.kind {
                CmdKind::Set | CmdKind::Ext if !item.options.is_empty() => {
                    option_list(item, option_cursor, palette)
                }
                // Run, Goto, and Ext-without-options: show help text.
                _ => help_lines(item, palette, footer),
            }
        }
    }
}

/// The tree-pane pipeline (docs/tree-ui-design-from-human-phase-2.md
/// item 2). JSON sources parse and pretty-print. A parse failure
/// shows the raw text. The result highlights with the tree-sitter
/// engine. The plain footer line follows. The highlight result is
/// memoized in `cache` per event seq. A re-visit of a seen event is
/// a cache hit.
fn tree_pane_lines(
    item: &PaletteItem,
    lang: &str,
    palette: &crate::color::Palette,
    footer: Option<&str>,
    cache: &mut TreePreviewCache,
) -> Vec<Vec<Seg>> {
    let source = if item.preview_kind == PreviewKind::Json {
        // Parse + pretty-print; the raw text on a parse failure.
        // The pane never crashes on bad JSON.
        match prettify_json(&item.help) {
            Ok(pretty) => pretty,
            Err(raw) => raw,
        }
    } else {
        item.help.clone()
    };
    let body = cache.get(&item.id).cloned().unwrap_or_else(|| {
        let highlighted = highlight_body(&source, lang, palette);
        cache.insert(&item.id, highlighted.clone());
        highlighted
    });
    let mut out = body;
    if let Some(text) = footer {
        // A blank separator, then the plain footer line. The
        // default-style segment picks up the pane's plain tone in
        // the renderer.
        out.push(Vec::new());
        out.push(vec![(Style::default(), text.to_string())]);
    }
    out
}

/// Parse `raw` as a single JSON value with `jaq-json` and pretty-
/// print it with a two-space indent
/// (docs/tree-ui-design-from-human-phase-2.md item 2). `Ok` is the
/// pretty text; `Err` is the raw input, for the fallback display.
fn prettify_json(raw: &str) -> Result<String, String> {
    let val = jaq_json::read::parse_single(raw.as_bytes()).map_err(|_| raw.to_string())?;
    let mut out: Vec<u8> = Vec::new();
    let pp = jaq_json::write::Pp {
        indent: Some("  ".to_string()),
        ..Default::default()
    };
    jaq_json::write::write(&mut out, &pp, 0, &val).map_err(|_| raw.to_string())?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Highlight `text` with the tree-sitter `tui-highlight` engine
/// (the same engine the transcript uses), lowering the Catppuccin
/// styles to the terminal capability level.
fn highlight_body(text: &str, lang: &str, palette: &crate::color::Palette) -> Vec<Vec<Seg>> {
    let lines = tui_highlight::highlight_lines(text, Some(lang));
    lines
        .into_iter()
        .map(|segs| {
            segs.into_iter()
                .map(|(style, text)| {
                    (crate::color::lower_style(style, palette.level()), text)
                })
                .collect()
        })
        .collect()
}

/// Lines for a `Set` / Ext-setting item: one line per option, the
/// current value marked with `*`, the selected option marked with
/// `>`.
fn option_list(
    item: &PaletteItem,
    option_cursor: usize,
    palette: &crate::color::Palette,
) -> Vec<Vec<Seg>> {
    let plain = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let dim = palette.style(crate::color::Role::Hint, Modifier::DIM);
    let accent = palette.style(crate::color::Role::Accent, Modifier::BOLD);

    let mut out: Vec<Vec<Seg>> = Vec::new();
    for (i, opt) in item.options.iter().enumerate() {
        let selected = i == option_cursor;
        let style = if selected {
            accent
        } else if opt.current {
            dim
        } else {
            plain
        };
        let mark = if selected {
            ">"
        } else if opt.current {
            "*"
        } else {
            " "
        };
        let text = format!("{mark} {}", opt.value);
        out.push(vec![(style, text)]);
    }
    if item.help.is_empty() {
        out
    } else {
        let mut lines = out;
        lines.push(Vec::new()); // blank separator
        lines.extend(help_lines(item, palette, None));
        lines
    }
}

/// Lines for help-text items. One `Seg` per line; blank line for an
/// empty help. An optional plain footer line follows.
fn help_lines(
    item: &PaletteItem,
    palette: &crate::color::Palette,
    footer: Option<&str>,
) -> Vec<Vec<Seg>> {
    let plain = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let mut out: Vec<Vec<Seg>> = Vec::new();
    for line in item.help.lines() {
        out.push(vec![(plain, line.to_string())]);
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    if let Some(text) = footer {
        out.push(Vec::new());
        out.push(vec![(Style::default(), text.to_string())]);
    }
    out
}

/// The LRU-bounded cache of highlighted tree-pane bodies, keyed by
/// the event's 1-based log seq (docs/tree-ui-design-from-human-
/// phase-2.md item 2). The model is the windowed highlighting of
/// docs/tui-preview-pane-plan.md. The value is the highlighter
/// output before palette-level lowering. A palette-level change
/// never invalidates an entry.
#[derive(Debug, Clone)]
pub struct TreePreviewCache {
    /// The cache capacity.
    cap: usize,
    /// Entries, oldest to newest: (seq, highlighted lines).
    entries: Vec<(String, Vec<Vec<Seg>>)>,
}

impl TreePreviewCache {
    /// The default cache capacity: thirty-two highlighted bodies.
    pub const DEFAULT_CAP: usize = 32;

    /// A cache at the default capacity.
    pub fn new() -> Self {
        Self {
            cap: Self::DEFAULT_CAP,
            entries: Vec::new(),
        }
    }

    /// Drop every entry. The session log changes on a palette
    /// open/close, so the seq-keyed entries are stale then.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The cached lines for `seq` on a hit. A hit promotes the entry
    /// to newest.
    pub fn get(&mut self, seq: &str) -> Option<&Vec<Vec<Seg>>> {
        let pos = self.entries.iter().rposition(|(k, _)| k == seq)?;
        let entry = self.entries.remove(pos);
        self.entries.push(entry);
        self.entries.last().map(|e| &e.1)
    }

    /// Store `lines` under `seq`. A same-key entry is replaced in
    /// place. An over-capacity insert evicts the oldest entry.
    pub fn insert(&mut self, seq: &str, lines: Vec<Vec<Seg>>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == seq) {
            self.entries.remove(pos);
        }
        if self.entries.len() >= self.cap {
            self.entries.remove(0);
        }
        self.entries.push((seq.to_string(), lines));
    }
}

impl Default for TreePreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid JSON parses and pretty-prints with a two-space indent.
    #[test]
    fn valid_json_parses_and_pretty_prints() {
        let out = prettify_json(r#"{"command":"make","exit_code":0}"#).unwrap();
        assert!(
            out.contains("  "),
            "two-space indent, got:\n{out}"
        );
        assert!(
            out.starts_with('{') && out.ends_with('}'),
            "object shape, got:\n{out}"
        );
        // The pretty form is multi-line with a nested two-space indent.
        assert!(out.contains('\n'), "pretty JSON spans lines:\n{out}");
    }

    /// A JSON-encoded string value parses to the string and pretty-
    /// prints as a quoted JSON string.
    #[test]
    fn a_json_encoded_string_value_parses() {
        let out = prettify_json(r#""hello world""#).unwrap();
        assert_eq!(out, r#""hello world""#, "got: {out}");
    }

    /// Bad JSON falls back to the raw text and never crashes.
    #[test]
    fn bad_json_falls_back_to_raw_text() {
        let raw = r#"{not valid json"#;
        let fallback = prettify_json(raw).unwrap_err();
        assert_eq!(fallback, raw, "the raw text is returned verbatim");
    }

    /// The empty string is not a valid JSON value: it falls back.
    #[test]
    fn empty_source_falls_back_to_raw_text() {
        let fallback = prettify_json("").unwrap_err();
        assert_eq!(fallback, "");
    }

    /// The cache is LRU-bounded: an over-capacity insert evicts the
    /// oldest entry.
    #[test]
    fn the_cache_is_lru_bounded() {
        let mut c = TreePreviewCache::new();
        let line = |n: u32| vec![vec![(Style::default(), n.to_string())]];
        for n in 0..(TreePreviewCache::DEFAULT_CAP as u32) {
            c.insert(&n.to_string(), line(n));
        }
        // Touching "5" promotes it to newest.
        assert!(c.get("5").is_some());
        // Insert one more: the oldest entry ("0") is evicted.
        c.insert("overflow", line(99));
        assert!(c.get("0").is_none(), "the oldest entry evicted");
        assert!(c.get("5").is_some(), "a touched entry survives");
        assert!(c.get("overflow").is_some());
    }

    /// A repeated get returns the cached body without re-running the
    /// highlighter (the windowed-highlighting model).
    #[test]
    fn a_repeat_get_is_a_cache_hit() {
        let mut c = TreePreviewCache::new();
        c.insert("7", vec![vec![(Style::default(), "body".to_string())]]);
        let hit = c.get("7").cloned().unwrap();
        assert_eq!(hit[0][0].1, "body");
    }
}
