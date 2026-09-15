# TUI large-content rendering research

Status: Research.
Last updated: 2026-09-13.
Scope: how existing ratatui apps and libraries keep the UI responsive
when content far exceeds the viewport (100k+ line transcripts, large
file viewers, long logs).
Basis: source review of 15 ratatui app and library repos plus the
ratatui docs and discussions. All findings cite source files.

This report answers one question for the project. The question is:
how do mature TUIs render content far larger than the viewport
without freezing the render thread. It informs Option A (background
build worker) from `docs/tui-perf-freeze-investigation.md`.

## Problem restated

`docs/tui-perf-freeze-investigation.md` isolates one remaining cost.
On every cache-key miss, `build_transcript` runs synchronously on
the render thread. It is O(all in-memory events). On the 24 MB log
it takes about 7 s. The cache key is a six-field tuple
(`events_version, width, ext_ver, palette_level, palette,
frac_epoch`). Triggers that re-key the cache:

- Browse enter or leave: the gutter and bar shift the width.
- Mouse scroll past the top: the position bar toggles.
- Each committed event: `events_version` bumps.
- Palette or scheme change: the palette or level field changes.
- Tool fold or expand toggle: the expanded height shifts the width.

Two options stand ready in the investigation doc. Option A: a
background build worker. Option B: a per-width cache. This report
surveys how the ecosystem solves the same class of problem and maps
the patterns onto the two options.

## Pattern table

Seven patterns recur across the surveyed apps:

| Pattern | Description | Evidence |
|---|---|---|
| Background build plus swap | A worker thread builds the content. The result returns as an event. The UI swaps it in. | zellij `RenderToClients`, gitui `AsyncSingleJob`, yazi scheduler, local `picker/fuzzy.rs` |
| Stale-while-revalidate | The UI keeps drawing the last good result while a rebuild runs. | gitui syntax highlighting, codex transcript history, bottom `DataStore` |
| Viewport windowing | Per-frame work is O(visible lines), not O(content). | helix, tui-scrollview, diff-tui, ratatui-markdown fork |
| Trailing debounce | Repeated triggers reset a deadline. Only the last trigger fires one rebuild. | codex 75 ms reflow, livediff 25 ms burst drain, zellij render coalesce, ftdv 5-column threshold |
| Width-keyed memo | Cached output is keyed by content plus width. A width change misses, a same-width repeat hits. | ratatui-markdown `ensure_rendered`, codex markdown cache, helix visible-range layout |
| Placeholder or progressive render | Show a skeleton, or amortize a one-off build across frames. | zellij `AnimatePluginLoading`, tui-skeleton, ratatui-splash-screen `render_steps` |
| Coalescing request queue | At most one build is pending. A new request drops the stale one. | gitui single-slot `next`, zellij `last_render_request` |

No surveyed app freezes its UI on a content miss. They all pair
stale-content rendering with off-thread rebuilds.

## App findings

### 1. Helix editor

Repo: https://github.com/helix-editor/helix

Pattern: viewport culling plus capped, off-thread heavy work.

Helix does not cache a per-viewport line layout in current master.
The `View` struct (`helix-view/src/view.rs`) stores the view offset
and a `doc_revisions` map. The `Document` struct stores the raw
`Rope` text. Layout work runs per frame, but only for the visible
lines.

Key structures:

- `EditorView::viewport_byte_range(text, row, height)` in
  `helix-term/src/ui/editor.rs` computes the byte range of the
  visible lines only. Syntax highlighting scopes to that range.
- `render_document` in `helix-term/src/ui/document.rs` walks
  graphemes and stops at `viewport.height`.
- `Document::text_format(viewport_width)` in
  `helix-view/src/document.rs` is a cheap config lookup, not a
  line cache.

Width changes re-wrap only the visible lines. The cost is
O(viewport height), not O(document). Helix re-derives the visible
layout on the next frame. There is no incremental re-wrap cache.

Background work:

- Tree-sitter parsing is capped: `PARSE_TIMEOUT = 500ms`.
  `Syntax::update` in `helix-core/src/syntax.rs` is incremental.
  Only the changed regions re-parse.
- Each language server runs in its own background task
  (`helix-lsp/src/client.rs`, `tokio::spawn`). The UI loop polls
  channels with timeouts and never blocks on an LSP reply.
- Git diff computation runs off-thread: `DiffHandle` in
  `helix-vcs/src/diff.rs` calls `tokio::spawn(worker.run(...))`.

