//! The command-palette state machine (docs/tui-command-palette.md section 11).
//!
//! `PaletteState` is a pure, crossterm-free state machine. It holds the
//! open flag, the query string, the sub-stage (`Root` or `SessionList`),
//! the cursor index, the visible window, and the preview pane state.
//!
//! Keys handled by the state machine:
//! - `Ctrl+J` / `Ctrl+K` or arrows → move the cursor (wrapping; the
//!   state machine wraps the index in the ring). Plain `j` and `k`
//!   type into the query.
//! - `PgUp` / `PgDn` → page (wrapping at both ends)
//! - `Home` / `End` → jump
//! - `Enter` → commit the highlighted item
//! - `Esc` → close (Root) or drop the sub-stage (SessionList)
//! - `Ctrl+U` / `Ctrl+D` → half-page scroll the focused pane
//! - `Ctrl+P` → toggle the preview pane
//! - `Ctrl+Shift+P` / `BackTab` → toggle list/preview focus
//! - `Ctrl+F` → cycle the tree event-type filter (TreeList stage only)
//! - `Tab` → complete the highlighted item into the query

use crate::event::EventKind;
use crate::float::Focus;
use crate::palette::preview::TreePreviewCache;

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

/// The tree-list event-type filter (docs/tree-ui-design-from-human-
/// phase-2.md item 3). `Ctrl+F` cycles it in the `TreeList` stage.
/// It narrows candidates before fuzzy ranking; it ANDs with the
/// query. It resets on stage exit and on palette close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeFilter {
    /// Every event.
    #[default]
    Full,
    /// `user_message` + `user_message_retract`.
    User,
    /// `assistant_message` only.
    Assistant,
    /// `tool_call` + `tool_result`.
    Tool,
    /// The user and assistant sets together.
    UserAssistant,
}

