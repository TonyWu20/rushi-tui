//! The command-palette state machine (docs/tui-command-palette.md section 11).
//!
//! `PaletteState` is a pure, crossterm-free state machine. It holds the
//! open flag, the query string, the sub-stage (`Root` or `SessionList`),
//! the cursor index, the visible window, and the preview pane state.
//!
//! Keys handled by the state machine:
//! - `j` / `k` or `Ctrl+J` / `Ctrl+K` → move cursor (or option cursor
//!   when a Set item is highlighted)
//! - `Down` / `Up` → same as j/k
//! - other printable chars + `Backspace` → edit the query
//! - `PgUp` / `PgDn` → page
//! - `Home` / `End` → jump
//! - `Enter` → commit the highlighted item
//! - `Esc` → close (Root) or drop the sub-stage (SessionList)
//! - `Ctrl+U` / `Ctrl+D` → scroll the preview pane
//! - `Ctrl+P` → toggle the preview pane

/// The sub-stage of the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteStage {
    /// The root command list.
    Root,
    /// The session buffer sub-list (entered via the `b` Goto item).
    SessionList,
    /// The session-log tree sub-list (the `tree` Goto item). It lists
    /// the active session's events, fuzzy-searchable
    /// (docs/tree-ui-design-from-human.md).
    TreeList,
    /// The four outcome options for a tree-picked event
    /// (docs/tree-ui-design-from-human.md). Entered from `TreeList` on
    /// an event commit. `Esc` returns to `TreeList`.
    TreeOptions,
}

/// The outcome of a palette key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    /// The query changed (a char was typed or backspaced). The caller
    /// should re-rank the item list.
    Query,
    /// The list cursor moved. No query change.
    Move,
    /// The option cursor moved (Set item options). No list change.
    OptionMove,
    /// The user confirmed the highlighted item. The caller reads the
    /// cursor and stage to decide the commit action.
    Commit,
    /// `Esc` in `SessionList` stage: drop back to `Root`, keep the
    /// palette open, reset the query.
    DropSubStage,
    /// `Esc` in `Root` stage: close the palette.
    Closed,
    /// The preview pane scrolled.
    ScrollPreview,
    /// The preview pane visibility toggled.
    TogglePreview,
    /// The key is not handled by the palette. The caller decides
    /// whether to fall through.
    Nothing,
}

