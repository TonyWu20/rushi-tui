//! The picker state machine (section 4.3).
//!
//! `PickerState` is a pure, crossterm-free state machine. It holds
//! the open flag, the query string, the cursor index, the visible
//! window, and the preview pane state. Tests drive it directly, like
//! `app.rs` and `browse.rs` do today.
//!
//! Keys the state machine handles:
//! - type a char → edit the query
//! - `Ctrl+J` / `Ctrl+K` or arrows → move the cursor
//! - `PgUp` / `PgDn` → page
//! - `Home` / `End` → jump
//! - `Enter` → commit
//! - `Esc` → close
//! - `Ctrl+U` / `Ctrl+D` → scroll the preview pane
//! - `Ctrl+I` → cycle the file scope (standard → ignored → hidden →
//!   standard, P9 in docs/tui-file-picker.md). In a standard terminal
//!   `Ctrl+I` is byte 0x09 — the same byte as `Tab` — so crossterm
//!   reports it as `KeyCode::Tab`. The picker therefore also binds
//!   `Tab` to the scope cycle.
//!
//! Multi-select and quickfix are later adds (section 9).

use super::items::FileScope;

/// The outcome of a picker key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PickAction {
    /// The query was modified (char typed or backspaced). The caller
    /// should push the new query to the matcher.
    Query,
    /// The cursor moved (j/k, page, home, end). No query change.
    Move,
    /// The user confirmed the selection. `Some(idx)` = a result was
    /// selected; `None` = zero results, commit the raw `@query` text.
    Commit(Option<usize>),
    /// The picker closed without committing.
    Closed,
    /// The preview pane scrolled.
    ScrollPreview,
    /// The preview pane visibility toggled.
    TogglePreview,
    /// The file scope cycled (`Ctrl+I` / `Tab`). The caller re-enumerates the
    /// item list under `PickerState::scope` and re-ranks the live
    /// query (docs/tui-file-picker.md P9).
    Recollect,
    /// The key is not handled by the picker. The caller falls through
    /// to the editor.
    Nothing,
}

