//! The picker state machine (section 4.3).
//!
//! `PickerState` is a pure, crossterm-free state machine. It holds
//! the open flag, the query string, the cursor index, the visible
//! window, and the preview pane state. Tests drive it directly, like
//! `app.rs` and `browse.rs` do today.
//!
//! Keys the state machine handles:
//! - type a char → edit the query
//! - `Ctrl+J` / `Ctrl+K` or arrows → move the cursor (wrapping in
//!   the ring; docs/tree-ui-design-from-human-phase-2.md item 4)
//! - `PgUp` / `PgDn` → page (wrapping at both ends)
//! - `Home` / `End` → jump
//! - `Enter` → commit
//! - `Esc` → close
//! - `Ctrl+U` / `Ctrl+D` → half-page scroll the focused pane
//! - `Ctrl+P` → toggle the preview pane
//! - `Ctrl+Shift+P` / `BackTab` → toggle list/preview focus
//! - `Ctrl+T` / `Ctrl+I` → cycle the file scope
//! - `Tab` → complete the highlighted item into the draft
//!
//! Multi-select and quickfix are later adds (section 9).

use super::items::FileScope;
use super::preview::PreviewLoad;
use crate::float::Focus;

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
    /// The list/preview focus toggled (docs/tree-ui-design-from-
    /// human-phase-2.md item 4).
    ToggleFocus,
    /// `Tab` completed the highlighted item. The caller inserts
    /// `@<path>` into the draft; the picker stays open
    /// (docs/tree-ui-design-from-human-phase-2.md item 5).
    Complete,
    /// The file scope cycled (`Ctrl+T`, or `Ctrl+I` where the
    /// terminal reports it distinctly; `Tab` no longer cycles it,
    /// docs/tree-ui-design-from-human-phase-2.md item 5). The
    /// caller re-enumerates the item list under
    /// `PickerState::scope` and re-ranks the live query
    /// (docs/tui-file-picker.md P9).
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
    /// (docs/tui-file-picker.md P9). Cycled by `Ctrl+T` / `Ctrl+I`;
    /// the caller re-collects items when it changes.
    pub scope: FileScope,
    /// The background preview read slot (docs/tui-preview-pane-plan.md,
    /// layer 2). Cancelled on every cursor change: an in-flight read
    /// for the old item is dropped when the cursor moves.
    pub preview_load: PreviewLoad,
    /// Which pane has focus (docs/tree-ui-design-from-human-phase-2.md
    /// item 4). Toggled by `Ctrl+Shift+P` / `BackTab`.
    pub focus: Focus,
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
            preview_load: PreviewLoad::None,
            focus: Focus::default(),
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
        self.preview_load = PreviewLoad::None;
        self.focus = Focus::default();
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
        self.preview_load = PreviewLoad::None;
        self.focus = Focus::default();
    }

    /// Cancel the preview load. A cursor or query change orients the
    /// pane at a different item, so any in-flight or settled load is
    /// stale. The caller dispatches the fresh read.
    fn cancel_preview_load(&mut self) {
        self.preview_load = PreviewLoad::None;
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
        self.cancel_preview_load();
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
        self.cancel_preview_load();
        PickAction::Query
    }

    /// Move the cursor down by one. Wrap in the ring: at the last
    /// row the cursor jumps to the first
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn move_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = (self.cursor + 1) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
        PickAction::Move
    }

    /// Move the cursor up by one. Wrap in the ring: at the first row
    /// the cursor jumps to the last
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn move_up(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = (self.cursor + count - 1) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
        PickAction::Move
    }

    /// Page down: move the cursor down by the visible window. A page
    /// that crosses the list end lands on the first row
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn page_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        let next = self.cursor + self.visible;
        self.cursor = if next >= count { 0 } else { next };
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
        PickAction::Move
    }

    /// Page up: move the cursor up by the visible window. A page that
    /// crosses the list start lands on the last row
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn page_up(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = if self.cursor >= self.visible {
            self.cursor - self.visible
        } else {
            count - 1
        };
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
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
        self.cancel_preview_load();
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
        self.cancel_preview_load();
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
        self.cancel_preview_load();
        PickAction::Recollect
    }

    /// Half-page scroll of the focused entry list
    /// (docs/tree-ui-design-from-human-phase-2.md item 4): the step
    /// is half the visible rows, minimum one, wrapping in the ring
    /// like the step keys.
    pub fn scroll_list_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        let step = (self.visible / 2).max(1);
        self.cursor = (self.cursor + step) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
        PickAction::Move
    }

    /// Half-page scroll up of the focused entry list (the mirror of
    /// [`scroll_list_down`]).
    pub fn scroll_list_up(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        let step = (self.visible / 2).max(1);
        self.cursor = (self.cursor + count - step) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.cancel_preview_load();
        PickAction::Move
    }

    /// Toggle focus between the entry list and the preview pane
    /// (docs/tree-ui-design-from-human-phase-2.md item 4). A no-op
    /// while the preview pane is hidden.
    pub fn toggle_focus(&mut self, count: usize, cutoff: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        if !self.preview_visible(count, cutoff) {
            return PickAction::Nothing;
        }
        self.focus = match self.focus {
            Focus::List => Focus::Preview,
            Focus::Preview => Focus::List,
        };
        PickAction::ToggleFocus
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
                let idx = if count > 0 {
                    Some(self.cursor.min(count - 1))
                } else {
                    None
                };
                self.close();
                PickAction::Commit(idx)
            }
            Key::Down | Key::CtrlJ => self.move_down(count),
            Key::Up | Key::CtrlK => self.move_up(count),
            Key::PgDn => self.page_down(count),
            Key::PgUp => self.page_up(count),
            Key::Home => self.go_home(),
            Key::End => self.go_end(count),
            // `Ctrl+U` / `Ctrl+D` half-page scroll the focused pane
            // (docs/tree-ui-design-from-human-phase-2.md item 4):
            // the list in half-visible-row steps, the preview in
            // `PREVIEW_PAGE` line steps.
            Key::CtrlU => match self.focus {
                Focus::List => self.scroll_list_up(count),
                Focus::Preview => self.scroll_preview_up(preview_page),
            },
            Key::CtrlD => match self.focus {
                Focus::List => self.scroll_list_down(count),
                Focus::Preview => self.scroll_preview_down(preview_page),
            },
            Key::CtrlP => self.toggle_preview(count, preview_cutoff),
            // `Ctrl+Shift+P` toggles the focus. `BackTab` is the
            // legacy fallback (docs/tree-ui-design-from-human-phase-
            // 2.md item 4).
            Key::CtrlShiftP | Key::BackTab => self.toggle_focus(count, preview_cutoff),
            // `Ctrl+T` cycles the file scope. `Ctrl+I` stays bound
            // for terminals that report it distinctly
            // (docs/tree-ui-design-from-human-phase-2.md item 5).
            Key::CtrlT | Key::CtrlI => self.cycle_scope(),
            // `Tab` completes the highlighted item into the draft
            // (docs/tree-ui-design-from-human-phase-2.md item 5). In
            // a standard terminal `Tab` is the `Ctrl+I` byte (0x09),
            // which crossterm parses as `KeyCode::Tab`.
            Key::Tab => PickAction::Complete,
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PickAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Key;

    /// An open picker with ten visible rows.
    fn open() -> PickerState {
        let mut s = PickerState::new();
        s.open("q", 10);
        s
    }

    // ── wrap (item 4) ─────────────────────────────────────────

    /// `Down` at the last row jumps to the first (ring wrap).
    #[test]
    fn move_down_wraps_from_last_to_first() {
        let mut s = open();
        s.cursor = 9;
        assert_eq!(s.move_down(10), PickAction::Move);
        assert_eq!(s.cursor(), 0);
    }

    /// `Up` at the first row jumps to the last (ring wrap).
    #[test]
    fn move_up_wraps_from_first_to_last() {
        let mut s = open();
        assert_eq!(s.move_up(10), PickAction::Move);
        assert_eq!(s.cursor(), 9);
    }

    /// `PgDn` at the last row lands on the first row.
    #[test]
    fn page_down_from_last_lands_on_first() {
        let mut s = open();
        s.cursor = 9;
        assert_eq!(s.page_down(10), PickAction::Move);
        assert_eq!(s.cursor(), 0);
    }

    /// `PgUp` at the first row lands on the last row.
    #[test]
    fn page_up_from_first_lands_on_last() {
        let mut s = open();
        assert_eq!(s.page_up(10), PickAction::Move);
        assert_eq!(s.cursor(), 9);
    }

    /// `Ctrl+U` / `Ctrl+D` on the focused list step half the visible
    /// rows and wrap in the ring like the step keys.
    #[test]
    fn half_page_scroll_wraps_like_the_step_keys() {
        let mut s = open();
        // The step is half the visible rows: 10 / 2 = 5.
        s.cursor = 4;
        assert_eq!(s.scroll_list_down(20), PickAction::Move);
        assert_eq!(s.cursor(), 9, "4 + 5");
        s.cursor = 15;
        assert_eq!(s.scroll_list_down(20), PickAction::Move);
        assert_eq!(s.cursor(), 0, "(15 + 5) % 20 wraps to the first");
        s.cursor = 4;
        assert_eq!(s.scroll_list_up(20), PickAction::Move);
        assert_eq!(s.cursor(), 19, "(4 + 20 - 5) % 20 wraps to the last");
    }

    // ── focus (item 4) ────────────────────────────────────────

    /// The toggle is a no-op while the preview pane is hidden.
    #[test]
    fn focus_toggle_noop_when_preview_hidden() {
        let mut s = open();
        assert_eq!(s.toggle_focus(2, 4), PickAction::Nothing);
        assert_eq!(s.focus, Focus::List);
    }

    /// The toggle flips when the preview pane is visible.
    #[test]
    fn focus_toggle_flips_when_preview_visible() {
        let mut s = open();
        assert_eq!(s.toggle_focus(10, 4), PickAction::ToggleFocus);
        assert_eq!(s.focus, Focus::Preview);
        assert_eq!(s.toggle_focus(10, 4), PickAction::ToggleFocus);
        assert_eq!(s.focus, Focus::List);
    }

    /// `Ctrl+Shift+P` and the legacy `BackTab` fallback both reach
    /// the toggle through `press`.
    #[test]
    fn focus_toggle_keys_reach_the_state_machine() {
        let mut s = open();
        assert_eq!(s.press(&Key::CtrlShiftP, 10, 5, 4), PickAction::ToggleFocus);
        assert_eq!(s.focus, Focus::Preview);
        assert_eq!(s.press(&Key::BackTab, 10, 5, 4), PickAction::ToggleFocus);
        assert_eq!(s.focus, Focus::List);
    }

    /// `Ctrl+U` / `Ctrl+D` on the focused preview scroll the pane in
    /// `PREVIEW_PAGE` line steps, not the list.
    #[test]
    fn preview_focus_scroll_moves_the_pane() {
        let mut s = open();
        s.focus = Focus::Preview;
        s.preview_scroll = 10;
        assert_eq!(s.press(&Key::CtrlU, 10, 5, 4), PickAction::ScrollPreview);
        assert_eq!(s.preview_scroll, 5);
        assert_eq!(s.press(&Key::CtrlD, 10, 5, 4), PickAction::ScrollPreview);
        assert_eq!(s.preview_scroll, 10);
        assert_eq!(s.cursor(), 0, "the list cursor is untouched");
    }

    // ── completion and scope (item 5) ─────────────────────────

    /// `Tab` completes the highlighted item; the picker stays open.
    #[test]
    fn tab_completes_the_highlighted_item() {
        let mut s = open();
        assert_eq!(s.press(&Key::Tab, 10, 5, 4), PickAction::Complete);
        assert!(s.open, "the picker stays open after a completion");
    }

    /// `Ctrl+T` cycles the file scope; `Tab` no longer does.
    #[test]
    fn ctrl_t_cycles_the_scope() {
        let mut s = open();
        assert_eq!(s.press(&Key::CtrlT, 10, 5, 4), PickAction::Recollect);
        assert_eq!(s.scope, FileScope::IncludeIgnored);
    }

    /// `Ctrl+I` stays bound for terminals that report it distinctly.
    #[test]
    fn ctrl_i_still_cycles_the_scope() {
        let mut s = open();
        assert_eq!(s.press(&Key::CtrlI, 10, 5, 4), PickAction::Recollect);
        assert_eq!(s.scope, FileScope::IncludeIgnored);
    }

    // ── resets (item 4) ───────────────────────────────────────

    /// Open and close reset the focus.
    #[test]
    fn open_and_close_reset_focus() {
        let mut s = open();
        s.focus = Focus::Preview;
        s.close();
        assert_eq!(s.focus, Focus::List);
        s.open("q", 10);
        assert_eq!(s.focus, Focus::List);
    }
}