/// The command-palette state machine.
#[derive(Debug, Clone)]
pub struct PaletteState {
    pub open: bool,
    /// The query string the user is typing after `:`.
    pub query: String,
    /// The current sub-stage.
    pub stage: PaletteStage,
    /// When entering `SessionList`, this records the query length at
    /// that moment. The session filter is `query[goto_prefix_len..]`.
    pub goto_prefix_len: usize,
    /// The cursor index into the ranked list.
    cursor: usize,
    /// The top visible item index (scroll window).
    top: usize,
    /// The number of visible rows for the result list.
    pub visible: usize,
    /// The preview pane's scroll offset (line index).
    pub preview_scroll: usize,
    /// The user's preview-pane preference (toggled by Ctrl+P).
    pub preview_shown: bool,
    /// Whether the user explicitly forced the pane on, overriding the
    /// auto-hide cutoff.
    pub preview_forced: bool,
    /// The option cursor index for Set items with options.
    pub option_cursor: usize,
    /// The 1-based log seq of the tree event the user picked while in
    /// the `TreeOptions` stage (docs/tree-ui-design-from-human.md).
    /// `None` outside that stage.
    pub tree_seq: Option<usize>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    /// A fresh closed palette state.
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            stage: PaletteStage::Root,
            goto_prefix_len: 0,
            cursor: 0,
            top: 0,
            visible: 10,
            preview_scroll: 0,
            preview_shown: true,
            preview_forced: false,
            option_cursor: 0,
            tree_seq: None,
        }
    }

    /// Open the palette with a blank query and the given visible row
    /// count. Resets the stage to `Root`.
    pub fn open(&mut self, visible: usize) {
        self.open = true;
        self.query.clear();
        self.stage = PaletteStage::Root;
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.visible = visible.max(1);
        self.preview_scroll = 0;
        self.preview_shown = true;
        self.preview_forced = false;
        self.option_cursor = 0;
        self.tree_seq = None;
    }

    /// Close the palette, resetting all state.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.stage = PaletteStage::Root;
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.preview_forced = false;
        self.option_cursor = 0;
        self.tree_seq = None;
    }

    /// The current cursor index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The top visible item index.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Enter the `SessionList` sub-stage. Records the current query
    /// length as the goto prefix so the filter is everything typed
    /// after the Goto item was committed.
    pub fn goto_session_list(&mut self) {
        self.stage = PaletteStage::SessionList;
        self.goto_prefix_len = self.query.len();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
    }

    /// Enter the `TreeList` sub-stage (the `tree` Goto item). Like the
    /// session list, it records the query length as the goto prefix so
    /// the fuzzy filter is everything typed after `tree`.
    /// (docs/tree-ui-design-from-human.md)
    pub fn goto_tree_list(&mut self) {
        self.stage = PaletteStage::TreeList;
        self.goto_prefix_len = self.query.len();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        self.tree_seq = None;
    }

    /// Enter the `TreeOptions` sub-stage, remembering the picked event's
    /// 1-based log seq (docs/tree-ui-design-from-human.md "On
    /// selection, shows hint of 4 options"). The event list clears and
    /// the four options take its place.
    pub fn goto_tree_options(&mut self, seq: usize) {
        self.stage = PaletteStage::TreeOptions;
        self.tree_seq = Some(seq);
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
    }

    /// The picked event's 1-based log seq, while in the `TreeOptions`
    /// stage. `None` outside that stage.
    pub fn tree_seq(&self) -> Option<usize> {
        self.tree_seq
    }

    /// Drop back one sub-stage. `TreeOptions` returns to `TreeList`;
    /// `TreeList` and `SessionList` return to `Root`.
    pub fn drop_sub_stage(&mut self) {
        self.stage = match self.stage {
            PaletteStage::TreeOptions => PaletteStage::TreeList,
            PaletteStage::TreeList | PaletteStage::SessionList => PaletteStage::Root,
            PaletteStage::Root => PaletteStage::Root,
        };
        self.query.clear();
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        self.tree_seq = None;
    }

    /// The effective filter query for the current stage. In
    /// `SessionList`, the filter is everything typed after the goto
    /// commit point, with leading whitespace trimmed so a natural
    /// `b <name>` typing pattern still fuzzy-matches.
    pub fn filter_query(&self) -> &str {
        match self.stage {
            PaletteStage::Root => &self.query,
            PaletteStage::SessionList | PaletteStage::TreeList => {
                let p = self.goto_prefix_len.min(self.query.len());
                self.query[p..].trim_start_matches(' ')
            }
            // The option list is a fixed set, not fuzzy-ranked. The
            // query is unused there, so return it as-is.
            PaletteStage::TreeOptions => &self.query,
        }
    }

    /// Type a character into the query. Resets the cursor and option
    /// cursor to the top.
    pub fn type_char(&mut self, c: char) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        // The option list has no query to edit.
        if self.stage == PaletteStage::TreeOptions {
            return PaletteAction::Nothing;
        }
        self.query.push(c);
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Query
    }

    /// Backspace from the query. In `SessionList` or `TreeList` stage,
    /// if the query would go below the goto prefix, the sub-stage is
    /// dropped. The `TreeOptions` stage has no query to edit.
    pub fn backspace(&mut self) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if self.stage == PaletteStage::TreeOptions {
            return PaletteAction::Nothing;
        }
        let is_goto =
            self.stage == PaletteStage::SessionList || self.stage == PaletteStage::TreeList;
        if is_goto && self.query.len() <= self.goto_prefix_len {
            self.drop_sub_stage();
            return PaletteAction::DropSubStage;
        }
        self.query.pop();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Query
    }

    /// Move the list cursor down by one, clamped. If the highlighted
    /// item has options, move the option cursor instead.
    pub fn move_down(&mut self, count: usize, highlighted_options: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if highlighted_options > 0 {
            self.option_cursor = (self.option_cursor + 1) % highlighted_options;
            return PaletteAction::OptionMove;
        }
        if count == 0 {
            return PaletteAction::Nothing;
        }
        if self.cursor < count - 1 {
            self.cursor += 1;
        }
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Move the list cursor up by one, clamped. If the highlighted
    /// item has options, move the option cursor instead.
    pub fn move_up(&mut self, highlighted_options: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if highlighted_options > 0 {
            if self.option_cursor == 0 {
                self.option_cursor = highlighted_options - 1;
            } else {
                self.option_cursor -= 1;
            }
            return PaletteAction::OptionMove;
        }
        self.cursor = self.cursor.saturating_sub(1);
        self.adjust_top_count();
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page down: move the cursor down by the visible window.
    pub fn page_down(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = (self.cursor + self.visible).min(count - 1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page up: move the cursor up by the visible window.
    pub fn page_up(&mut self, count: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(self.visible);
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Jump the cursor to the top of the list.
    pub fn go_home(&mut self) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Jump the cursor to the last item.
    pub fn go_end(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = count - 1;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Clamp the cursor and top to the current list length.
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

    /// Adjust top without a count (for move_up where count is unknown
    /// to the state machine).
    fn adjust_top_count(&mut self) {
        // Without the count, just make sure top does not exceed
        // cursor. The renderer will call `sync` to clamp.
        if self.cursor < self.top {
            self.top = self.cursor;
        }
    }

    /// Scroll the preview pane up by `page` lines.
    pub fn scroll_preview_up(&mut self, page: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
        PaletteAction::ScrollPreview
    }

    /// Scroll the preview pane down by `page` lines.
    pub fn scroll_preview_down(&mut self, page: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_add(page);
        PaletteAction::ScrollPreview
    }

    /// Whether the preview pane is actually visible for a given result
    /// count and cutoff.
    pub fn preview_visible(&self, count: usize, cutoff: usize) -> bool {
        self.preview_shown && (count >= cutoff || self.preview_forced)
    }

    /// Toggle the preview pane visibility.
    pub fn toggle_preview(&mut self, count: usize, cutoff: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if self.preview_visible(count, cutoff) {
            self.preview_shown = false;
            self.preview_forced = false;
        } else {
            self.preview_shown = true;
            self.preview_forced = true;
        }
        PaletteAction::TogglePreview
    }

    /// Handle one key. `count` is the current ranked item count.
    /// `highlighted_options` is the option count of the item at the
    /// current cursor (0 for non-Set items). `preview_page` and
    /// `preview_cutoff` mirror the picker constants.
    pub fn press(
        &mut self,
        key: &crate::app::Key,
        count: usize,
        highlighted_options: usize,
        preview_page: usize,
        preview_cutoff: usize,
    ) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        use crate::app::Key;
        match key {
            Key::Esc => match self.stage {
                PaletteStage::Root => {
                    self.close();
                    PaletteAction::Closed
                }
                PaletteStage::SessionList | PaletteStage::TreeList | PaletteStage::TreeOptions => {
                    self.drop_sub_stage();
                    PaletteAction::DropSubStage
                }
            },
            Key::Enter => PaletteAction::Commit,
            Key::Char('j') | Key::CtrlJ | Key::Down => self.move_down(count, highlighted_options),
            Key::Char('k') | Key::CtrlK | Key::Up => self.move_up(highlighted_options),
            Key::PgDn => self.page_down(count),
            Key::PgUp => self.page_up(count),
            Key::Home => self.go_home(),
            Key::End => self.go_end(count),
            Key::CtrlU => self.scroll_preview_up(preview_page),
            Key::CtrlD => self.scroll_preview_down(preview_page),
            Key::CtrlP => self.toggle_preview(count, preview_cutoff),
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PaletteAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────