/// The picker state machine.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub open: bool,
    /// The query string the user is typing after the `@`.
    pub query: String,
    /// The cursor index into the ranked snapshot.
    cursor: usize,
    /// The top visible item index (scroll window).
    top: usize,
    /// The number of visible rows for the result list.
    pub visible: usize,
    /// The preview pane's scroll offset (line index into the
    /// previewer's output).
    pub preview_scroll: usize,
    /// The user's preview-pane preference (toggled by a key, auto-hidden
    /// by `preview_cutoff` while it stays at the default).
    pub preview_shown: bool,
    /// Whether the user explicitly forced the pane on, overriding the
    /// `preview_cutoff` auto-hide. Set by the toggle key when the pane
    /// is currently hidden.
    pub preview_forced: bool,
    /// The file scope the item list was enumerated at
    /// (docs/tui-file-picker.md P9). Cycled by `Ctrl+I` / `Tab`; the
    /// caller re-collects items when it changes.
    pub scope: FileScope,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// A fresh closed picker state.
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            cursor: 0,
            top: 0,
            visible: 10,
            preview_scroll: 0,
            preview_shown: true,
            preview_forced: false,
            scope: FileScope::Standard,
        }
    }

    /// Open the picker with an initial query.
    pub fn open(&mut self, query: &str, visible: usize) {
        self.open = true;
        self.query = query.to_string();
        self.cursor = 0;
        self.top = 0;
        self.visible = visible.max(1);
        self.preview_scroll = 0;
        self.preview_shown = true;
        self.preview_forced = false;
        self.scope = FileScope::Standard;
    }

    /// Close the picker without committing.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.preview_forced = false;
        self.scope = FileScope::Standard;
    }

    /// The current cursor index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The top visible item index.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Type a character into the query. Resets the cursor to the top.
    pub fn type_char(&mut self, c: char) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.query.push(c);
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        PickAction::Query
    }

    /// Backspace from the query. If the query becomes empty the picker
    /// stays open (showing all items) but the query is empty.
    pub fn backspace(&mut self) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.query.pop();
        self.preview_scroll = 0;
        PickAction::Query
    }

    /// Move the cursor down by one, clamped to the item count.
    pub fn move_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        if self.cursor < count - 1 {
            self.cursor += 1;
        }
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Move the cursor up by one.
    pub fn move_up(&mut self, count: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Page down: move the cursor down by the visible window.
    pub fn page_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = (self.cursor + self.visible).min(count - 1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Page up: move the cursor up by the visible window.
    pub fn page_up(&mut self, count: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(self.visible);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Jump the cursor to the top of the list.
    pub fn go_home(&mut self) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Jump the cursor to the last item.
    pub fn go_end(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = count - 1;
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Clamp the cursor and top to the current snapshot length.
    /// Called by the renderer each frame before drawing.
    pub fn sync(&mut self, count: usize) {
        if count == 0 {
            self.cursor = 0;
            self.top = 0;
            return;
        }
        self.cursor = self.cursor.min(count - 1);
        self.adjust_top(count);
    }

    fn adjust_top(&mut self, count: usize) {
        if self.cursor >= self.top + self.visible {
            self.top = self.cursor - self.visible + 1;
        }
        if self.cursor < self.top {
            self.top = self.cursor;
        }
        let max_top = count.saturating_sub(self.visible);
        if self.top > max_top {
            self.top = max_top;
        }
    }

    /// Scroll the preview pane up by `page` lines.
    pub fn scroll_preview_up(&mut self, page: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
        PickAction::ScrollPreview
    }

    /// Scroll the preview pane down by `page` lines.
    pub fn scroll_preview_down(&mut self, page: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_add(page);
        PickAction::ScrollPreview
    }

    /// Whether the preview pane is actually visible for a given result
    /// count and cutoff. Auto-hidden below the cutoff unless the user
    /// explicitly forced it on with the toggle key.
    pub fn preview_visible(&self, count: usize, cutoff: usize) -> bool {
        self.preview_shown && (count >= cutoff || self.preview_forced)
    }

    /// Toggle the preview pane visibility. The toggle is smart: it
    /// flips the effective visibility. When the pane is currently
    /// visible it turns it off; when it is hidden (by the cutoff or a
    /// previous off) it forces it on, overriding the cutoff. This is
    /// what lets `Ctrl+P` bring the pane back after the cutoff
    /// auto-hid it.
    pub fn toggle_preview(&mut self, count: usize, cutoff: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        if self.preview_visible(count, cutoff) {
            // Currently visible: turn off and drop the forced override.
            self.preview_shown = false;
            self.preview_forced = false;
        } else {
            // Hidden (by the cutoff or a previous off): force it on,
            // overriding the cutoff.
            self.preview_shown = true;
            self.preview_forced = true;
        }
        PickAction::TogglePreview
    }

    /// Cycle the file scope: standard → ignored → hidden → standard
    /// (docs/tui-file-picker.md P9). The item list is re-enumerated
    /// by the caller on the returned [`PickAction::Recollect`], so
    /// reset the cursor to the top of the fresh list.
    pub fn cycle_scope(&mut self) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.scope = self.scope.next();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        PickAction::Recollect
    }

    /// Handle one key. `count` is the current snapshot item count.
    /// `preview_page` is the scroll page size for Ctrl+U/D.
    /// `preview_cutoff` is the auto-hide threshold for the toggle.
    pub fn press(
        &mut self,
        key: &crate::app::Key,
        count: usize,
        preview_page: usize,
        preview_cutoff: usize,
    ) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        use crate::app::Key;
        match key {
            Key::Esc => {
                self.close();
                PickAction::Closed
            }
            Key::Enter => {
                let idx = if count > 0 { Some(self.cursor.min(count - 1)) } else { None };
                self.close();
                PickAction::Commit(idx)
            }
            Key::Down | Key::CtrlJ => self.move_down(count),
            Key::Up | Key::CtrlK => self.move_up(count),
            Key::PgDn => self.page_down(count),
            Key::PgUp => self.page_up(count),
            Key::Home => self.go_home(),
            Key::End => self.go_end(count),
            Key::CtrlU => self.scroll_preview_up(preview_page),
            Key::CtrlD => self.scroll_preview_down(preview_page),
            Key::CtrlP => self.toggle_preview(count, preview_cutoff),
            // Ctrl+I and Tab are the same key in a standard terminal
            // (both send byte 0x09; crossterm parses that byte as
            // `KeyCode::Tab`). Either one cycles the file scope.
            Key::CtrlI | Key::Tab => self.cycle_scope(),
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PickAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