UI responsiveness: per-frame layout is bounded to the viewport.
Heavy work runs off the main loop or is capped. The main loop polls
channels with timeouts.

### 2. Atuin

Repo: https://github.com/atuinsh/atuin

Pattern: index-backed async search plus windowed result rendering.
Atuin searches a SQLite history database, not an in-memory vector.

Key structures:

- `crates/atuin-client/src/database.rs` exposes `pub async fn
  search`. Fuzzy mode builds a SQL predicate per token. FullText
  mode uses SQLite FTS. Filter, dedup, ordering, and limit apply
  in SQL.
- The interactive engine caps results at 200
  (`crates/atuin/src/command/client/search/engines/db.rs`).
- The `daemon-fuzzy` mode sends ranking to the atuin daemon
  process over IPC.

The UI thread stays responsive
(`crates/atuin/src/command/client/search/interactive.rs`):

- The app paints the UI before the first query. The source comment
  states the intent in plain words: show the search UI immediately
  rather than a frozen terminal.
- The full-table history count runs in a detached `tokio::spawn`.
  The comment says a full scan must not hold up the first frame.
- The event loop is a `tokio::select!` between the async query and
  a 250 ms input poll. Input never blocks the runtime.

Results display: `history_list.rs` renders only the visible window:
`self.history.iter().skip(state.offset).take(end - start)`.

Benchmarks: the repo ships `crates/atuin-search-bench` for search
latency. No published numbers appear in the repo.

### 3. Zellij

Repo: https://github.com/zellij-org/zellij

Pattern: off-thread plugin sandboxes, message buses, and render
coalescing.

Plugin architecture:

- Plugins are WASM (wasmi) instances. Each instance lives in
  `RunningWorker { instance, store, name }`
  (`zellij-server/src/plugins/plugin_worker.rs`).
- A dedicated tokio task per instance runs on the shared global
  runtime. The task loops over an unbounded channel of
  `MessageToWorker::{Message, Exit}`.
- Host to plugin: a protobuf message is written via
  `wasi_write_object`, then the WASM entry point runs. Blocking
  WASM work stays on the plugin task, off the UI thread.
- Plugin to UI: host functions queue `PluginInstruction` onto
  `Bus<PluginInstruction>` (`zellij-server/src/thread_bus.rs`).
  The screen thread applies instructions at frame boundaries. The
  UI thread never runs plugin code.

Long-running work and coalescing
(`zellij-server/src/background_jobs.rs`):

- `BackgroundJob` is an enum: file search, web requests, plugin
  load, `RenderToClients`. Each job spawns a tokio task on the
  global runtime.
- `RenderToClients` coalesces pending renders. A `last_render_request`
  Mutex holds at most one pending render task (a `REPAINT_DELAY_MS`
  sleep). A new request during a pending render is re-queued
  instead of stacked. Rapid input produces one render.
- `AnimatePluginLoading` and `StopPluginLoadingAnimation` jobs show
  a loading animation while WASM plugins compile in the background.
  This is a skeleton-screen pattern: placeholder UI while the real
  content builds.

UI responsiveness: the screen thread only polls its bus with
timeouts and applies atomic state changes. All slow work runs in
spawned tasks and reports back as bus messages.

### 4. ftdv and diff-tui

Repos: https://github.com/wtnqk/ftdv (crate `ftdv` 0.1.2),
https://github.com/chouxcreams/diff-tui (crate `diff-tui` 0.1.1)

Pattern: lazy per-file load plus viewport culling. Neither app uses
virtualized background loading. Both load the diff of one file at a
time instead of the whole repository.

ftdv key structures:

- File selection triggers `git_executor.get_file_diff(mode, path)`
  in `src/main.rs`. The diff is a synchronous `git` subprocess.
  Only the selected file loads.
- `render_diff_content` in `src/render.rs` builds a `Paragraph`
  from `Text` (ANSI parsed with `ansi-to-tui`) with
  `.scroll((v, h))` and `.wrap(Wrap { trim: false })`. Ratatui's
  `Paragraph` renders only the visible window.
- Width-change debounce: `should_refresh_diff_width` re-fetches the
  diff only when the width shifts by more than 5 columns.

diff-tui key structures:

- `src/git/diff.rs`: `get_diff` shells out to `delta` when present
  and falls back to `git diff`. The diff text arrives whole.
