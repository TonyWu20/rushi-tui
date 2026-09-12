//! Previewers (section 4.4).
//!
//! The `Previewer` trait is the seam that swaps the preview pane
//! content. [`FilePreviewer`] shows file text with syntax
//! highlighting (section 9 follow-up: "code highlight is a later
//! add"); the [`NullPreviewer`] shows nothing.

use super::items::PickerItem;
use crate::color::Palette;
use crate::highlight::{language_from_path, highlight_text_lines, Seg};

/// The previewer seam. A picker body takes one previewer and renders
/// the preview pane for the selected item.
pub trait Previewer {
    /// Whether this previewer renders anything. The null previewer
    /// returns `false` and the pane is dropped.
    fn enabled(&self) -> bool;

    /// The header line for the preview pane (path, size, line count).
    /// `None` when the item cannot be previewed.
    fn header(&self, item: &PickerItem) -> Option<String>;

    /// The preview lines for the selected item, as styled segments
    /// per hard line (`Seg` = `(Style, String)`). Highlighted runs
    /// carry the palette syntax roles; plain runs keep the default
    /// style, which the pane paints with the plain-text tone.
    fn content(&self, item: &PickerItem, palette: &Palette) -> Vec<Vec<Seg>>;
}

/// A previewer that reads and shows the text content of a file.
///
/// The item's `payload` is the absolute path. The header shows the
/// path, byte size, and line count. The content is the file text,
/// capped at `max_lines` lines.
pub struct FilePreviewer {
    max_lines: usize,
}

impl FilePreviewer {
    /// Create a file previewer that shows at most `max_lines` lines.
    pub fn new(max_lines: usize) -> Self {
        Self { max_lines }
    }
}

impl Previewer for FilePreviewer {
    fn enabled(&self) -> bool {
        true
    }

    fn header(&self, item: &PickerItem) -> Option<String> {
        let path = &item.payload;
        let Ok(meta) = std::fs::metadata(path) else {
            return Some(format!("{} (unreadable)", item.label));
        };
        let size = meta.len();
        let line_count = std::fs::read_to_string(path)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        Some(format!(
            "{}  {}  {} lines",
            item.label,
            human_size(size),
            line_count
        ))
    }

    fn content(&self, item: &PickerItem, palette: &Palette) -> Vec<Vec<Seg>> {
        let path = &item.payload;
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                return vec![vec![(
                    palette.style(crate::color::Role::Error, ratatui::style::Modifier::empty()),
                    format!("(cannot read {})", item.label),
                )]]
            }
        };
        // The shared syntax-highlight entry point: the language is
        // detected from the file path; unknown types stay plain.
        let lang = language_from_path(path);
        highlight_text_lines(&text, lang, palette)
            .into_iter()
            .take(self.max_lines)
            .collect()
    }
}

/// A previewer that shows nothing. The preview pane is dropped.
///
/// Day 0 wires `FilePreviewer`; this is the off seam for later
/// previews that need no pane. Unused in production today, so the
/// dead-code lint is suppressed.
#[allow(dead_code)]
pub struct NullPreviewer;

impl Previewer for NullPreviewer {
    fn enabled(&self) -> bool {
        false
    }

    fn header(&self, _item: &PickerItem) -> Option<String> {
        None
    }

    fn content(&self, _item: &PickerItem, _palette: &Palette) -> Vec<Vec<Seg>> {
        Vec::new()
    }
}

/// Format a byte count as a human-readable string.
fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1}G", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1}M", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}K", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}
