//! Application state: what the TUI shows and how keys map to actions.
//!
//! This module never touches the port or I/O: it turns key events into
//! [`Action`]s and folds port results back in. `main.rs` executes the
//! actions; the tests here drive the state machine directly.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use ratatui::text::Line;

use crate::event::{Event, EventKind};
use crate::port::{LoopHandle, LoopLine, SessionId, WatchItem};
use crate::vim_editor::{Editor, Mode};
use serde_json::Value;

/// Normalized key input. crossterm-free so tests can drive the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Backspace,
    Tab,
    BackTab,
    PgUp,
    PgDn,
    CtrlR,
    CtrlC,
    CtrlE,
    CtrlU,
    CtrlD,
    /// The global tool fold/expand toggle (docs/tui-tool-display-port.md
    /// section 2, the expand part): every collapsed block expands to
    /// the full output, and back. The key follows `pi`'s expand key.
    CtrlO,
    /// The thinking-block show/hide toggle (docs/tui-thinking-block.md
    /// section 4). `Ctrl+H` is unsafe: most terminals send the
    /// backspace byte for it, so the toggle takes `Ctrl+T` instead.
    CtrlT,
    /// The thinking-block collapse/expand toggle (docs/tui-thinking-
    /// block.md section 4).
    CtrlX,
    /// The input queue toggle: the next draft sends to the follow
    /// queue (docs/tui-pending-user-messages.md stage 2).
    CtrlF,
    /// The reasoning-effort cycle: the next effort value in the
    /// effort order, written to the active model's config entry
    /// (docs/tui-thinking-block.md section 4, the effort control).
    CtrlL,
    /// The multi-line editor's newline key: in insert mode it
    /// inserts a hard newline; in normal mode it is the `j` motion.
    /// `Enter` sends the draft (docs/tui.md section 7).
    CtrlJ,
    /// The picker list navigation: Ctrl+K moves up, Ctrl+J moves
    /// down (docs/tui-file-picker.md section 5).
    CtrlK,
    /// The picker preview-pane toggle (docs/tui-file-picker.md
    /// section 4.4).
    CtrlP,
    /// The picker file-scope cycle (docs/tui-file-picker.md P9):
    /// standard → show git-ignored → also show hidden → back to
    /// standard. The item list is re-enumerated on each press.
    CtrlI,
    /// Standard Unix job control: suspend the TUI with SIGTSTP so the
    /// shell can background it. The user resumes with `fg`
    /// (docs/tui_feature_requests_from_human.md, issue #2).
    CtrlZ,
    /// Alt+Up: recall the pending message queue into the editor
    /// (docs/user-message-editing.md).
    AltUp,
    Esc,
    Quit,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    Wheel(i32),
    Char(char),
}

/// An answer to the oldest pending `approval_request`
/// (docs/tui.md section 7: y / n / e).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Edit,
}

/// Port-level work the UI thread must perform for a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Cycle to the next/previous session, then reload its log.
    ///
    /// Reserved for the session-navigation design (docs/tui_feature_
    /// requests_from_human.md). The Tab / BackTab bindings were freed
    /// for the file picker, so this variant is currently unconstructed
    /// but kept as the seam for the redesigned session navigator.
    #[allow(dead_code)]
    CycleSessions(i32),
    /// `SessionPort::spawn_loop(active)` (docs/tui.md key Ctrl+R).
    /// Main gates the spawn on the persistent loop probe (FT-003): a
    /// live loop for the session blocks the start, across a TUI
    /// restart.
    RunLoop,
    /// Stop the active session's loop and append a `cancel` event.
    /// With no local handle, main stops the external group through the
    /// persistent `loop.pid` (FT-003).
    StopLoop,
    /// Append the draft as a `user_message` (docs/tui.md key Enter).
    SendDraft,
    /// Open `$EDITOR` on the draft (docs/tui.md key Ctrl+E).
    OpenEditor,
    /// Append an `approval` event (docs/tui.md keys y / n / e).
    AnswerApproval(Decision),
    /// Confirm the session name the user typed after starting `tui`
    /// without a session argument. The TUI activates it and opens its
    /// (possibly empty) log.
    ConfirmNewSession(String),
    /// Switch to the handoff session a `context_exhausted` event
    /// seeded, and start the loop there. The one-key resume of the
    /// automatic handoff (correction 57). The old session's local
    /// loop stops when it is still running: the handoff supersedes
    /// it.
    Handoff(String),
    /// Quit the TUI. Loops keep running: each lives in its own session
    /// and survives as an orphan. Only Ctrl+C stops a loop. The log
    /// stays intact, so a restart re-renders the live session.
    Quit,
    /// The global tool fold/expand toggle (Ctrl+O). Main redraws; the
    /// state lives on the app (docs/tui-tool-display-port.md section 2,
    /// the expand part).
    ToggleToolExpand,
    /// The thinking-block show/hide toggle (Ctrl+T, docs/tui-thinking-
    /// block.md section 4).
    ToggleThinking,
    /// The thinking-block collapse/expand toggle (Ctrl+X).
    ToggleThinkingExpand,
    /// The input queue toggle (Ctrl+F, docs/tui-pending-user-messages.md
    /// stage 2): the next draft sends to the follow queue.
    ToggleFollowQueue,
    /// The reasoning-effort cycle (Ctrl+L, docs/tui-thinking-block.md
    /// section 4, the effort control). Main writes the next effort
    /// value to the active model's config entry and flashes the
    /// change.
    CycleEffort,
    /// Switch to a session by name (docs/tui-command-palette.md §7).
    SwitchSession(String),
    /// Set the reasoning effort to a specific value (palette `effort` item).
    SetEffort(String),
    /// Invoke an extension-owned command (docs/tui-command-palette.md §10).
    InvokeExtCommand {
        ext: String,
        id: String,
        value: Option<String>,
    },
    /// Bulk recall of pending user messages (docs/user-message-editing.md).
    RecallQueue,
    /// Suspend the TUI process via SIGTSTP (standard Unix job control,
    /// issue #2). The shell backgrounds the process; `fg` resumes it.
    Suspend,
    TreeViewOnly,
    RewindNoSummary {
        target_seq: u64,
        mode: String,
        restore_text: Option<String>,
    },
}

/// The oldest pending `approval_request` in the active session log.
/// A request is pending until an `approval` event with the same `id`
/// appears later in the log — approval recovery (refinement policy G6).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub request_id: String,
    pub prompt: Option<String>,
    pub call_id: Option<String>,
    /// The tool_call arguments, so an edit-then-allow can carry edited
    /// arguments in the `approval` event.
    pub arguments: Option<Value>,
}

#[derive(Default)]
pub struct LoopState {
    pub handle: Option<Box<dyn LoopHandle>>,
    pub lines_rx: Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>>,
    pub running: bool,
    pub last_line: Option<String>,
    pub exit_code: Option<i32>,
}

/// In-progress model response accumulated from the stream channel
/// (docs/tui-streaming-response.md §3.2). One JSON line per SSE delta
/// event; the TUI polls the file each frame and folds new lines in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamBuf {
    /// Accumulated assistant text (`text` deltas).
    pub text: String,
    /// Accumulated reasoning text per item id (`reasoning` deltas).
    pub reasoning: std::collections::HashMap<String, String>,
    /// Partial tool-call arguments: call_id → (tool_name, args_so_far).
    pub tool_args: std::collections::HashMap<String, (String, String)>,
    /// Set when the channel's `done` line was read.
    pub done: bool,
}