- `src/app.rs` stores `diff_lines: Vec<Line<'static>>` and
  `diff_scroll: u16`. `draw_diff_view` slices the visible window:
  `diff_lines.iter().skip(diff_scroll).take(height)` feeds a plain
  `Paragraph`. This is slice virtualization: O(viewport) per
  frame, O(file) in memory.

Both viewers converge on the same answer for large diffs: load one
file, materialize its lines, cull to the viewport. Neither moves
the load into a background worker. The blocking cost lands on open,
not on scroll.

### 5. Livediff

Repo: https://github.com/SoCkEt7/Livediff (crate `livediff` 3.4.0)

Pattern: debounced burst batching of file events, event-driven
redraws, per-file diff computation.

Key structures:

- `src/adapters/watcher.rs` runs a `notify::RecommendedWatcher`
  into an `mpsc::unbounded_channel`. `FileMonitor::run` sleeps
  `debounce_ms` (default 25 ms), then drains the burst with
  `try_recv` in a loop. One batched `FilesChanged` event leaves
  per burst. The code comment reads "Debounce / batch burst
  modifications."
- `src/main.rs` merges all sources (Key, Mouse, Tick, Watcher)
  into a single `mpsc::channel(4096)`. Input and tick run on a
  dedicated OS thread to avoid blocking the runtime.
- Tick rate is a runtime-tunable `AtomicU64` (default 150 ms,
  user-cycled with `+` / `-`).
- Diff computation uses the `similar` crate in
  `src/domain/diff_engine.rs`, triggered per batched change.

For large file changes the cost is bounded: only changed files
re-diff per batch, and the UI redraws only on events. There is no
background diff thread. The diff runs in the async loop but stays
bounded to the batch.

### 6. tui-scrollview

Repo: https://github.com/ratatui/tui-widgets (crate
`tui-scrollview` 0.6.7, ~454k downloads)

Pattern: content double buffering. Pre-render the whole content
once, copy only the visible window per frame.

Key structures:

- `ScrollView::new(Size)` owns a `Buffer` of full content size.
  `render_widget` / `render_stateful_widget` draw content into that
  buffer. `render(area, buf, &mut ScrollViewState)` copies only
  the visible window. Per-frame cost is O(visible_rows x cols),
  one Cell clone per cell, zero heap allocation.
- `ScrollViewState` is a small `Copy` struct (offset and page
  size) with `scroll_up` / `scroll_down`, page scrolling,
  `scroll_to_top` / `scroll_to_bottom`, and `is_at_bottom()` for
  tail-follow.
- The docs state the intended use: store the `ScrollView` when the
  content is expensive to rebuild. A stored scroll view renders by
  reference while `ScrollViewState` keeps the current offset.

There is no built-in background content building. The caller builds
the content buffer (possibly on another thread, then swaps it in)
and stores it. The crate provides the storage and the state. This
is exactly the double-buffer swap that Option A needs.

Memory caveat for this project: the content buffer holds every
cell of every line. At 146,149 lines x 120 columns that is about
17.5 M cells, which is hundreds of MB even with cell SSO.
`tui-scrollview` fits a bounded content buffer (the last N
transcript lines) or moderate documents. For the full transcript,
keep the `Vec<Line>` plus visible-window slice and adopt only the
`ScrollViewState` offset math.

### 7. rat-salsa

Repo: https://github.com/thscharler/rat-salsa

Pattern: an event-loop framework with a task system. Background
results re-enter the main loop as ordinary events.

Key structures (`rat-salsa/src/lib.rs`, `thread_pool/mod.rs`):

- `SalsaContext::spawn(task)` runs
  `FnOnce() -> Result<Control<Event>, Error> + Send` on a
  `ThreadPool` of N OS threads. Tasks flow through an unbounded
  channel. Results flow through a second unbounded channel.
- Worker threads wrap tasks in `catch_unwind` and mark `Liveness`.
  Panics never kill the pool.
- `spawn_ext` hands the task a `Cancel` token (cooperative
  `AtomicBool`) and a `Sender` for multiple results.
- `spawn_async(future)` runs on a tokio runtime and returns an
  `AbortHandle`.
- Results rejoin the main loop as `Control<Event>` through
  `PollTasks` / `PollTimers`. The render thread only polls.

Companion crates:

- `rat-ftable` renders only visible cells. The README states this
  makes rendering effectively O(1) in the number of rows. This is
  row virtualization for large data sets.
- `rat-scrolled` wraps content in scroll state.

