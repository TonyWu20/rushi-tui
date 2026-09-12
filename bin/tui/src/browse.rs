//! The conversation browse mode and the position bar
//! (docs/tui-conversation-browsing.md).
//!
//! `Browse` is the state machine behind the double-`s` overlay: the
//! cursor over the rendered transcript, the line-number gutter, the
//! scrolloff view following, and the stage-2 regex search. The host
//! routes keys through [`Browse::key`] while the mode holds
//! (`app.rs` owns the arm window and the host-role keys).
//!
//! The pure helpers here are the section-3 bar geometry, the section
//! 4.3 gutter numbering, and the section 4.5 scrolloff follow. They
//! take plain integers so the conformance tests (section 9) drive
//! them directly.

use std::collections::HashSet;

use base64::Engine;
use regex::Regex;

use crate::app::Key;

/// The `scrolloff` margin of the machine's neovim (section 6.1):
/// at least three lines above and below the cursor.
pub const SCROLLOFF: usize = 3;
/// The count cap of the key table (section 4.4).
pub const COUNT_CAP: u32 = 99_999;
/// The one-line status hint of the key table (section 4.4, the
/// section 11.4 growth: the select-and-yank rows).
pub const BROWSE_HINT: &str = "browse: v select, y yank, yy lines, yw word, b back, ss leave";

/// One search direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Forward,
    Backward,
}

/// A remembered view for the `jumpoptions = "stack,view"` restore
/// (section 7.3): the view state held before a forward jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedView {
    pub scroll: usize,
    pub line: usize,
    pub col: usize,
}

/// The open command line of the browse mode.
pub enum Typing {
    /// The `:N` goto line: digits plus an accepted `j` / `k` suffix.
    Goto { digits: String },
    /// The `/` or `?` pattern input (section 7.3).
    Search {
        dir: Dir,
        input: String,
        /// The cursor position when the search opened: `incsearch`
        /// jumps from here, not from the live cursor.
        origin: (usize, usize),
    },
}

/// The visual selection of browse mode (docs/tui-conversation-
/// browsing.md section 11.4): the anchor pins at the entry or a
/// swap (`o` / `O`); the active end is the cursor itself, and `y`
/// yanks the span between the two.
struct VisualSel {
    anchor: (usize, usize),
    /// A `V` entry (linewise visual): the span shades whole display
    /// rows and yanks whole lines.
    linewise: bool,
}

/// The outcome of one command-line key.
enum TypeKeyOutcome {
    /// Consumed, nothing to show.
    Handled,
    /// Consumed, with the hint to show (`line: 1..total`, the regex
    /// error).
    Hint(String),
    /// Closed the line: the key must act in the normal state.
    Pass,
}

/// The search state (section 7.3). The last valid compiled pattern
/// survives an invalid input; the `active` flag is the highlight bit
/// (an `Esc` clears it, leaving the pattern for a repeat).
pub struct Search {
    pattern: Option<String>,
    re: Option<Regex>,
    active: bool,
    forward: bool,
    /// The current match, in transcript line coordinates.
    active_match: Option<(usize, usize)>,
    /// The match-line cache for the renderer, with the total and the
    /// pattern version it was built from.
    hl: HashSet<usize>,
    hl_total: usize,
    hl_version: u64,
    re_version: u64,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            pattern: None,
            re: None,
            active: false,
            forward: true,
            active_match: None,
            hl: HashSet::new(),
            hl_total: 0,
            hl_version: 0,
            re_version: 0,
        }
    }
}

/// The browse state machine (section 4). The cursor is `(line, col)`:
/// `line` is a zero-based transcript line index, `col` a zero-based
/// character position within that line. Every move clamps `col` to
/// the line length (section 4.1).

pub struct Browse {
    active: bool,
    /// Entry is pending a render pass: the cursor lands on the first
    /// visible line, col 0, and the view does not move (section 4.2).
    pending_entry: bool,
    line: usize,
    col: usize,
    /// The count prefix of the next motion, capped at [`COUNT_CAP`].
    /// A bare motion (no count typed) counts as 1; a typed `0`
    /// counts as a no-op motion (section 4.4).
    pending: u32,
    /// A count digit was typed: `0<key>` is then a no-op motion,
    /// while a bare key counts as 1.
    has_count: bool,
    /// A `g` awaiting its second `g`.
    pending_g: bool,
    typing: Option<Typing>,
    search: Search,
    /// The last search move, for the `N` view restore (section 7.3).
    last_move: Option<Dir>,
    saved_view: Option<SavedView>,
    last_total: usize,
    last_h: usize,
    /// The visual selection (section 11.4): `None` outside visual /
    /// linewise visual.
    visual: Option<VisualSel>,
    /// The register target of the next yank (section 11.4): the
    /// unnamed `"` by default; the `"` prefix retargets it.
    yank_reg: char,
    /// Awaiting the register char after the `"` key.
    reg_prefix: bool,
    /// The pending `y` operator of the normal state (section 11.4):
    /// a motion or a text object follows.
    yank_pending: bool,
    /// The count captured when the `y` opened (`3y` opens with 3):
    /// the doubled form multiplies it into the line count (the
    /// editor's `pending_operator_count` rule).
    yank_op_count: u32,
    /// The pending text object prefix (`i` / `a` after `y`).
    obj_prefix: Option<char>,
    /// The OSC 52 host-clipboard write of the last yank that reached
    /// the host clipboard (section 11.3): the full escape, drained
    /// by the host before the next frame.
    host_clipboard: Option<String>,
    /// The `[tui] clipboard = "unnamed"` flag (section 11.3): a bare
    /// unnamed yank also reaches the host clipboard.
    clipboard_unnamed: bool,
}

impl Default for Browse {
    fn default() -> Self {
        Self {
            active: false,
            pending_entry: false,
            line: 0,
            col: 0,
            pending: 0,
            has_count: false,
            pending_g: false,
            typing: None,
            search: Search::default(),
            last_move: None,
            saved_view: None,
            last_total: 0,
            last_h: 0,
            visual: None,
            yank_reg: '"',
            reg_prefix: false,
            yank_pending: false,
            yank_op_count: 0,
            obj_prefix: None,
            host_clipboard: None,
            clipboard_unnamed: false,
        }
    }
}

/// The view the browse handler moves: the rendered transcript, its
/// height, the scroll offset, and the half-page distance.
pub struct View<'a> {
    /// The total rendered transcript lines.
    pub total: usize,
    /// The visible view height.
    pub h: usize,
    /// The scroll offset, written by the view motions.
    pub scroll: &'a mut usize,
    /// The half-page scroll distance of `Ctrl+U` / `Ctrl+D`.
    pub half: usize,
    /// The rendered transcript line texts, oldest first. Search runs
    /// over these; the renderer builds them each frame.
    pub texts: &'a [String],
    /// The per-line raw source text (section 11.3): `line_raw[i]` is
    /// the shareable source for rendered line `i` (`None` on
    /// separators, UI chrome, and wrapped continuations). A yank
    /// collects the non-`None` entries in the range.
    /// Empty when unavailable (synthetic test layouts).
    pub line_raw: &'a [Option<String>],
}