impl StreamBuf {
    /// The reasoning values joined in sorted-id order, the exact
    /// string the live block feeds to `wrap_thinking` (the thinking
    /// body of docs/tui-streaming-simplify.md section 3). Sorting the
    /// keys makes the join deterministic regardless of `HashMap`
    /// iteration order, so a cache keyed on this string is stable.
    pub fn reasoning_text(&self) -> String {
        #[cfg(test)]
        {
            // The join-skip test
            // (docs/tui-perf-streaming-incremental-plan.md) asserts
            // this O(T) join does not run on idle frames.
            REASONING_JOIN_CALLS.with(|c| c.set(c.get() + 1));
        }
        let mut ids: Vec<&String> = self.reasoning.keys().collect();
        ids.sort();
        ids.iter()
            .filter_map(|id| self.reasoning.get(*id))
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// Test-only counter: how many times the O(T) join in
// `StreamBuf::reasoning_text` actually ran on the calling thread.
// A match on the cache's join-skip fingerprint must leave it
// untouched (docs/tui-perf-streaming-incremental-plan.md test 7).
#[cfg(test)]
thread_local! {
    pub(crate) static REASONING_JOIN_CALLS: std::cell::Cell<u32> = std::cell::Cell::new(0);
}

/// Incremental cache for the live stream block.
/// See docs/tui-perf-streaming-incremental-plan.md.
///
/// Holds the wrapped output for the thinking and response-text
/// sections plus the source prefix that produced them. On each call
/// to `stream_block_lines`, only the appended suffix is re-wrapped.
/// The stateful `CodeHl` persists. This keeps code fences that
/// span the append boundary coherent.
///
/// The wrapped sections are shared through [`Rc`]. They are not
/// borrowed out of [`App`]. The `draw` call site interleaves
/// `&mut App` methods between uses of the stream block. A view
/// that borrows `App` would conflict with them. On a cache hit the
/// `Rc` clone is an O(1) refcount bump. An idle frame stays cheap.
/// This is the plan's clone-avoidance goal. The `Rc` is used because
/// the caller consumes the lines directly, not via `to_vec()`.
///
/// The type is `!Send`. It owns a `CodeHl`, which may hold a
/// tree-sitter parser. It also owns `Rc`. That is fine. The cache
/// lives on the main thread alongside [`App`].
pub(crate) struct StreamBlockCache {
    // ── thinking section ────────────────────────────────
    /// The settled, fully-wrapped thinking source. This is the
    /// complete hard lines, ending at a `\n` boundary. It is a
    /// prefix of the joined reasoning text. When the joined text
    /// starts with this, only the suffix is re-wrapped.
    pub(crate) think_src: String,
    /// The in-progress last hard line of the reasoning text.
    /// It has no trailing `\n`. It is not yet wrapped into
    /// [`Self::think_lines`]. It is wrapped fresh each frame for
    /// the live display. Once a `\n` completes it, it is promoted
    /// into `think_lines`.
    pub(crate) think_held: String,
    /// The wrapped, gutter-prefixed display lines for
    /// [`Self::think_held`]. Recomputed only when `think_held`, the
    /// fence state, or the config snapshot changes. An idle frame
    /// reuses it, so the live block stays O(1) with no highlight
    /// work.
    pub(crate) think_held_lines: Vec<Line<'static>>,
    /// The byte length of `think_held` when `think_held_lines` was
    /// last computed. An idle frame (fingerprint unchanged) reuses
    /// `think_held_lines` and skips the re-wrap. A frame that grew
    /// the held line recomputes. The idle-path O(1) guard.
    pub(crate) think_held_wrapped_for: usize,
    /// Wrapped, gutter-prefixed lines for [`Self::think_src`]. The
    /// in-progress held line is wrapped separately. The settled
    /// lines stay stable and are reused on cache hits.
    pub(crate) think_lines: Rc<Vec<Line<'static>>>,
    /// The persistent code highlighter, carried across calls. This
    /// keeps block-comment state in code fences open.
    pub(crate) think_hl: crate::tool_display::CodeHl,
    /// Fence state at the end of `think_src`. This records whether
    /// we are inside a code fence and what language tag opened it.
    pub(crate) think_in_fence: bool,
    pub(crate) think_fence_lang: Option<String>,

    // ── response-text section ───────────────────────────
    /// The response text that `text_lines` was produced from.
    /// `render_markdown_lines` is a whole-document parser. We can
    /// only skip the re-parse when the text is unchanged.
    pub(crate) text_src: String,
    /// Rendered, gutter-prefixed lines for `text_src`.
    pub(crate) text_lines: Rc<Vec<Line<'static>>>,

    // ── config snapshot for invalidation ────────────────
    /// The width, palette level, highlight engine, and
    /// thinking_expanded flag at the time the cache was built.
    /// A change to any of these invalidates the cache.
    pub(crate) width: usize,
    pub(crate) palette_level: crate::color::Level,
    pub(crate) engine: crate::tool_display::HighlightEngine,
    pub(crate) thinking_expanded: bool,

    // ── join-skip fingerprint ───────────────────────────
    /// A cheap way to detect that the `reasoning` map is unchanged
    /// without re-joining it. `think_reasoning_keys` is the sorted
    /// id set. `think_reasoning_len` is the total byte length
    /// across all values. The pair changes only when the map
    /// changes. Values grow only via `push_str`. The id set only
    /// grows within a stream. On a match the O(T) join is
    /// skipped. This is the plan's join-skip optimisation.
    pub(crate) think_reasoning_keys: Vec<String>,
    pub(crate) think_reasoning_len: usize,
}

impl StreamBlockCache {
    /// Construct the cache for the active highlight engine.
    /// `CodeHl` is engine-dependent and has no `Default`, so the
    /// cache must be built with the engine it will use.
    pub(crate) fn new(engine: crate::tool_display::HighlightEngine) -> Self {
        // `think_hl` starts fresh. All other fields start empty or
        // default and are filled on the first build.
        Self {
            think_src: String::new(),
            think_held: String::new(),
            think_held_lines: Vec::new(),
            think_held_wrapped_for: 0,
            think_lines: Rc::new(Vec::new()),
            think_hl: crate::tool_display::CodeHl::new(engine),
            think_in_fence: false,
            think_fence_lang: None,
            text_src: String::new(),
            text_lines: Rc::new(Vec::new()),
            width: 0,
            palette_level: crate::color::Level::DEFAULT,
            engine,
            thinking_expanded: false,
            think_reasoning_keys: Vec::new(),
            think_reasoning_len: 0,
        }
    }
}

/// The kind of a paced stream delta: the accumulator key the release
/// applies to (docs/tui-streaming-response.md §6.5).
#[derive(Clone, Debug)]
enum StreamDeltaKind {
    /// Response text.
    Text,
    /// Reasoning text, keyed by the reasoning item id.
    Reasoning(String),
    /// Partial tool-call arguments, keyed by the call id and the
    /// observed tool name.
    ToolArgs(String, String),
}

/// One stream delta waiting to be released into the live buffer by
/// [`App::pump_stream_pacing`]: the kind plus the raw delta text.
#[derive(Clone, Debug)]
struct PendingStreamDelta {
    kind: StreamDeltaKind,
    payload: String,
}

/// The paced-release window: the pending backlog clears in about this
/// many frames (docs/tui-streaming-response.md §6.5). At the 60 FPS
/// streaming cadence that is a quarter-second catch-up; the per-frame
/// release is `max(1, backlog / STREAM_PACE_FRAMES)` chars, so a small
/// backlog reads as a steady typewriter and an arrival burst smooths
/// out instead of jumping the view.
const STREAM_PACE_FRAMES: usize = 15;

/// The last rendered browse layout (docs/tui-conversation-browsing.md
/// section 4.6): total line count, window height `h`, display text
/// per line, and raw source per line. The renderer refreshes it each
/// frame while browse is active. Browse motions and the search read
/// it. Tests prime it by hand.
#[derive(Debug, Clone)]
pub(crate) struct BrowseLayout {
    pub total: usize,
    pub h: usize,
    pub texts: Vec<String>,
    pub line_raw: Vec<Option<String>>,
}

/// The cached transcript build, plus every key field the
/// invalidation check reads. A scroll redraw reuses the cache, so it
/// is O(viewport) instead of O(total lines).
#[derive(Debug, Clone)]
pub(crate) struct TranscriptCache {
    /// The event version the build consumed.
    pub events_version: u64,
    /// The terminal width (columns) the lines were wrapped at.
    pub width: usize,
    /// The extension reply version at build time; 0 with no ext host.
    pub ext_ver: u64,
    /// The palette capability level at build time.
    pub palette_level: crate::color::Level,
    /// The palette the lines were colored with.
    pub palette: crate::color::Palette,
    /// The wrapped, rendered transcript lines.
    pub lines: Vec<Line<'static>>,
    /// Per-line raw source map, in step with `lines`.
    pub line_raw: Vec<Option<String>>,
    /// Tool-result block spans: tool id to (start, end) line.
    pub block_spans: std::collections::HashMap<String, (usize, usize)>,
    /// The first rendered line of each in-memory event.
    pub event_line_starts: Vec<Option<usize>>,
    /// The fraction epoch at build time.
    pub frac_epoch: u64,
    /// The per-block display texts.
    pub texts: Vec<String>,
    /// The turn-fold epoch at build time.
    pub turn_fold_epoch: u64,
    /// Whether the loop was running when the build was keyed.
    pub loop_running: bool,
}

pub struct App {
    sessions: Vec<SessionId>,
    active: Option<SessionId>,
    events: Vec<Event>,
    /// Visual lines scrolled up from the end. 0 means "follow the tail".
    scroll: usize,
    /// The multi-line message editor with vim modal input. The draft
    /// is `editor.lines`; sending trims and appends it as a
    /// `user_message`.
    editor: Editor,
    /// The shared vim register store (docs/tui-conversation-browsing.md
    /// section 11.3): one store between the `Editor` and the `Browse`
    /// overlay. The editor's `p` / operators read and write it through
    /// [`App::editor_press`]; the browse `y` writes it.
    registers: std::collections::HashMap<char, crate::vim_editor::RegContent>,
    /// The pending OSC 52 host-clipboard writes (docs/tui-
    /// conversation-browsing.md section 11.3): a browse `y` that
    /// targets the `+` / `*` register — or the unnamed one under
    /// `[tui] clipboard = "unnamed"` — queues the escape here; the
    /// host writes it to the terminal before the next frame. A
    /// terminal side effect, not persisted state.
    host_clipboard: Vec<String>,
    /// The first editor line shown in the input area (the area shows
    /// two editor lines plus its border; scrolling moves this window).
    edit_scroll: usize,
    loops: HashMap<SessionId, LoopState>,
    status: Option<(String, Instant)>,
    watch_rx: Option<std::sync::mpsc::Receiver<WatchItem>>,
    quitting: bool,
    /// Armed since the first `q`; a second `q` inside the window quits.
    /// Any other key disarms. Arms only while the quit gate is open
    /// (normal mode, empty draft; FT-012). Mistouch safety (single-
    /// key `q` is too easy to hit by accident mid-typing).
    quit_arm: Option<Instant>,
    /// The new-session name being typed, when the user started `tui`
    /// without a session argument. `None` means the name input is off.
    pending_name: Option<String>,
    /// The browse mode state machine (docs/tui-conversation-browsing.
    /// md section 4). The double-`s` overlay over the transcript.
    browse: crate::browse::Browse,
    /// Armed since the first `s`; a second `s` inside the window
    /// enters or leaves browse mode. Any other key disarms. Mirrors
    /// the `q q` arm of FT-012 for the `s s` gate (section 4.2).
    ss_arm: Option<Instant>,
    /// The last rendered browse layout (see `BrowseLayout`).
    browse_layout: Option<BrowseLayout>,
    /// A rendered event landed since the last browse sync
    /// (section 4.6): the transcript growth is event growth, not a
    /// pane rewrap, so the browse view does not follow.
    events_grew: bool,
    last_stream_len: usize,
    /// The transcript pane height set by the last draw, in lines.
    /// Drives the half-page distance of Ctrl+U / Ctrl+D.
    viewport: usize,
    /// Bumped whenever the event list changes. The transcript cache is
    /// valid only while this number is unchanged.
    events_version: u64,
    events_base_seq: usize,
    view_only_target: Option<usize>,
    /// The cached transcript build (see `TranscriptCache`).
    transcript_cache: Option<TranscriptCache>,
    /// The incremental live-stream block cache. See
    /// docs/tui-perf-streaming-incremental-plan.md. None until the
    /// first draw of a stream. Cleared in clear_stream so the held
    /// tree-sitter parser and cached lines return to baseline.
    /// Not Send. It owns a CodeHl and Rc. Lives on the main thread.
    stream_block_cache: Option<StreamBlockCache>,
    /// The background transcript build worker (docs/tui-perf-background-
    /// build-plan.md, stage 2). `None` until `attach_transcript_worker`.
    /// The miss path then falls back to the synchronous main-thread build.
    transcript_worker: Option<crate::transcript_worker::TranscriptWorker>,
    /// The monotonic sequence of dispatched transcript builds.
    transcript_build_seq: u64,
    /// A cache-key miss recorded the key it wants. The main loop
    /// dispatches one background build before the next draw.
    transcript_rebuild_requested: bool,
    /// A background build is in flight. Cleared when its result
    /// arrives, swaps in, or drops as stale.
    transcript_build_in_flight: bool,
    /// The cache key the pending rebuild targets. The last miss's key.
    transcript_desired_key: Option<crate::transcript_worker::BuildKey>,
    /// The deadline of the trailing 75 ms window for a pending
    /// width-triggered rebuild (docs/tui-perf-background-build-plan.md,
    /// stage 4). Set on a width miss, reset by every new width value.
    /// `None` for event-commit misses, which dispatch immediately.
    transcript_width_debounce: Option<std::time::Instant>,
    /// The cache holds the tail-window fast build, not the full
    /// transcript (docs/tui-perf-background-build-plan.md, stage 2).
    /// While set, browse yank and `gg` stay disabled.
    transcript_partial: bool,
    /// Optional transcript-rebuild trace sink, enabled when the
    /// `TUI_TRANSCRIPT_TRACE` env var is set
    /// (docs/tui-perf-background-build-audit.md). Every miss,
    /// dispatch, debounce hold, cancel, and settle writes one line.
    /// `None` unless enabled; the lock keeps App usable across the
    /// tests that share one.
    transcript_trace: Option<std::sync::Mutex<std::fs::File>>,
    /// The wall-clock moment the current in-flight build dispatched.
    /// Set by [`App::dispatch_transcript_build`], read by
    /// [`App::poll_transcript_worker`] to log settle latency.
    transcript_dispatched_at: Option<std::time::Instant>,
    /// The terminal's color capability the built-in palette is lowered
    /// to, and the selected color scheme (docs/tui-color-scheme.md
    /// section 3). Set by the host in `main`; `new` defaults to the
    /// built-in palette at detected capability, so render tests are
    /// deterministic.
    palette: crate::color::Palette,
    /// The tool-result display config ([`tui] tool_display` table,
    /// docs/tui-tool-display-port.md section 2). Set from the harness
    /// config in `main`; `new` defaults to the `opencode` preset.
    tool_display: crate::tool_display::ToolDisplay,
    /// The global tool fold/expand toggle (Ctrl+O, docs/tui-tool-
    /// display-port.md section 2, the expand part). `false` shows each
    /// block at its output mode's lines; `true` expands every
    /// collapsed block to the full body, capped at
    /// `expanded_preview_max_lines`.
    tool_expanded: bool,
    /// Per-block expand fractions for the animation system
    /// (docs/tui-tool-display-fancy.md section 6). Keys are tool-result
    /// event IDs. A value in `[0.0, 1.0]` is the current progress of
    /// the block's expand/collapse transition. An empty map means no
    /// animation is in flight; the global `tool_expanded` bool applies
    /// as-is.
    block_fracs: std::collections::HashMap<String, f64>,
    /// Per-block expand targets (0.0 or 1.0). The animation moves
    /// `block_fracs` toward these values.
    block_targets: std::collections::HashMap<String, f64>,
    /// The fraction value at the moment the current animation started.
    block_anim_from: std::collections::HashMap<String, f64>,
    /// Per-block animation start time, in milliseconds since process
    /// start. Used to compute the eased progress.
    block_anim_start: std::collections::HashMap<String, u64>,
    /// The spawn time of each tool-result block, in milliseconds since
    /// process start. Drives the fade-in animation
    /// (docs/tui-tool-display-fancy.md section 6).
    block_spawn_ms: std::collections::HashMap<String, u64>,
    /// The last `pump_animations` timestamp, in milliseconds.
    last_anim_tick_ms: u64,
    /// Screen-row spans of tool-result blocks in the transcript, keyed
    /// by event ID. Populated during the draw pass so mouse clicks can
    /// be hit-tested against block boundaries.
    block_spans: std::collections::HashMap<String, (usize, usize)>,
    /// The screen row where the transcript area begins (inside the
    /// outer border). Set each draw frame.
    transcript_top_row: u16,
    /// The index into the full transcript of the first visible line.
    /// Set each draw frame.
    transcript_visible_start: usize,
    /// Bumped whenever `block_fracs` changes so the transcript cache
    /// is invalidated and rebuilt with the new per-block expand state.
    frac_epoch: u64,
    turn_fold: std::collections::HashSet<u64>,
    turn_fold_epoch: u64,
    z_fold_arm: Option<std::time::Instant>,
    fold_cursor_target: Option<crate::fold::FoldCursorTarget>,
    last_event_line_starts: Vec<Option<usize>>,
    /// The thinking-block visibility (Ctrl+T, docs/tui-thinking-block.md
    /// section 4). `true` renders the block; `false` hides it
    /// entirely.
    thinking_shown: bool,
    /// The thinking-block expand state (Ctrl+T). `false` shows the
    /// collapsed header row; `true` shows the full thinking text.
    pub(crate) thinking_expanded: bool,
    /// The input queue toggle (Ctrl+F, docs/tui-pending-user-messages.md
    /// stage 2). `true`: the next draft sends to the follow queue.
    follow_queue: bool,
    /// The `@` file-picker state machine (docs/tui-file-picker.md
    /// section 4.3). `open` is `true` while the floating window is
    /// visible.
    picker: crate::picker::state::PickerState,
    /// The background frizbee ranker for the picker. `None` while the
    /// picker is closed. Spawned when the picker opens, dropped when
    /// it closes.
    picker_matcher: Option<crate::picker::fuzzy::PickerMatcher>,
    /// The file source backing the picker (the section 4.5 seam where
    /// later symbol and git-file sources plug in).
    picker_source: Option<crate::picker::items::FileItemSource>,
    /// The search root the current matcher list was enumerated under.
    /// Path queries re-root the search; a changed root re-enumerates.
    picker_root: std::path::PathBuf,
    /// The `(row, col)` of the `@` token that opened the picker,
    /// so `sync_picker` can compute the query from that fixed
    /// position instead of re-detecting the token (which would
    /// fail if the user types another `@` into the query).
    picker_at: Option<(usize, usize)>,
    /// The `:` command-palette state machine
    /// (docs/tui-command-palette.md section 11).
    palette_state: crate::palette::state::PaletteState,
    /// Extension-provided palette commands, refreshed when a
    /// `commands_list` reply lands or a command times out.
    ext_commands: Vec<crate::palette::items::PaletteItem>,
    /// The active model's current reasoning effort, used to mark
    /// the current option in the palette `effort` item.
    effort_current: String,
    /// Set when the palette opens so the main loop can request
    /// `commands` from extensions.
    palette_cmd_requested: bool,
    /// Latest `ext_status` values, id to value, for the active
    /// session. Maintained incrementally: `set_active` builds it
    /// and each appended watch event updates it. A tick reads this
    /// map in O(1) instead of rescanning the whole log
    /// (ui-extension-plan stage 1, tick payload).
    ext_status_values: HashMap<String, Value>,
    /// The update order of the ext_status ids, most recent last.
    /// Drops the oldest id when the map holds the cap
    /// (ui-extension-plan stage 4: the log-growth bound).
    ext_status_order: VecDeque<String>,
    /// The timestamp of the event that last set each ext_status id
    /// value (docs/tui-model-wait-indicator.md). Each id maps to
    /// the raw `ts` string of that event. The map drops an id with
    /// the value map at the cap. An event without a `ts` field
    /// updates the value but leaves the entry untouched.
    ext_status_ts: HashMap<String, String>,
    /// The in-progress model response, accumulated from the session-local
    /// stream channel file (docs/tui-streaming-response.md §6.1).
    /// `None` when no model call is in flight or the response settled.
    stream_buf: Option<StreamBuf>,
    /// Byte offset into the stream channel file already consumed.
    /// Reset when the file is truncated or recreated.
    stream_offset: u64,
    /// Stream deltas queued for the paced release into the live buffer
    /// (docs/tui-streaming-response.md §6.5): the text advances a few
    /// characters per frame instead of jumping whole deltas.
    stream_pending: VecDeque<PendingStreamDelta>,
    /// The char count of `stream_pending` — the per-frame release
    /// budget is `max(1, chars / STREAM_PACE_FRAMES)` (§6.5).
    stream_pending_chars: usize,
}

/// The cap on the distinct ext_status ids the in-memory map holds.
/// A chatty publisher cannot grow the map without bound. At the cap
/// the least-recently-updated id drops first. The log keeps every
/// event: the log is the audit record, and the bound bounds the
/// TUI's memory only (ui-extension-plan stage 4, open items).
const EXT_STATUS_ID_CAP: usize = 128;

/// The `ext_status` id that carries the active loop's phase
/// (docs/tui-model-wait-indicator.md). The loop publishes `wait`
/// before the model call and `tools` before routing. The TUI reads
/// the last value, gated on the loop-running bit. No TUI decision
/// logic: the value is published log state.
pub const LOOP_PHASE_STATUS_ID: &str = "loop_phase";

/// A session name must stay a plain directory name inside the sessions
/// root. Mirrors `port_file::session_dir`: no absolute paths, no
/// parent-directory component, no empty or `.` name (a `.` session
/// would write its log into the sessions root itself, where
/// `list_sessions` never finds it).
fn valid_session_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

/// The `ext_status` values of one event list, id to value. Later
/// events win, like the log order. An event without a string `id`
/// adds no entry; a missing `value` counts as `null`. The triple
/// is the value map, the event-timestamp side map (id to the raw
/// `ts` of the event that last set the id), and the update order
/// (most recent last). Both maps cap at
/// [`EXT_STATUS_ID_CAP`] distinct ids: at the cap the
/// least-recently-updated id drops, like the incremental path.
fn ext_status_map(
    events: &[Event],
) -> (
    HashMap<String, Value>,
    HashMap<String, String>,
    VecDeque<String>,
) {
    let mut m = HashMap::new();
    let mut ts = HashMap::new();
    let mut order: VecDeque<String> = VecDeque::new();
    for e in events {
        if e.kind() != EventKind::ExtStatus {
            continue;
        }
        let Some(id) = e.get_str("id") else {
            continue;
        };
        let value = e.get("value").cloned().unwrap_or(Value::Null);
        let event_ts = e.get_str("ts").map(str::to_string);
        if m.insert(id.to_string(), value).is_none() {
            order.push_back(id.to_string());
            while order.len() > EXT_STATUS_ID_CAP {
                let old = order.pop_front().expect("cap keeps the order non-empty");
                m.remove(&old);
                ts.remove(&old);
            }
        } else {
            if let Some(pos) = order.iter().position(|x| x == id) {
                order.remove(pos);
            }
            order.push_back(id.to_string());
        }
        if let Some(t) = event_ts {
            ts.insert(id.to_string(), t);
        }
    }
    (m, ts, order)
}

const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(4);

/// The `ext_status` id that carries the active model's thinking
/// level. The loop (or a policy hook) publishes it; the TUI reads it
/// to color the input area.
pub const THINKING_STATUS_ID: &str = "model_thinking";
/// The thinking levels this build knows: 0 (no thinking) through 4.
pub const THINKING_LEVELS: u32 = 5;
/// The level shown while no `model_thinking` event is in the log.
pub const DEFAULT_THINKING_LEVEL: u32 = 0;

/// The window in which a second `q` confirms the quit.
const QUIT_ARM_TTL: std::time::Duration = std::time::Duration::from_secs(3);
/// The window in which a second `s` enters or leaves browse mode
/// (docs/tui-conversation-browsing.md section 4.2, the FT-012
/// mirror).
const SS_ARM_TTL: std::time::Duration = std::time::Duration::from_secs(3);
// No cap on the scroll distance or on the events held in memory: the
// whole session log stays reachable (docs/tui-conversation-browsing.md
// section 4.6). The view clamps to the rendered total at draw time.

impl App {
    pub fn new() -> Self {
        App {
            sessions: Vec::new(),
            active: None,
            events: Vec::new(),
            scroll: 0,
            editor: Editor::new(),
            registers: std::collections::HashMap::new(),
            host_clipboard: Vec::new(),
            edit_scroll: 0,
            loops: HashMap::new(),
            status: None,
            watch_rx: None,
            quitting: false,
            quit_arm: None,
            pending_name: None,
            browse: crate::browse::Browse::new(),
            ss_arm: None,
            browse_layout: None,
            events_grew: false,
            last_stream_len: 0,
            ext_status_values: HashMap::new(),
            ext_status_order: VecDeque::new(),
            ext_status_ts: HashMap::new(),
            viewport: 0,
            events_version: 0,
            events_base_seq: 1,
            view_only_target: None,
            transcript_cache: None,
            stream_block_cache: None,
            transcript_worker: None,
            transcript_build_seq: 0,
            transcript_rebuild_requested: false,
            transcript_build_in_flight: false,
            transcript_desired_key: None,
            transcript_width_debounce: None,
            transcript_partial: false,
            transcript_trace: None,
            transcript_dispatched_at: None,
            palette: crate::color::Palette::builtin(crate::color::Level::detect()),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
            tool_expanded: false,
            block_fracs: std::collections::HashMap::new(),
            block_targets: std::collections::HashMap::new(),
            block_anim_from: std::collections::HashMap::new(),
            block_anim_start: std::collections::HashMap::new(),
            block_spawn_ms: std::collections::HashMap::new(),
            last_anim_tick_ms: 0,
            block_spans: std::collections::HashMap::new(),
            transcript_top_row: 0,
            transcript_visible_start: 0,
            frac_epoch: 0,
            turn_fold: std::collections::HashSet::new(),
            turn_fold_epoch: 0,
            z_fold_arm: None,
            fold_cursor_target: None,
            last_event_line_starts: Vec::new(),
            thinking_shown: true,
            // Thinking blocks start collapsed (docs/tui-turn-fold.md).
            // The block is a one-line label until Ctrl+T expands it.
            thinking_expanded: false,
            follow_queue: false,
            picker: crate::picker::state::PickerState::new(),
            picker_matcher: None,
            picker_source: None,
            picker_root: std::path::PathBuf::new(),
            picker_at: None,
            palette_state: crate::palette::state::PaletteState::new(),
            ext_commands: Vec::new(),
            effort_current: "medium".to_string(),
            palette_cmd_requested: false,
            stream_buf: None,
            stream_offset: 0,
            stream_pending: VecDeque::new(),
            stream_pending_chars: 0,
        }
    }

    /// The color palette every built-in style lowers to: the
    /// capability level plus the selected color scheme
    /// (docs/tui-color-scheme.md section 3). Set from the harness
    /// config override in [main]; defaults to the built-in palette
    /// at environment detection, so tests and the no-config path
    /// keep their own detection result.
    pub fn palette(&self) -> &crate::color::Palette {
        &self.palette
    }

    /// Override the palette the built-in render path lowers to.
    pub fn set_palette(&mut self, palette: crate::color::Palette) {
        self.palette = palette;
    }

    /// The tool-result display config ([`tui] tool_display` table,
    /// docs/tui-tool-display-port.md section 2). Set from the harness
    /// config in `main`; defaults to the `opencode` preset.
    pub fn set_tool_display(&mut self, td: crate::tool_display::ToolDisplay) {
        self.tool_display = td;
    }

    /// The tool-result display config, for the render path.
    pub fn tool_display(&self) -> &crate::tool_display::ToolDisplay {
        &self.tool_display
    }

    /// The global fold/expand toggle state (Ctrl+O).
    pub fn tool_expanded(&self) -> bool {
        self.tool_expanded
    }

    /// The thinking-block visibility state. Toggled with `Ctrl+X`.
    pub fn thinking_shown(&self) -> bool {
        self.thinking_shown
    }

    /// The thinking-block expand state. Toggled with `Ctrl+T`.
    pub fn thinking_expanded(&self) -> bool {
        self.thinking_expanded
    }

    /// Per-block expand fractions for the animation system
    /// (docs/tui-tool-display-fancy.md section 6). An empty map means
    /// no animation is in flight; callers fall back to the global
    /// `tool_expanded` bool.
    pub fn expand_fracs(&self) -> &std::collections::HashMap<String, f64> {
        &self.block_fracs
    }

    /// The tool-result block spans (transcript-line ranges), set each
    /// draw frame. Returns an empty map before the first draw.
    pub fn block_spans(&self) -> &std::collections::HashMap<String, (usize, usize)> {
        &self.block_spans
    }

    /// Whether any expand/collapse animation or fade-in is in progress.
    /// Used by the main loop to raise the poll cadence to ~60 fps.
    pub fn is_animating(&self) -> bool {
        // An expand/collapse animation is active until the fraction
        // converges to its target.
        let expand_collapse_active = self.block_fracs.iter().any(|(id, frac)| {
            let target = self.block_targets.get(id).copied().unwrap_or(0.0);
            (frac - target).abs() >= 0.01
        });
        if expand_collapse_active {
            return true;
        }

        // A fade-in is active until anim_ms elapses since spawn.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let dur = self.tool_display.anim_ms.max(1);
        self.block_spawn_ms
            .values()
            .any(|&spawn| now.saturating_sub(spawn) < dur)
    }

    /// Advance all in-flight expand/collapse animations toward their
    /// targets. Call this once per frame while `is_animating()` is
    /// true. Uses a linear ramp over `anim_ms` milliseconds.
    /// (docs/tui-tool-display-fancy.md section 6)
    pub fn pump_animations(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let dur = self.tool_display.anim_ms.max(1);

        // Snapshot the ids so we can mutate the maps without borrow conflicts.
        let ids: Vec<String> = self.block_fracs.keys().cloned().collect();
        let mut to_remove: Vec<String> = Vec::new();

        for id in &ids {
            let frac = self.block_fracs.get(id).copied().unwrap_or(0.0);
            let target = self.block_targets.get(id).copied().unwrap_or(0.0);
            if (frac - target).abs() < 0.005 {
                // Snap to target; clean up if fully collapsed.
                self.block_fracs.insert(id.clone(), target);
                if target == 0.0 {
                    to_remove.push(id.clone());
                }
                continue;
            }
            let start = self.block_anim_start.get(id).copied().unwrap_or(now);
            let from = self.block_anim_from.get(id).copied().unwrap_or(frac);
            let elapsed = now.saturating_sub(start).min(dur);
            let progress = (elapsed as f64) / (dur as f64);
            // Ease-in-out (smoothstep).
            let eased = progress * progress * (3.0 - 2.0 * progress);
            let new_frac = from + (target - from) * eased;
            self.block_fracs.insert(id.clone(), new_frac);
        }

        for id in &to_remove {
            self.block_fracs.remove(id);
            self.block_targets.remove(id);
            self.block_anim_from.remove(id);
            self.block_anim_start.remove(id);
        }

        // Clean up stale fade-spawn entries.
        self.block_spawn_ms
            .retain(|_, spawn| now.saturating_sub(*spawn) < dur);

        // Bump the fraction epoch so the transcript cache is invalidated
        // and rebuilt with the new per-block expand state.
        self.frac_epoch = self.frac_epoch.wrapping_add(1);

        self.last_anim_tick_ms = now;
    }

    /// Start (or restart) an expand/collapse animation for a tool-result
    /// block. `target` is the destination fraction (0.0 collapsed, 1.0
    /// expanded).
    pub fn animate_block(&mut self, tool_id: &str, target: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let current = self.block_fracs.get(tool_id).copied().unwrap_or(0.0);
        if target > 0.0 && self.block_targets.get(tool_id).copied().unwrap_or(0.0) == target {
            return; // already at target
        }
        if target == 0.0 && current <= 0.005 {
            return; // already collapsed
        }
        self.block_targets.insert(tool_id.to_string(), target);
        self.block_fracs.entry(tool_id.to_string()).or_insert(current);
        self.block_anim_from.insert(tool_id.to_string(), current);
        self.block_anim_start.insert(tool_id.to_string(), now);
        self.frac_epoch = self.frac_epoch.wrapping_add(1);
    }

    /// Toggle expand state for a tool-result block (mouse-click, click
    /// mode). (docs/tui-tool-display-fancy.md section 6)
    pub fn toggle_block_expand(&mut self, tool_id: &str) {
        let current = self.block_fracs.get(tool_id).copied().unwrap_or(0.0);
        let target = if current > 0.5 { 0.0 } else { 1.0 };
        self.animate_block(tool_id, target);
    }

    /// Set the focus block (focus mode). Only this block is expanded;
    /// all others collapse. (docs/tui-tool-display-fancy.md section 6)
    pub fn set_focus_block(&mut self, tool_id: &str) {
        // Steady focus is a no-op. Do not bump the cache epoch. The
        // focus loop calls this every frame. A steady bump would miss
        // the cache and force a full rebuild each idle frame.
        if self.block_targets.get(tool_id).copied() == Some(1.0)
            && !self
                .block_targets
                .iter()
                .any(|(id, t)| id != tool_id && *t > 0.0)
        {
            return
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Collapse all previously expanded blocks (set target + anim
        // state directly so the guard in `animate_block` does not skip
        // a block that has not yet been pumped from 0.0).
        let ids: Vec<String> = self
            .block_targets
            .iter()
            .filter(|(_, t)| **t > 0.0)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if id != tool_id {
                let current = self.block_fracs.get(&id).copied().unwrap_or(0.0);
                self.block_targets.insert(id.clone(), 0.0);
                self.block_anim_from.insert(id.clone(), current);
                self.block_anim_start.insert(id.clone(), now);
            }
        }

        // Expand the focus block.
        if !self.block_targets.contains_key(tool_id) {
            self.block_fracs.entry(tool_id.to_string()).or_insert(0.0);
        }
        self.animate_block(tool_id, 1.0);
        self.frac_epoch = self.frac_epoch.wrapping_add(1);
    }

    /// Record the spawn time of a new tool-result block so it can fade
    /// in. No-op if the block was already registered.
    pub fn register_block_spawn(&mut self, tool_id: &str) {
        if !self.block_spawn_ms.contains_key(tool_id) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.block_spawn_ms.insert(tool_id.to_string(), now);
        }
    }

    /// Compute the fade-in alpha (0.0–1.0) for a tool-result block.
    /// Returns 1.0 when the fade-in is complete or the block was not
    /// recently spawned.
    pub fn fade_alpha(&self, tool_id: &str) -> f64 {
        let Some(spawn) = self.block_spawn_ms.get(tool_id) else {
            return 1.0;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let dur = self.tool_display.anim_ms.max(1);
        let elapsed = now.saturating_sub(*spawn);
        if elapsed >= dur {
            return 1.0;
        }
        (elapsed as f64) / (dur as f64)
    }

    /// Set the screen-row spans of tool-result blocks. Populated by
    /// the render pass each frame so mouse clicks can be hit-tested.
    pub fn set_block_spans(&mut self, spans: std::collections::HashMap<String, (usize, usize)>) {
        self.block_spans = spans;
    }

    /// The screen row where the transcript area begins.
    pub fn transcript_top_row(&self) -> u16 {
        self.transcript_top_row
    }

    /// Set the screen row where the transcript area begins.
    pub fn set_transcript_top_row(&mut self, row: u16) {
        self.transcript_top_row = row;
    }

    /// The index into the full transcript of the first visible line.
    pub fn transcript_visible_start(&self) -> usize {
        self.transcript_visible_start
    }

    /// Set the index into the full transcript of the first visible line.
    pub fn set_transcript_visible_start(&mut self, idx: usize) {
        self.transcript_visible_start = idx;
    }

    /// Test-only accessor for the per-block animation targets.
    #[cfg(test)]
    pub fn block_targets_for_test(&self) -> &std::collections::HashMap<String, f64> {
        &self.block_targets
    }

    /// The transcript pane height in lines (set during draw).
    pub fn viewport_height(&self) -> usize {
        self.viewport
    }

    /// Find the tool-result block ID whose span contains the given
    /// transcript line index. Returns `None` if no block matches.
    pub fn block_at_transcript_line(&self, line: usize) -> Option<&str> {
        self.block_spans
            .iter()
            .find(|(_, &(start, end))| line >= start && line < end)
            .map(|(id, _)| id.as_str())
    }

    /// Find the tool-result block ID closest to the given transcript
    /// line. Used by focus mode to auto-expand the nearest block.
    pub fn nearest_block_to_line(&self, line: usize) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (id, &(start, end)) in &self.block_spans {
            let dist = if line >= start && line < end {
                0
            } else if line < start {
                start.saturating_sub(line)
            } else {
                line.saturating_sub(end)
            };
            match best {
                Some((_, d)) if dist < d => {
                    best = Some((id, dist));
                }
                None => {
                    best = Some((id, dist));
                }
                _ => {}
            }
        }
        best.map(|(id, _)| id)
    }

    /// The turn-fold state every transcript build consumes
    /// (docs/tui-turn-fold.md). The fold applies in the main view
    /// and the browse view alike. The `z` keys operate the state
    /// while browse is active. The state persists across view
    /// switches and sessions until a session switch clears it.
    pub fn fold_input(&self) -> std::collections::HashSet<u64> {
        self.turn_fold.clone()
    }

    pub fn set_event_line_starts(&mut self, starts: Vec<Option<usize>>) {
        self.last_event_line_starts = starts;
    }

    fn cursor_turn(&self) -> Option<crate::fold::Turn> {
        let (line, _) = self.browse.line_col();
        let idx = crate::fold::event_at_line(&self.last_event_line_starts, line)?;
        let running = self.active().is_some_and(|s| self.loop_running(s));
        let turns = crate::fold::turns(self.events(), self.events_base_seq(), running);
        let ti = crate::fold::turn_at(&turns, idx)?;
        Some(turns[ti].clone())
    }

    fn fold_cursor_hit(&self) -> Option<crate::fold::FoldCursorTarget> {
        let (line, _) = self.browse.line_col();
        if let Some(id) = self.block_at_transcript_line(line) {
            return Some(crate::fold::FoldCursorTarget::Block(id.to_string()));
        }
        self.cursor_turn()
            .map(|t| crate::fold::FoldCursorTarget::Turn(t.seq))
    }

    fn bump_turn_fold(&mut self) {
        self.turn_fold_epoch = self.turn_fold_epoch.wrapping_add(1);
    }

    fn fold_toggle_cursor(&mut self) {
        let hit = match self.fold_cursor_hit() {
            Some(h) => h,
            None => return,
        };
        match hit {
            crate::fold::FoldCursorTarget::Block(id) => {
                self.toggle_block_expand(&id);
                self.fold_cursor_target =
                    Some(crate::fold::FoldCursorTarget::Block(id));
            }
            crate::fold::FoldCursorTarget::Turn(seq) => {
                if self.turn_fold.contains(&seq) {
                    self.turn_fold.remove(&seq);
                } else {
                    self.turn_fold.insert(seq);
                }
                self.bump_turn_fold();
                self.fold_cursor_target =
                    Some(crate::fold::FoldCursorTarget::Turn(seq));
            }
        }
    }

    fn fold_set_cursor_turn(&mut self, open: bool) {
        let hit = match self.fold_cursor_hit() {
            Some(h) => h,
            None => return,
        };
        match hit {
            crate::fold::FoldCursorTarget::Block(id) => {
                let v: f64 = if open { 1.0 } else { 0.0 };
                self.block_targets.insert(id.clone(), v);
                self.block_fracs.insert(id.clone(), v);
                self.frac_epoch = self.frac_epoch.wrapping_add(1);
                self.fold_cursor_target =
                    Some(crate::fold::FoldCursorTarget::Block(id));
            }
            crate::fold::FoldCursorTarget::Turn(seq) => {
                if open {
                    self.turn_fold.insert(seq);
                } else {
                    self.turn_fold.remove(&seq);
                }
                self.bump_turn_fold();
                self.fold_cursor_target =
                    Some(crate::fold::FoldCursorTarget::Turn(seq));
            }
        }
    }

    fn fold_toggle_results_in_turn(&mut self) {
        let Some(t) = self.cursor_turn() else {
            return;
        };
        let ids = crate::fold::tool_ids(self.events(), t.start + 1, t.end);
        let any_open = ids.iter().any(|id| {
            self.block_fracs
                .get(id)
                .copied()
                .unwrap_or(0.0)
                > 0.5
                || self
                    .block_targets
                    .get(id)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.5
        });
        let target: f64 = if any_open { 0.0 } else { 1.0 };
        for id in &ids {
            self.block_targets.insert(id.clone(), target);
            self.block_fracs.insert(id.clone(), target);
        }
        if !ids.is_empty() {
            self.frac_epoch = self.frac_epoch.wrapping_add(1);
        }
        self.fold_cursor_target =
            Some(crate::fold::FoldCursorTarget::Turn(t.seq));
    }

    fn fold_open_all(&mut self) {
        let running = self.active().is_some_and(|s| self.loop_running(s));
        let turns =
            crate::fold::turns(self.events(), self.events_base_seq(), running);
        for t in &turns {
            self.turn_fold.insert(t.seq);
        }
        for id in self.all_tool_call_ids() {
            self.block_targets.insert(id.clone(), 1.0);
            self.block_fracs.insert(id.clone(), 1.0);
        }
        self.bump_turn_fold();
        self.frac_epoch = self.frac_epoch.wrapping_add(1);
        if let Some(t) = self.cursor_turn() {
            self.fold_cursor_target =
                Some(crate::fold::FoldCursorTarget::Turn(t.seq));
        }
    }

    fn fold_close_all(&mut self) {
        self.turn_fold.clear();
        for id in self.all_tool_call_ids() {
            self.block_targets.insert(id.clone(), 0.0);
            self.block_fracs.insert(id.clone(), 0.0);
        }
        self.bump_turn_fold();
        self.frac_epoch = self.frac_epoch.wrapping_add(1);
        if let Some(t) = self.cursor_turn() {
            self.fold_cursor_target =
                Some(crate::fold::FoldCursorTarget::Turn(t.seq));
        }
    }

    fn all_tool_call_ids(&self) -> Vec<String> {
        self.events()
            .iter()
            .filter(|e| e.kind() == crate::event::EventKind::ToolCall)
            .filter_map(|e| e.get_str("id").map(String::from))
            .collect()
    }

    fn fold_jump(&mut self, total: usize, h: usize, next: bool) {
        let (line, _) = self.browse.line_col();
        let idx = match crate::fold::event_at_line(&self.last_event_line_starts, line) {
            Some(i) => i,
            None => return,
        };
        let running = self.active().is_some_and(|s| self.loop_running(s));
        let turns =
            crate::fold::turns(self.events(), self.events_base_seq(), running);
        if turns.is_empty() {
            return;
        }
        let cur = crate::fold::turn_at(&turns, idx).unwrap_or(0);
        let target = if next {
            if cur + 1 >= turns.len() {
                return;
            }
            cur + 1
        } else if cur > 0 {
            cur - 1
        } else {
            return;
        };
        let Some(line) = self
            .last_event_line_starts
            .get(turns[target].start)
            .copied()
            .flatten()
        else {
            return;
        };
        self.browse.goto(total, h, line, &mut self.scroll);
    }

    pub fn apply_fold_cursor_target(&mut self) {
        let Some(target) = self.fold_cursor_target.take() else {
            return;
        };
        let line = match target {
            crate::fold::FoldCursorTarget::Turn(seq) => {
                let running =
                    self.active().is_some_and(|s| self.loop_running(s));
                let turns =
                    crate::fold::turns(self.events(), self.events_base_seq(), running);
                turns
                    .iter()
                    .find(|t| t.seq == seq)
                    .and_then(|t| self.last_event_line_starts.get(t.start))
                    .and_then(|s| *s)
            }
            crate::fold::FoldCursorTarget::Block(id) => {
                let ev_idx = self.events().iter().position(|e| {
                    e.kind() == crate::event::EventKind::ToolResult
                        && e.get_str("id") == Some(id.as_str())
                });
                ev_idx
                    .and_then(|i| self.last_event_line_starts.get(i))
                    .and_then(|s| *s)
            }
        };
        let Some(line) = line else {
            return;
        };
        let Some(layout) = self.browse_layout.as_ref() else {
            return;
        };
        self.browse.goto(layout.total, layout.h, line, &mut self.scroll);
    }

    pub fn fold_key(&mut self, c: char, total: usize, h: usize) -> bool {
        if c == 'z' {
            if self.browse.typing()
                || self.browse.visual_selection().is_some()
            {
                return false;
            }
            self.z_fold_arm = Some(std::time::Instant::now());
            self.flash("fold: z + a o c A R M j k");
            return true;
        }
        let Some(armed_at) = self.z_fold_arm.take() else {
            return false;
        };
        if armed_at.elapsed() > SS_ARM_TTL {
            return false;
        }
        match c {
            'a' => self.fold_toggle_cursor(),
            'o' => self.fold_set_cursor_turn(true),
            'c' => self.fold_set_cursor_turn(false),
            'A' => self.fold_toggle_results_in_turn(),
            'R' => self.fold_open_all(),
            'M' => self.fold_close_all(),
            'j' => self.fold_jump(total, h, true),
            'k' => self.fold_jump(total, h, false),
            _ => {
                self.flash(format!("z{c} unbound"));
            }
        }
        true
    }

    /// Browse-mode click: move the cursor to the clicked transcript
    /// line and toggle the L3 tool-result fold under it
    /// (docs/tui-turn-fold.md, key table: "click").
    pub fn browse_click(&mut self, line: usize, col: usize) {
        if !self.browse.active() {
            return;
        }
        let Some(layout) = self.browse_layout.as_ref() else {
            return;
        };
        self.browse
            .goto_line(layout.total, layout.h, line, col, &mut self.scroll);
        let id = self.block_at_transcript_line(line).map(str::to_owned);
        if let Some(id) = id {
            self.toggle_block_expand(&id);
            self.fold_cursor_target = Some(crate::fold::FoldCursorTarget::Block(id));
        }
    }

    /// The live tally of the in-progress turn, merged into the
    /// working row while the loop runs (docs/tui-turn-fold.md).
    /// `None` when no loop is running or the live turn is empty.
    pub fn live_fold_tally(&self) -> Option<String> {
        let sid = self.active()?;
        if !self.loop_running(sid) {
            return None;
        }
        let events = self.events();
        let turns = crate::fold::turns(events, self.events_base_seq(), true);
        let t = turns.last()?;
        crate::fold::tally_text(events, t.start + 1, t.end)
    }

    /// The input queue toggle state (Ctrl+F): the next draft sends
    /// to the follow queue when `true`.
    pub fn follow_queue(&self) -> bool {
        self.follow_queue
    }

    // ── sessions ────────────────────────────────────────────────

    pub fn set_sessions(&mut self, list: Vec<SessionId>) {
        self.sessions = list;
    }

    pub fn active(&self) -> Option<&SessionId> {
        self.active.as_ref()
    }

    /// Pick the session to show for `CycleSessions(delta)`.
    /// `delta` walks the list from the active session; a missing active
    /// session starts at the first entry.
    pub fn cycle_target(&self, delta: i32) -> Option<SessionId> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        let idx = self
            .active
            .as_ref()
            .and_then(|a| self.sessions.iter().position(|s| s == a))
            .unwrap_or(0);
        let next = ((idx as i64 + delta as i64).rem_euclid(n as i64)) as usize;
        Some(self.sessions[next].clone())
    }

    /// Switch the visible session. Reloads its log and resets scroll.
    /// The caller restarts the port watch afterwards. `log_lines` is
    /// the session's total log line count (the port's line count) and
    /// seeds `events_base_seq`, the 1-based log seq of the window's
    /// first retained event.
    pub fn set_active(
        &mut self,
        id: SessionId,
        events: Vec<Event>,
        log_lines: u64,
    ) {
        // No front trim: the whole session log is held in memory so
        // the beginning stays reachable (docs/tui-conversation-
        // browsing.md section 4.6, no replay cap).
        let (statuses, status_ts, order) = ext_status_map(&events);
        // A new session voids a pending scroll-to-event target: it was
        // resolved against the previous session's event indices. A same-
        // session re-read (a watch delivery, a marker append) keeps the
        // target: the picked event's index is unchanged.
        if self.active.as_ref() != Some(&id) {
            self.view_only_target = None;
            // A new session voids the old session's cached transcript
            // (docs/tui-perf-background-build-plan.md, stage 2). The
            // next draw runs the tail-window fast build for the new
            // session. An in-flight old-session result drops as
            // stale at the poll point.
            self.transcript_cache = None;
            self.transcript_partial = false;
            self.transcript_rebuild_requested = false;
            self.transcript_desired_key = None;
            self.transcript_build_in_flight = false;
            self.transcript_width_debounce = None;
            self.turn_fold.clear();
            self.turn_fold_epoch = self.turn_fold_epoch.wrapping_add(1);
            // A fresh session starts all folded at every level
            // (docs/tui-turn-fold.md): clear the per-result expand
            // state of the old session too.
            self.block_fracs.clear();
            self.block_targets.clear();
            self.block_anim_from.clear();
            self.block_anim_start.clear();
            self.frac_epoch = self.frac_epoch.wrapping_add(1);
            self.z_fold_arm = None;
            self.fold_cursor_target = None;
            self.last_event_line_starts.clear();
        }
        self.active = Some(id);
        self.events = events;
        // 1-based log seq of the retained window's first event: the
        // log's total line count minus the retained window length, plus
        // one (docs/tree-ui-design-from-human.md rewind target seqs).
        self.events_base_seq =
            (log_lines.saturating_sub(self.events.len() as u64) + 1) as usize;
        self.scroll = 0;
        // The browse state never survives a session switch (section
        // 4.7): the reset rides the scroll reset.
        self.browse.reset(&mut self.scroll);
        // The shared register store resets with the browse state
        // (section 11.3, "no persisted state across a session
        // switch or a TUI restart"), like the scroll reset.
        self.registers.clear();
        self.ss_arm = None;
        self.events_grew = false;
        self.events_version += 1;
        self.ext_status_values = statuses;
        self.ext_status_ts = status_ts;
        self.ext_status_order = order;
        self.clear_stream();
        // A fresh session has no live stream. Clear the stream-length
        // baseline so the first draw of the new session cannot report
        // a phantom stream shrink against the prior session's tail.
        self.last_stream_len = 0;
    }

    // ── model stream channel (docs/tui-streaming-response.md §6) ──

    /// The in-progress model response, if one is streaming. Returns
    /// `None` when no model call is in flight or the response has
    /// already settled into the transcript.
    pub fn stream_buf(&self) -> Option<&StreamBuf> {
        self.stream_buf.as_ref()
    }

    /// Clear the live stream buffer and reset the byte offset. Called
    /// when a settle event (`assistant_message`, `error`, `cancel`) or
    /// a session switch replaces the in-progress content with the
    /// authoritative log entry.
    pub fn clear_stream(&mut self) {
        self.stream_buf = None;
        self.stream_offset = 0;
        self.stream_pending.clear();
        self.stream_pending_chars = 0;
        // Release the incremental live-stream cache. This drops the
        // held CodeHl (and its tree-sitter parser) and the cached
        // wrapped lines, so memory returns to baseline
        // (docs/tui-perf-streaming-incremental-plan.md).
        self.stream_block_cache = None;
    }

    /// Test hook: replace the live stream buffer wholesale,
    /// bypassing the stream-channel pacing path. The incremental
    /// cache tests
    /// (docs/tui-perf-streaming-incremental-plan.md) use it to
    /// drive `stream_block_lines` with controlled buffers.
    #[cfg(test)]
    pub fn set_stream_buf(&mut self, buf: StreamBuf) {
        self.stream_buf = Some(buf);
    }

    /// The live stream block cache
    /// (docs/tui-perf-streaming-incremental-plan.md). The render pass
    /// mutates it in place. Cache hits reuse the cached wrapped lines.
    /// Cache misses rebuild and store them. It lives on the main
    /// thread next to the render loop.
    pub(crate) fn stream_block_cache_mut(&mut self) -> &mut Option<StreamBlockCache> {
        &mut self.stream_block_cache
    }

    /// The live stream block cache, shared. The render pass reads the
    /// cached wrapped sections through this on cache hits.
    pub(crate) fn stream_block_cache_ref(&self) -> &Option<StreamBlockCache> {
        &self.stream_block_cache
    }

    /// Poll the session-local stream channel file and fold new lines
    /// into the live buffer (docs/tui-streaming-response.md §6.2).
    ///
    /// A missing file settles the buffer (the loop deleted it when the
    /// model call ended). A file that shrank is a new channel (the
    /// loop re-created it for the next call): reset and re-read from 0.
    /// A file that grew is the normal case: parse new complete lines
    /// and advance the offset. A trailing partial line stays for the
    /// next poll. Transient I/O errors are dropped: the buffer keeps
    /// its last good state.
    pub fn refresh_stream(&mut self, path: &std::path::Path) {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.clear_stream();
                return;
            }
            Err(_) => return,
        };
        if (data.len() as u64) < self.stream_offset {
            // The loop re-created the file for a new model call.
            self.clear_stream();
        }
        let region = &data[self.stream_offset as usize..];
        let complete_end: usize = region.rfind('\n').map(|i| i + 1).unwrap_or(0);
        for line in region[..complete_end].lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(delta) = rushi_common::stage::ModelDelta::from_json_line(line) {
                self.queue_stream_delta(delta);
            }
        }
        self.stream_offset += complete_end as u64;
    }

    /// Queue a new stream delta for the paced release. Content deltas
    /// (`text`, `reasoning`, `tool_call_delta`) enter `stream_pending`
    /// and are released into the live buffer a few characters at a
    /// time by [`App::pump_stream_pacing`]. `done` is applied
    /// immediately — the channel state flips now, and the queued
    /// content drains at once on the next pump, so the final text
    /// settles promptly instead of trickling.
    fn queue_stream_delta(&mut self, delta: rushi_common::stage::ModelDelta) {
        // The first delta of a call opens the live buffer (the header
        // row shows), even when the payload is empty.
        self.stream_buf.get_or_insert_with(StreamBuf::default);
        match delta {
            rushi_common::stage::ModelDelta::Text(t) => {
                if !t.is_empty() {
                    self.push_pending(StreamDeltaKind::Text, t);
                }
            }
            rushi_common::stage::ModelDelta::Reasoning {
                item_id,
                delta,
            } => {
                if !delta.is_empty() {
                    self.push_pending(StreamDeltaKind::Reasoning(item_id), delta);
                }
            }
            rushi_common::stage::ModelDelta::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                if !args_delta.is_empty() {
                    self.push_pending(
                        StreamDeltaKind::ToolArgs(call_id, name),
                        args_delta,
                    );
                }
            }
            rushi_common::stage::ModelDelta::Done { .. } => {
                self.stream_buf
                    .as_mut()
                    .expect("opened above")
                    .done = true;
            }
        }
    }

    /// Append a delta to the pace queue and count its characters.
    fn push_pending(&mut self, kind: StreamDeltaKind, payload: String) {
        self.stream_pending_chars += payload.chars().count();
        self.stream_pending.push_back(PendingStreamDelta { kind, payload });
    }

    /// Release queued stream content into the live buffer at a smooth
    /// per-frame pace (docs/tui-streaming-response.md §6.5). The main
    /// loop calls this once per frame, right after
    /// [`App::refresh_stream`].
    ///
    /// Each call releases `max(1, backlog / STREAM_PACE_FRAMES)`
    /// characters: the backlog clears in about that many frames, so a
    /// fast stream lags by at most a fraction of a second and then
    /// catches up at a uniform rate, while a slow stream reads as a
    /// steady typewriter. Once the channel is `done`, the whole
    /// remainder releases at once. The release is strictly FIFO and
    /// character-bounded, so the displayed text is always a prefix of
    /// the channel's accumulated text (the P3 prefix growth of the
    /// Lean spec).
    pub fn pump_stream_pacing(&mut self) {
        if self.stream_pending.is_empty() {
            return;
        }
        let Some(buf) = self.stream_buf.as_mut() else {
            // Cleared by a settle event: the transcript is
            // authoritative, so drop the queue.
            self.clear_stream();
            return;
        };
        let backlog = self.stream_pending_chars;
        let budget = if buf.done {
            backlog
        } else {
            std::cmp::max(1, backlog / STREAM_PACE_FRAMES)
        };
        let mut remaining = budget;
        while remaining > 0 {
            let item = self
                .stream_pending
                .front()
                .cloned()
                .expect("the queue stays non-empty until it drains");
            let chars_left = item.payload.chars().count();
            if chars_left <= remaining {
                self.stream_pending.pop_front();
                self.stream_pending_chars -= chars_left;
                remaining -= chars_left;
                apply_paced(buf, &item.kind, &item.payload);
            } else {
                // Split at a character boundary: release the first
                // `remaining` characters, keep the rest queued.
                let take_len: usize = item
                    .payload
                    .chars()
                    .take(remaining)
                    .map(char::len_utf8)
                    .sum();
                let take = item.payload[..take_len].to_string();
                if let Some(front) = self.stream_pending.front_mut() {
                    front.payload = item.payload[take_len..].to_string();
                }
                self.stream_pending_chars -= remaining;
                apply_paced(buf, &item.kind, &take);
                remaining = 0;
            }
        }
    }

    /// True while a response is in flight: content is queued for the
    /// paced release, or the channel is still open. The main loop
    /// ticks at a high frame rate while this holds, so the paced text
    /// advances smoothly (docs/tui-streaming-response.md §6.5).
    pub fn stream_live(&self) -> bool {
        !self.stream_pending.is_empty()
            || self.stream_buf.as_ref().is_some_and(|b| !b.done)
    }

    /// The characters waiting in the pace queue (§6.5). Test and
    /// diagnostic use: the main loop decides the cadence with
    /// [`App::stream_live`].
    #[cfg(test)]
    pub fn stream_pending_chars(&self) -> usize {
        self.stream_pending_chars
    }

    // ── new-session name input ──────────────────────────────

    /// Begin the name input. Used at startup when `tui` runs without a
    /// session argument; editing keys route to the name until it is
    /// confirmed (Enter) or cancelled (Esc).
    pub fn start_naming(&mut self) {
        self.pending_name = Some(String::new());
    }

    /// The name being typed, when the name input is active.
    pub fn pending_name(&self) -> Option<&str> {
        self.pending_name.as_deref()
    }

    /// Swap the watcher receiver; the old tailer thread dies when its
    /// channel is dropped.
    pub fn set_watch_rx(&mut self, rx: std::sync::mpsc::Receiver<WatchItem>) {
        self.watch_rx = Some(rx);
    }

    pub fn drain_watch(&mut self) -> Option<WatchItem> {
        self.watch_rx.as_mut()?.try_recv().ok()
    }

    pub fn on_watch_item(&mut self, item: WatchItem) {
        match item {
            WatchItem::Event { event, .. } => {
                // A new ext_status event updates the map in place.
                // Later events win, like the log order. The id set
                // is capped: at the cap the least-recently-updated
                // id drops first.
                if event.kind() == EventKind::ExtStatus {
                    let value = event.get("value").cloned().unwrap_or(Value::Null);
                    if let Some(id) = event.get_str("id") {
                        self.record_ext_status(id, value, event.get_str("ts"));
                    }
                } else {
                    // A rendered event: the transcript total may
                    // grow (section 4.6). The browse sync uses this
                    // to tell event growth from a pane-rewrap
                    // growth (section 4.7).
                    self.events_grew = true;
                    // Settle the live stream buffer when the
                    // authoritative event lands (docs/tui-
                    // streaming-response.md §6.4).
                    if matches!(
                        event.kind(),
                        EventKind::AssistantMessage
                            | EventKind::Error
                            | EventKind::Cancel
                    ) {
                        self.clear_stream();
                    }
                }
                self.events.push(event);
                self.events_version += 1;
            }
            WatchItem::Gone => self.flash("session log missing — waiting for it to come back"),
            WatchItem::Resumed => self.flash("session log restored — resuming"),
            WatchItem::IoError { message } => self.flash(format!("log read error: {message}")),
        }
    }

    pub fn take_events_grew(&mut self) -> bool {
        let f = self.events_grew;
        self.events_grew = false;
        f
    }

    /// Record the live stream length, in rendered lines, at each
    /// draw. Report whether it changed, in either direction, since
    /// the previous draw. The browse sync treats a stream-tail
    /// change, growth or shrink, like a settled-event change and
    /// pins the view in place
    /// (docs/tui-conversation-browsing.md section 4.6). A shrink is
    /// the thinking block sliding its window or the stream settling
    /// into a shorter event.
    pub fn note_stream_changed(&mut self, stream_len: usize) -> bool {
        let changed = stream_len != self.last_stream_len;
        self.last_stream_len = stream_len;
        changed
    }

    /// Set the transcript pane height (the renderer does this every
    /// frame). Used by the half-page keys Ctrl+U / Ctrl+D.
    pub fn set_viewport_height(&mut self, h: usize) {
        self.viewport = h;
    }

    /// The half-viewport scroll distance for Ctrl+U / Ctrl+D, like vim.
    /// A small or unset viewport falls back to the page-key distance.
    fn half_page(&self) -> usize {
        if self.viewport >= 4 {
            (self.viewport - 1) / 2
        } else {
            10
        }
    }

    /// The wrapped transcript lines at `width`, oldest first.
    ///
    /// Cached by (events_version, width, ext reply version, palette
    /// level, palette, frac_epoch). A scroll redraw or a draw with no
    /// new events reuses the cache instead of rewrapping every line.
    ///
    /// Stage 2 (docs/tui-perf-background-build-plan.md): a miss
    /// returns the last good cache and records the wanted key. The
    /// main loop dispatches the background build. The first build of
    /// a session renders the tail window on the main thread,
    /// O(viewport), and the full build follows in the background.
    /// With no worker attached the miss builds on the main thread.
    pub fn transcript_lines(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> &[Line<'static>] {
        // The transcript key folds in the transcript-visible ext
        // version, not `replies_version`. Status, frame, and row
        // replies update their own UI regions and do not bump it, so
        // a statusline tick at idle does not force a full transcript
        // rebuild (docs/tui-perf-background-build-audit.md).
        let ext_ver = ext.map(|h| h.transcript_replies_version()).unwrap_or(0);
        let key = crate::transcript_worker::BuildKey {
            events_version: self.events_version,
            width,
            ext_ver,
            palette_level: self.palette.level(),
            palette: self.palette.clone(),
            frac_epoch: self.frac_epoch,
            turn_fold_epoch: self.turn_fold_epoch,
            loop_running: self.active().is_some_and(|s| self.loop_running(s)),
        };
        // A partial tail cache matches the key but does not satisfy
        // it: the full build is still owed, so it misses too.
        if self.transcript_cache_matches(&key) && !self.transcript_partial {
            // A hit satisfies the current draw. Any pending rebuild
            // (and its debounce deadline) is stale: the user settled
            // back on a width we already built, so drop the state.
            self.transcript_rebuild_requested = false;
            self.transcript_desired_key = None;
            self.transcript_width_debounce = None;
            return &self.transcript_cache.as_ref().unwrap().lines;
        }
        // Cache-key miss: record the wanted key and ask for a
        // rebuild. The main loop dispatches one background build
        // when nothing is in flight.
        //
        // Stage 4: classify the miss. A width change arms the 75 ms
        // trailing window. A new width value resets the deadline.
        // Redraws at the pending width keep it. Misses at the
        // built width (event commit, palette, fraction) bypass it.
        let width_triggered = self
            .transcript_cache
            .as_ref()
            .is_some_and(|c| c.width != key.width);
        if width_triggered {
            // The pending key is the last miss's key. Comparing
            // widths against it tells whether this is a new width
            // event or a redraw at the pending width.
            let new_width_event = self
                .transcript_desired_key
                .as_ref()
                .is_none_or(|d| d.width != key.width);
            if new_width_event {
                self.transcript_width_debounce = Some(
                    Instant::now()
                        + crate::transcript_worker::TRANSCRIPT_WIDTH_DEBOUNCE,
                );
            }
        } else {
            // Same width as the last build: no window, build now.
            self.transcript_width_debounce = None;
        }
        // Log the miss only when the key is new, not every frame
        // while the build is in flight.
        let key_is_new = self
            .transcript_desired_key
            .as_ref()
            .is_none_or(|prev| prev != &key);
        if key_is_new {
            self.trace_transcript(&format!("miss {}", Self::fmt_key(&key)));
        }
        self.transcript_desired_key = Some(key.clone());
        self.transcript_rebuild_requested = true;
        // The natural `if let Some(cached)` return extends the
        // cache borrow over the first-build store below (E0502).
        // The `is_some` check plus a fresh borrow at the return
        // keeps the borrow checker happy.
        #[allow(clippy::unnecessary_unwrap)]
        if self.transcript_cache.is_some() {
            // Stale-while-revalidate: draw the last good cache while
            // the background build runs.
            return &self.transcript_cache.as_ref().unwrap().lines;
        }
        // First build: no good cache yet. The main thread renders the
        // visible tail window, O(viewport), while the full build
        // follows in the background.
        if self.transcript_worker.is_some() {
            let tail = self.viewport.saturating_add(1);
            let build = crate::render::build_transcript_tail(self, width, ext, tail);
            let truncated = self.events().len() > tail.max(1);
            self.store_transcript_build(&key, &build, truncated);
            if !truncated {
                // The tail window covered every event: the build is
                // complete, so no full build is owed.
                self.transcript_rebuild_requested = false;
                self.transcript_desired_key = None;
            }
        } else {
            // No worker attached (tests and the worker-death
            // fallback): build on the main thread.
            let build = crate::render::build_transcript(self, width, ext);
            self.store_transcript_build(&key, &build, false);
            self.transcript_rebuild_requested = false;
            self.transcript_desired_key = None;
        }
        &self.transcript_cache.as_ref().unwrap().lines
    }

    /// True when the cached transcript matches the build key.
    fn transcript_cache_matches(
        &self,
        key: &crate::transcript_worker::BuildKey,
    ) -> bool {
        self.transcript_cache.as_ref().is_some_and(|c| {
            c.events_version == key.events_version
                && c.width == key.width
                && c.ext_ver == key.ext_ver
                && c.palette_level == key.palette_level
                && c.palette == key.palette
                && c.frac_epoch == key.frac_epoch
                && c.turn_fold_epoch == key.turn_fold_epoch
                && c.loop_running == key.loop_running
        })
    }

    /// Store a finished build under its key and set the partial bit.
    fn store_transcript_build(
        &mut self,
        key: &crate::transcript_worker::BuildKey,
        build: &crate::render::TranscriptBuild,
        partial: bool,
    ) {
        self.transcript_cache = Some(TranscriptCache {
            events_version: key.events_version,
            width: key.width,
            ext_ver: key.ext_ver,
            palette_level: key.palette_level,
            palette: key.palette.clone(),
            lines: build.lines.clone(),
            line_raw: build.line_raw.clone(),
            block_spans: build.block_spans.clone(),
            event_line_starts: build.event_line_starts.clone(),
            frac_epoch: key.frac_epoch,
            texts: build.texts.clone(),
            turn_fold_epoch: key.turn_fold_epoch,
            loop_running: key.loop_running,
        });
        self.transcript_partial = partial;
    }

    /// Attach the background transcript build worker
    /// (docs/tui-perf-background-build-plan.md, stage 2).
    /// `main` calls this once before the event loop. Without it the
    /// miss path falls back to the synchronous main-thread build.
    pub fn attach_transcript_worker(&mut self) {
        self.transcript_worker =
            Some(crate::transcript_worker::TranscriptWorker::spawn());
    }

    /// Enable the transcript-rebuild trace by opening a sink file
    /// (docs/tui-perf-background-build-audit.md). `main` wires this
    /// up when `TUI_TRANSCRIPT_TRACE` is set. Every miss, dispatch,
    /// cancel, and settle writes one timestamped line.
    pub fn set_transcript_trace(&mut self, file: std::fs::File) {
        self.transcript_trace = Some(std::sync::Mutex::new(file));
    }

    /// Append one line to the transcript trace sink, if enabled.
    fn trace_transcript(&mut self, msg: &str) {
        let Some(trace) = self.transcript_trace.as_mut() else {
            return;
        };
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let f = trace.get_mut().unwrap();
        let _ = writeln!(f, "{ts} {msg}");
    }

    /// One-line render of a build key for the trace log.
    fn fmt_key(key: &crate::transcript_worker::BuildKey) -> String {
        format!(
            "ev={} w={} ext={} frac={} fold={} run={}",
            key.events_version,
            key.width,
            key.ext_ver,
            key.frac_epoch,
            key.turn_fold_epoch,
            key.loop_running
        )
    }

    /// Dispatch a recorded transcript rebuild to the worker
    /// (docs/tui-perf-background-build-plan.md, stage 2).
    ///
    /// The main loop calls this before each draw. At most one build
    /// is in flight. The snapshot is taken here on the main thread
    /// because the ext replies are pre-resolved against the host,
    /// which is not `Send`.
    ///
    /// Without a worker, or when the send fails on a dead worker,
    /// the build runs on the main thread as the fallback. Returns
    /// true when a build was queued or run.
    pub fn dispatch_transcript_build(
        &mut self,
        ext: Option<&crate::ext::ExtHost>,
    ) -> bool {
        if !self.transcript_rebuild_requested || self.transcript_build_in_flight {
            return false;
        }
        let Some(key) = self.transcript_desired_key.clone() else {
            self.transcript_rebuild_requested = false;
            return false;
        };
        // Stage 4: width-triggered misses carry a 75 ms trailing
        // window. Each new width value reset the deadline. While the
        // window is open, hold the dispatch and retry on the next
        // frame. Event-commit misses carry no deadline and build now.
        if let Some(deadline) = self.transcript_width_debounce {
            if Instant::now() < deadline {
                return false;
            }
        }
        self.transcript_width_debounce = None;
        // The pending key may now match the last built cache: the
        // user toggled back to a width we already hold. The pending
        // width is compared to the last built width, not the last
        // observed width. A settled width that arrived after a build
        // still misses here, so it still builds.
        if self.transcript_cache_matches(&key) && !self.transcript_partial {
            self.trace_transcript(&format!("cancel cache-match {}", Self::fmt_key(&key)));
            self.transcript_rebuild_requested = false;
            self.transcript_desired_key = None;
            return false;
        }
        let input = crate::render::TranscriptBuildInput::from_app(self, key.width, ext);
        let Some(worker) = self.transcript_worker.as_ref() else {
            self.trace_transcript(&format!("dispatch main-thread {}", Self::fmt_key(&key)));
            self.store_transcript_build(
                &key,
                &crate::render::build_transcript(self, key.width, ext),
                false,
            );
            self.transcript_rebuild_requested = false;
            self.transcript_desired_key = None;
            return true;
        };
        self.transcript_build_seq += 1;
        let seq = self.transcript_build_seq;
        // The key moves into the request. Clone it so the dead-worker
        // fallback below still reads it.
        match worker.send(crate::transcript_worker::BuildRequest {
            seq,
            key: key.clone(),
            input,
        }) {
            Ok(()) => {
                self.transcript_build_in_flight = true;
                self.transcript_rebuild_requested = false;
                self.transcript_dispatched_at = Some(Instant::now());
                self.trace_transcript(&format!("dispatch worker seq={} {}", seq, Self::fmt_key(&key)));
                true
            }
            Err(_) => {
                // The worker thread died. The failed send is the
                // safety valve: drop the worker and build here.
                self.transcript_worker = None;
                self.transcript_build_in_flight = false;
                self.trace_transcript(&format!("dispatch main-thread(fallback) {}", Self::fmt_key(&key)));
                self.store_transcript_build(
                    &key,
                    &crate::render::build_transcript(self, key.width, ext),
                    false,
                );
                self.transcript_rebuild_requested = false;
                self.transcript_desired_key = None;
                true
            }
        }
    }

    /// Poll the worker for finished builds
    /// (docs/tui-perf-background-build-plan.md, stage 2).
    ///
    /// The main loop calls this before each draw. A result matching
    /// the desired key swaps into the cache and clears the request
    /// state. A stale result is dropped and the in-flight bit clears
    /// so the next dispatch re-requests.
    pub fn poll_transcript_worker(&mut self) {
        let (results, worker_gone) = {
            let Some(worker) = self.transcript_worker.as_ref() else {
                return;
            };
            let mut results: Vec<crate::transcript_worker::BuildResult> = Vec::new();
            let mut gone = false;
            loop {
                match worker.try_recv_result() {
                    Ok(r) => results.push(r),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        gone = true;
                        break;
                    }
                }
            }
            (results, gone)
        };
        if worker_gone {
            // The worker thread died. The next dispatch falls back
            // to the main-thread build.
            self.transcript_worker = None;
            self.transcript_build_in_flight = false;
        }
        for result in results {
            self.transcript_build_in_flight = false;
            if self.transcript_desired_key.as_ref() == Some(&result.key) {
                if let Some(at) = self.transcript_dispatched_at.take() {
                    let ms = at.elapsed().as_millis();
                    self.trace_transcript(&format!(
                        "settle {} in {} ms",
                        Self::fmt_key(&result.key),
                        ms
                    ));
                }
                self.store_transcript_build(&result.key, &result.build, false);
                self.transcript_rebuild_requested = false;
                self.transcript_desired_key = None;
                self.transcript_width_debounce = None;
            }
            // A result whose key no longer matches the desired key is
            // dropped: the newer miss owns the rebuild.
        }
    }

    /// True while a transcript rebuild is recorded or a build is in
    /// flight (docs/tui-perf-background-build-plan.md, stage 2).
    /// The rebuilding indicator shows while this is true.
    pub fn transcript_rebuilding(&self) -> bool {
        self.transcript_rebuild_requested || self.transcript_build_in_flight
    }

    /// True while the cached transcript is a partial tail build.
    /// Browse yank and `gg` stay disabled until the full build
    /// lands.
    pub fn transcript_partial(&self) -> bool {
        self.transcript_partial
    }

    /// The per-line display text of the settled transcript (docs/tui-
    /// conversation-browsing.md section 11.3). Cached in step with
    /// [`Self::transcript_lines`] so the browse layout can store it
    /// instead of re-stringifying every line on each frame.
    pub fn transcript_texts(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> &Vec<String> {
        let _lines = self.transcript_lines(width, ext);
        &self.transcript_cache.as_ref().unwrap().texts
    }

    /// The per-line raw source map a browse yank reads (docs/tui-
    /// conversation-browsing.md section 11.3): `line_raw[i]` is the
    /// shareable source for rendered line `i` (`None` on separators
    /// and UI chrome). Sourced from the transcript cache which
    /// `transcript_lines` keeps in step with it.
    pub fn transcript_raw(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> Vec<Option<String>> {
        let _lines = self.transcript_lines(width, ext);
        self.transcript_cache.as_ref().unwrap().line_raw.clone()
    }

    /// Tool-result block spans (event ID → start/end transcript line
    /// indices, exclusive end) for the current cached transcript.
    pub fn transcript_block_spans(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> std::collections::HashMap<String, (usize, usize)> {
        let _ = self.transcript_lines(width, ext);
        self.transcript_cache.as_ref().unwrap().block_spans.clone()
    }

    /// The first rendered transcript line of each in-memory event
    /// (docs/tree-ui-design-from-human.md view-only scroll):
    /// `event_line_starts[i]` is where in-memory event `i` begins in
    /// the cached transcript, `None` for suppressed or out-of-window
    /// events. Sourced from the transcript cache which
    /// `transcript_lines` keeps in step with it.
    pub fn transcript_event_line_starts(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> Vec<Option<usize>> {
        let _ = self.transcript_lines(width, ext);
        self.transcript_cache.as_ref().unwrap().event_line_starts.clone()
    }

    /// Events of the active session, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// 1-based log seq of the first in-memory event. Rewind markers and
    /// the active-path mask use it to map a window index to the log seq
    /// that target_seq requires.
    pub fn events_base_seq(&self) -> usize {
        self.events_base_seq
    }

    /// The active-path ranges of the current log (docs/rewind-fork-
    /// design.md section 3), or None when no rewind marker exists. The
    /// marker events are parsed from the in-memory window using the
    /// shared kernel parser; the ranges are in 1-based log seq.
    pub fn rewind_active_ranges(&self) -> Option<Vec<(usize, usize)>> {
        use rushi_common::rewind;
        let base = self.events_base_seq;
        let mut refs = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            if let Some(obj) = e.obj() {
                if let Some(ref_) = rewind::parse_rewind_event(obj, base + i) {
                    refs.push(ref_);
                }
            }
        }
        if refs.is_empty() {
            return None;
        }
        let end = base + self.events.len() - 1;
        Some(rewind::active_ranges(end, &refs))
    }

    /// Consume the pending one-shot scroll-to-event target (the in-memory
    /// event index to bring to the top of the viewport). The draw reads it
    /// and pins the viewport there; the sticky scroll then holds until the
    /// user scrolls (docs/tree-ui-design-from-human.md View-only).
    pub fn take_view_only_target(&mut self) -> Option<usize> {
        let target = self.view_only_target;
        self.view_only_target = None;
        target
    }


    /// Map tool_call id -> (name, arguments), for result rendering.
    /// The arguments are the call arguments verbatim: the write
    /// diff of docs/tui-tool-result-truncation.md needs the
    /// `content` argument of the write call, and the render holds
    /// it next to the result line (pure presentation lookup, not
    /// decision logic).
    pub fn call_details(&self) -> HashMap<String, (String, Value)> {
        let mut m: HashMap<String, (String, Value)> = HashMap::new();
        for e in &self.events {
            if e.kind() == EventKind::ToolCall {
                if let Some(id) = e.get_str("id") {
                    let name = e.get_str("name").unwrap_or("").to_string();
                    let args = e.get("arguments").cloned().unwrap_or(Value::Null);
                    m.insert(id.to_string(), (name, args));
                }
            }
        }
        m
    }

    /// Latest `ext_status` values, id to value, for the active
    /// session. The extension host sends this map in every `tick`
    /// op, so a statusline consumes shared UI state through the log
    /// (docs/ui-extension.md section 5). The map is maintained
    /// incrementally: each watch event updates it, so a tick reads
    /// O(1) state instead of a whole-log scan.
    pub fn ext_statuses(&self) -> &HashMap<String, Value> {
        &self.ext_status_values
    }

    /// Record one ext_status value in log order. A later event for
    /// the same id wins. The id set is capped:
    /// [`EXT_STATUS_ID_CAP`] distinct ids, oldest-updated first out.
    /// The timestamp side map follows: it records the event `ts` and
    /// drops an id with the value map.
    fn record_ext_status(&mut self, id: &str, value: Value, ts: Option<&str>) {
        if self
            .ext_status_values
            .insert(id.to_string(), value)
            .is_none()
        {
            self.ext_status_order.push_back(id.to_string());
            while self.ext_status_order.len() > EXT_STATUS_ID_CAP {
                let old = self
                    .ext_status_order
                    .pop_front()
                    .expect("the cap keeps the order non-empty");
                self.ext_status_values.remove(&old);
                self.ext_status_ts.remove(&old);
            }
        } else if let Some(pos) = self.ext_status_order.iter().position(|x| x == id) {
            self.ext_status_order.remove(pos);
            self.ext_status_order.push_back(id.to_string());
        }
        if let Some(t) = ts {
            self.ext_status_ts.insert(id.to_string(), t.to_string());
        }
    }

    /// The raw `ts` of the event that last set the [`LOOP_PHASE_STATUS_ID`]
    /// value of the active session. `None` when the log holds no
    /// marker, or the marker event carries no timestamp. The render
    /// parses the value with chrono; a parse failure hides the
    /// phase row, the title bit still shows the state
    /// (docs/tui-model-wait-indicator.md section 4).
    pub fn loop_phase_ts(&self) -> Option<&str> {
        self.ext_status_ts
            .get(LOOP_PHASE_STATUS_ID)
            .map(|s| s.as_str())
    }

    // ── thinking level ─────────────────────────────────────────

    /// The model's thinking level, 0 (none) through
    /// [`THINKING_LEVELS`] (the highest published). The level is
    /// published into the log as an `ext_status` event with id
    /// `model_thinking` (the shared-UI-state channel, docs/ui-
    /// extension.md section 5); a value outside the known range or a
    /// missing event falls back to the default. The input area's
    /// border color correlates to this value.
    pub fn thinking_level(&self) -> u32 {
        let v = self
            .ext_status_values
            .get(THINKING_STATUS_ID)
            .and_then(|v| v.as_u64());
        match v {
            Some(n) if (n as u32) < THINKING_LEVELS => n as u32,
            Some(_) => THINKING_LEVELS - 1,
            None => DEFAULT_THINKING_LEVEL,
        }
    }

    // ── scroll ──────────────────────────────────────────────────

    /// Visual lines kept between the viewport top and the end of the
    /// log. 0 = follow the tail.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn set_scroll(&mut self, s: usize) {
        self.scroll = s;
    }

    // ── browse mode (docs/tui-conversation-browsing.md) ──────

    /// The browse state machine (section 4).
    pub fn browse(&mut self) -> &mut crate::browse::Browse {
        &mut self.browse
    }

    /// The browse state, read-only.
    pub fn browse_ref(&self) -> &crate::browse::Browse {
        &self.browse
    }

    /// The browse layout of the last render: `(total, h, texts,`
    /// `line_raw)`. The renderer refreshes it each
    /// frame while browse is active; browse motions and the search read
    /// it (section 4.1). `line_raw[j]` is the shareable raw source
    /// for rendered line `j` (`None` on separators and UI chrome),
    /// used by the raw-source yank (section 11.3).
    pub fn set_browse_layout(
        &mut self,
        total: usize,
        h: usize,
        texts: Vec<String>,
        line_raw: Vec<Option<String>>,
    ) {
        self.browse_layout = Some(BrowseLayout {
            total,
            h,
            texts,
            line_raw,
        });
    }

    /// The browse highlight inputs for one frame: the match-line
    /// cache (cloned: the cache borrows the machine) and the active
    /// match, owned so no app borrow crosses the lines borrow in the
    /// renderer.
    pub fn browse_highlight(
        &mut self,
        total: usize,
    ) -> (std::collections::HashSet<usize>, Option<(usize, usize)>) {
        match &self.browse_layout {
            Some(l) => {
                let (hl, am) = self.browse.highlight_lines(total, l.texts.as_slice());
                (hl.clone(), am)
            }
            None => (std::collections::HashSet::new(), None),
        }
    }

    /// The total of the last browse layout: the gutter width hint of
    /// the width fixpoint (section 4.3).
    pub fn browse_layout_total(&self) -> usize {
        self.browse_layout.as_ref().map(|l| l.total).unwrap_or(0)
    }

    /// The browse entry gate (section 4.2, loosened 2026-09-15): the
    /// editor in normal mode with no name input up. The 2026-09-15
    /// request dropped the empty-draft condition, so a held draft no
    /// longer blocks a double-`s`. The quit gate (`q q`) still needs
    /// an empty draft.
    fn browse_gate_open(&self) -> bool {
        self.pending_name.is_none() && self.editor.mode() == Mode::Normal
    }

    /// One browse-owned key (section 4.4): the key table over the
    /// rendered transcript. The view comes from the last rendered
    /// layout, so a key before the first frame is a no-op.
    fn browse_key(&mut self, key: Key) {
        // The double-`s` exit arm (section 4.2): the shared arm
        // window, the FT-012 mirror. It is suppressed while the
        // command line is open (a `/` or `?` search, a `:N`
        // goto): there `s` is a query character and must reach
        // the typing handler, not the exit arm.
        if let Key::Char('s') = key {
            if !self.browse.typing() {
                match self.ss_arm {
                    Some(at) if at.elapsed() < SS_ARM_TTL => {
                        self.ss_arm = None;
                        self.browse.exit();
                        self.flash("browse left — view kept");
                    }
                    _ => {
                        self.ss_arm = Some(std::time::Instant::now());
                        self.flash("ss: press s again to leave browse");
                    }
                }
                return;
            }
        }
        // While the cached transcript is a partial tail build
        // (docs/tui-perf-background-build-plan.md, stage 2), yank
        // and `gg` reach outside the built cells. Hold them until
        // the full background build lands.
        if self.transcript_partial() && matches!(key, Key::Char('y') | Key::Char('g')) {
            self.flash("building: y and gg wait for the full build");
            return;
        }
        let (total, h) = match &self.browse_layout {
            Some(l) => (l.total, l.h),
            None => return,
        };
        if let Key::Char(c) = key {
            if self.fold_key(c, total, h) {
                return;
            }
        }
        let (texts, line_raw) = match &self.browse_layout {
            Some(l) => (l.texts.as_slice(), l.line_raw.as_slice()),
            None => return,
        };
        let half = self.half_page();
        let mut view = crate::browse::View {
            total,
            h,
            scroll: &mut self.scroll,
            half,
            texts,
            line_raw,
        };
        if let Some(hint) = self.browse.key(key, &mut view, &mut self.registers) {
            self.flash(hint);
        }
        // The OSC 52 host-clipboard write of a browse yank
        // (section 11.3): queued for the host, written to the
        // terminal before the next frame.
        if let Some(esc) = self.browse.take_host_clipboard() {
            self.host_clipboard.push(esc);
        }
    }

    /// The `[tui] clipboard = "unnamed"` flag (section 11.3): a bare
    /// unnamed browse yank also emits the OSC 52 host-clipboard
    /// write. Set once at config load.
    pub fn set_clipboard_unnamed(&mut self, on: bool) {
        self.browse.set_clipboard_unnamed(on);
    }

    /// The pending OSC 52 host-clipboard escapes (section 11.3),
    /// drained in order. The host writes each to the terminal.
    pub fn drain_host_clipboard(&mut self) -> Vec<String> {
        std::mem::take(&mut self.host_clipboard)
    }

    /// The picker state machine, read-only.
    pub fn picker_ref(&self) -> &crate::picker::state::PickerState {
        &self.picker
    }

    /// The picker state machine, mutable. The renderer reads it; the
    /// key handler mutates it through the public method on the state.
    pub fn picker(&mut self) -> &mut crate::picker::state::PickerState {
        &mut self.picker
    }

    /// The background ranker, for the renderer to read the latest
    /// snapshot without blocking.
    pub fn picker_matcher_ref(&self) -> &Option<crate::picker::fuzzy::PickerMatcher> {
        &self.picker_matcher
    }

    /// Open the picker: enumerate files for the seed query's root,
    /// spawn the matcher, open the state, and push the initial query.
    fn open_picker(&mut self) {
        if self.picker.open {
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        // The source is a `FileItemSource` for now; later sources
        // (symbols, git files) plug in at this seam
        // (docs/tui-file-picker.md section 4.5).
        let source = crate::picker::items::FileItemSource::new(cwd.clone());
        // Seed the query from the editor's `@` token if present.
        let (at_col, query) = match self.editor().at_token_info() {
            Some(info) => info,
            None => return,
        };
        self.picker_at = Some((self.editor().row, at_col));
        // Path queries (`/abs`, `~/x`, `../y`) re-root the search;
        // plain queries stay under the cwd.
        let (root, tail) = crate::picker::items::query_root_tail(&query, &cwd);
        let items = source.collect_in(&root, self.picker.scope);
        let matcher = crate::picker::fuzzy::PickerMatcher::new(items);
        self.picker.open(&query, 10);
        if !tail.is_empty() {
            matcher.query_ranked(&query, &tail);
        }
        self.picker_source = Some(source);
        self.picker_root = root;
        self.picker_matcher = Some(matcher);
    }

    /// Keep the picker in sync with the editor `@query` token:
    /// - token present and picker open: refresh the query in the
    ///   matcher (re-roots when the query crosses into a new path).
    /// - token gone and picker open: close the picker (the draft keeps
    ///   the text as typed; nothing is replaced).
    ///
    /// The picker only *opens* in direct response to a freshly typed
    /// `@` keypress (see the key handler below); a stale `@` left in
    /// the draft after a previous pick or dismiss is inert and does
    /// not re-trigger the overlay.
    fn sync_picker(&mut self) {
        if !self.picker.open {
            return;
        }
        let (row, at_col) = match self.picker_at {
            Some(p) => p,
            None => {
                self.picker.close();
                self.picker_matcher = None;
                self.picker_source = None;
                return;
            }
        };
        // Verify the `@` is still at the tracked position.
        let chars: Vec<char> = match self.editor.lines.get(row) {
            Some(line) => line.chars().collect(),
            None => {
                self.picker.close();
                self.picker_matcher = None;
                self.picker_source = None;
                self.picker_at = None;
                return;
            }
        };
        if chars.get(at_col) != Some(&'@') {
            // The `@` was deleted or the line changed; close the picker.
            self.picker.close();
            self.picker_matcher = None;
            self.picker_source = None;
            self.picker_at = None;
            return;
        }
        // Compute the query: text from just after the `@` to the cursor.
        let cursor_col = self.editor.col.min(chars.len());
        let query = if cursor_col > at_col + 1 {
            chars[at_col + 1..cursor_col].iter().collect()
        } else {
            String::new()
        };
        self.picker.query = query.clone();
        self.update_picker_ranker(&query);
    }

    /// Re-root and re-rank the picker when the `@` query changes.
    /// Path queries (`/abs`, `~/x`, `../y`) move the search root;
    /// a changed root re-enumerates the item list before the rank.
    fn update_picker_ranker(&mut self, query: &str) {
        let Some(src) = self.picker_source.as_ref() else {
            return;
        };
        let base = src.base().to_path_buf();
        let (root, tail) = crate::picker::items::query_root_tail(query, &base);
        let need_replace = root != self.picker_root;
        let new_items = if need_replace {
            Some(src.collect_in(&root, self.picker.scope))
        } else {
            None
        };
        if let Some(m) = &mut self.picker_matcher {
            if let Some(items) = new_items {
                m.replace_items(items);
                self.picker_root = root;
            }
            m.query_ranked(query, &tail);
        }
    }

    /// Re-enumerate the picker's item list after `Ctrl+I` / `Tab`
    /// cycled the file scope (docs/tui-file-picker.md P9): collect the
    /// current search root at the new scope, swap the matcher's list,
    /// and re-rank the live query against the fresh list. The flash
    /// line names the new mode so the user sees the switch without
    /// inspecting the list.
    fn recollect_picker_items(&mut self) {
        let Some(src) = self.picker_source.as_ref() else {
            return;
        };
        let base = src.base().to_path_buf();
        let scope = self.picker.scope;
        let items = src.collect_in(&self.picker_root, scope);
        let (_, tail) = crate::picker::items::query_root_tail(&self.picker.query, &base);
        if let Some(m) = &mut self.picker_matcher {
            m.replace_items(items);
            m.query_ranked(&self.picker.query, &tail);
        }
        self.flash(scope.hint());
    }

    /// Commit the picker. The chosen item's value replaces the
    /// `@query` token, prefixed with `@`. The model sees `@path` as
    /// an explicit file reference. With zero results the `@` token
    /// and query text stay in the draft, so no stripping happens.
    /// The caller closed the state already, so this drops the
    /// matcher. The caret stays at the replacement end.
    fn commit_picker(&mut self, sel: Option<usize>) {
        match sel {
            Some(idx) => {
                if let Some(m) = &self.picker_matcher {
                    let snap = m.snapshot();
                    if let Some(item) = snap.items.get(idx) {
                        if let Some((_, at_col)) = self.picker_at {
                            // Keep the `@` so the model receives an
                            // unambiguous file-reference marker.
                            self.editor()
                                .replace_at_token(at_col, &format!("@{}", item.value));
                        }
                    }
                }
            }
            None => {
                // Zero results: keep the `@` and query text as-is in
                // the draft. The user can keep editing or delete the
                // text manually.
            }
        }
        self.picker_matcher = None;
        self.picker_at = None;
    }

    // ── command palette ────────────────────────────────────────

    pub fn palette_state(&self) -> &crate::palette::state::PaletteState {
        &self.palette_state
    }

    pub fn palette_state_mut(&mut self) -> &mut crate::palette::state::PaletteState {
        &mut self.palette_state
    }

    /// Open the `:` command palette. The pending_name bar takes
    /// priority: if it is active the palette does not open.
    pub fn open_palette(&mut self) {
        if self.pending_name.is_some() {
            return;
        }
        self.palette_state.open(self.viewport.saturating_sub(4).max(5));
        self.palette_cmd_requested = true;
    }

    /// The full un-rank palette item list (built-ins + extension commands).
    pub fn palette_items(&self) -> Vec<crate::palette::items::PaletteItem> {
        let mut items = crate::palette::items::builtins(&self.effort_current);
        items.extend(self.ext_commands.iter().cloned());
        items
    }

    /// The ranked palette items for the current stage and query.
    pub fn palette_ranked(&self) -> Vec<crate::palette::items::PaletteItem> {
        let state = &self.palette_state;
        match state.stage {
            crate::palette::state::PaletteStage::SessionList => {
                let filter = state.filter_query();
                let items: Vec<crate::palette::items::PaletteItem> = self
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(i, sid)| {
                        let is_active = self.active.as_ref() == Some(sid);
                        let running = self.loop_running(sid);
                        let mut help = String::new();
                        help.push_str(&format!("recency {} of {}\n", i + 1, self.sessions.len()));
                        help.push_str(&format!(
                            "active: {}\n",
                            if is_active { "yes" } else { "no" }
                        ));
                        help.push_str(&format!(
                            "loop: {}\n",
                            if running { "running" } else { "stopped" }
                        ));
                        crate::palette::items::PaletteItem {
                            id: sid.as_str().to_string(),
                            label: sid.as_str().to_string(),
                            kind: crate::palette::items::CmdKind::Goto,
                            hint: if is_active { "active".to_string() } else { String::new() },
                            help,
                            options: Vec::new(),
                            ext: None,
                        }
                    })
                    .collect();
                // Rank by the filter portion of the query (text after the goto prefix).
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                let ranked = crate::picker::fuzzy::rank_fuzzy(&labels, filter);
                ranked
                    .into_iter()
                    .map(|i| items[i].clone())
                    .collect()
            }
            // The tree sub-list: the active session's events, fuzzy-
            // searchable (docs/tree-ui-design-from-human.md).
            crate::palette::state::PaletteStage::TreeList => {
                self.tree_event_items(state.filter_query())
            }
            // The four outcome options for a tree-picked event. Fixed
            // order; the query does not rank them.
            crate::palette::state::PaletteStage::TreeOptions => {
                self.tree_option_items()
            }
            // The root command list.
            crate::palette::state::PaletteStage::Root => {
                let items = self.palette_items();
                let labels: Vec<String> = items.iter().map(|i| {
                    format!("{} {}", i.label, i.hint)
                })
                .collect();
                let query = state.filter_query();
                let ranked = crate::picker::fuzzy::rank_fuzzy(&labels, query);
                ranked
                    .into_iter()
                    .map(|i| items[i].clone())
                    .collect()
            }
        }
    }

    /// The tree sub-list items (docs/tree-ui-design-from-human.md): the
    /// active session's events, one line each, type-tagged and
    /// fuzzy-ranked by `filter`. Each item's `id` is the event's 1-based
    /// log seq. ExtStatus events are skipped (they add no transcript row).
    fn tree_event_items(
        &self,
        filter: &str,
    ) -> Vec<crate::palette::items::PaletteItem> {
        use crate::palette::items::{CmdKind, PaletteItem};
        use crate::picker::fuzzy::rank_fuzzy;
        let events = self.events();
        // The transcript has no render cap, so every in-memory event
        // maps to a rendered line and the whole history is reachable.
        let start = 0;
        let mut candidates: Vec<(usize, String)> = Vec::new();
        for (i, e) in events[start..].iter().enumerate() {
            let i = start + i;
            if e.kind() == EventKind::ExtStatus {
                continue;
            }
            candidates.push((i, tree_row_label(e)));
        }
        let labels: Vec<String> = candidates.iter().map(|(_, l)| l.clone()).collect();
        let ranked = rank_fuzzy(&labels, filter);
        let base = self.events_base_seq;
            ranked
            .into_iter()
            .map(|i| {
                let idx = candidates[i].0;
                let label = labels[i].clone();
                let seq = base + idx;
                let picked = &events[idx];
                let help = format!(
                    "{}\n\nEnter offers the four options (View-only, Rewind without summary, Summarize the branch, Summarize with custom prompt).",
                    tree_event_body(picked)
                );
                PaletteItem {
                    id: seq.to_string(),
                    label,
                    kind: CmdKind::Goto,
                    hint: format!("#{seq}"),
                    help,
                    options: Vec::new(),
                    ext: None,
                }
            })
            .collect()
    }

    /// The four outcome options for a tree-picked event (docs/tree-ui-
    /// design-from-human.md). View-only and Rewind without summary are
    /// wired; the two summarize options flash that the kernel compact
    /// flags are pending.
    fn tree_option_items(&self) -> Vec<crate::palette::items::PaletteItem> {
        use crate::palette::items::{CmdKind, PaletteItem};
        vec![
            PaletteItem {
                id: "view-only".into(),
                label: "View-only".into(),
                kind: CmdKind::Run,
                hint: String::new(),
                help: "Scroll the viewport to this event. No rewind marker, no fork, no state change. Allowed while the loop runs.".into(),
                options: Vec::new(),
                ext: None,
            },
            PaletteItem {
                id: "rewind-no-summary".into(),
                label: "Rewind without summary".into(),
                kind: CmdKind::Run,
                hint: String::new(),
                help: "Append a rewind marker here (reason tui_pick). The abandoned branch drops out of the transcript; the fork marker stays. Requires the loop to be idle.".into(),
                options: Vec::new(),
                ext: None,
            },
            PaletteItem {
                id: "summarize-branch".into(),
                label: "Summarize the branch".into(),
                kind: CmdKind::Run,
                hint: "pending kernel".into(),
                help: "Append the rewind marker and run bin/compact --up-to. Pending the kernel compact flag.".into(),
                options: Vec::new(),
                ext: None,
            },
            PaletteItem {
                id: "summarize-custom".into(),
                label: "Summarize with custom prompt".into(),
                kind: CmdKind::Run,
                hint: "pending kernel".into(),
                help: "Append the rewind marker and run bin/compact --up-to --prompt. Pending the kernel compact flag.".into(),
                options: Vec::new(),
                ext: None,
            },
        ]
    }

    /// Handle the `Enter` key when the palette is open. Returns the
    /// actions to execute (if any). The caller (press) dispatches
    /// them through the main loop.
    pub fn commit_palette(&mut self) -> Vec<Action> {
        let state = self.palette_state();
        let ranked = self.palette_ranked();
        let cursor = state.cursor().min(ranked.len().saturating_sub(1));
        let item = match ranked.get(cursor) {
            Some(item) => item,
            None => {
                self.palette_state_mut().close();
                return Vec::new();
            }
        };
        use crate::palette::items::CmdKind;
        match item.kind {
            CmdKind::Goto => {
                // Goto items transition the palette sub-stage
                // (docs/tree-ui-design-from-human.md, docs/tui-command-
                // palette.md).
                match state.stage {
                    crate::palette::state::PaletteStage::Root => {
                        // The `tree` item opens the event tree. Every
                        // other Goto item opens the session list.
                        if item.id == "tree" {
                            self.palette_state_mut().goto_tree_list();
                        } else {
                            self.palette_state_mut().goto_session_list();
                        }
                        Vec::new()
                    }
                    crate::palette::state::PaletteStage::TreeList => {
                        // A tree event was picked: show the four options.
                        let seq = item.id.parse::<usize>().unwrap_or(0);
                        self.palette_state_mut().goto_tree_options(seq);
                        Vec::new()
                    }
                    // The session sub-list switches sessions. TreeOptions
                    // carries no Goto items, so this guards a stray pick.
                    crate::palette::state::PaletteStage::SessionList
                    | crate::palette::state::PaletteStage::TreeOptions => {
                        self.palette_state_mut().close();
                        vec![Action::SwitchSession(item.id.clone())]
                    }
                }
            }
            CmdKind::Run => {
                // Tree outcome options (View-only / Rewind without summary
                // / the two pending summarize options) are committed
                // through their own path, which owns the close.
                if state.stage == crate::palette::state::PaletteStage::TreeOptions {
                    return self.commit_tree_option(item.id.as_str());
                }
                self.palette_state_mut().close();
                match item.id.as_str() {
                    "toggle-tools" => {
                        self.tool_expanded = !self.tool_expanded;
                        self.events_version += 1;
                        vec![Action::ToggleToolExpand]
                    }
                    "toggle-thinking" => {
                        self.thinking_shown = !self.thinking_shown;
                        self.events_version += 1;
                        vec![Action::ToggleThinking]
                    }
                    "expand-thinking" => {
                        self.thinking_expanded = !self.thinking_expanded;
                        self.events_version += 1;
                        vec![Action::ToggleThinkingExpand]
                    }
                    "bn" => vec![Action::CycleSessions(1)],
                    "bp" => vec![Action::CycleSessions(-1)],
                    "new-session" => {
                        self.start_naming();
                        Vec::new()
                    }
                    "edit-queue" => vec![Action::RecallQueue],
                    "e" => vec![Action::OpenEditor],
                    "q" => vec![Action::Quit],
                    _ => Vec::new(),
                }
            }
            CmdKind::Set => {
                // Pick the option at option_cursor.
                let opts = &item.options;
                let idx = state
                    .option_cursor
                    .min(opts.len().saturating_sub(1));
                let value = opts.get(idx).map(|o| o.value.clone()).unwrap_or_default();
                self.palette_state_mut().close();
                match item.id.as_str() {
                    "thinking-level" => vec![Action::SetEffort(value)],
                    _ => Vec::new(),
                }
            }
            CmdKind::Ext => {
                let value = if item.options.is_empty() {
                    None
                } else {
                    let opts = &item.options;
                    let idx = state
                        .option_cursor
                        .min(opts.len().saturating_sub(1));
                    opts.get(idx)
                        .map(|o| o.value.clone())
                };
                let ext_name = item.ext.clone().unwrap_or_default();
                // Extract the command id (part after the first dot).
                let cmd_id = item.id.split('.').nth(1).unwrap_or(&item.id).to_string();
                self.palette_state_mut().close();
                vec![Action::InvokeExtCommand {
                    ext: ext_name,
                    id: cmd_id,
                    value,
                }]
            }
        }
    }

    /// Commit a tree outcome option (docs/tree-ui-design-from-human.md).
    /// The picked event is remembered in the palette state as a 1-based
    /// log seq. View-only and Rewind without summary are wired; the two
    /// summarize outcomes flash that the kernel compact flags are pending.
    fn commit_tree_option(&mut self, id: &str) -> Vec<Action> {
        let seq = self.palette_state().tree_seq().unwrap_or(0);
        let base = self.events_base_seq();
        // The current in-memory index of the picked event (log seq minus
        // the window base). The window may have front-trimmed since the
        // pick, so clamp into the current window.
        let idx = seq
            .saturating_sub(base)
            .min(self.events().len().saturating_sub(1));
        let picked = self.events().get(idx);
        match id {
            "view-only" => {
                // Navigation only: no marker, no fork, no state change.
                // Allowed while the loop runs.
                self.palette_state_mut().close();
                self.view_only_target = Some(idx);
                vec![Action::TreeViewOnly]
            }
            "rewind-no-summary" => {
                // The fork outcomes need an idle loop so the marker lands
                // cleanly. If busy, show the hint and keep the options open.
                let busy = self
                    .active
                    .as_ref()
                    .is_some_and(|sid| self.loop_running(sid));
                if busy {
                    self.flash("loop busy, wait for the step");
                    return Vec::new();
                }
                // A user-message target uses `before` (the message waits in
                // the input box, unsent). Every other target uses `on`.
                let mode = match picked.map(|e| e.kind()) {
                    Some(EventKind::UserMessage) => "before",
                    _ => "on",
                };
                let restore_text = if mode == "before" {
                    picked.and_then(|e| e.get_str("content")).map(String::from)
                } else {
                    None
                };
                self.palette_state_mut().close();
                // Go to the picked event (docs/tree-ui-design-from-human.md):
                // scroll the viewport to it once the marker lands.
                self.view_only_target = Some(idx);
                vec![Action::RewindNoSummary {
                    target_seq: seq as u64,
                    mode: mode.to_string(),
                    restore_text,
                }]
            }
            "summarize-branch" | "summarize-custom" => {
                // Not wired yet: the kernel bin/compact gains the
                // --up-to / --prompt flags. Keep the options open so the
                // user can fall back to View-only or Rewind without summary.
                self.flash("summarize modes are pending the kernel compact flags");
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn set_ext_commands(&mut self, items: Vec<crate::palette::items::PaletteItem>) {
        self.ext_commands = items;
    }

    pub fn drop_ext_commands(&mut self, ext_name: &str) {
        self.ext_commands.retain(|i| i.ext.as_deref() != Some(ext_name));
    }

    pub fn set_effort_current(&mut self, effort: String) {
        self.effort_current = effort;
    }

    /// Update the in-memory thinking level so the input-area border
    /// colour reflects the new value immediately (docs/tui-thinking-
    /// block.md section 4).
    pub fn set_thinking_level(&mut self, level: u32) {
        let value = serde_json::Value::from(level);
        self.record_ext_status(THINKING_STATUS_ID, value, None);
    }

    pub fn palette_cmd_requested(&mut self) -> bool {
        let requested = self.palette_cmd_requested;
        self.palette_cmd_requested = false;
        requested
    }

    /// Auto-enter the Goto sub-stage when the user types a space
    /// after a Goto command label in the Root stage. This makes the
    /// natural ":b <session-name>" flow work without a separate
    /// Enter press (docs/tui-command-palette.md section 7).
    pub fn maybe_autogoto(&mut self) {
        let st = &self.palette_state;
        if !st.open || st.stage != crate::palette::state::PaletteStage::Root {
            return;
        }
        let query = st.query.clone();
        let space_pos = match query.find(' ') {
            Some(p) => p,
            None => return,
        };
        let token = &query[..space_pos];
        let items = crate::palette::items::builtins(&self.effort_current);
        let is_tree = items
            .iter()
            .any(|i| i.kind == crate::palette::items::CmdKind::Goto && i.label == token && i.id == "tree");
        let is_goto = items.iter().any(|i| {
            i.kind == crate::palette::items::CmdKind::Goto && i.label == token
        });
        if is_tree {
            self.palette_state_mut().goto_tree_list();
        } else if is_goto {
            self.palette_state_mut().goto_session_list();
        }
    }

    /// Bulk recall: load the combined text of all pending user
    /// messages into the editor, move the cursor to the end, and
    /// switch to insert mode (docs/user-message-editing.md).
    /// Returns the ids of the retracted messages so the caller can
    /// append `user_message_retract` events to the log.
    pub fn recall_queue(&mut self) -> Vec<String> {
        let pending: Vec<(String, String, Option<String>)> = self
            .pending_user_messages()
            .into_iter()
            .map(|e| {
                let queue = e.get_str("queue").unwrap_or("steer").to_string();
                let content = e.get_str("content").unwrap_or("").to_string();
                let id = e.get_str("id").map(String::from);
                (queue, content, id)
            })
            .collect();
        if pending.is_empty() {
            self.flash("no pending messages to recall");
            return Vec::new();
        }
        // Build the joined text with queue markers.
        let blocks: Vec<String> = pending
            .iter()
            .map(|(queue, content, _)| format!("[{queue}] {content}"))
            .collect();
        let combined = blocks.join("\n\n");
        let n = pending.len();
        self.editor().set_text(&combined);
        // Move the cursor to the end of the text.
        let ed = self.editor();
        let last_row = ed.lines.len().saturating_sub(1);
        ed.row = last_row;
        ed.col = ed.lines[last_row].chars().count();
        ed.mode = crate::vim_editor::Mode::Insert;
        self.flash(format!(
            "recalled {n} queued message(s) into the editor"
        ));
        pending.into_iter().filter_map(|(_, _, id)| id).collect()
    }

    // ── draft / editor ──────────────────────────────────────────

    /// The message editor: multi-line textarea plus the vim modal
    /// state machine (docs/tui.md section 7.1; modes
    /// normal/insert/replace/visual, operator-pending).
    pub fn editor(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// One editor key over the shared register store
    /// (docs/tui-conversation-browsing.md section 11.3): the editor
    /// and the browse overlay both run against `self.registers`, so
    /// a browse `y` lands where the editor's `p` reads. The two
    /// disjoint field borrows keep the call borrow-checker clean.
    pub fn editor_press(&mut self, key: Key) -> Option<String> {
        let registers = &mut self.registers;
        let editor = &mut self.editor;
        editor.press(key, registers)
    }

    /// The editor's scroll window, so the renderer keeps the cursor
    /// line in view.
    pub fn edit_scroll(&self) -> usize {
        self.edit_scroll
    }

    /// How many display rows the draft wraps to at `width` columns
    /// (at least 1). The renderer sizes the input box to this so a
    /// long line wraps to the box instead of running off the edge, and
    /// a multi-line message is shown in full, not just a two-line
    /// scroll window.
    pub fn draft_lines(&self, width: usize) -> usize {
        self.editor.display_row_count(width)
    }

    /// Scroll the editor window so the cursor row is inside a window
    /// of `height` display rows. A short draft keeps scroll 0; a long
    /// one follows the cursor. `width` is the box interior width, so a
    /// wrapped cursor line scrolls on display rows, not logical lines.
    pub fn editor_scroll_to_cursor(&mut self, height: usize, width: usize) {
        let row = self.editor.cursor_display(width).0;
        let window = height.max(1).saturating_sub(1);
        if row <= window {
            self.edit_scroll = 0;
        } else if row.saturating_sub(window) > self.edit_scroll {
            self.edit_scroll = row.saturating_sub(window);
        } else if row < self.edit_scroll {
            self.edit_scroll = row;
        }
        // Never scroll past the end of the text.
        let max_scroll = self.editor.display_row_count(width).saturating_sub(window);
        self.edit_scroll = self.edit_scroll.min(max_scroll);
    }

    /// The draft text as it would be sent: the editor lines joined
    /// with newlines, trimmed.
    pub fn draft(&self) -> String {
        self.editor.text().trim().to_string()
    }

    /// The quit gate (docs/tui.md section 7, FT-012): the quit key
    /// (`q`, `Ctrl+Q`) arms and fires only in normal mode with an
    /// empty draft, like the pi Ctrl-d rule that blocks the exit
    /// key over a live prompt. In every other state `q` is plain
    /// text for the editor.
    fn quit_gate_open(&self) -> bool {
        self.editor.mode() == Mode::Normal && self.editor.text().trim().is_empty()
    }

    /// Take the draft for sending, leaving the editor empty.
    pub fn take_draft(&mut self) -> String {
        let t = self.editor.text().trim().to_string();
        self.editor.clear();
        self.edit_scroll = 0;
        t
    }

    pub fn set_draft(&mut self, text: String) {
        self.editor.set_text(&text);
        self.edit_scroll = 0;
    }

    /// The editor's modal state label, for the status row
    /// (`[NORMAL]`, `[INSERT]`, ...; `[d-PENDING]` while an
    /// operator waits for its motion). Mirrors the pi-vim
    /// `formatStatus` output.
    pub fn editor_mode_label(&self) -> String {
        if let Some(p) = self.editor.pending_label() {
            p
        } else {
            format!("[{}]", self.editor.mode().label())
        }
    }

    // ── handoff ───────────────────────────────────────────────

    /// The pending handoff of the active session's log: the
    /// `new_session` of the last `context_exhausted` event that
    /// seeded a session. `None` when the log holds no marker, or the
    /// last marker seeded none (the summary call failed). A marker
    /// followed by a user message is superseded by that turn's own
    /// marker, closer to the log end.
    pub fn pending_handoff(&self) -> Option<String> {
        self.events
            .iter()
            .rev()
            .find(|e| e.kind() == EventKind::ContextExhausted)
            .and_then(|e| {
                e.get_str("new_session")
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
    }

    // ── pending user messages ─────────────────────────────────

    /// The unconsumed `user_message` events of the active session,
    /// in log order. A message is consumed when the loop answers
    /// it: an `assistant_message` event follows it in the log. A
    /// message sent while the loop is busy stays pending until the
    /// next step answers it (the loop's steering behavior;
    /// docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    /// The derivation is log-only, like
    /// [`App::oldest_pending_approval`]: it survives a TUI restart.
    pub fn pending_user_messages(&self) -> Vec<&Event> {
        let last_answered = self
            .events
            .iter()
            .rposition(|e| e.kind() == EventKind::AssistantMessage);
        let from = last_answered.map(|i| i + 1).unwrap_or(0);
        let retracted: std::collections::HashSet<String> = self
            .events
            .iter()
            .filter(|e| e.kind() == EventKind::UserMessageRetract)
            .filter_map(|e| e.get_str("target").map(String::from))
            .collect();
        self.events[from..]
            .iter()
            .filter(|e| {
                e.kind() == EventKind::UserMessage
                    && e.get_str("id").map(|id| !retracted.contains(id)).unwrap_or(true)
            })
            .collect()
    }

    /// The pending `steer` queue: the unconsumed messages without the
    /// `follow` marker (a missing field means steer; stage 2 of
    /// docs/tui-pending-user-messages.md). They inject at the next
    /// step of the running loop.
    pub fn pending_steering(&self) -> Vec<&Event> {
        self.pending_user_messages()
            .into_iter()
            .filter(|e| e.get_str("queue") != Some("follow"))
            .collect()
    }

    /// The pending `follow` queue: the unconsumed messages marked
    /// `queue: "follow"` (stage 2 of docs/tui-pending-user-messages.
    /// md). They run only after the loop would stop, as new turns.
    pub fn pending_follows(&self) -> Vec<&Event> {
        self.pending_user_messages()
            .into_iter()
            .filter(|e| e.get_str("queue") == Some("follow"))
            .collect()
    }

    // ── approvals ───────────────────────────────────────

    /// Oldest pending `approval_request`, or `None` when the log has no
    /// unanswered request. Derivation is log-only (G6: pending state
    /// survives a TUI restart and is answered from the log alone).
    pub fn oldest_pending_approval(&self) -> Option<PendingApproval> {
        let approvals: Vec<(usize, String)> = self
            .events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                if e.kind() == EventKind::Approval {
                    e.get_str("id").map(|id| (i, id.to_string()))
                } else {
                    None
                }
            })
            .collect();
        for (i, e) in self.events.iter().enumerate() {
            if e.kind() != EventKind::ApprovalRequest {
                continue;
            }
            let Some(request_id) = e.get_str("id").map(|s| s.to_string()) else {
                continue;
            };
            let answered = approvals.iter().any(|(j, id)| *j > i && id == &request_id);
            if answered {
                continue;
            }
            let call_id = e.get_str("call_id").map(|s| s.to_string());
            let arguments = call_id.as_ref().and_then(|cid| {
                self.events
                    .iter()
                    .find(|e| {
                        e.kind() == EventKind::ToolCall && e.get_str("id") == Some(cid.as_str())
                    })
                    .and_then(|e| e.get("arguments").cloned())
            });
            return Some(PendingApproval {
                prompt: e.get_str("prompt").map(|s| s.to_string()),
                request_id,
                call_id,
                arguments,
            });
        }
        None
    }

    // ── loops ───────────────────────────────────────────────────

    pub fn attach_loop(
        &mut self,
        sid: SessionId,
        handle: Box<dyn LoopHandle>,
        lines: tokio::sync::mpsc::UnboundedReceiver<LoopLine>,
    ) {
        let st = self.loops.entry(sid).or_default();
        st.handle = Some(handle);
        st.lines_rx = Some(lines);
        st.running = true;
        st.exit_code = None;
    }

    /// Register a live loop this TUI did not start (FT-003): the
    /// persistent probe found a live group for the session. The state
    /// marks the session running without a local handle. Stop goes
    /// through the port's external path. Idempotent.
    pub fn attach_external_loop(&mut self, sid: SessionId) {
        let st = self.loops.entry(sid).or_default();
        st.running = true;
        st.exit_code = None;
    }

    /// Clear the running flag of one session's loop state. The
    /// persistent probe found no live group (FT-003). A session with
    /// no loop state is a no-op.
    pub fn clear_loop_running(&mut self, sid: &SessionId) {
        if let Some(st) = self.loops.get_mut(sid) {
            st.running = false;
        }
    }

    /// True when the session's loop runs without a local handle: this
    /// TUI did not start it, the persistent probe reattached it.
    pub fn is_external_loop(&self, sid: &SessionId) -> bool {
        self.loop_state(sid)
            .is_some_and(|s| s.running && s.handle.is_none())
    }

    pub fn loop_state(&self, sid: &SessionId) -> Option<&LoopState> {
        self.loops.get(sid)
    }

    pub fn loop_running(&self, sid: &SessionId) -> bool {
        self.loop_state(sid).is_some_and(|s| s.running)
    }

    /// Mutable access to one session's loop state (taking the handle
    /// out on stop).
    pub fn loops_mut_for(&mut self, sid: &SessionId) -> Option<&mut LoopState> {
        self.loops.get_mut(sid)
    }

    pub fn running_loops(&self) -> usize {
        self.loops.values().filter(|s| s.running).count()
    }

    pub fn other_running_loops(&self) -> usize {
        self.running_loops()
            - self
                .active
                .as_ref()
                .map(|a| usize::from(self.loop_running(a)))
                .unwrap_or(0)
    }

    /// Drain loop output lines for every running loop.
    pub fn drain_loop_lines(&mut self) -> Vec<(SessionId, LoopLine)> {
        let mut out = Vec::new();
        for (sid, st) in self.loops.iter_mut() {
            let Some(rx) = st.lines_rx.as_mut() else {
                continue;
            };
            while let Ok(line) = rx.try_recv() {
                match &line {
                    LoopLine::Exited(code) => {
                        st.running = false;
                        st.exit_code = Some(*code);
                    }
                    LoopLine::Stdout(s) | LoopLine::Stderr(s) => {
                        st.last_line = Some(s.clone());
                    }
                }
                out.push((sid.clone(), line));
            }
        }
        out
    }

    /// Take all loop handles so the caller can stop them before exit.
    pub fn detach_all_handles(&mut self) -> Vec<(SessionId, Box<dyn LoopHandle>)> {
        let mut out = Vec::new();
        for (sid, st) in self.loops.iter_mut() {
            if let Some(h) = st.handle.take() {
                out.push((sid.clone(), h));
            }
            st.running = false;
        }
        out
    }

    // ── status line ─────────────────────────────────────────────

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// The transient status message, if it is still fresh.
    pub fn status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|(msg, at)| (at.elapsed() < STATUS_TTL).then_some(msg.as_str()))
    }

    // ── key handling ────────────────────────────────────────────

    /// Handle one key. Returns the port-level actions for `main` to
    /// execute. Keys that only touch local state (typing, scroll)
    /// return an empty list.
    pub fn press(&mut self, key: Key) -> Vec<Action> {
        if self.quitting {
            return Vec::new();
        }
        // Ctrl+Z is a system-level suspend (SIGTSTP): it works in every
        // mode and suspends the whole TUI so the shell can background
        // it. `fg` resumes (docs/tui_feature_requests_from_human.md
        // issue #2).
        if key == Key::CtrlZ {
            return vec![Action::Suspend];
        }
        // Any key other than the second `q` disarms a pending quit.
        if key != Key::Quit {
            self.quit_arm = None;
        }
        // Any key other than the second `s` disarms a pending browse
        // arm (the FT-012 mirror, section 4.2).
        if !matches!(key, Key::Char('s')) {
            self.ss_arm = None;
        }
        // The browse overlay owns its key table while it holds
        // (section 4.4). The host roles fall through: the loop keys,
        // the toggles, `Tab` (which leaves the mode first), and the
        // `q q` quit gate.
        if self.browse.active() {
            match key {
                Key::Tab | Key::BackTab => {
                    // Leave browse (section 4.4). Tab is free for the
                    // picker; no session cycle for now.
                    self.browse.exit();
                    self.ss_arm = None;
                    return Vec::new();
                }
                Key::CtrlC => {
                    // The host stop role: the editor stays frozen
                    // under the overlay, so no session means a
                    // flash instead of an editor insert-exit.
                    if self.active().is_none() {
                        self.flash("no session to stop the loop for");
                        return Vec::new();
                    }
                    return vec![Action::StopLoop];
                }
                Key::CtrlR => {
                    // The host start role (docs/tui.md key Ctrl+R).
                    if self.active().is_none() {
                        self.flash("no session to run the loop for");
                        return Vec::new();
                    }
                    return vec![Action::RunLoop];
                }
                // The unowned keys act in the browse state machine.
                Key::Quit
                | Key::CtrlO
                | Key::CtrlT
                | Key::CtrlX
                | Key::CtrlF
                | Key::CtrlL => {}
                _ => {
                    self.browse_key(key);
                    return Vec::new();
                }
            }
        }
        // The `@` picker overlay owns its key table while open
        // (docs/tui-file-picker.md section 5). The host roles fall
        // through: Ctrl+C, Ctrl+R, and Tab.
        if self.picker.open {
            let count = self.picker_matcher.as_ref()
                .map(|m| m.snapshot().items.len())
                .unwrap_or(0);
            match key {
                // Backspace and printable chars edit the draft: the
                // editor owns the text, and the picker re-syncs its
                // query from the `@` token.
                Key::Backspace => {
                    if let Some(h) = self.editor_press(Key::Backspace) {
                        self.flash(h);
                    }
                    self.sync_picker();
                    return Vec::new();
                }
                Key::CtrlJ | Key::CtrlK => {
                    let _ = self.picker.press(
                        &key,
                        count,
                        crate::picker::render::PREVIEW_PAGE,
                        crate::picker::render::PREVIEW_CUTOFF,
                    );
                    return Vec::new();
                }
                Key::Char(c) => {
                    if let Some(h) = self.editor_press(Key::Char(c)) {
                        self.flash(h);
                    }
                    self.sync_picker();
                    return Vec::new();
                }
                // Host keys pass through even while the picker is open.
                // `Tab` is deliberately not here: in a standard terminal
                // it is the `Ctrl+I` key (both send byte 0x09, which
                // crossterm parses as `KeyCode::Tab`), and the picker
                // binds it to the file-scope cycle (docs/tui-file-picker.md
                // P9), so it reaches the state machine below.
                Key::Quit | Key::CtrlC | Key::CtrlR
                | Key::BackTab => {}
                // Esc, Enter, arrows, paging, preview keys: the state
                // machine decides (docs/tui-file-picker.md section 5).
                _ => {
                    match self.picker.press(
                        &key,
                        count,
                        crate::picker::render::PREVIEW_PAGE,
                        crate::picker::render::PREVIEW_CUTOFF,
                    ) {
                        crate::picker::state::PickAction::Commit(sel) => {
                            self.commit_picker(sel);
                        }
                        crate::picker::state::PickAction::Closed => {
                            self.picker_matcher = None;
                            self.picker_at = None;
                            // No stripping: the `@` stays in the draft
                            // as inert text. The picker only reopens
                            // on a freshly typed `@` keypress.
                            self.picker.close();
                            self.flash("picker closed — draft kept");
                        }
                        crate::picker::state::PickAction::Query => {
                            // Re-rank after an in-state query edit.
                            if let Some(m) = &self.picker_matcher {
                                m.query(&self.picker.query);
                            }
                        }
                        crate::picker::state::PickAction::Move
                        | crate::picker::state::PickAction::ScrollPreview
                        | crate::picker::state::PickAction::TogglePreview
                        | crate::picker::state::PickAction::Nothing => {}
                        crate::picker::state::PickAction::Recollect => {
                            // `Ctrl+I` / `Tab` cycled the file scope
                            // (docs/tui-file-picker.md P9): re-enumerate
                            // the current search root at the new scope,
                            // re-rank the live query, and flash the
                            // scope line so the user sees the switch.
                            self.recollect_picker_items();
                        }
                    }
                    return Vec::new();
                }
            }
            // Ctrl+C, Ctrl+R, and Tab fall through to the normal handler.
        }
        // The `:` command palette owns its key table while open
        // (docs/tui-command-palette.md).
        if self.palette_state.open {
            let items = self.palette_ranked();
            let highlighted = if items.is_empty() { 0 } else {
                let cursor = self.palette_state.cursor().min(items.len() - 1);
                items[cursor].options.len()
            };
            let n = items.len();
            // `q` is mapped to `Key::Quit` in main.rs. Inside the palette,
            // `q` is a filter character (types into the query to match the
            // quit command), not the quit gate. Normalize it to a char.
            let palette_key = if key == Key::Quit {
                Key::Char('q')
            } else {
                key
            };
            let pa = self.palette_state.press(
                &palette_key,
                n,
                highlighted,
                crate::picker::render::PREVIEW_PAGE,
                crate::picker::render::PREVIEW_CUTOFF,
            );
            use crate::palette::state::PaletteAction;
            match pa {
                PaletteAction::Query => {
                    // Auto-transition: when the user types a space
                    // after a Goto item's label in Root stage, enter
                    // the Goto sub-stage so the typed text filters
                    // the session list (docs/tui-command-palette.md
                    // section 7: "The typed text after b filters
                    // the list").
                    self.maybe_autogoto();
                    return Vec::new();
                }
                PaletteAction::Move
                | PaletteAction::OptionMove
                | PaletteAction::ScrollPreview
                | PaletteAction::TogglePreview
                | PaletteAction::Nothing => {
                    return Vec::new();
                }
                PaletteAction::Commit => {
                    return self.commit_palette();
                }
                PaletteAction::DropSubStage => {
                    // Stay open, back to root stage.
                    return Vec::new();
                }
                PaletteAction::Closed => {
                    // Palette closed; fall through to normal handling.
                    return Vec::new();
                }
            }
        }
        // The name input swallows editing keys while it is active.
        // Other keys (q, Ctrl+R, Tab, ...) fall through unchanged.
        if self.pending_name.is_some() {
            match key {
                Key::Char(c) => {
                    self.pending_name.as_mut().unwrap().push(c);
                    return Vec::new();
                }
                Key::Backspace => {
                    self.pending_name.as_mut().unwrap().pop();
                    return Vec::new();
                }
                Key::Esc => {
                    self.pending_name = None;
                    self.flash(
                        "name input cancelled — pass a session argument",
                    );
                    return Vec::new();
                }
                Key::Enter => {
                    let name = self.pending_name.take().unwrap_or_default();
                    if !valid_session_name(&name) {
                        self.pending_name = Some(name);
                        self.flash("invalid session name — plain directory name only");
                        return Vec::new();
                    }
                    return vec![Action::ConfirmNewSession(name)];
                }
                // Tab / BackTab are free: reserved for a future
                // session-navigation design. No-op for now.
                Key::Tab | Key::BackTab => {}
                _ => {}
            }
        }
        match key {
            Key::Quit => {
                // The quit gate (docs/tui.md section 7, FT-012):
                // the key arms and fires only in normal mode with
                // an empty draft, like the pi Ctrl-d rule. In every
                // other state it is a plain `q`: it types into the
                // name input, the search box, or the composer.
                if self.quit_gate_open() {
                    match self.quit_arm {
                        Some(at) if at.elapsed() < QUIT_ARM_TTL => {
                            self.quitting = true;
                            vec![Action::Quit]
                        }
                        _ => {
                            self.quit_arm = Some(Instant::now());
                            self.flash("press q again to quit");
                            Vec::new()
                        }
                    }
                } else if self.pending_name.is_some() {
                    // The name input is up: `q` is a name char.
                    self.pending_name.as_mut().unwrap().push('q');
                    Vec::new()
                } else if self.editor.mode() == Mode::Normal {
                    // Normal mode types nothing: the draft holds
                    // text, so the gate stays closed. Hint the
                    // escape instead of acting.
                    self.flash("clear the draft, then q q quits");
                    Vec::new()
                } else {
                    // A typing mode or the search box: `q` is text.
                    if let Some(h) = self.editor_press(Key::Char('q')) {
                        self.flash(h);
                    }
                    Vec::new()
                }
            }
            Key::Enter => {
                // Enter sends the whole draft; Ctrl-J inserts a
                // newline in it (docs/tui.md: Enter = send, multi-line
                // via Ctrl-J). The naming bar confirms on Enter.
                if self.editor().mode() == Mode::CommandLine {
                    // The search command line runs its query on Enter.
                    if let Some(h) = self.editor_press(Key::Enter) {
                        self.flash(h);
                    }
                    return Vec::new();
                }
                if let Some(name) = self.pending_name().map(str::to_string) {
                    if name.trim().is_empty() {
                        self.flash("empty name — type a session name first");
                        return Vec::new();
                    }
                    return vec![Action::ConfirmNewSession(name)];
                }
                if self.draft().is_empty() {
                    self.flash("empty message — type something first");
                    Vec::new()
                } else if self.active.is_none() {
                    self.flash("no session — pass a session name argument");
                    Vec::new()
                } else {
                    vec![Action::SendDraft]
                }
            }
            Key::CtrlJ => {
                // The multi-line editor's newline key. In insert mode
                // it splits the line; in normal mode it moves down.
                if self.pending_name().is_some() {
                    // The naming bar is single-line: ignore.
                    return Vec::new();
                }
                if let Some(h) = self.editor_press(Key::CtrlJ) {
                    self.flash(h);
                }
                Vec::new()
            }
            Key::CtrlK => {
                // The picker list navigation key (docs/tui-file-picker.md
                // section 5): Ctrl+K moves up in the picker list. The
                // picker handles it when open; when closed it is a no-op.
                Vec::new()
            }
            Key::CtrlI => {
                // The picker file-scope cycle (docs/tui-file-picker.md
                // P9): Ctrl+I cycles standard → ignored → hidden. The
                // picker handles it when open; when closed it is a no-op.
                Vec::new()
            }
            Key::Backspace
            | Key::Delete
            | Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End => {
                // The editor decides what these do in its current
                // mode: in insert they edit or move (the newline key
                // is Ctrl-J; Enter sends the draft); in normal, they
                // are motions.
                if let Some(h) = self.editor_press(key) {
                    self.flash(h);
                }
                self.sync_picker();
                Vec::new()
            }
            Key::CtrlR => {
                // Redo (the pi-vim mapping). The duplicate-start
                // guard is not here: main resolves it through the
                // persistent loop.pid probe (FT-003), so a restarted
                // TUI blocks a second loop for a live session instead
                // of starting one.
                if self.editor().mode() == Mode::Normal && self.editor().has_redo() {
                    self.editor().redo();
                    return Vec::new();
                }
                if self.active.is_none() {
                    self.flash("no session to run the loop for");
                    Vec::new()
                } else {
                    vec![Action::RunLoop]
                }
            }
            Key::CtrlC => {
                // The stop resolves in main against the persistent
                // loop.pid probe (FT-003): it stops a live loop this
                // TUI did not start. Main flashes the outcome. In a
                // pre-session editor, Ctrl-C is the vim insert-exit
                // key and the editor decides.
                if self.active.is_none() {
                    if let Some(h) = self.editor_press(Key::CtrlC) {
                        self.flash(h);
                    }
                    Vec::new()
                } else {
                    vec![Action::StopLoop]
                }
            }
            Key::CtrlE => {
                if self.active.is_none() {
                    self.flash("no session — pass a session name argument");
                    Vec::new()
                } else {
                    vec![Action::OpenEditor]
                }
            }
            // Tab / BackTab are free: reserved for a future session
            // navigation design (docs/tui_feature_requests_from_human.md).
            Key::Tab | Key::BackTab => Vec::new(),
            Key::PgUp => {
                self.scroll_up(10);
                Vec::new()
            }
            Key::PgDn => {
                self.scroll_down(10);
                Vec::new()
            }
            Key::CtrlU => {
                // In the search command line it clears the input
                // (the pi-vim mapping). In the idle composer's
                // insert mode it kills the current line (the base
                // editor's Ctrl-U). Otherwise it is the half-page
                // log scroll.
                if self.editor().mode() == Mode::CommandLine {
                    self.editor_press(Key::CtrlU);
                    return Vec::new();
                }
                if self.editor().mode() == Mode::Insert && self.active.is_none() {
                    self.editor().clear_current_line();
                    return Vec::new();
                }
                self.scroll_up(self.half_page());
                Vec::new()
            }
            Key::CtrlD => {
                // Half-page down, like vim: back toward the tail.
                self.scroll_down(self.half_page());
                Vec::new()
            }
            Key::CtrlO => {
                // The global tool fold/expand toggle (docs/tui-tool-
                // display-port.md section 2, the expand part): every
                // collapsed block expands to the full output, and
                // back. The state is app-local; the version bump
                // rebuilds the transcript on the next draw.
                self.tool_expanded = !self.tool_expanded;
                self.events_version += 1;
                vec![Action::ToggleToolExpand]
            }
            Key::CtrlT => {
                // The thinking-block collapse/expand toggle (docs/tui-
                // thinking-block.md section 4, the pi
                // `app.thinking.toggle` keymap): `false` collapses every
                // block to its one-line label; `true` expands the full
                // reasoning text. `Ctrl+X` is the separate show/hide.
                self.thinking_expanded = !self.thinking_expanded;
                self.events_version += 1;
                vec![Action::ToggleThinkingExpand]
            }
            Key::CtrlX => {
                // The thinking-block show/hide toggle (docs/tui-
                // thinking-block.md section 4): `false` hides every
                // thinking block; `true` restores them. `Ctrl+T` is
                // the pi collapse/expand key, so hide takes `Ctrl+X`.
                self.thinking_shown = !self.thinking_shown;
                self.events_version += 1;
                vec![Action::ToggleThinking]
            }
            Key::CtrlF => {
                // The input queue toggle (docs/tui-pending-user-
                // messages.md stage 2): the next draft sends to the
                // follow queue, and back to steer. The state is the
                // input area's own: no transcript rebuild.
                self.follow_queue = !self.follow_queue;
                self.flash(if self.follow_queue {
                    "queue: follow — the next message waits for the loop to stop"
                } else {
                    "queue: steer — the next message injects at the next step"
                });
                vec![Action::ToggleFollowQueue]
            }
            Key::CtrlL => {
                // The reasoning-effort cycle (docs/tui-thinking-level-
                // input-box.md section 3). Main owns the config
                // write-back: it reads the active model's effort,
                // steps to the next value in the effort order, and
                // writes the active model's config entry.
                vec![Action::CycleEffort]
            }
            Key::CtrlP => {
                // Preview pane toggle for the @ picker
                // (docs/tui-file-picker.md section 4.4).
                // No-op when the picker is closed.
                let _ = self.picker.toggle_preview(0, crate::picker::render::PREVIEW_CUTOFF);
                Vec::new()
            }
            Key::AltUp => {
                // Bulk recall of pending user messages
                // (docs/user-message-editing.md).
                self.recall_queue();
                Vec::new()
            }
            Key::Wheel(delta) => {
                // Wheel up scrolls back in history; wheel down chases
                // the tail.
                if delta < 0 {
                    self.scroll_up(3);
                } else {
                    self.scroll_down(3);
                }
                Vec::new()
            }
            Key::Esc => {
                // Esc only ever cancels: it drops the editing mode to
                // normal and cancels a pending operator. It must never
                // touch the draft text — vim's Esc cancels, it does
                // not delete. The draft is cleared only by send (Enter)
                // or an explicit delete motion.
                if let Some(h) = self.editor_press(Key::Esc) {
                    self.flash(h);
                }
                Vec::new()
            }
            Key::CtrlZ => {
                // Unreachable: handled at the top of press() before
                // reaching this match. Satisfies the compiler's
                // exhaustiveness check.
                Vec::new()
            }
            Key::Char(c) => {
                // The one-key handoff (correction 57): the log holds a
                // seeded `context_exhausted` marker and no loop runs.
                // It preempts the editor in that state only; in a
                // live session `h` stays the vim left motion.
                if c == 'h'
                    && self.active().is_some_and(|s| !self.loop_running(s))
                    && self.pending_handoff().is_some()
                {
                    return vec![Action::Handoff(self.pending_handoff().unwrap())];
                }
                // y / n / e answer the oldest pending approval_request
                // (docs/tui.md section 7); without a pending request
                // they are ordinary keys for the editor.
                let lower = c.to_ascii_lowercase();
                if self.oldest_pending_approval().is_some() {
                    match lower {
                        'y' => return vec![Action::AnswerApproval(Decision::Allow)],
                        'n' => return vec![Action::AnswerApproval(Decision::Deny)],
                        'e' => return vec![Action::AnswerApproval(Decision::Edit)],
                        _ => {}
                    }
                }
                // The double-`s` browse gate (section 4.2, loosened
                // 2026-09-15): normal mode with no name input. The
                // draft may be held. The first `s` arms; the second
                // `s` inside the window enters browse.
                if c == 's' && self.browse_gate_open() {
                    match self.ss_arm {
                        Some(at) if at.elapsed() < SS_ARM_TTL => {
                            self.ss_arm = None;
                            self.browse.enter();
                            self.flash(crate::browse::BROWSE_HINT);
                            return Vec::new();
                        }
                        _ => {
                            self.ss_arm = Some(Instant::now());
                            self.flash("ss: press s again to browse");
                            return Vec::new();
                        }
                    }
                }
                // `:` opens the command palette (docs/tui-command-palette.md).
                // Only in normal mode; in insert mode `:` is a regular
                // character typed into the draft.
                if c == ':'
                    && self.editor.mode() == Mode::Normal
                    && self.pending_name.is_none()
                {
                    self.open_palette();
                    return Vec::new();
                }
                if let Some(h) = self.editor_press(Key::Char(c)) {
                    self.flash(h);
                }
                // Open the picker when `@` is freshly typed at a valid
                // position (start of line or preceded only by
                // whitespace). A stale `@` left in the draft after a
                // previous pick or dismiss does not re-trigger it.
                if c == '@'
                    && !self.picker.open
                    && self.editor().at_token_info().is_some()
                {
                    self.open_picker();
                }
                // Sync the picker after any editor mutation so the
                // `@query` text stays in step with the draft.
                self.sync_picker();
                Vec::new()
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quitting
    }

}

/// The one-line tree row for an event (docs/tree-ui-design-from-human.md):
/// a type tag up front and a truncated single-line preview. No wrapping.
fn tree_row_label(e: &Event) -> String {
    let tag = tree_event_tag(e);
    let preview = tree_event_preview(e);
    if preview.is_empty() {
        format!("<{tag}>")
    } else {
        format!("<{tag}> {preview}")
    }
}

/// The short type tag at the front of a tree row.
fn tree_event_tag(e: &Event) -> String {
    match e.kind() {
        EventKind::UserMessage => "user".to_string(),
        EventKind::AssistantMessage => "assistant".to_string(),
        EventKind::ToolCall => match e.get_str("name") {
            Some(n) => format!("tool:{n}"),
            None => "tool".to_string(),
        },
        EventKind::ToolResult => "tool-result".to_string(),
        EventKind::ApprovalRequest => "approval".to_string(),
        EventKind::Approval => "decision".to_string(),
        EventKind::Cancel => "cancel".to_string(),
        EventKind::Error => "error".to_string(),
        EventKind::ContextExhausted => "context-exhausted".to_string(),
        EventKind::CompactionStarted => "compact-start".to_string(),
        EventKind::CompactionSummary => "compact".to_string(),
        EventKind::CompactionFailed => "compact-failed".to_string(),
        EventKind::UserMessageRetract => "retract".to_string(),
        EventKind::Rewind => "rewind".to_string(),
        _ => "event".to_string(),
    }
}

/// The one-line preview of an event's content for the tree row.
fn tree_event_preview(e: &Event) -> String {
    let raw = match e.kind() {
        EventKind::UserMessage | EventKind::AssistantMessage => {
            e.get_str("content").unwrap_or("").to_string()
        }
        EventKind::ToolCall => e
            .get("arguments")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        EventKind::ToolResult => e
            .get("value")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        EventKind::Rewind => format!(
            "rewound to seq {} ({})",
            e.get("target_seq")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            e.get_str("mode").unwrap_or("on")
        ),
        EventKind::ApprovalRequest => e.get_str("prompt").unwrap_or("").to_string(),
        _ => String::new(),
    };
    truncate_one_line(&raw)
}

/// The multi-line body of an event for the tree preview pane
/// (docs/tree-ui-design-from-human.md "The preview pane shows the
/// preview of the full content of the event"). Capped so one event does
/// not dominate the pane.
fn tree_event_body(e: &Event) -> String {
    let raw = match e.kind() {
        EventKind::UserMessage | EventKind::AssistantMessage => {
            e.get_str("content").unwrap_or("").to_string()
        }
        EventKind::ToolCall => e
            .get("arguments")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        EventKind::ToolResult => e
            .get("value")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        EventKind::Rewind => format!(
            "rewound to seq {} ({})",
            e.get("target_seq")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            e.get_str("mode").unwrap_or("on")
        ),
        EventKind::ApprovalRequest => e.get_str("prompt").unwrap_or("").to_string(),
        _ => String::new(),
    };
    const CAP: usize = 600;
    if raw.chars().count() <= CAP {
        raw
    } else {
        let cut: String = raw.chars().take(CAP).collect();
        format!("{cut}...")
    }
}

/// Collapse to the first line and cap the length, appending `...` when
/// truncated (docs/tree-ui-design-from-human.md "Show ... when the
/// message is too long").
fn truncate_one_line(s: &str) -> String {
    let one = s.lines().next().unwrap_or("").trim();
    const CAP: usize = 120;
    if one.chars().count() <= CAP {
        one.to_string()
    } else {
        let cut: String = one.chars().take(CAP).collect();
        format!("{cut}...")
    }
}

/// Fold one paced release into the live buffer accumulators (the
/// consumer of the §6.5 pace queue).
fn apply_paced(buf: &mut StreamBuf, kind: &StreamDeltaKind, payload: &str) {
    match kind {
        StreamDeltaKind::Text => {
            buf.text.push_str(payload);
        }
        StreamDeltaKind::Reasoning(id) => {
            buf.reasoning.entry(id.clone()).or_default().push_str(payload);
        }
        StreamDeltaKind::ToolArgs(call_id, name) => {
            let entry = buf
                .tool_args
                .entry(call_id.clone())
                .or_insert_with(|| (String::new(), String::new()));
            if !name.is_empty() {
                entry.0 = name.clone();
            }
            entry.1.push_str(payload);
        }
    }
}

#[cfg(test)]
mod full_history_tests {
    //! End to end guards for the removed replay caps
    //! (docs/tui-conversation-browsing.md section 4.6, no replay
    //! cap). A session past the old 2000 event render cap must
    //! still expose its beginning in the tree and in the rendered
    //! transcript.

    use super::App;
    use crate::event::Event;
    use crate::port::SessionId;

    fn many_events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Event::parse_line(
                        &format!(
                            r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"line {i}"}}"#
                        ),
                    )
                    .unwrap()
                } else {
                    Event::parse_line(
                        &format!(
                            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a{i}","content":"reply {i}","tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":1,"output_tokens":1}},"reasoning":{{}}}}"#
                        ),
                    )
                    .unwrap()
                }
            })
            .collect()
    }

    /// The tree list spans the whole log, not a capped tail. Past
    /// the old 2000 event cap, event 1 must still lead the list.
    #[test]
    fn tree_list_reaches_the_first_event_past_the_old_cap() {
        let n = 3000;
        let mut app = App::new();
        app.set_active(SessionId::new("s1"), many_events(n), n as u64);
        let items = app.tree_event_items("");
        assert_eq!(items.len(), n, "the tree lists every event");
        assert_eq!(items[0].id, "1", "the first log seq leads the list");
        assert!(
            items[0].label.contains("line 0"),
            "the first event shows in the row: {}",
            items[0].label
        );
        assert_eq!(items.last().unwrap().id, n.to_string());
    }

    /// The transcript's rewind marker line reads "rewound to seq N
    /// (mode)". The tree row for the marker reuses that exact wording.
    /// What the user saw in the transcript is findable in the fuzzy
    /// box of the tree palette (docs/tree-ui-design-from-human.md,
    /// "Rewind transcript masking (hide, not dim)").
    #[test]
    fn tree_row_labels_rewind_marker_like_the_transcript() {
        use crate::picker::fuzzy::rank_fuzzy;
        let events = vec![
            Event::parse_line(
                r#"{"v":1,"type":"user_message","ts":"t","id":"u1","content":"hello"}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"hi","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":1,"output_tokens":1},"reasoning":{}}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"rewind","ts":"t","id":"w1","target_seq":2,"mode":"before","reason":"tui_pick"}"#,
            )
            .unwrap(),
        ];
        let mut app = App::new();
        app.set_active(SessionId::new("s1"), events, 3);
        let items = app.tree_event_items("");
        let marker = items
            .iter()
            .find(|it| it.label.contains("rewind"))
            .expect("the rewind marker row is in the tree list");
        assert_eq!(marker.id, "3", "the marker's 1-based log seq is the row id");
        assert!(
            marker.label.contains("rewound to seq 2 (before)"),
            "the row reuses the transcript marker wording: {}",
            marker.label
        );
        // Typing what the transcript line shows finds the marker row.
        let labels: Vec<String> = items.iter().map(|it| it.label.clone()).collect();
        let ranked = rank_fuzzy(&labels, "rewound to seq 2");
        assert!(
            ranked.iter().any(|&i| labels[i].contains("rewound to seq 2 (before)")),
            "the fuzzy box finds the marker from the transcript wording"
        );
    }

    /// The rendered transcript starts at the first event, not at a
    /// capped window start.
    #[test]
    fn transcript_renders_from_the_first_event() {
        let n = 3000;
        let mut app = App::new();
        app.set_active(SessionId::new("s1"), many_events(n), n as u64);
        let lines = crate::render::build_transcript_lines(&app, 100, None);
        assert!(
            lines.len() >= n,
            "each event yields at least one line, got {}",
            lines.len()
        );
        let head: String = lines.iter().take(8).map(|l| l.to_string()).collect();
        assert!(
            head.contains("line 0"),
            "the transcript head shows the first event: {head}"
        );
    }

    /// The stream-change flag reports growth and shrink alike, so a
    /// live tail that shrinks (thinking window slide, settle into a
    /// shorter event) still pins the browse view instead of re-
    /// centering on the cursor (the cc08fa6 regression).
    #[test]
    fn stream_changed_flag_fires_on_growth_and_shrink() {
        let mut app = App::new();
        assert!(!app.note_stream_changed(0), "no stream yet");
        assert!(app.note_stream_changed(40), "growth reports a change");
        assert!(!app.note_stream_changed(40), "steady length does not");
        assert!(app.note_stream_changed(24), "shrink reports a change");
        assert!(!app.note_stream_changed(24), "steady again does not");
        // A session switch resets the baseline, so a fresh session
        // with no stream reports no phantom change.
        app.set_active(SessionId::new("s2"), Vec::new(), 0);
        assert!(!app.note_stream_changed(0), "fresh session baseline is zero");
    }

    /// Load the real session log when it exists. The `sessions/`
    /// tree is runtime data, gitignored, so the test skips when the
    /// file is absent. When present, the tree must lead with the
    /// first log event even for a long log.
    #[test]
    fn tree_list_spans_the_real_session_log_when_present() {
        use crate::event::EventKind;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sessions/browse-mode-issues/events.jsonl");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let events: Vec<Event> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| Event::parse_line(l))
            .collect();
        assert!(
            events.len() >= 2800,
            "the real session is a long log, got {}",
            events.len()
        );
        let mut app = App::new();
        app.set_active(
            SessionId::new("browse-mode-issues"),
            events.clone(),
            events.len() as u64,
        );
        let items = app.tree_event_items("");
        let first_idx = events
            .iter()
            .position(|e| e.kind() != EventKind::ExtStatus)
            .unwrap();
        assert_eq!(items[0].id, "1", "the first log seq leads the list");
        assert_eq!(
            items[0].label,
            super::tree_row_label(&events[first_idx]),
            "the first row is the first listed event"
        );
        let lines = crate::render::build_transcript_lines(&app, 100, None);
        assert!(!lines.is_empty(), "the transcript is not empty");
        // The transcript head must show the first event's content, so
        // `gg` in browse lands on the session start. The user message
        // renders in its bordered panel with the body at the left edge.
        let content = events[first_idx]
            .get_str("content")
            .unwrap_or("");
        let marker: String = content.chars().take(12).collect();
        let head: String = lines.iter().take(12).map(|l| l.to_string()).collect();
        assert!(
            head.contains(&marker),
            "the transcript head shows the first event, head: {head}"
        );
    }
}