For this project: `spawn` plus a result channel plus polling is a
minimal, dependency-light sketch of the background build worker.

### 8. tui-skeleton and ratatui-splash-screen

Repos: https://github.com/jharsono/tui-skeleton (crate
`tui-skeleton` 0.3.0), https://github.com/orhun/ratatui-splash-screen
(crate 0.1.5)

Pattern: placeholder content while the real content builds.

- `tui-skeleton` is a library of placeholder widgets
  (`SkeletonBlock`, `SkeletonTable`, `SkeletonList`,
  `SkeletonText`, `SkeletonBarChart`, `SkeletonStreamingText`).
  They pulse, sweep, or shimmer while data loads. All widgets are
  stateless: pass `elapsed_ms` from the event loop and the
  animation is a pure function of the timestamp. The app shows the
  skeleton while a worker builds real content, then swaps in the
  real widget on data arrival.
- `ratatui-splash-screen` turns any image into a splash screen.
  `SplashConfig { image_data, sha256sum, render_steps, use_colors }`
  advances one step per frame and reports done via
  `is_rendered()`. This is progressive rendering: the one-off
  image-to-halfblock conversion is amortized over `render_steps`
  frames instead of blocking one frame.

Both answer the "what does the user see during the 7-second
build" question. A skeleton or the last good window is the
standard answer. Neither crate builds content in the background.
They only provide the placeholder half.

### 9. ratatui-markdown (project fork)

Repo: https://github.com/celestia-island/ratatui-markdown (rev
`3a8bcbe`, the git dependency in `bin/tui/Cargo.toml`). Source
read from the local cargo checkout.

Pattern: pre-rendered line vector plus viewport windowing, with a
dual-mode ("hybrid") scroll system.

Key structures:

- `HybridScrollView` (`src/scroll/hybrid_scroll/mod.rs`) holds
  pre-rendered `Vec<Line<'static>>` and `Vec<FocusableRegion>`.
  `render()` slices `(scroll_offset .. scroll_offset +
  viewport_height)` into a `Paragraph`. Per-frame cost is
  O(visible lines).
- "Hybrid" refers to navigation modes, not to incremental
  building: free row scrolling, and an "engaged" mode that
  navigates focusable items with a cursor.
- `MarkdownViewer` (`src/viewer/mod.rs`) memoizes: `ensure_rendered`
  renders all lines once per (content, width, theme) triple. A
  test named `duplicate_content_does_not_rerender` pins this.
  There is no background build. The caller builds the lines.
- `FollowScrollState` (`src/scroll/follow_scroll.rs`) handles
  tail-follow for streaming content. It follows the tail while
  streaming and pins a manual offset after user scroll.

The hybrid scroll system renders only the visible portion of large
markdown documents by windowing a pre-rendered line vector. It
does not move the build off the thread. For this project the fork's
viewer pattern matches the current `build_transcript` design: the
build is the expensive step, and the scroll system only solves the
render step.

### 10. ratatui Buffer diffing

Repo: https://github.com/ratatui/ratatui

- Source: `ratatui-core/src/terminal.rs`, `buffer/diff.rs`
- Docs: https://ratatui.rs/concepts/rendering/under-the-hood/

How the immediate-mode model handles expensive frames:

- `Terminal` keeps two buffers (`buffers: [Buffer; 2]`). Each draw
  pass starts from an empty current buffer. Widgets redraw the
  whole viewport into it.
- `Terminal::flush` diffs the current buffer against the previous
  one with a zero-allocation `BufferDiff` iterator. Only changed
  cells reach the backend. `swap_buffers` flips the pair.
- On resize, the previous buffer resets, so the next draw is a
  full redraw.

The per-frame cost is therefore: (1) the render callback building
content, plus (2) an O(viewport cells) diff, plus (3) terminal
I/O. Steps 2 and 3 are cheap. Step 1 is where the 7-second
`build_transcript` lives. The docs state the consequence directly:
if the rendering thread blocks, the UI will not update until the
thread resumes.

Recurring guidance from the ratatui discussions and issues:

- Discussion #1927: even a no-op `draw` at 60 FPS costs CPU
  because buffer diffing runs every frame. The thread converges on
  gating the draw loop on a dirty flag.
- Issue #1798: the failure mode is rebuilding or cloning
  `Paragraph` content every frame. Keep the `Text` and re-render
  it.