impl Browse {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// The cursor in transcript coordinates, `(line, col)`.
    pub fn line_col(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    /// The active visual selection (section 11.4, the renderer's
    /// input): `(anchor, active_end, linewise)`, transcript
    /// coordinates. The active end is the cursor; `None` outside
    /// visual / linewise visual.
    pub fn visual_selection(&self) -> Option<((usize, usize), (usize, usize), bool)> {
        self.visual
            .as_ref()
            .map(|s| (s.anchor, (self.line, self.col), s.linewise))
    }

    /// The `[tui] clipboard = "unnamed"` flag (section 11.3): set at
    /// config load; a bare unnamed yank then also emits the OSC 52
    /// host-clipboard write.
    pub fn set_clipboard_unnamed(&mut self, on: bool) {
        self.clipboard_unnamed = on;
    }

    /// The OSC 52 escape of the last yank that reached the host
    /// clipboard (section 11.3), drained by the host before the next
    /// frame. The yank itself always lands in the in-memory register
    /// store; this is only the terminal side effect.
    pub fn take_host_clipboard(&mut self) -> Option<String> {
        self.host_clipboard.take()
    }

    /// The command-line prompt for the input box title
    /// (`:42`, `/pat`, `?pat`), `None` outside the command line.
    pub fn prompt(&self) -> Option<String> {
        match &self.typing {
            Some(Typing::Goto { digits }) => Some(format!(":{digits}")),
            Some(Typing::Search { dir, input, .. }) => {
                let p = match dir {
                    Dir::Forward => '/',
                    Dir::Backward => '?',
                };
                Some(format!("{p}{input}"))
            }
            None => None,
        }
    }

    /// The highlight state for the renderer: the match-line cache
    /// refreshed against the current total, and the accent line.
    /// The cache is one pass over the texts per total or pattern
    /// change, not per frame.
    pub fn highlight_lines(
        &mut self,
        total: usize,
        texts: &[String],
    ) -> (&HashSet<usize>, Option<(usize, usize)>) {
        if self.search.active
            && (self.search.hl_total != total || self.search.hl_version != self.search.re_version)
        {
            self.search.hl = self.match_lines(texts);
            self.search.hl_total = total;
            self.search.hl_version = self.search.re_version;
        }
        (
            &self.search.hl,
            self.search.active_match.filter(|_| self.search.active),
        )
    }

    /// The active match position (the accent line), if any. Test
    /// only: the renderer reads it through `App::browse_highlight`.
    #[allow(dead_code)]
    pub fn active_match(&self) -> Option<(usize, usize)> {
        self.search.active_match.filter(|_| self.search.active)
    }

    /// The last valid pattern, redisplayed by the host. Test only:
    /// the conformance rows of section 9 read it.
    #[allow(dead_code)]
    pub fn last_pattern(&self) -> Option<&str> {
        self.search.pattern.as_deref()
    }

    /// The command line is open: the hardware caret parks on the
    /// prompt, like the editor's search line.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    // ── entry and exit (section 4.2) ─────────────────────────────

    /// Enter browse mode. The cursor resolves to the first visible
    /// line at the next [`Browse::sync`] (the view does not move).
    pub fn enter(&mut self) {
        self.active = true;
        self.pending_entry = true;
        self.line = 0;
        self.col = 0;
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.typing = None;
        self.search.active = false;
        self.search.active_match = None;
        self.last_move = None;
        self.saved_view = None;
        self.visual = None;
        self.yank_reg = '"';
        self.reg_prefix = false;
        self.yank_pending = false;
        self.yank_op_count = 0;
        self.obj_prefix = None;
    }

    /// Leave browse mode: the view stays where browse put it
    /// (section 4.2), and the highlight drops (section 7.3).
    pub fn exit(&mut self) {
        self.active = false;
        self.pending_entry = false;
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.typing = None;
        self.search.active = false;
        self.search.active_match = None;
        self.last_move = None;
        self.saved_view = None;
        // The stage-3 state never survives leaving the mode (section
        // 11.8): the selection and the pending yank operator clear;
        // the register store lives on the host (section 11.3).
        self.visual = None;
        self.yank_reg = '"';
        self.reg_prefix = false;
        self.yank_pending = false;
        self.yank_op_count = 0;
        self.obj_prefix = None;
    }

    /// A session switch resets the browse state with the scroll
    /// (section 4.7): everything, including the last pattern.
    pub fn reset(&mut self, scroll: &mut usize) {
        *scroll = 0;
        self.exit();
        self.search.pattern = None;
        self.search.re = None;
        self.search.re_version = 0;
        self.search.hl.clear();
        self.last_total = 0;
        self.last_h = 0;
    }

    /// The renderer's layout sync (sections 4.6 and 4.7): when the
    /// total or the height changes under a held cursor, the cursor
    /// clamps to the new total and the view re-centers on it with
    /// the scrolloff margins. A pending entry resolves here instead.
    /// A pure tail growth (a rendered event landed, `grew = true`)
    /// keeps the view put: no auto-follow (section 4.6). A growth
    /// without a new event is a pane rewrap and re-centers
    /// (section 4.7).
    pub fn sync(&mut self, total: usize, h: usize, scroll: &mut usize, grew: bool) {
        let changed = total != self.last_total || h != self.last_h;
        let pure_growth = total > self.last_total && grew && h == self.last_h;
        self.last_total = total;
        self.last_h = h;
        if total == 0 {
            self.line = 0;
            self.col = 0;
            *scroll = 0;
            self.pending_entry = false;
            return;
        }
        // The cursor pins to its line number (section 4.6): a cap
        // drop shifts the lines, the number keeps pointing, the
        // fold / thinking toggle clamps to the new total.
        let clamped = self.line >= total;
        self.line = self.line.min(total - 1);
        if self.pending_entry {
            // Entry: the cursor is the first visible line, col 0,
            // and the view does not move (section 4.2).
            let start = total.saturating_sub(*scroll + h);
            *scroll = (*scroll).min(total.saturating_sub(h));
            self.line = start.min(total - 1);
            self.col = 0;
            self.pending_entry = false;
        } else if changed && (!pure_growth || clamped) {
            // A shrink, a height change, a rewrap growth, or a
            // clamped cursor re-centers with the scrolloff margins
            // (section 4.7).
            *scroll = follow_view(total, h, self.line, *scroll);
        }
    }

    /// Clamp the cursor column to the rendered line length
    /// (section 4.1). The col may sit one past the last character
    /// (the blank block cell at line end).
    pub fn clamp_col(&mut self, line_len: usize) {
        self.col = self.col.min(line_len);
    }

    // ── the key table (sections 4.4 and 7.3) ─────────────────────

    /// One browse-owned key. Returns the flash hint to show, if any.
    /// The host keys (`Ctrl+C` / `Ctrl+R` / the toggles / `Tab` /
    /// the quit gate) never reach here: the host routes them
    /// (`app.rs`), and the double-`s` exit arm is the host's too.
    /// The `registers` store is the shared one from `App` (section
    /// 11.3): a `y` here lands where the editor's `p` reads.
    pub fn key(
        &mut self,
        key: Key,
        v: &mut View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
    ) -> Option<String> {
        if !self.active {
            return None;
        }
        if let Some(t) = self.typing.take() {
            match self.typing_key(key, t, v) {
                TypeKeyOutcome::Handled => return None,
                TypeKeyOutcome::Hint(h) => return Some(h),
                // A structural key closed the line: act on it in the
                // normal state, with the count cleared.
                TypeKeyOutcome::Pass => {
                    self.pending = 0;
                    return self.normal_key(key, v, registers);
                }
            }
        }
        self.normal_key(key, v, registers)
    }

    /// One key while the command line is open.
    fn typing_key(&mut self, key: Key, t: Typing, v: &mut View) -> TypeKeyOutcome {
        match t {
            Typing::Goto { mut digits } => match key {
                Key::Char(c) if c.is_ascii_digit() => {
                    digits.push(c);
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                // The `j` / `k` suffix is accepted and ignored: the
                // ex form already names the line (section 4.4).
                Key::Char('j') | Key::Char('k') => {
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                // Anything but a line number: no move, the hint
                // names the range (section 4.4).
                Key::Char(_) => {
                    self.typing = None;
                    TypeKeyOutcome::Hint(Self::goto_hint(v.total))
                }
                Key::Enter => {
                    // A `u64` parse keeps a huge `:N` clamping to
                    // the last line instead of reading as 0.
                    let n: u64 = digits.trim().parse().unwrap_or(u64::MAX);
                    self.typing = None;
                    if n == 0 || v.total == 0 {
                        return TypeKeyOutcome::Hint(Self::goto_hint(v.total));
                    }
                    self.line = (n as usize - 1).min(v.total - 1);
                    let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                    self.col = self.col.min(len);
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                    TypeKeyOutcome::Handled
                }
                Key::Backspace => {
                    digits.pop();
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                Key::Esc => {
                    // Esc cancels the line and clears the active
                    // highlight; it never leaves browse.
                    self.search.active = false;
                    self.search.active_match = None;
                    TypeKeyOutcome::Handled
                }
                // Any other key closes the line and acts.
                _ => TypeKeyOutcome::Pass,
            },
            Typing::Search { dir, mut input, origin } => match key {
                Key::Char(c) => {
                    input.push(c);
                    self.search.forward = dir == Dir::Forward;
                    // `incsearch`: each keystroke re-matches, and the
                    // cursor jumps to the first match in the search
                    // direction (section 7.3).
                    let outcome = match compile_smart(&input) {
                        Ok(re) => {
                            self.search.re = Some(re);
                            self.search.re_version += 1;
                            self.search.pattern = Some(input.clone());
                            self.search.active = true;
                            if let Some(m) = next_match(
                                v.texts,
                                self.search.re.as_ref().unwrap(),
                                origin,
                                dir,
                            ) {
                                self.search.active_match = Some(m);
                                self.line = m.0;
                                self.col = m.1;
                                *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                            }
                            TypeKeyOutcome::Handled
                        }
                        Err(e) => {
                            // An invalid pattern keeps the last valid
                            // one; the command line hints the error
                            // (section 7.3).
                            TypeKeyOutcome::Hint(format!("regex: {e}"))
                        }
                    };
                    self.typing = Some(Typing::Search { dir, input, origin });
                    outcome
                }
                Key::Backspace => {
                    if input.pop().is_none() {
                        // A backspace on the empty input cancels.
                        return TypeKeyOutcome::Handled;
                    }
                    self.typing = Some(Typing::Search { dir, input, origin });
                    TypeKeyOutcome::Handled
                }
                Key::Enter => {
                    // Commit: the pattern is live, and the view
                    // centers on the match (the machine's `zz`).
                    if input.is_empty() {
                        return TypeKeyOutcome::Handled;
                    }
                    if let Ok(re) = compile_smart(&input) {
                        self.search.re = Some(re);
                        self.search.re_version += 1;
                        self.search.pattern = Some(input);
                        self.search.active = true;
                        self.last_move = Some(dir);
                        // The view state before the jump is the
                        // restore point of a backward `N`.
                        self.saved_view = Some(SavedView {
                            scroll: *v.scroll,
                            line: self.line,
                            col: self.col,
                        });
                        if let Some(m) = next_match(v.texts, self.search.re.as_ref().unwrap(), origin, dir) {
                            self.search.active_match = Some(m);
                            self.line = m.0;
                            self.col = m.1;
                            center_view(v, self.line);
                        }
                    }
                    TypeKeyOutcome::Handled
                }
                Key::Esc => {
                    // Esc clears the active highlight (section 6.2).
                    self.search.active = false;
                    self.search.active_match = None;
                    TypeKeyOutcome::Handled
                }
                _ => TypeKeyOutcome::Pass,
            },
        }
    }

    /// The `:N` command-line hint (section 4.4): the valid range.
    fn goto_hint(total: usize) -> String {
        format!("line: 1..{total}")
    }

    /// The motion count of the pending digits: a bare motion counts
    /// as 1, a typed `0` counts as a no-op (section 4.4). Clears the
    /// count either way.
    fn motion_count(&mut self) -> u32 {
        let n = if self.has_count { self.pending } else { 1 };
        self.pending = 0;
        self.has_count = false;
        n
    }

    // ── the select-and-yank plumbing (section 11) ─────────────────

    /// Clear the pending yank operator state (the editor's
    /// `resetOperatorState`, the yank subset).
    fn cancel_yank(&mut self) {
        self.yank_pending = false;
        self.yank_op_count = 0;
        self.obj_prefix = None;
    }

    /// The completed yank: the range text to the target register
    /// (the editor's `yank_to_register` merge rule, section 11.3),
    /// and the OSC 52 host-clipboard write when the target reaches
    /// the host clipboard (the `+` / `*` register, or the unnamed
    /// one under `[tui] clipboard = "unnamed"`). The operator state
    /// clears either way.
    fn complete_yank(
        &mut self,
        v: &View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
        range: &crate::vim_editor::OpRange,
    ) {
        // The raw source text is preferred (section 11.3): it is the
        // shareable markdown / command / output, not the rendered
        // display lines. The line extent of the range maps to the
        // events it covers; their raw texts are joined. When the view
        // carries no raw mapping (a synthetic test layout) or the
        // covered events have no shareable body, fall back to the
        // rendered text.
        let lo = range.start.0.min(range.end.0);
        let hi = range.start.0.max(range.end.0);
        let text = match Self::raw_yank_text(v, lo, hi) {
            Some(raw) => raw,
            None => crate::vim_editor::extract_text(v.texts, range),
        };
        let reg = self.yank_reg;
        crate::vim_editor::yank_to_register(registers, reg, &text, range.linewise);
        if reg == '+' || reg == '*' || (reg == '"' && self.clipboard_unnamed) {
            self.host_clipboard = Some(host_clipboard_escape(&text));
        }
        self.cancel_yank();
        self.yank_reg = '"';
    }

    /// The raw source text of the transcript line range `[lo, hi]`
    /// (section 11.3): each rendered line carries its own shareable
    /// source fragment (`line_raw`), so a word / line / partial-line
    /// yank returns only the selected portion, never the whole event.
    /// `None` when the view carries no raw mapping (synthetic test
    /// layouts) or the covered lines have no shareable body.
    fn raw_yank_text(v: &View, lo: usize, hi: usize) -> Option<String> {
        if v.line_raw.is_empty() {
            return None;
        }
        let hi = hi.min(v.line_raw.len().saturating_sub(1));
        if lo > hi {
            return None;
        }
        let raw: String = v.line_raw[lo..=hi]
            .iter()
            .filter_map(|o| o.as_deref())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if raw.is_empty() {
            None
        } else {
            Some(raw)
        }
    }

    /// The yank motion keys (section 11.4): `y` + `j` / `k` / `h` /
    /// `l` / `w` / `0` / `^` / `$` / `G`. The motions are the
    /// editor's pi-vim primitives over the rendered lines
    /// (section 11.5): `j` / `k` / `G` are the linewise forms, the
    /// rest the char forms; `w` carries the `extend_w_eol`
    /// extension; the operator count multiplies the motion count
    /// (the editor `motion_count` rule).
    fn yank_motion(
        &mut self,
        m: char,
        v: &View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
    ) {
        let count_explicit = self.has_count;
        let motion_count = self.yank_op_count.saturating_mul(self.motion_count());
        if motion_count == 0 {
            // A typed count of 0 is a no-op (section 4.4).
            self.cancel_yank();
            return;
        }
        let cursor = (self.line.min(v.total.saturating_sub(1)), self.col);
        let range = browse_motion_range(v.texts, cursor, m, motion_count, count_explicit);
        if let Some(range) = range {
            self.complete_yank(v, registers, &range);
            // The cursor is the motion target (the range end, ordered
            // start <= end): `yw` lands at the word end, `y$` at the
            // line end (section 11.4). A linewise yank parks at col 0.
            self.line = range.end.0;
            self.col = if range.linewise { 0 } else { range.end.1 };
        }
    }

    /// The doubled operator (`yy` / `Y` / `<n>yy`): `n` whole lines
    /// from the cursor line (the editor's `applyLinewiseOperator`).
    fn yank_linewise(
        &mut self,
        v: &View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
    ) {
        let n = (self.yank_op_count.saturating_mul(self.motion_count())).max(1) as usize;
        let last = v.total.saturating_sub(1);
        let end_line = (self.line + n - 1).min(last);
        let end_len = v.texts.get(end_line).map(|t| line_len(t)).unwrap_or(0);
        let range = crate::vim_editor::OpRange {
            start: (self.line, 0),
            end: (end_line, end_len),
            linewise: true,
            inclusive: true,
        };
        self.complete_yank(v, registers, &range);
        self.col = 0;
    }

    /// The visual yank range (section 11.4, the section 11.9 visual
    /// yank row): a linewise selection — or a char selection that
    /// spans lines — yanks whole lines (the linewise join); a
    /// single-line char visual yanks the char span, inclusive.
    fn visual_yank_range(
        anchor: (usize, usize),
        end: (usize, usize),
        linewise: bool,
    ) -> crate::vim_editor::OpRange {
        let lo = anchor.0.min(end.0);
        let hi = anchor.0.max(end.0);
        if linewise || lo != hi {
            crate::vim_editor::OpRange {
                start: (lo, 0),
                end: (hi, 0),
                linewise: true,
                inclusive: true,
            }
        } else {
            let cs = anchor.1.min(end.1);
            let ce = anchor.1.max(end.1);
            crate::vim_editor::OpRange {
                start: (lo, cs),
                end: (hi, ce),
                linewise: false,
                inclusive: true,
            }
        }
    }

    /// One key in the browse normal state (the command line closed).
    fn normal_key(
        &mut self,
        key: Key,
        v: &mut View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
    ) -> Option<String> {
        match key {
            Key::Char(c) => self.char_key(c, v, registers),
            Key::Esc => {
                // A visual cancel (section 11.4): no register write,
                // the cursor returns to the anchor. Then the section
                // 4.4 Esc role: cancel a pending count, clear the
                // active search highlight; never leave browse mode.
                if let Some(sel) = self.visual.take() {
                    self.line = sel.anchor.0;
                    self.col = sel.anchor.1;
                }
                self.pending = 0;
                self.has_count = false;
                self.pending_g = false;
                self.yank_pending = false;
                self.yank_op_count = 0;
                self.obj_prefix = None;
                self.reg_prefix = false;
                self.search.active = false;
                self.search.active_match = None;
                None
            }
            Key::CtrlU => {
                // Half a page up; the stranded cursor lands on the
                // new top line (section 4.4).
                self.view_move(v, true, v.half);
                None
            }
            Key::CtrlD => {
                self.view_move(v, false, v.half);
                None
            }
            Key::Wheel(d) => {
                // The wheel moves the view first (three lines, the
                // host distance); the stranded cursor follows.
                self.view_move(v, d < 0, 3);
                None
            }
            // The unbound keys: a no-op, the status hint names the
            // one line of the key table (section 4.4).
            _ => Some(BROWSE_HINT.to_string()),
        }
    }

    /// The character keys of the browse state: the section 4.4
    /// motions, the section 11.4 select-and-yank rows, and the
    /// operator-pending / visual sub-states.
    fn char_key(
        &mut self,
        c: char,
        v: &mut View,
        registers: &mut std::collections::HashMap<char, crate::vim_editor::RegContent>,
    ) -> Option<String> {
        // The `"` register prefix (the editor rule, section 11.4):
        // the next key retargets the yank; an invalid register char
        // cancels and re-dispatches the key.
        if self.reg_prefix {
            self.reg_prefix = false;
            if crate::vim_editor::is_valid_register(c) {
                self.yank_reg = c;
                return None;
            }
        }
        // The pending text object key (after `i` / `a`): the object
        // char completes the yank; an unmatched object is a no-op
        // with the hint that names it (section 11.8); an unrelated
        // key cancels the operator and re-dispatches (the vim rule:
        // `yij` cancels the yank and moves the cursor).
        if let Some(prefix) = self.obj_prefix.take() {
            if let Some(obj) = crate::vim_editor::resolve_text_object(prefix, c) {
                let cursor = (self.line, self.col);
                match obj(v.texts, cursor) {
                    Some(range) => {
                        self.complete_yank(
                            v,
                            registers,
                            &crate::vim_editor::text_object_to_range(&range),
                        );
                        return None;
                    }
                    None => {
                        self.cancel_yank();
                        return Some(format!("unmatched {prefix}{c} text object"));
                    }
                }
            }
            // Not an object char: cancel the pending yank and fall
            // through to the key's normal browse role below.
            self.cancel_yank();
        }
        // The pending `y` operator (section 11.4): the key is a
        // motion, the doubled `y` / `Y`, a text-object prefix, the
        // `"` register retarget, or the motion count digits (the
        // vim `y3w` rule).
        if self.yank_pending {
            match c {
                'y' | 'Y' => {
                    self.yank_linewise(v, registers);
                    return None;
                }
                c @ 'i' | c @ 'a' => {
                    self.obj_prefix = Some(c);
                    return None;
                }
                '"' => {
                    // The register retarget mid-operator (the editor
                    // rule): the next key names the register; the
                    // operator stays pending.
                    self.reg_prefix = true;
                    return None;
                }
                // The motion count digits between the operator and
                // the motion (the vim `y3w` rule): a `0` is a count
                // digit only once a count has started; a bare `0`
                // is the line-start motion below.
                d @ '0'..='9' if d != '0' || self.has_count => {
                    self.pending =
                        (self.pending * 10 + (d as u32 - '0' as u32)).min(COUNT_CAP);
                    self.has_count = true;
                    return None;
                }
                m @ 'j' | m @ 'k' | m @ 'h' | m @ 'l' | m @ 'w' | m @ 'b'
                | m @ '0' | m @ '^' | m @ '$' | m @ 'G' => {
                    self.yank_motion(m, v, registers);
                    return None;
                }
                _ => {
                    // An unrelated key cancels the operator and keeps
                    // its browse role below.
                    self.cancel_yank();
                }
            }
        }
        if c.is_ascii_digit() {
            // Counts prefix the motions, capped at 99999. A count of
            // 0 is a no-op motion (section 4.4).
            self.pending = (self.pending * 10 + (c as u32 - '0' as u32)).min(COUNT_CAP);
            self.has_count = true;
            return None;
        }
        match c {
            'j' => {
                let n = self.motion_count() as usize;
                if n > 0 && v.total > 0 {
                    self.line = (self.line + n).min(v.total - 1);
                    self.clamp_to_line(v);
                    // Linewise visual parks at col 0 of the new line
                    // (the vim `V` movement).
                    if matches!(self.visual, Some(VisualSel { linewise: true, .. })) {
                        self.col = 0;
                    }
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                }
                None
            }
            'k' => {
                let n = self.motion_count() as usize;
                if n > 0 && v.total > 0 {
                    self.line = self.line.saturating_sub(n);
                    self.clamp_to_line(v);
                    if matches!(self.visual, Some(VisualSel { linewise: true, .. })) {
                        self.col = 0;
                    }
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                }
                None
            }
            'h' => {
                let n = self.motion_count() as usize;
                if n > 0 {
                    self.col = self.col.saturating_sub(n);
                }
                None
            }
            'l' => {
                let n = self.motion_count() as usize;
                if n > 0 {
                    // The ceiling is the line end: the col may rest
                    // one past the last character (section 4.1).
                    let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                    self.col = (self.col + n).min(len);
                }
                None
            }
            'w' => {
                // The word motion (section 11.4): it extends the
                // visual selection and the `y` operator; a bare `w`
                // moves the cursor to the next word start.
                let n = self.motion_count() as usize;
                if n > 0 && !v.texts.is_empty() {
                    let cursor = (self.line.min(v.total - 1), self.col);
                    let res = crate::vim_editor::word_forward(v.texts, cursor, n.max(1) as u32);
                    self.line = res.pos.0;
                    self.col = res.pos.1;
                    self.clamp_to_line(v);
                    // Linewise visual parks at col 0 of the new line
                    // (the vim `V` movement).
                    if matches!(self.visual, Some(VisualSel { linewise: true, .. })) {
                        self.col = 0;
                    }
                }
                None
            }
            'b' => {
                // The word-backward motion (the mirror of `w`): it
                // extends the visual selection and the `y` operator;
                // a bare `b` moves the cursor to the previous word
                // start.
                let n = self.motion_count() as usize;
                if n > 0 && !v.texts.is_empty() {
                    let cursor = (self.line.min(v.total - 1), self.col);
                    let res = crate::vim_editor::word_backward(v.texts, cursor, n.max(1) as u32);
                    self.line = res.pos.0;
                    self.col = res.pos.1;
                    self.clamp_to_line(v);
                    // Linewise visual parks at col 0 of the new line.
                    if matches!(self.visual, Some(VisualSel { linewise: true, .. })) {
                        self.col = 0;
                    }
                }
                None
            }
            '0' | '^' => {
                // The line start / the first non-blank char (section
                // 11.4): the cursor move that extends the visual
                // selection and the `y` operator.
                if v.total > 0 {
                    if c == '0' {
                        self.col = 0;
                    } else {
                        let res =
                            crate::vim_editor::first_nonblank_motion(v.texts, (self.line, self.col), 1);
                        let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                        self.col = res.pos.1.min(len);
                    }
                }
                None
            }
            '$' => {
                // The line end (section 11.4): the inclusive col,
                // one past the last char at most (section 4.1).
                let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                self.col = len;
                None
            }
            'g' => {
                if self.pending_g {
                    // `gg`: the cursor to line 1, the view to the
                    // top; a count takes line n (section 4.4).
                    let n = self.motion_count() as usize;
                    self.pending_g = false;
                    if v.total > 0 && n > 0 {
                        self.line = (n - 1).min(v.total - 1);
                        self.clamp_to_line(v);
                        // Seed the view at the top; the scrolloff
                        // margins apply (section 4.5).
                        *v.scroll = follow_view(
                            v.total,
                            v.h,
                            self.line,
                            v.total.saturating_sub(v.h),
                        );
                    }
                } else {
                    self.pending_g = true;
                }
                None
            }
            'G' => {
                // The cursor to the last line, the view to the tail;
                // a count takes line n (section 4.4). A typed `0`
                // is a no-op motion.
                let n = self.pending as usize;
                let has = self.has_count;
                self.pending = 0;
                self.has_count = false;
                if v.total > 0 {
                    if has && n > 0 {
                        self.line = (n - 1).min(v.total - 1);
                    } else if !has {
                        self.line = v.total - 1;
                    }
                    if !has || n > 0 {
                        self.clamp_to_line(v);
                        *v.scroll = follow_view(v.total, v.h, self.line, 0);
                    }
                }
                None
            }
            ':' => {
                // The ex form goto line (section 4.4).
                self.pending = 0;
                self.has_count = false;
                self.pending_g = false;
                self.typing = Some(Typing::Goto { digits: String::new() });
                None
            }
            '/' => {
                self.open_search(v, Dir::Forward);
                None
            }
            '?' => {
                self.open_search(v, Dir::Backward);
                None
            }
            'n' => {
                self.search_step(v, Dir::Forward);
                None
            }
            'N' => {
                self.search_step(v, Dir::Backward);
                None
            }
            '*' => {
                self.search_word(v, Dir::Forward);
                None
            }
            '#' => {
                self.search_word(v, Dir::Backward);
                None
            }
            // ── the select-and-yank rows (section 11.4) ──
            '"' => {
                // The register prefix: the next key is the register
                // the next yank targets (the editor rule).
                self.reg_prefix = true;
                None
            }
            'v' => {
                // Char-visual entry; in visual or linewise visual it
                // toggles back to normal (the vim `v` toggle).
                if self.visual.is_some() {
                    self.visual = None;
                } else {
                    self.visual = Some(VisualSel {
                        anchor: (self.line, self.col),
                        linewise: false,
                    });
                }
                None
            }
            'V' => {
                // Linewise visual entry; the anchor holds the
                // cursor line; toggles out of visual the same way.
                if self.visual.is_some() {
                    self.visual = None;
                } else {
                    self.visual = Some(VisualSel {
                        anchor: (self.line, 0),
                        linewise: true,
                    });
                    self.col = 0;
                }
                None
            }
            'o' | 'O' => {
                // The anchor/active-end swap: the selection inverts
                // around the cursor (the vim `o` / `O`).
                if let Some(sel) = self.visual.as_mut() {
                    let (l, c) = std::mem::replace(
                        &mut sel.anchor,
                        (self.line, self.col),
                    );
                    self.line = l;
                    self.col = c;
                    if sel.linewise {
                        self.col = 0;
                    }
                }
                None
            }
            'y' => {
                if let Some(sel) = self.visual.take() {
                    // The visual yank (section 11.4): the selection
                    // to the register; leave visual; the cursor is
                    // the selection end (the active end).
                    let end = (self.line, self.col);
                    let range = Self::visual_yank_range(sel.anchor, end, sel.linewise);
                    self.complete_yank(v, registers, &range);
                    // The cursor is the selection end; a linewise or
                    // multi-line span parks at col 0.
                    self.col = if range.linewise { 0 } else { range.end.1 };
                } else {
                    // Open the yank operator (section 11.4): the
                    // count typed before the `y` is the operator
                    // count (`3y...`; a bare `y` is 1), and a
                    // motion or a text object follows.
                    self.yank_pending = true;
                    self.yank_op_count = if self.has_count {
                        self.pending.max(1)
                    } else {
                        1
                    };
                    // The operator takes the count slot: the motion
                    // count starts fresh (the editor's `y` rule).
                    self.pending = 0;
                    self.has_count = false;
                }
                None
            }
            _ => Some(BROWSE_HINT.to_string()),
        }
    }

    /// Clamps the cursor col to the current line's rendered length.
    fn clamp_to_line(&mut self, v: &View) {
        let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
        self.col = self.col.min(len);
    }

    /// The view scroll with the stranded-cursor rule (section 4.4
    /// and 4.5): the view moves first. A cursor at the moving edge
    /// band, or stranded outside the new view, jumps to the new
    /// edge line (the top for a scroll up, the bottom for a scroll
    /// down).
    fn view_move(&mut self, v: &mut View, up: bool, dist: usize) {
        if v.total == 0 {
            return;
        }
        let old_start = v.total.saturating_sub(*v.scroll + v.h);
        if up {
            *v.scroll = v.scroll.saturating_add(dist);
        } else {
            *v.scroll = v.scroll.saturating_sub(dist);
        }
        let start = v.total.saturating_sub(*v.scroll + v.h).min(v.total.saturating_sub(v.h));
        let bottom = start.saturating_add(v.h - 1).min(v.total - 1);
        if up {
            // The top edge moved up: a cursor at the old top band,
            // or stranded below the new view, lands on the new top.
            if self.line.saturating_sub(old_start) <= SCROLLOFF || self.line > bottom {
                self.line = start.min(v.total - 1);
            }
        } else {
            // The bottom edge moved down: a cursor at the old
            // bottom band, or stranded above the new view, lands on
            // the new bottom.
            if bottom.saturating_sub(self.line) <= SCROLLOFF || self.line < start {
                self.line = bottom;
            }
        }
    }

    fn open_search(&mut self, v: &View, dir: Dir) {
        // The origin is the cursor position: `incsearch` jumps from
        // here (section 7.3). The saved view is the `N` restore
        // point of a forward jump.
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.saved_view = Some(SavedView {
            scroll: *v.scroll,
            line: self.line,
            col: self.col,
        });
        self.typing = Some(Typing::Search {
            dir,
            input: String::new(),
            origin: (self.line, self.col),
        });
    }

    /// The `n` / `N` step, counted and wrapping the ends
    /// (section 7.3, the `wrapscan` match). The view centers on the
    /// match (the machine's `zz` remap, section 6.2), except a
    /// backward `N` after a forward jump, which restores the saved
    /// view (the `jumpoptions` pair, section 7.3).
    fn search_step(&mut self, v: &mut View, dir: Dir) {
        let re = match self.search.re.clone() {
            Some(r) => r,
            None => return,
        };
        let count = self.motion_count();
        let was_forward = self.last_move == Some(Dir::Forward);
        if count == 0 {
            // A typed `0` is a no-op motion (section 4.4).
            return;
        }
        // A forward jump remembers its view for a backward `N`.
        if dir == Dir::Forward {
            self.saved_view = Some(SavedView {
                scroll: *v.scroll,
                line: self.line,
                col: self.col,
            });
        }
        let mut pos = (self.line, self.col);
        let mut found = false;
        for _ in 0..count {
            match next_match(v.texts, &re, pos, dir) {
                Some(m) => {
                    pos = m;
                    found = true;
                }
                None => break,
            }
        }
        if !found {
            return;
        }
        self.line = pos.0;
        self.col = pos.1;
        self.last_move = Some(dir);
        self.search.active = true;
        self.search.active_match = Some(pos);
        if dir == Dir::Backward && was_forward {
            // The restore case: one remembered pair (section 7.3).
            if let Some(sv) = self.saved_view.take() {
                *v.scroll = sv.scroll;
            } else {
                center_view(v, self.line);
            }
        } else {
            center_view(v, self.line);
        }
    }

    /// The `*` / `#` word search (section 7.3): the word under the
    /// cursor, escaped into a pattern, searched forward / backward.
    fn search_word(&mut self, v: &mut View, dir: Dir) {
        let text = match v.texts.get(self.line) {
            Some(t) => t,
            None => return,
        };
        let (word, start, end, in_word) = match word_under_cursor(text, self.col, dir) {
            Some(w) => w,
            None => return,
        };
        let _ = end;
        let escaped = regex::escape(&word);
        let re = match compile_smart(&escaped) {
            Ok(r) => r,
            Err(_) => return,
        };
        self.search.pattern = Some(escaped);
        self.search.re = Some(re.clone());
        self.search.re_version += 1;
        self.search.forward = dir == Dir::Forward;
        self.search.active = true;
        // A forward word search remembers its view, like `n`.
        let was_forward = self.last_move == Some(Dir::Forward);
        if dir == Dir::Forward {
            self.saved_view = Some(SavedView {
                scroll: *v.scroll,
                line: self.line,
                col: self.col,
            });
        }
        // The origin skips the occurrence the cursor sits on: in a
        // word the word start, off a word the cursor itself.
        let origin = if in_word {
            (self.line, start)
        } else {
            (self.line, self.col)
        };
        let m = match next_match(v.texts, &re, origin, dir) {
            Some(m) => m,
            None => return,
        };
        self.line = m.0;
        self.col = m.1;
        self.last_move = Some(dir);
        self.search.active_match = Some(m);
        if dir == Dir::Backward && was_forward {
            if let Some(sv) = self.saved_view.take() {
                *v.scroll = sv.scroll;
            } else {
                center_view(v, self.line);
            }
        } else {
            center_view(v, self.line);
        }
    }

    /// Every match of the current pattern, in transcript line
    /// coordinates. The renderer's highlight runs over these.
    pub fn match_lines(&self, texts: &[String]) -> HashSet<usize> {
        let re = match &self.search.re {
            Some(r) => r,
            None => return HashSet::new(),
        };
        let mut out = HashSet::new();
        for (i, t) in texts.iter().enumerate() {
            if re.is_match(t) {
                out.insert(i);
            }
        }
        out
    }
}

/// The rendered length of a transcript line, in characters. The
/// cursor col is a character position (section 4.1).
fn line_len(t: &str) -> usize {
    t.chars().count()
}

/// The yank range of one browse motion key (section 11.4, the
/// section 11.5 reuse): the editor's pi-vim motion primitives over
/// the rendered lines. `j` / `k` / `G` are the linewise forms
/// (the editor's `dj` / `dk` / `dG` ranges); the rest are the char
/// forms, `w` carrying the `extend_w_eol` operator extension.
/// `count` is the multiplied operator-motion count. `None` when the
/// view is empty (an empty transcript is a no-op, section 11.8).
pub(crate) fn browse_motion_range(
    texts: &[String],
    cursor: (usize, usize),
    m: char,
    count: u32,
    count_explicit: bool,
) -> Option<crate::vim_editor::OpRange> {
    use crate::vim_editor as ve;
    if texts.is_empty() {
        return None;
    }
    let cursor = (cursor.0.min(texts.len() - 1), cursor.1);
    let range = match m {
        // `j` / `k` with an operator are linewise (the editor rule).
        'j' => {
            let n = count.max(1) as usize;
            let end_line = (cursor.0 + n).min(texts.len() - 1);
            ve::OpRange {
                start: (cursor.0, 0),
                end: (end_line, line_len(&texts[end_line])),
                linewise: true,
                inclusive: true,
            }
        }
        'k' => {
            let n = count.max(1) as usize;
            let start_line = cursor.0.saturating_sub(n);
            ve::OpRange {
                start: (start_line, 0),
                end: (cursor.0, line_len(&texts[cursor.0])),
                linewise: true,
                inclusive: true,
            }
        }
        // `G` without an explicit count goes to the last line; with
        // one, to line n (the editor's `G` rule).
        'G' => {
            let n = if count_explicit { count.max(1) } else { texts.len() as u32 };
            let res = ve::go_to_last_line(texts, cursor, n);
            ve::motion_to_range(cursor, &res)
        }
        'w' => {
            let res = ve::word_forward(texts, cursor, count.max(1));
            // The operator `w` rule: the `extend_w_eol` extension
            // (the final word reaches the line end).
            let res = ve::extend_w_eol(texts, cursor, res);
            ve::motion_to_range(cursor, &res)
        }
        'b' => {
            let res = ve::word_backward(texts, cursor, count.max(1));
            ve::motion_to_range(cursor, &res)
        }
        '$' => {
            let res = ve::line_end(texts, cursor, count.max(1));
            ve::motion_to_range(cursor, &res)
        }
        '0' => {
            let res = ve::line_start(texts, cursor, 1);
            ve::motion_to_range(cursor, &res)
        }
        '^' => {
            let res = ve::first_nonblank_motion(texts, cursor, 1);
            ve::motion_to_range(cursor, &res)
        }
        'h' => {
            let res = ve::char_left(texts, cursor, count.max(1));
            ve::motion_to_range(cursor, &res)
        }
        'l' => {
            // The operator `l` rule: inclusive, capped at the last
            // char (the editor's `dl` deletes the char under the
            // cursor; a `y` of it yanks the span).
            let len = line_len(&texts[cursor.0]);
            let target = if len == 0 {
                0
            } else {
                (cursor.1.min(len - 1) + count.max(1) as usize).min(len - 1)
            };
            let res = ve::MotionResult {
                pos: (cursor.0, target),
                linewise: false,
                inclusive: true,
            };
            ve::motion_to_range(cursor, &res)
        }
        _ => return None,
    };
    Some(range)
}

/// The OSC 52 host-clipboard escape (section 11.3):
/// `\x1b]52;c;<base64>\x07`. The payload is the yanked text
/// base64-encoded with the `base64` crate (the `bin/tui` direct
/// dep, already transitive). The sequence passes through tmux and
/// ssh to the terminal, where the host's clipboard manager picks it
/// up; a terminal without OSC 52 support ignores it, and the
/// in-memory register still serves the editor's `p`.
pub(crate) fn host_clipboard_escape(text: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\u{1b}]52;c;{b64}\u{7}")
}

/// The scrolloff view follow (section 4.5): the smallest scroll
/// move that keeps `SCROLLOFF` lines above and below the cursor.
/// When a log edge sits closer, the view pins to that edge. A view
/// too short for both margins centers on the cursor.
pub fn follow_view(total: usize, h: usize, line: usize, scroll: usize) -> usize {
    if total == 0 || h == 0 {
        return 0;
    }
    if total <= h {
        return 0;
    }
    let line = line.min(total - 1);
    let cur = total.saturating_sub(scroll + h);
    let mut t = cur;
    // The top margin: at least SCROLLOFF lines above the cursor.
    if line < t + SCROLLOFF {
        t = line.saturating_sub(SCROLLOFF);
    }
    // The bottom margin: at least SCROLLOFF lines below the cursor.
    if t.saturating_add(h).saturating_sub(1) < line + SCROLLOFF {
        t = line.saturating_add(SCROLLOFF + 1).saturating_sub(h);
    }
    // A view shorter than 2 * margin + 2 cannot hold both margins:
    // the cursor centers.
    if h < 2 * SCROLLOFF + 2 {
        t = line.saturating_sub(h / 2);
    }
    t = t.min(total - h);
    total - t - h
}

/// Center the view on the line (the machine's `zz`, section 6.2).
fn center_view(v: &mut View, line: usize) {
    if v.total == 0 || v.h == 0 {
        return;
    }
    if v.total <= v.h {
        *v.scroll = 0;
        return;
    }
    let t = line.saturating_sub(v.h / 2).min(v.total - v.h);
    *v.scroll = v.total - t - v.h;
}

/// The position bar geometry (section 3). The track is the pane
/// height `h`; one cell maps to `total / h` log lines, rounded up.
/// The thumb bottom sits `scroll` lines above the track bottom, and
/// the tail marker anchors the latest position. The cursor marker
/// is the browse-mode addition.
pub struct BarGeom {
    /// The top track cell of the thumb, 0-based from the top.
    pub thumb_top: usize,
    /// The thumb height in cells.
    pub thumb_h: usize,
    /// The tail marker cell (the track bottom).
    pub tail_cell: usize,
    /// The cursor marker cell, browse mode only.
    pub cursor_cell: Option<usize>,
}

pub fn bar_geometry(total: usize, h: usize, scroll: usize, cursor: Option<usize>) -> Option<BarGeom> {
    if total == 0 || h == 0 {
        return None;
    }
    // One track cell maps to `total / h` lines, rounded up, at least
    // one (section 3).
    let step = total.div_ceil(h);
    // The thumb height is `max(1, h * h / total)`, capped at the
    // track (section 3). A total that fits the view spans it.
    let thumb_h = (h * h / total).max(1).min(h);
    // The window is the last `h` lines after skipping `scroll`; its
    // bottom sits `scroll` lines above the track bottom.
    let s = scroll.min(total - 1);
    let bottom_line = total - 1 - s;
    let thumb_bottom = bottom_line / step;
    let thumb_top = thumb_bottom.saturating_sub(thumb_h - 1);
    Some(BarGeom {
        thumb_top,
        thumb_h,
        tail_cell: h - 1,
        cursor_cell: cursor.map(|c| c.min(total - 1) / step),
    })
}

/// The gutter width of section 4.3: the digit count of `total` plus
/// one trailing space, like neovim.
pub fn gutter_width(total: usize) -> usize {
    if total == 0 {
        return 1;
    }
    total.to_string().len() + 1
}

/// The gutter number of a visible line (section 4.3): the absolute
/// line number at the cursor, the relative distance elsewhere. Lines
/// are zero-based; the absolute number is the line plus one.
pub fn gutter_number(cursor: usize, line: usize) -> u32 {
    if line == cursor {
        (line + 1) as u32
    } else if line > cursor {
        (line - cursor) as u32
    } else {
        (cursor - line) as u32
    }
}

/// The `ignorecase` + `smartcase` compile (section 7.3): a pattern
/// without an uppercase compiles case-blind, with one case-sensitive.
fn compile_smart(pat: &str) -> Result<Regex, regex::Error> {
    let case_blind = !pat.chars().any(|c| c.is_uppercase());
    if case_blind {
        Regex::new(&format!("(?i){pat}"))
    } else {
        Regex::new(pat)
    }
}

/// The next match strictly after / before `origin`, in `dir`. The
/// match wraps the ends; a pattern with no match at all yields
/// `None` (the motion is a no-op). `origin` is exclusive: a match
/// starting at the origin position is skipped, so a repeat never
/// re-finds the match the cursor sits on.
fn next_match(
    texts: &[String],
    re: &Regex,
    origin: (usize, usize),
    dir: Dir,
) -> Option<(usize, usize)> {
    let mut matches: Vec<(usize, usize)> = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        for m in re.find_iter(t) {
            matches.push((i, m.start()));
        }
    }
    if matches.is_empty() {
        return None;
    }
    match dir {
        Dir::Forward => {
            for m in &matches {
                if m.0 > origin.0 || (m.0 == origin.0 && m.1 > origin.1) {
                    return Some(*m);
                }
            }
            Some(matches[0])
        }
        Dir::Backward => {
            for m in matches.iter().rev() {
                if m.0 < origin.0 || (m.0 == origin.0 && m.1 < origin.1) {
                    return Some(*m);
                }
            }
            Some(*matches.last().unwrap())
        }
    }
}

/// The word under the cursor: the longest alphanumeric-plus-
/// underscore run (section 7.3). On a non-word character the search
/// takes the next word forward, or the previous word backward.
/// Returns `(word, start, end, in_word)`, the end exclusive, and
/// `in_word` says the cursor character is a word character.
fn word_under_cursor(
    text: &str,
    col: usize,
    dir: Dir,
) -> Option<(String, usize, usize, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let col = col.min(chars.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // The cursor sits in a word: extend to its run.
    if col > 0 && is_word(chars[col]) {
        let mut s = col;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = col + 1;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        return Some((chars[s..e].iter().collect(), s, e, true));
    }
    match dir {
        Dir::Forward => {
            let mut i = col;
            while i < chars.len() && !is_word(chars[i]) {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            let s = i;
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            Some((chars[s..i].iter().collect(), s, i, false))
        }
        Dir::Backward => {
            let mut i = col;
            while i > 0 && !is_word(chars[i - 1]) {
                i -= 1;
            }
            if i == 0 {
                return None;
            }
            let e = i;
            while i > 0 && is_word(chars[i - 1]) {
                i -= 1;
            }
            Some((chars[i..e].iter().collect(), i, e, false))
        }
    }
}