#[cfg(test)]
mod background_build_tests {
    use super::{App, Key};
    use crate::event::Event;
    use crate::port::SessionId;
    use std::time::{Duration, Instant};

    fn user_event(i: u32) -> Event {
        Event::parse_line(
            &format!(
                r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"line {i}"}}"#
            ),
        )
        .unwrap()
    }

    fn assistant_event(i: u32) -> Event {
        Event::parse_line(
            &format!(
                r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a{i}","content":"reply {i}","tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":1,"output_tokens":1}},"reasoning":{{}}}}"#
            ),
        )
        .unwrap()
    }

    fn make_events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    user_event(i as u32)
                } else {
                    assistant_event(i as u32)
                }
            })
            .collect()
    }

    /// Poll the worker until no build is requested or in flight.
    fn settle(app: &mut App) {
        let start = std::time::Instant::now();
        while app.transcript_rebuilding() {
            app.poll_transcript_worker();
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the background build did not settle within 10 s"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn joined_text(app: &mut App, width: usize) -> String {
        app.transcript_lines(width, None)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn width_miss_returns_stale_cache_until_the_build_lands() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        app.set_active(
            SessionId::new("s1"),
            make_events(3000),
            3000,
        );
        let first = app.transcript_lines(100, None).to_vec();
        assert!(app.transcript_partial(), "a truncated tail is partial");
        let stale = app.transcript_lines(120, None).to_vec();
        assert_eq!(
            stale, first,
            "the miss returns the last good cache"
        );
        let stale_text = stale
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !stale_text.contains("line 0"),
            "the stale tail cache does not hold the old head"
        );
        assert!(app.transcript_rebuilding(), "a rebuild is requested");
        // The width miss armed the 75 ms trailing window (stage 4).
        // Let it close before dispatching.
        std::thread::sleep(Duration::from_millis(100));
        assert!(app.dispatch_transcript_build(None));
        assert!(
            !app.dispatch_transcript_build(None),
            "no second build while one is in flight"
        );
        settle(&mut app);
        assert!(!app.transcript_rebuilding());
        assert!(!app.transcript_partial(), "the full build replaced the tail");
        let full_text = joined_text(&mut app, 120);
        assert!(
            full_text.contains("line 0"),
            "the settled build is the full transcript"
        );
    }

    #[test]
    fn key_burst_coalesces_to_the_newest_key() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        app.set_active(
            SessionId::new("s1"),
            make_events(2000),
            2000,
        );
        let _ = app.transcript_lines(80, None);
        let _ = app.transcript_lines(90, None);
        let _ = app.transcript_lines(100, None);
        assert!(app.transcript_rebuilding());
        // The width burst armed the 75 ms trailing window (stage 4).
        // Let it close so the settled width can dispatch.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            app.dispatch_transcript_build(None),
            "one dispatch queues the newest key"
        );
        assert!(
            !app.dispatch_transcript_build(None),
            "a second dispatch is a no-op while in flight"
        );
        settle(&mut app);
        assert!(
            !app.transcript_rebuilding(),
            "the settled key is a hit"
        );
        let _ = app.transcript_lines(100, None);
        assert!(!app.transcript_rebuilding());
        let _ = app.transcript_lines(90, None);
        assert!(
            app.transcript_rebuilding(),
            "the superseded key misses and requests its own build"
        );
    }

    #[test]
    fn first_build_uses_the_tail_window_and_gates_browse() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(5);
        app.set_active(
            SessionId::new("s1"),
            make_events(50),
            50,
        );
        let text = joined_text(&mut app, 120);
        assert!(app.transcript_partial());
        assert!(
            text.contains("line 44"),
            "the newest events show in the tail build: {text}"
        );
        assert!(
            !text.contains("line 0"),
            "the first build does not render the old head"
        );
        let starts = app.transcript_event_line_starts(120, None);
        assert_eq!(starts.len(), 50);
        assert!(starts[0].is_none());
        assert!(starts[44].is_some());
        let total = app.transcript_lines(120, None).len();
        let texts = app.transcript_texts(120, None).clone();
        let raw = app.transcript_raw(120, None);
        app.browse().enter();
        app.set_browse_layout(total, 5, texts, raw);
        app.browse_key(Key::Char('j'));
        assert_eq!(
            app.browse_ref().line_col().0,
            1,
            "the cursor moves down"
        );
        app.browse_key(Key::Char('y'));
        assert_eq!(
            app.browse_ref().line_col().0,
            1,
            "y is held while the build is partial"
        );
        app.browse_key(Key::Char('g'));
        app.browse_key(Key::Char('g'));
        assert_eq!(
            app.browse_ref().line_col().0,
            1,
            "gg is held while the build is partial"
        );
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert!(!app.transcript_partial());
        let full = joined_text(&mut app, 120);
        assert!(full.contains("line 0"), "the full build holds the head");
        let total = app.transcript_lines(120, None).len();
        let texts = app.transcript_texts(120, None).clone();
        let raw = app.transcript_raw(120, None);
        app.set_browse_layout(total, 5, texts, raw);
        app.browse_key(Key::Char('j'));
        app.browse_key(Key::Char('g'));
        app.browse_key(Key::Char('g'));
        assert_eq!(
            app.browse_ref().line_col().0,
            0,
            "gg lands at the head once the full build is in"
        );
    }

    #[test]
    fn first_build_without_a_worker_builds_sync() {
        let mut app = App::new();
        app.set_viewport_height(5);
        app.set_active(
            SessionId::new("s1"),
            make_events(50),
            50,
        );
        let text = joined_text(&mut app, 120);
        assert!(
            !app.transcript_partial(),
            "the sync fallback stores a full build"
        );
        assert!(!app.transcript_rebuilding());
        assert!(text.contains("line 0"), "the sync build holds the head");
    }

    /// Stage 4 gate: a burst of width events produces one build at
    /// the settled width. Each new width value resets the 75 ms
    /// trailing window; redraws at the pending width do not.
    #[test]
    fn width_burst_produces_one_build_at_the_settled_width() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        app.set_active(
            SessionId::new("s1"),
            make_events(2000),
            2000,
        );
        // Establish a last-built cache at width 80.
        let _ = app.transcript_lines(80, None);
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert!(!app.transcript_rebuilding());

        // Burst: 90 -> 110 -> 90 -> 110 -> 110. Each new width
        // value resets the deadline; the redraws at 110 keep it.
        let _ = app.transcript_lines(90, None);
        let _ = app.transcript_lines(110, None);
        let _ = app.transcript_lines(90, None);
        let _ = app.transcript_lines(110, None);
        let _ = app.transcript_lines(110, None);
        assert!(app.transcript_rebuilding());
        assert_eq!(
            app.transcript_desired_key.as_ref().unwrap().width,
            110,
            "the pending key is the settled width"
        );
        let deadline = app
            .transcript_width_debounce
            .expect("the width miss armed the debounce");
        if Instant::now() < deadline {
            assert!(
                !app.dispatch_transcript_build(None),
                "the dispatch is held inside the trailing window"
            );
            assert!(
                !app.transcript_build_in_flight,
                "no build started during the window"
            );
        }
        // Let the window close, then dispatch at the settled width.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            app.dispatch_transcript_build(None),
            "the settled width builds after the window"
        );
        assert!(app.transcript_build_in_flight);
        settle(&mut app);
        assert!(!app.transcript_rebuilding());
        assert_eq!(
            app.transcript_cache.as_ref().unwrap().width,
            110,
            "the one build landed at the settled width"
        );
        let _ = app.transcript_lines(110, None);
        assert!(
            !app.transcript_rebuilding(),
            "the settled width is a cache hit"
        );
    }

    /// Event-commit misses bypass the width debounce (stage 4).
    /// An event commit at the built width builds immediately with
    /// no 75 ms wait.
    #[test]
    fn event_commit_miss_bypasses_the_width_debounce() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        let sid = SessionId::new("s1");
        app.set_active(sid.clone(), make_events(300), 300);
        let _ = app.transcript_lines(80, None);
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert!(!app.transcript_rebuilding());

        // Commit an event at the same width: the events version
        // bumps, the width is unchanged. The miss must not wait
        // out the width window.
        app.set_active(sid, make_events(301), 301);
        let _ = app.transcript_lines(80, None);
        assert!(app.transcript_rebuilding());
        assert!(
            app.transcript_width_debounce.is_none(),
            "an event-commit miss carries no deadline"
        );
        assert!(
            app.dispatch_transcript_build(None),
            "the commit builds immediately, no 75 ms wait"
        );
        assert!(app.transcript_build_in_flight);
        settle(&mut app);
        assert!(!app.transcript_rebuilding());
        let full = joined_text(&mut app, 80);
        assert!(
            full.contains("line 300"),
            "the committed event is in the build"
        );
    }

    /// Toggling back to the last built width cancels the pending
    /// build (stage 4). The pending width is compared to the last
    /// built width, not the last observed width.
    #[test]
    fn width_toggle_back_to_built_width_cancels_the_build() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        app.set_active(
            SessionId::new("s1"),
            make_events(500),
            500,
        );
        let _ = app.transcript_lines(80, None);
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert!(!app.transcript_rebuilding());

        // 80 -> 120 arms a debounced build at 120.
        let _ = app.transcript_lines(120, None);
        assert!(app.transcript_rebuilding());
        assert!(app.transcript_width_debounce.is_some());
        // Toggle back to the last built width. The hit cancels the
        // pending build and its deadline.
        let _ = app.transcript_lines(80, None);
        assert!(
            !app.transcript_rebuilding(),
            "the hit at the built width cancels the pending build"
        );
        assert!(
            app.transcript_width_debounce.is_none(),
            "the pending window is dropped"
        );
        // The dropped deadline lapses. Nothing dispatches.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !app.dispatch_transcript_build(None),
            "no build runs after the toggle-back"
        );
        assert_eq!(
            app.transcript_cache.as_ref().unwrap().width,
            80,
            "the cache still holds the built width"
        );
    }

    /// A settled width that arrives after a build still builds,
    /// even though that width was built before (stage 4). The
    /// comparison is against the last built width, not the last
    /// observed width.
    #[test]
    fn settled_width_after_a_later_build_still_builds() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(200);
        app.set_active(
            SessionId::new("s1"),
            make_events(500),
            500,
        );
        let _ = app.transcript_lines(80, None);
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert_eq!(app.transcript_cache.as_ref().unwrap().width, 80);

        // 80 -> 120 settles and builds. Last built width is 120.
        let _ = app.transcript_lines(120, None);
        std::thread::sleep(Duration::from_millis(100));
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert_eq!(app.transcript_cache.as_ref().unwrap().width, 120);

        // 120 -> 80: 80 was observed and built earlier, but the
        // last build is 120. The settled 80 builds again.
        let _ = app.transcript_lines(80, None);
        assert!(app.transcript_rebuilding());
        std::thread::sleep(Duration::from_millis(100));
        assert!(app.dispatch_transcript_build(None));
        settle(&mut app);
        assert_eq!(app.transcript_cache.as_ref().unwrap().width, 80);
        let _ = app.transcript_lines(80, None);
        assert!(
            !app.transcript_rebuilding(),
            "the rebuilt width is a cache hit"
        );
    }
}