- Issue #1514: multi-line `List` items cause late rendering when
  scrolling. Large content in scrolling widgets is a known pain
  point.

The synthesis: pre-build content, render a slice, gate the tick
loop.

### 11. gitui

Repo: https://github.com/gitui-org/gitui

Pattern: a global thread pool, a single-slot coalescing job queue,
and stale-while-revalidate content.

Key structures:

- A global rayon pool of 4 threads (`src/main.rs`,
  `rayon_core::ThreadPoolBuilder`). The UI loop `select`s on
  crossbeam channel receivers and applies async notifications on
  the UI thread.
- `AsyncSingleJob` (`asyncgit/src/asyncjob/mod.rs`) is the core
  primitive. Its doc comment: "a FIFO task queue that will only
  queue up one `next` job. It keeps overwriting the next job until
  it is actually taken to be processed."
- Fields: `next: Arc<Mutex<Option<J>>>` (coalesced pending work),
  `last: Arc<Mutex<Option<J>>>` (finished result, read once via
  `take_last()`), `progress: Arc<RwLock<J::Progress>>`,
  `sender: Sender<J::Notification>`.
- `spawn(task)` starts the task if nothing runs, otherwise
  schedules it as `next`, overwriting the pending job. Rapid
  spawn calls collapse to one run. This is the coalescing pattern
  in its purest form.

Stale-while-revalidate: gitui shows raw file text immediately on
open, then a syntax job runs on the pool and reports
`Progress` / `Done` on the app channel. On `Done` the component
calls `take_last()` and swaps in the highlighted text. A progress
bar reads `progress()`. The user never sees a blank pane.

Invalidation coalescing: `src/queue.rs` defines `NeedsUpdate`
bitflags (`ALL | DIFF | COMMANDS | BRANCHES | REMOTES`). A burst
of invalidations becomes one update pass.

### 12. bottom

Repo: https://github.com/ClementTsang/bottom

Pattern: three worker threads, one event channel, last-good-data
between updates.

Key structures (`src/lib.rs`):

- A collection thread, an input thread, and a cleaning thread all
  push `BottomEvent`s into one `mpsc::channel`. The main loop
  `recv()`s and applies.
- The app draws once before entering the loop, with the comment
  that the first frame must not feel frozen.
- `BottomEvent::Update(Box<Data>)` arrives at the configured
  `update_rate`. Until the next update, the last dataset stays on
  screen. This is stale-while-revalidate at the data level.
- The cleaning thread trims time-series history after
  `retention_ms`, so stale data is bounded in memory.

### 13. codex-TUI (closest sibling)

Repo: https://github.com/openai/codex (the `codex-rs/tui` crate)

codex-TUI renders LLM session transcripts, the same content class
as this project. It is the closest sibling surveyed.

Pattern: trailing-debounced transcript reflow, width-keyed cell
cache, and partial-history states.

Reflow state (`codex-rs/tui/src/transcript_reflow.rs`, verified
from source):

- `TRANSCRIPT_REFLOW_DEBOUNCE: Duration = Duration::from_millis(75)`.
- `TranscriptReflowState` tracks `last_observed_width`,
  `last_reflow_width`, `pending_reflow_width`, and
  `pending_until` (a trailing-debounce deadline).
- Repeated resize events push the deadline out. A drag-resize
  rebuilds at the final width, not at intermediate ones.
- The state separates the observed width from the rebuilt width.
  A terminal reports intermediate sizes during a drag, then
  settles. Comparing against the last rebuilt width lets the
  follow-up draw request one more rebuild.
- A `schedule_immediate()` escape hatch exists for cases where
  waiting would leave stale wrapped stream rows visible.

Width-keyed cell cache (`codex-rs/tui/src/history_cell/
markdown_render_cache.rs`):

- Each markdown history cell keeps
  `Mutex<Option<(MarkdownRenderCacheKey, Vec<HyperlinkLine>)>>`.
- The key includes the width, the terminal render mode, and a
  content revision that the app bumps when the active cell
  changes. A width change misses the cache and re-wraps. The same
  width hits it.
- The transcript is `Vec<Arc<dyn HistoryCell>>`. Cells expose
  `desired_height(width)` so layout math stays cheap.

History pagination (`codex-rs/tui/src/app/history_pagination.rs`):

- `TranscriptHistoryState::{Partial, Complete, LoadingBeginning}`.
  While an older page loads, the view stays on the last built
  cells and marks the top `Partial`. Only `LoadingBeginning`
  (no cells yet) shows a spinner.