impl TreeFilter {
    /// The next value in the `Ctrl+F` cycle:
    /// full → user → assistant → tool → user+assistant → full.
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::User,
            Self::User => Self::Assistant,
            Self::Assistant => Self::Tool,
            Self::Tool => Self::UserAssistant,
            Self::UserAssistant => Self::Full,
        }
    }

    /// The short label for the input-bar hint, e.g. `tool`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::UserAssistant => "user+assistant",
        }
    }

    /// Whether an event of `kind` survives the filter.
    pub fn keeps(self, kind: EventKind) -> bool {
        match self {
            Self::Full => true,
            Self::User => matches!(
                kind,
                EventKind::UserMessage | EventKind::UserMessageRetract
            ),
            Self::Assistant => kind == EventKind::AssistantMessage,
            Self::Tool => matches!(kind, EventKind::ToolCall | EventKind::ToolResult),
            Self::UserAssistant => matches!(
                kind,
                EventKind::UserMessage
                    | EventKind::UserMessageRetract
                    | EventKind::AssistantMessage
            ),
        }
    }
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
    /// The list/preview focus toggled (docs/tree-ui-design-from-
    /// human-phase-2.md item 4).
    ToggleFocus,
    /// The tree event-type filter cycled (docs/tree-ui-design-from-
    /// human-phase-2.md item 3). The caller rebuilds the ranked
    /// list, as for `Query`.
    CycleFilter,
    /// `Tab` completed the highlighted item. The caller reads the
    /// cursor and inserts the item text into the query
    /// (docs/tree-ui-design-from-human-phase-2.md item 5).
    Complete,
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
    /// Which pane has focus (docs/tree-ui-design-from-human-phase-2.md
    /// item 4). Toggled by `Ctrl+Shift+P` / `BackTab`.
    pub focus: Focus,
    /// The tree-list event-type filter (docs/tree-ui-design-from-
    /// human-phase-2.md item 3). Cycled by `Ctrl+F` in the
    /// `TreeList` stage; resets on stage exit and palette close.
    pub tree_filter: TreeFilter,
    /// The LRU-bounded cache of highlighted tree-pane bodies, keyed
    /// by the event's 1-based log seq (docs/tree-ui-design-from-
    /// human-phase-2.md item 2, the windowed-highlighting model of
    /// docs/tui-preview-pane-plan.md).
    pub tree_preview_cache: TreePreviewCache,
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
            focus: Focus::default(),
            tree_filter: TreeFilter::default(),
            tree_preview_cache: TreePreviewCache::default(),
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
        self.focus = Focus::default();
        self.tree_filter = TreeFilter::default();
        self.tree_preview_cache.clear();
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
        self.focus = Focus::default();
        self.tree_filter = TreeFilter::default();
        self.tree_preview_cache.clear();
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
        // The filter is owned by the tree stage; enter it fresh.
        self.tree_filter = TreeFilter::default();
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
        // The filter resets on stage exit
        // (docs/tree-ui-design-from-human-phase-2.md item 3).
        self.tree_filter = TreeFilter::default();
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

    /// Move the list cursor down by one. Wrap in the ring: at the
    /// last row the cursor jumps to the first
    /// (docs/tree-ui-design-from-human-phase-2.md item 4). If the
    /// highlighted item has options, move the option cursor instead.
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
        self.cursor = (self.cursor + 1) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Move the list cursor up by one. Wrap in the ring: at the
    /// first row the cursor jumps to the last
    /// (docs/tree-ui-design-from-human-phase-2.md item 4). If the
    /// highlighted item has options, move the option cursor instead.
    pub fn move_up(&mut self, count: usize, highlighted_options: usize) -> PaletteAction {
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
        if count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = (self.cursor + count - 1) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page down: move the cursor down by the visible window. A page
    /// that crosses the list end lands on the first row
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn page_down(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        let next = self.cursor + self.visible;
        self.cursor = if next >= count { 0 } else { next };
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page up: move the cursor up by the visible window. A page that
    /// crosses the list start lands on the last row
    /// (docs/tree-ui-design-from-human-phase-2.md item 4).
    pub fn page_up(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = if self.cursor >= self.visible {
            self.cursor - self.visible
        } else {
            count - 1
        };
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

    /// Half-page scroll of the focused entry list
    /// (docs/tree-ui-design-from-human-phase-2.md item 4): the step
    /// is half the visible rows, minimum one, wrapping in the ring
    /// like the step keys.
    pub fn scroll_list_down(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        let step = (self.visible / 2).max(1);
        self.cursor = (self.cursor + step) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Half-page scroll up of the focused entry list (the mirror of
    /// [`scroll_list_down`]).
    pub fn scroll_list_up(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        let step = (self.visible / 2).max(1);
        self.cursor = (self.cursor + count - step) % count;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Toggle focus between the entry list and the preview pane
    /// (docs/tree-ui-design-from-human-phase-2.md item 4). A no-op
    /// while the preview pane is hidden.
    pub fn toggle_focus(&mut self, count: usize, cutoff: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if !self.preview_visible(count, cutoff) {
            return PaletteAction::Nothing;
        }
        self.focus = match self.focus {
            Focus::List => Focus::Preview,
            Focus::Preview => Focus::List,
        };
        PaletteAction::ToggleFocus
    }

    /// Cycle the tree event-type filter
    /// (docs/tree-ui-design-from-human-phase-2.md item 3). Tree stage
    /// only; other stages leave the key unhandled.
    pub fn cycle_filter(&mut self) -> PaletteAction {
        if !self.open || self.stage != PaletteStage::TreeList {
            return PaletteAction::Nothing;
        }
        self.tree_filter = self.tree_filter.next();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::CycleFilter
    }

    /// `Tab` completion: replace the typed filter portion with `text`
    /// (the highlighted item's label) and reset the window
    /// (docs/tree-ui-design-from-human-phase-2.md item 5). The root
    /// stage replaces the whole query; a sub-stage keeps its goto
    /// prefix and replaces the filter typed after it. This mirrors
    /// the picker's `@<path>` replacement model. The palette stays
    /// open, so typing continues after the completion.
    pub fn apply_complete(&mut self, text: &str) {
        if !self.open {
            return;
        }
        let prefix_len = self.goto_prefix_len.min(self.query.len());
        let prefix = self.query[..prefix_len].to_string();
        self.query = if prefix.is_empty() {
            text.to_string()
        } else {
            format!("{prefix} {text}")
        };
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
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
            // Plain `j` / `k` fall through to the generic char arm
            // below: they type into the query
            // (docs/tree-ui-design-from-human-phase-2.md item 4).
            Key::CtrlJ | Key::Down => self.move_down(count, highlighted_options),
            Key::CtrlK | Key::Up => self.move_up(count, highlighted_options),
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
            // `Ctrl+Shift+P` toggles focus; `BackTab` is the legacy
            // fallback (docs/tree-ui-design-from-human-phase-2.md
            // item 4).
            Key::CtrlShiftP | Key::BackTab => self.toggle_focus(count, preview_cutoff),
            // `Ctrl+F` cycles the event-type filter in the tree stage
            // only (docs/tree-ui-design-from-human-phase-2.md item 3).
            Key::CtrlF => self.cycle_filter(),
            // `Tab` completes the highlighted item into the query.
            // The option list has no query to complete into, so it
            // is a no-op there (docs/tree-ui-design-from-human-
            // phase-2.md item 5).
            Key::Tab => match self.stage {
                PaletteStage::TreeOptions => PaletteAction::Nothing,
                _ => PaletteAction::Complete,
            },
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PaletteAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Key;

    /// An open palette with the default focus, filter, and pane.
    fn open() -> PaletteState {
        let mut s = PaletteState::new();
        s.open(10);
        s
    }

    // ── wrap (item 4) ─────────────────────────────────────────

    /// `Down` at the last row jumps to the first (ring wrap).
    #[test]
    fn move_down_wraps_from_last_to_first() {
        let mut s = open();
        s.cursor = 9;
        assert_eq!(s.move_down(10, 0), PaletteAction::Move);
        assert_eq!(s.cursor(), 0, "at the last row, down wraps to the first");
    }

    /// `Up` at the first row jumps to the last (ring wrap).
    #[test]
    fn move_up_wraps_from_first_to_last() {
        let mut s = open();
        assert_eq!(s.move_up(10, 0), PaletteAction::Move);
        assert_eq!(s.cursor(), 9, "at the first row, up wraps to the last");
    }

    /// `PgDn` at the last row lands on the first row.
    #[test]
    fn page_down_from_last_lands_on_first() {
        let mut s = open();
        s.cursor = 9;
        assert_eq!(s.page_down(10), PaletteAction::Move);
        assert_eq!(s.cursor(), 0);
    }

    /// `PgUp` at the first row lands on the last row.
    #[test]
    fn page_up_from_first_lands_on_last() {
        let mut s = open();
        assert_eq!(s.page_up(10), PaletteAction::Move);
        assert_eq!(s.cursor(), 9);
    }

    /// `Ctrl+U` / `Ctrl+D` on the focused list step half the visible
    /// rows and wrap in the ring like the step keys.
    #[test]
    fn half_page_scroll_wraps_like_the_step_keys() {
        let mut s = open();
        // The step is half the visible rows: 10 / 2 = 5.
        s.cursor = 4;
        assert_eq!(s.scroll_list_down(20), PaletteAction::Move);
        assert_eq!(s.cursor(), 9, "4 + 5");
        s.cursor = 9;
        assert_eq!(s.scroll_list_down(20), PaletteAction::Move);
        assert_eq!(s.cursor(), 14, "9 + 5, still in range");
        s.cursor = 15;
        assert_eq!(s.scroll_list_down(20), PaletteAction::Move);
        assert_eq!(s.cursor(), 0, "(15 + 5) % 20 wraps to the first");
        s.cursor = 4;
        assert_eq!(s.scroll_list_up(20), PaletteAction::Move);
        assert_eq!(s.cursor(), 19, "(4 + 20 - 5) % 20 wraps to the last");
    }

    /// Plain `j` / `k` type into the query instead of moving the
    /// cursor (item 4).
    #[test]
    fn plain_jk_type_into_the_query() {
        let mut s = open();
        assert_eq!(s.press(&Key::Char('j'), 5, 0, 5, 4), PaletteAction::Query);
        assert_eq!(s.query, "j");
        assert_eq!(s.cursor(), 0, "a query edit resets the cursor");
        assert_eq!(s.press(&Key::Char('k'), 5, 0, 5, 4), PaletteAction::Query);
        assert_eq!(s.query, "jk");
    }

    // ── focus (item 4) ────────────────────────────────────────

    /// The toggle is a no-op while the preview pane is hidden.
    #[test]
    fn focus_toggle_noop_when_preview_hidden() {
        let mut s = open();
        // Two results is below the cutoff of four: the pane hides.
        assert_eq!(s.toggle_focus(2, 4), PaletteAction::Nothing);
        assert_eq!(s.focus, Focus::List);
    }

    /// The toggle flips when the preview pane is visible.
    #[test]
    fn focus_toggle_flips_when_preview_visible() {
        let mut s = open();
        assert_eq!(s.toggle_focus(10, 4), PaletteAction::ToggleFocus);
        assert_eq!(s.focus, Focus::Preview);
        assert_eq!(s.toggle_focus(10, 4), PaletteAction::ToggleFocus);
        assert_eq!(s.focus, Focus::List);
    }

    /// `Ctrl+Shift+P` and the legacy `BackTab` fallback both reach
    /// the toggle through `press`.
    #[test]
    fn focus_toggle_keys_reach_the_state_machine() {
        let mut s = open();
        assert_eq!(s.press(&Key::CtrlShiftP, 10, 0, 5, 4), PaletteAction::ToggleFocus);
        assert_eq!(s.focus, Focus::Preview);
        assert_eq!(s.press(&Key::BackTab, 10, 0, 5, 4), PaletteAction::ToggleFocus);
        assert_eq!(s.focus, Focus::List);
    }

    /// `Ctrl+U` / `Ctrl+D` on the focused preview scroll the pane in
    /// `PREVIEW_PAGE` line steps, not the list.
    #[test]
    fn preview_focus_scroll_moves_the_pane() {
        let mut s = open();
        s.focus = Focus::Preview;
        s.preview_scroll = 10;
        assert_eq!(s.press(&Key::CtrlU, 10, 0, 5, 4), PaletteAction::ScrollPreview);
        assert_eq!(s.preview_scroll, 5, "a page up of five lines");
        assert_eq!(s.press(&Key::CtrlD, 10, 0, 5, 4), PaletteAction::ScrollPreview);
        assert_eq!(s.preview_scroll, 10, "a page down of five lines");
        assert_eq!(s.cursor(), 0, "the list cursor is untouched");
    }

    // ── filter (item 3) ───────────────────────────────────────

    /// `Ctrl+F` walks the five-state cycle: full → user → assistant
    /// → tool → user+assistant → full.
    #[test]
    fn filter_cycles_through_the_five_states() {
        let mut s = open();
        s.goto_tree_list();
        let expected = [
            TreeFilter::User,
            TreeFilter::Assistant,
            TreeFilter::Tool,
            TreeFilter::UserAssistant,
            TreeFilter::Full,
        ];
        for exp in expected {
            assert_eq!(s.cycle_filter(), PaletteAction::CycleFilter);
            assert_eq!(s.tree_filter, exp, "the cycle order");
        }
    }

    /// The filter keeps exactly its set of event kinds.
    #[test]
    fn filter_keeps_only_the_events_of_its_set() {
        use crate::event::EventKind;
        let user = EventKind::UserMessage;
        let retract = EventKind::UserMessageRetract;
        let assistant = EventKind::AssistantMessage;
        let call = EventKind::ToolCall;
        let result = EventKind::ToolResult;

        assert!(TreeFilter::Full.keeps(user));
        assert!(TreeFilter::Full.keeps(result));

        assert!(TreeFilter::User.keeps(user));
        assert!(TreeFilter::User.keeps(retract));
        assert!(!TreeFilter::User.keeps(assistant));
        assert!(!TreeFilter::User.keeps(call));

        assert!(TreeFilter::Assistant.keeps(assistant));
        assert!(!TreeFilter::Assistant.keeps(user));
        assert!(!TreeFilter::Assistant.keeps(result));

        assert!(TreeFilter::Tool.keeps(call));
        assert!(TreeFilter::Tool.keeps(result));
        assert!(!TreeFilter::Tool.keeps(user));

        assert!(TreeFilter::UserAssistant.keeps(user));
        assert!(TreeFilter::UserAssistant.keeps(retract));
        assert!(TreeFilter::UserAssistant.keeps(assistant));
        assert!(!TreeFilter::UserAssistant.keeps(call));
    }

    /// The filter resets on stage exit and on palette close.
    #[test]
    fn filter_resets_on_stage_exit_and_close() {
        let mut s = open();
        s.goto_tree_list();
        s.cycle_filter();
        assert_eq!(s.tree_filter, TreeFilter::User);
        s.drop_sub_stage();
        assert_eq!(s.tree_filter, TreeFilter::Full, "stage exit resets it");
        s.goto_tree_list();
        s.cycle_filter();
        s.close();
        assert_eq!(s.tree_filter, TreeFilter::Full, "close resets it");
    }

    /// `Ctrl+F` binds in the tree stage only. Other stages leave it
    /// unhandled.
    #[test]
    fn filter_key_binds_only_in_the_tree_stage() {
        let mut s = open();
        assert_eq!(
            s.press(&Key::CtrlF, 10, 0, 5, 4),
            PaletteAction::Nothing,
            "the root stage leaves it unhandled"
        );
        s.goto_session_list();
        assert_eq!(
            s.press(&Key::CtrlF, 10, 0, 5, 4),
            PaletteAction::Nothing,
            "the session sub-list is unaffected"
        );
        s.goto_tree_list();
        assert_eq!(s.press(&Key::CtrlF, 10, 0, 5, 4), PaletteAction::CycleFilter);
        assert_eq!(s.tree_filter, TreeFilter::User);
    }

    // ── completion (item 5) ───────────────────────────────────

    /// `Tab` completes the highlighted item into the query and keeps
    /// the palette open.
    #[test]
    fn tab_completes_the_item_into_the_query() {
        let mut s = open();
        assert_eq!(s.press(&Key::Tab, 10, 0, 5, 4), PaletteAction::Complete);
        s.apply_complete("toggle-tools");
        assert_eq!(s.query, "toggle-tools");
        assert!(s.open, "the window stays open");
        assert_eq!(s.cursor(), 0, "the window resets after the completion");
    }

    /// In the session sub-list, `Tab` inserts the full session name.
    #[test]
    fn tab_in_the_session_list_inserts_the_full_name() {
        let mut s = open();
        s.goto_session_list();
        assert_eq!(s.press(&Key::Tab, 3, 0, 5, 4), PaletteAction::Complete);
        s.apply_complete("browse-mode-issues");
        assert_eq!(s.query, "browse-mode-issues");
    }

    /// The completion replaces the typed filter text; it does not
    /// append to it (the picker's `@<path>` model, item 5).
    #[test]
    fn tab_replaces_the_typed_query_in_the_root_stage() {
        let mut s = open();
        s.query = "th".to_string();
        assert_eq!(s.press(&Key::Tab, 10, 0, 5, 4), PaletteAction::Complete);
        s.apply_complete("toggle-thinking");
        assert_eq!(s.query, "toggle-thinking", "the typed text is replaced");
    }

    /// In a sub-stage the goto prefix stays; only the filter typed
    /// after it is replaced by the completion.
    #[test]
    fn tab_replaces_the_filter_after_the_goto_prefix() {
        let mut s = open();
        s.query = "b".to_string();
        s.goto_session_list();
        s.query = "b br".to_string();
        assert_eq!(s.press(&Key::Tab, 3, 0, 5, 4), PaletteAction::Complete);
        s.apply_complete("browse-mode-issues");
        assert_eq!(s.query, "b browse-mode-issues");
    }

    /// Same model in the tree sub-stage: the prefix survives, the
    /// filter is replaced by the completed row label.
    #[test]
    fn tab_replaces_the_tree_filter_with_the_row_label() {
        let mut s = open();
        s.query = "tree".to_string();
        s.goto_tree_list();
        s.query = "tree sha".to_string();
        assert_eq!(s.press(&Key::Tab, 2, 0, 5, 4), PaletteAction::Complete);
        s.apply_complete("bash make -j4");
        assert_eq!(s.query, "tree bash make -j4");
    }

    /// In the `TreeOptions` stage, `Tab` is a no-op.
    #[test]
    fn tab_is_a_noop_in_tree_options() {
        let mut s = open();
        s.goto_tree_options(1);
        assert_eq!(s.press(&Key::Tab, 4, 0, 5, 4), PaletteAction::Nothing);
        assert!(s.query.is_empty());
    }

    // ── resets (item 4) ───────────────────────────────────────

    /// Open and close reset the focus and the filter.
    #[test]
    fn open_and_close_reset_focus_and_filter() {
        let mut s = open();
        s.tree_filter = TreeFilter::Tool;
        s.focus = Focus::Preview;
        s.close();
        assert_eq!(s.tree_filter, TreeFilter::Full);
        assert_eq!(s.focus, Focus::List);
        s.open(10);
        assert_eq!(s.tree_filter, TreeFilter::Full);
        assert_eq!(s.focus, Focus::List);
    }
}