#[cfg(test)]
mod perf_bgbuild_tests {
    //! Performance regression tests for the promised level in
    //! docs/tui-perf-background-build-plan.md.
    //!
    //! The plan's test plan says: no frame over 100 ms during a
    //! cache-key miss on a large log. Before the stage-2 fix, that
    //! miss ran the full build inline for ~7 s. These tests build a
    //! heavy log where one full build takes seconds, then time the
    //! main-thread frame path while that build runs in the
    //! background. They fail loudly if the inline build returns.
    use super::App;
    use crate::event::Event;
    use crate::port::SessionId;
    use std::time::{Duration, Instant};

    /// Heavy events that make one full build take seconds.
    /// Calibrated: 5000 events -> ~3.8 s in the debug build.
    const N_HEAVY: usize = 5000;
    /// The promised frame budget from the plan's test plan.
    const FRAME_BUDGET_MS: u128 = 100;

    /// Interleaved user and assistant events with heavy content.
    /// Markdown fences, tool calls, and long reasoning blocks drive
    /// the per-event render cost that made the 24 MB build slow.
    fn heavy_events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Event::parse_line(
                        &format!(
                            r#"{{"v":1,"type":"user_message","ts":"t","id":"u{i}","content":"Please implement a complex algorithm that handles edge cases and performance optimization for step {i}. It must be generic over the input container, stream arbitrarily large data with bounded memory, and emit a machine-readable diagnostic report of every optimization decision, plus a documented example a user can adapt."}}"#
                        ),
                    )
                    .unwrap()
                } else {
                    Event::parse_line(
                        &format!(
                            r#"{{"v":1,"type":"assistant_message","ts":"t","id":"a{i}","content":"Here is the full solution for step {i}.\n\n```rust\npub fn solve<C: Container>(c: &C, step: {i}) -> Result<Report, Error> {{\n    let mut report = Report::new();\n    for item in c.iter() {{\n        let optimized = optimize(item, step);\n        report.record(&optimized);\n    }}\n    Ok(report.finish())\n}}\n```\n\n- Step 1: analyze the container layout and pick a traversal order.\n- Step 2: implement the streaming core with bounded memory.\n- Step 3: generate the diagnostic report (summary + JSON block).","tool_calls":[{{"id":"tc{i}","name":"bash","args":{{"command":"cargo test step_{i} -- --nocapture && cargo doc --no-deps"}}}}],"stop_reason":"stop","usage":{{"input_tokens":1000,"output_tokens":2000}},"reasoning":{{"content":"step {i}: trade streaming throughput against per-item diagnostic cost; keep diagnostics behind a feature flag so the hot path stays fast, and make the resume logic idempotent so a partial run followed by a full run does not double-count work."}}}}"#
                        ),
                    )
                    .unwrap()
                }
            })
            .collect()
    }

    /// Poll the worker until no rebuild is pending or in flight.
    fn settle(app: &mut App, budget_s: u64) {
        let start = Instant::now();
        while app.transcript_rebuilding() {
            app.poll_transcript_worker();
            std::thread::sleep(Duration::from_millis(10));
            assert!(
                start.elapsed() < Duration::from_secs(budget_s),
                "the background build did not settle within {budget_s} s"
            );
        }
    }

    /// The main-thread frame path during a cache-key miss.
    /// Read the stale lines, dispatch a pending build, poll results.
    /// This is the work that used to hold the full inline build.
    fn frame_path_ms(app: &mut App, width: usize) -> u128 {
        let t0 = Instant::now();
        let _ = app.transcript_lines(width, None);
        app.dispatch_transcript_build(None);
        app.poll_transcript_worker();
        t0.elapsed().as_millis()
    }

    /// Seed a settled full cache at `width` on a heavy log.
    fn seed_settled_cache(app: &mut App, events: Vec<Event>, width: usize) {
        let n = events.len();
        app.set_active(SessionId::new("perf"), events, n as u64);
        let _ = app.transcript_lines(width, None);
        assert!(app.dispatch_transcript_build(None));
        settle(app, 60);
        assert!(!app.transcript_rebuilding());
    }

    /// An event-commit cache-key miss dispatches the full build to
    /// the worker with no width debounce. While that build runs for
    /// seconds, the frame path stays under the 100 ms budget.
    #[test]
    fn event_commit_miss_keeps_frames_under_budget() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(50);
        seed_settled_cache(&mut app, heavy_events(N_HEAVY), 80);

        // Commit one more event at the same width. The events version
        // bump is a cache-key miss that must build immediately.
        let sid = app.active().clone().expect("an active session");
        app.set_active(sid.clone(), heavy_events(N_HEAVY + 1), (N_HEAVY + 1) as u64);
        let t_miss = Instant::now();
        let _ = app.transcript_lines(80, None);
        let miss_ms = t_miss.elapsed().as_millis();
        assert!(app.transcript_rebuilding());
        assert!(
            app.transcript_width_debounce.is_none(),
            "an event-commit miss bypasses the width debounce"
        );
        assert!(
            app.dispatch_transcript_build(None),
            "the commit builds immediately, no debounce wait"
        );
        let dispatch_ms = t_miss.elapsed().as_millis();
        assert!(
            app.transcript_build_in_flight,
            "the full build is running on the worker"
        );

        // The background build takes seconds. While it is in flight
        // the frame path stays far under the promised budget.
        let frame_ms = frame_path_ms(&mut app, 80);
        assert!(
            frame_ms < FRAME_BUDGET_MS,
            "the frame path took {frame_ms} ms while the background \
             build was in flight (budget {FRAME_BUDGET_MS} ms); \
             stale_ms={miss_ms} dispatch_ms={dispatch_ms}"
        );

        // The settled build must land with the new events version.
        let t_settle = Instant::now();
        settle(&mut app, 60);
        let build_ms = t_settle.elapsed().as_millis();
        eprintln!(
            "perf_bgbuild timing: frame_path={} ms, miss_read={} ms, \
             snapshot+dispatch={} ms, background_full_build={} ms",
            frame_ms, miss_ms, dispatch_ms, build_ms
        );
        assert!(
            !app.transcript_rebuilding(),
            "the background build settles"
        );
    }

    /// A width miss renders the stale cache through the 75 ms
    /// debounce window and while the settled build runs in the
    /// background. Every frame stays under the budget. A browse
    /// toggle never hitches.
    #[test]
    fn width_miss_frames_stay_under_budget() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(50);
        seed_settled_cache(&mut app, heavy_events(N_HEAVY), 80);

        // The width miss returns the stale cache and arms the window.
        let t0 = Instant::now();
        let _ = app.transcript_lines(120, None);
        let miss_ms = t0.elapsed().as_millis();
        assert!(app.transcript_rebuilding());
        assert!(
            app.transcript_width_debounce.is_some(),
            "the width miss armed the trailing window"
        );

        // Inside the window the dispatch is held. Frames keep
        // rendering the stale cache under the budget.
        let held_ms = frame_path_ms(&mut app, 120);
        assert!(
            held_ms < FRAME_BUDGET_MS,
            "a frame inside the debounce window took {held_ms} ms \
             (budget {FRAME_BUDGET_MS} ms)"
        );

        // The window lapses. One build fires at the settled width.
        std::thread::sleep(
            app.transcript_width_debounce
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or_default()
                + Duration::from_millis(5),
        );
        assert!(
            app.dispatch_transcript_build(None),
            "the settled width dispatches one build after the window"
        );
        assert!(app.transcript_build_in_flight);
        let build_ms = frame_path_ms(&mut app, 120);
        assert!(
            build_ms < FRAME_BUDGET_MS,
            "a frame while the settled build ran took {build_ms} ms \
             (budget {FRAME_BUDGET_MS} ms); miss_ms={miss_ms}"
        );

        settle(&mut app, 60);
        assert_eq!(
            app.transcript_cache.as_ref().unwrap().width,
            120,
            "the settled-width build landed in the cache"
        );
    }

    /// The first build of a session renders only the visible tail
    /// window on the main thread. That portion is viewport-sized and
    /// far under the budget. The full build follows in the
    /// background.
    #[test]
    fn first_build_tail_window_is_fast_on_the_main_thread() {
        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(50);
        app.set_active(
            SessionId::new("perf"),
            heavy_events(N_HEAVY),
            N_HEAVY as u64,
        );

        let t0 = Instant::now();
        let n_lines = {
            let lines = app.transcript_lines(80, None);
            lines.len()
        };
        let tail_ms = t0.elapsed().as_millis();
        assert!(
            app.transcript_partial(),
            "the first build stores a partial tail cache"
        );
        assert!(
            tail_ms < FRAME_BUDGET_MS,
            "the tail-window first build took {tail_ms} ms on the main \
             thread (budget {FRAME_BUDGET_MS} ms)"
        );
        assert!(
            n_lines > 0,
            "the tail window produced rendered lines"
        );
        assert!(
            app.transcript_rebuilding(),
            "the full build is owed and in flight after the tail window"
        );

        // Frames while the full build runs stay under the budget.
        let frame_ms = frame_path_ms(&mut app, 80);
        assert!(
            frame_ms < FRAME_BUDGET_MS,
            "a frame during the first full build took {frame_ms} ms \
             (budget {FRAME_BUDGET_MS} ms)"
        );
        settle(&mut app, 60);
        assert!(!app.transcript_partial());
    }

    /// Candidate paths for the 24 MB battlefield log:
    /// env override, then repo-relative, then the known absolute path.
    fn fixture_paths() -> Vec<std::path::PathBuf> {
        let mut v = Vec::new();
        if let Ok(p) = std::env::var("TUI_PERF_FIXTURE") {
            if !p.is_empty() {
                v.push(std::path::PathBuf::from(p));
            }
        }
        v.push(std::path::PathBuf::from(
            "../../../rushi-tui/sessions/tui-diff-spec-lean/events.jsonl",
        ));
        v.push(std::path::PathBuf::from(
            "/home/tony/programming/rushi-tui/sessions/tui-diff-spec-lean/events.jsonl",
        ));
        v
    }

    /// Load the real 24 MB session log, or None when no candidate exists.
    fn fixture_events() -> Option<(std::path::PathBuf, Vec<Event>)> {
        for p in fixture_paths() {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let evts: Vec<Event> =
                text.lines().filter_map(|l| Event::parse_line(l.trim())).collect();
            if !evts.is_empty() {
                return Some((p, evts));
            }
        }
        None
    }

    /// The battlefield test. The real 24 MB log drives a multi-second
    /// full build on the worker. The frame path must stay under the
    /// 100 ms budget while it runs. Skips gracefully when the
    /// fixture is absent.
    #[test]
    fn real_24mb_log_frames_stay_under_budget() {
        let Some((path, events)) = fixture_events() else {
            eprintln!("SKIP: 24 MB fixture not found (set TUI_PERF_FIXTURE)");
            return;
        };
        let n = events.len();
        eprintln!("real fixture: {n} events from {}", path.display());
        assert!(n > 2000, "the fixture should be the large log");

        let mut app = App::new();
        app.attach_transcript_worker();
        app.set_viewport_height(50);
        app.set_active(SessionId::new("real"), events, n as u64);

        // First build: tail window on the main thread, full build owed.
        let t0 = Instant::now();
        let tail_lines = {
            let l = app.transcript_lines(80, None);
            l.len()
        };
        let tail_ms = t0.elapsed().as_millis();
        eprintln!("real fixture tail-window build: {tail_ms} ms ({tail_lines} lines)");
        assert!(
            app.transcript_partial(),
            "the large log gives a partial tail cache"
        );
        assert!(
            tail_ms < FRAME_BUDGET_MS,
            "the tail build took {tail_ms} ms (budget {FRAME_BUDGET_MS} ms)"
        );

        // Dispatch the full build. It takes seconds on the real log.
        assert!(
            app.dispatch_transcript_build(None),
            "the full build dispatches to the worker"
        );
        assert!(app.transcript_build_in_flight);

        // While the full build is in flight, the frame path stays fast.
        let frame_ms = frame_path_ms(&mut app, 80);
        assert!(
            frame_ms < FRAME_BUDGET_MS,
            "the frame path took {frame_ms} ms while the full build ran \
             (budget {FRAME_BUDGET_MS} ms)"
        );

        // Settle with a generous budget: the real log builds for minutes
        // in a debug build.
        let t1 = Instant::now();
        settle(&mut app, 600);
        let build_ms = t1.elapsed().as_millis();
        eprintln!(
            "real fixture full background build: {build_ms} ms; \
             frame path was {frame_ms} ms",
        );
        assert!(!app.transcript_partial());
        assert!(
            !app.transcript_rebuilding(),
            "the real-log build settles"
        );
    }
}

#[cfg(test)]
mod browse_gate_tests {
    use super::{App, Key};

    #[test]
    fn double_s_enters_browse_with_a_held_draft() {
        // The 2026-09-15 loosening: a held draft no longer blocks
        // the double-`s` browse gate. The first `s` arms the gate
        // instead of taking the editor replace role.
        let mut app = App::new();
        app.set_draft("compose a message".into());
        let _ = app.press(Key::Esc); // insert -> normal, the gate state
        let _ = app.press(Key::Char('s'));
        assert!(
            !app.browse.active(),
            "the first s only arms the gate"
        );
        assert_eq!(
            app.editor.text(),
            "compose a message",
            "the armed s does not take the editor replace role"
        );
        let _ = app.press(Key::Char('s'));
        assert!(
            app.browse.active(),
            "the second s enters browse even with a held draft"
        );
    }

    #[test]
    fn double_s_still_enters_browse_with_an_empty_draft() {
        let mut app = App::new();
        let _ = app.press(Key::Esc); // insert -> normal
        let _ = app.press(Key::Char('s'));
        let _ = app.press(Key::Char('s'));
        assert!(app.browse.active());
    }
}