- The tick gate matches this project's own audit doc section 7.2:
  16 ms while live, 100 ms otherwise.

### 14. yazi

Repo: https://github.com/sxywu/yazi (crates `yazi-scheduler`,
`yazi-shared`)

Pattern: typed worker threads per task class, plus reusable
debounce and throttle combinators.

Key structures:

- `yazi-scheduler/src/worker.rs` runs a scheduler of typed worker
  threads. Each task class (file, plugin, fetch, preload, size,
  process, hook) gets its own unbounded channel and N worker
  threads. Results come back as `TaskOut` messages into the app
  event stream.
- `yazi-shared/src/debounce.rs` ships a `Debounce<S: Stream>`
  combinator. It holds the last value and emits only after an
  `interval` of silence.
- `yazi-shared/src/throttle.rs` ships a `Throttle<T>` combinator
  that batches items and flushes them per interval.

### 15. ratatui's own examples and local precedent

- `ratatui/examples/apps/async-github/src/main.rs` is the
  canonical `Arc<RwLock>` plus `tokio::spawn` form. The widget
  clones the `Arc`, the worker writes under `write()`, the UI
  renders under `read()`. The docs note that ongoing updates use a
  channel that refreshes on demand or on a timer.
- `bin/tui/src/picker/fuzzy.rs` already implements the worker
  shape in this repo. `PickerMatcher` spawns a `std::thread`
  worker on an `mpsc` `Cmd` channel. The worker publishes
  immutable snapshots behind `Arc<Mutex<Arc<Snapshot>>>`. The UI
  reads via `snapshot()` without blocking. The transcript builder
  reuses this shape. It does not invent a new one.

## Patterns with sketches

### P1. Background builder plus atomic swap

The gitui shape, trimmed to this project:

```rust
struct BuildWorker {
    req: mpsc::Sender<BuildRequest>,   // one worker thread
    ready: Arc<RwLock<Option<(CacheKey, Arc<Transcript>)>>>,
}
```

- One worker thread owns the build. It reads a `mpsc` request
  channel. Each request carries a snapshot of the build inputs
  (events, width, palette, ext replies) plus the target cache key.
- The worker writes the finished build into `ready` under the
  write lock. The draw path reads `ready` under the read lock,
  checks the key, and renders the visible window.
- The draw path never calls `build_transcript`.

### P2. Stale-while-revalidate

The render decision while a build is in flight:

- Build finished and key matches: render it. No indicator.
- Build in flight, last good result present: render the last good
  result and show a small loading indicator. A stale width or a
  stale palette is acceptable for a moment.
- No result yet (first build of the session): render a placeholder
  or the visible tail window only.

This is the gitui raw-text-then-highlight shape, the codex
`Partial` state, and the tui-scrollview store-and-render-by-
reference guidance.

### P3. Trailing debounce for width bursts

The codex 75 ms shape:

- On a width-triggered miss, do not build immediately. Record
  `pending_width` and a deadline of now plus 75 ms.
- Every new width event resets the deadline. A drag-resize or a
  browse toggle plus scroll burst fires one build at the final
  width.
- Compare the pending width against the last built width, not the
  last observed width. A settled width that arrives after a build
  still triggers one more build.
- Event-commit misses (`events_version` bumps) build without
  debounce. The user is not generating those in bursts.

### P4. Width-keyed memo (Option B)

- Key each event's wrapped `Line` output on
  `(event_content_hash, width)`.
- A new event wraps only the tail. A width change re-wraps only
  events whose cached width differs.
- The worker consults the memo before building. Repeated browse
  toggles hit the memo and stay off the hot path.
- This composes with P1: the memo is the worker's fast path. The
  first build of a new width still runs off-thread.

### P5. Visible window first

- On a miss, build the tail window (the visible region) first and
  swap it in. The full build follows in the background.
- This is the helix split: lay out the viewport range immediately,
  keep the full build off-thread.
- It makes even the first miss non-blocking.

## Synthesis for this project

The freeze investigation isolates one remaining cost: a
synchronous full `build_transcript` on the render thread at every
cache-key miss (about 7 s on the 24 MB log). The ecosystem
consensus on the fix shape is unanimous:

1. Draw the last good window while a worker thread rebuilds from a
   snapshot. This is the zellij `RenderToClients` shape, the atuin
   paint-before-first-query rule, the bottom last-data rule, and
   the tui-scrollview store-and-render-by-reference guidance.
2. Deliver the finished build back to the render thread as an
   event, then swap it into the cache. This is the gitui
   `AsyncSingleJob` shape and the rat-salsa task pattern.
3. Coalesce the request queue. A single-slot pending build drops
   superseded requests. This is gitui's overwrite-`next` and
   zellij's `last_render_request`.
4. Debounce the width triggers with a 75 ms trailing window. Build
   at the settled width, not at drag intermediates. This is the
   codex `transcript_reflow` rule.
5. Build a visible window first, the full document second. Helix
   proves the split. It makes even the first miss non-blocking.
6. Option B is the width-keyed memo from P4. It removes repeated
   toggles from the hot path but not the first build of a new
   width. Option A subsumes it, as the investigation already
   notes. Option A and Option B compose: the worker consults the
   memo before building.

The local `picker/fuzzy.rs` worker already proves the thread plus
channel plus snapshot-swap shape. The transcript worker reuses it.
No new dependency is required. `std::thread` plus `mpsc` plus
`Arc<RwLock>` covers the design.

## Source index

- Helix: https://github.com/helix-editor/helix
  - `helix-view/src/view.rs`, `helix-view/src/document.rs`,
    `helix-term/src/ui/editor.rs`, `helix-term/src/ui/document.rs`,
    `helix-core/src/syntax.rs`, `helix-vcs/src/diff.rs`,
    `helix-lsp/src/client.rs`
- tree-house: https://github.com/helix-editor/tree-house
- Atuin: https://github.com/atuinsh/atuin
  - `crates/atuin-client/src/database.rs`,
    `crates/atuin/src/command/client/search/interactive.rs`,
    `crates/atuin/src/command/client/search/history_list.rs`,
    `crates/atuin/src/command/client/search/engines/engines.rs`,
    `crates/atuin/src/command/client/search/engines/db.rs`,
    `crates/atuin-search-bench`
- Zellij: https://github.com/zellij-org/zellij
  - `zellij-server/src/plugins/plugin_worker.rs`,
    `zellij-server/src/thread_bus.rs`,
    `zellij-server/src/background_jobs.rs`
- ftdv: https://github.com/wtnqk/ftdv (`src/main.rs`, `src/render.rs`)
- diff-tui: https://github.com/chouxcreams/diff-tui
  (`src/app.rs`, `src/git/diff.rs`)
- Livediff: https://github.com/SoCkEt7/Livediff
  (`src/main.rs`, `src/adapters/watcher.rs`,
  `src/use_cases/process_file_change.rs`)
- tui-scrollview: https://github.com/ratatui/tui-widgets
- rat-salsa: https://github.com/thscharler/rat-salsa
  (`rat-salsa/src/lib.rs`, `thread_pool/mod.rs`, `rat-ftable`)
- tui-skeleton: https://github.com/jharsono/tui-skeleton
- ratatui-splash-screen:
  https://github.com/orhun/ratatui-splash-screen
- ratatui-markdown fork:
  https://github.com/celestia-island/ratatui-markdown (rev `3a8bcbe`)
- ratatui: https://github.com/ratatui/ratatui
  - `ratatui-core/src/terminal.rs`, `buffer/diff.rs`
  - https://ratatui.rs/concepts/rendering/under-the-hood/
  - discussion #1927, issue #1798, issue #1514
  - `examples/apps/async-github/src/main.rs`
- gitui: https://github.com/gitui-org/gitui
  - `src/main.rs`, `src/queue.rs`,
    `asyncgit/src/asyncjob/mod.rs`, `asyncgit/src/cached/branchname.rs`
- bottom: https://github.com/ClementTsang/bottom (`src/lib.rs`)
- codex: https://github.com/openai/codex
  - `codex-rs/tui/src/transcript_reflow.rs`,
    `codex-rs/tui/src/app/resize_reflow.rs`,
    `codex-rs/tui/src/app/history_pagination.rs`,
    `codex-rs/tui/src/history_cell/markdown_render_cache.rs`
- yazi: https://github.com/sxywu/yazi
  - `yazi-scheduler/src/worker.rs`,
    `yazi-shared/src/debounce.rs`, `yazi-shared/src/throttle.rs`
- Local: `bin/tui/src/picker/fuzzy.rs`, `bin/tui/src/app.rs`
  (`transcript_lines`), `bin/tui/src/render.rs`
  (`build_transcript`)
