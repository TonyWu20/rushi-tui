# TUI background transcript build: decision and fix plan

Status: Plan.
Last updated: 2026-09-13.
Scope: `bin/tui` transcript build and cache.
Basis: `docs/tui-perf-freeze-investigation.md` and
`docs/tui-large-content-research.md`.

## Decision.

Option A (background build worker) is the final option.
Option B (width-keyed memo) is adopted as a fast path inside
the worker. The two compose.

### Rationale.

The research surveyed 15 ratatui apps and libraries.
None freezes its UI on a content miss.
All pair stale-content rendering with off-thread rebuilds.
The background-worker-plus-swap shape is the consensus.

Option B alone does not fix the symptom.
A repeated browse toggle hits the memo.
But the first build of a new width still hitches
for about 7 seconds on the 24 MB log.
Event-commit misses are not width-keyed, so Option B
alone leaves every committed event a one-time freeze.

Option A subsumes Option B and makes every trigger
non-blocking. Browse enter and leave, scroll past the
top, committed events, palette changes, and fold toggles
all stop blocking the render thread.

The local `picker/fuzzy.rs` already implements the thread
plus channel plus snapshot shape with `std::thread` and
`mpsc`. The transcript worker reuses that shape.
No new dependency is needed.

### Rejected.

Option B standalone fixes repeated toggles only.
The first build of each new width still blocks.
Every event-commit miss still blocks.
It does not resolve the full symptom set.

Shipping no fix leaves a 7 s freeze on every
cache-key miss. The two surgical fixes already landed
removed only the chronic per-frame cost. The one-time
full build remains.

## Architecture.

One worker thread owns the full transcript build.
The render thread never calls `build_transcript`.

```
struct TranscriptWorker {
    tx: mpsc::Sender<BuildRequest>,
    result_rx: mpsc::Receiver<BuildResult>,
    _thread: thread::JoinHandle<()>,
}
```

A `BuildRequest` carries a snapshot of every input the
build reads, plus the target cache key and a monotonic
sequence number. The worker builds the snapshot into a
`TranscriptBuild` and sends it back over the channel.
The main loop polls the result before each draw.
It swaps the result into `transcript_cache` only when
the key matches the current cache key.

## The snapshot.

`build_transcript` at `render.rs` line 2621 reads a
specific set of `App` fields. Each field is `Clone` and
`Send`. A snapshot is a cheap move, not a borrow.

The snapshot fields and their sources:

- `events: Vec<Event>` — from `app.events()` line 1385.

- `events_base_seq: usize` — from `app.events_base_seq()` line 1392.

- `call_details: HashMap<String, (String, Value)>` — from `app.call_details()` line 1435.

- `pending: Option<PendingApproval>` — from `app.oldest_pending_approval()` line 2487.

- `palette: Palette` — from `app.palette()` line 609.

- `tool_display: ToolDisplay` — from `app.tool_display()` line 626.

- `tool_expanded: bool` — from `app.tool_expanded()` line 631.

- `thinking_shown: bool` — from `app.thinking_shown()` line 636.

- `thinking_expanded: bool` — from `app.thinking_expanded()` line 641.

- `expand_fracs: HashMap<String, f64>` — from `app.expand_fracs()` line 649.

- `rewind_active_ranges` — from `app.rewind_active_ranges()` line 1400.

- `active: Option<SessionId>` — from `app.active()` line 926.

- `loop_running: bool` — computed on main thread.

- `width: usize` — the render width for this frame.

- `ext_lines: HashMap<u64, Vec<ExtLine>>` — pre-resolved on main thread.

The cache key is the six-field tuple from the
investigation doc. It is
`(events_version, width, ext_ver, palette_level,
palette, frac_epoch)`. See `app.rs` line 312.

### Ext pre-resolution.

`build_transcript` calls `ext.lookup_lines` for events
an extension owns. `ExtHost` at `ext.rs` line 1169
holds an `mpsc::Receiver`. It is not `Send`.
It cannot cross to the worker.

`ExtHost::lookup_lines` at `ext.rs` line 1918 is a
synchronous cache read. It returns an owned
`Vec<ExtLine>`. `ExtLine` is `Clone` (line 561).
Before dispatch, the main thread pre-resolves ext lines:

- For each event where `ext.owner_for_kind(kind)` is
  `Some`, call `ext.lookup_lines(owner, event_id)`.

- Collect the `Some` results into
  `ext_lines: HashMap<u64, Vec<ExtLine>>`.

- Pass that map in the snapshot.

The worker uses the pre-resolved map instead of calling
`ext`. Cost is `O(events)` cache lookups.
That is negligible against the 7 s build.

### The pure build function.

Refactor `build_transcript` into
`build_transcript_input(&TranscriptBuildInput) ->
TranscriptBuild`. The per-event loop in `event_lines`
(render.rs line 311) is unchanged.
It is a pure function of the snapshot.
The existing `build_transcript(&App, width, ext)`
becomes a thin wrapper. It builds the snapshot on the
spot, then calls the pure function. Tests and the
main-thread fallback keep the `&App` form.

## Draw path: stale-while-revalidate.

The render thread no longer blocks on a build.
On a cache-key miss, `App::transcript_lines` (app.rs
line 1292) records the desired key and a "rebuild
requested" flag. It returns the last good cache.
It does not call `build_transcript`.

The main loop, before the draw call at main.rs line
1361, checks the flag. If no build is in flight, it
takes the snapshot and dispatches one request.
It sets the "in flight" flag.

`poll_transcript_worker()` runs before each draw.
It does a non-blocking `try_recv`. A result whose key
matches the current cache key swaps into
`transcript_cache`. A stale result is dropped.

### Stale-while-revalidate decision table.

- Build finished, key matches: render new cache, no indicator.

- Build in flight, last good cache present: render last good cache dimmed, show spinner.

- No cache yet (first build): render tail-window fast build or placeholder.

A stale width or palette is acceptable for the build
window. The view briefly shows the previous state.
This matches the zellij and bottom patterns.

### First build of a session.

There is no last good cache. The plan builds the
visible tail window synchronously on the main thread.
That is `O(viewport)` and under 1 ms. The full build
follows in the background. This is the helix split
from the research doc (pattern P5).

Browse features such as yank and `gg` are disabled
until the full build lands. The state is the codex
`Partial` state. The view stays on built cells and
marks the top partial.

### Transient states.

`frac_epoch`, palette, and `tool_expanded` misses are
usually transient. The coalescing queue means only the
settled state is actually built. While the transient
build is in flight, the user sees the stale render.
The tail-window fast build covers the visible region
when a transition must render fast.

## Coalescing.

At most one build is pending. A new request drops the
stale one. This is the gitui overwrite-next shape and
the zellij `last_render_request` shape.

The worker holds a single `pending` slot. If a build
is in flight and a new request arrives, it overwrites
the pending slot. Rapid width or palette changes
collapse to one build at the settled value.

When a build finishes, the worker drains the channel.
It keeps only the last request and drops superseded
ones. The main loop also enforces result-level
staleness: a result whose key no longer matches is
dropped at the swap point.

## Width-keyed memo (Option B fast path).

The memo is an LRU keyed by `(events_version, width)`.
Default capacity is 4. The worker consults the memo
before building. A repeated browse toggle between two
widths returns the cached build in microseconds.
No 7 s hitch on repeated toggles.

The memo also serves as the stale source for
stale-while-revalidate. When a miss occurs, the memo
may already hold the answer.

The memo composes with the worker. The worker
consults the memo first. A hit skips the build.
A miss runs the full off-thread build.

## Debounce.

Width-triggered misses use a 75 ms trailing window.
Repeated resize or browse-toggle events reset the
deadline. One build fires at the settled width.
This is the codex `transcript_reflow` rule.

Event-commit misses are not debounced. The user does
not generate those in bursts. They build immediately.

The pending width is compared against the last built
width, not the last observed width. A settled width
that arrives after a build still triggers one more
build.

## Staged work breakdown.

Each stage is independently committable. Every stage
passes the build gate and full test suite before
commit.

### Stage 0: committed.

Two surgical fixes from the investigation doc.
Per-frame `O(n)` copy removal. Focus-mode churn guard.
Both are in the tree and verified. Committed in the
code commit `c4ee892`.

### Stage 1: snapshot plus pure build.

Files: `bin/tui/src/render.rs`, `bin/tui/src/app.rs`.

Add the `TranscriptBuildInput` snapshot struct.
Refactor `build_transcript` into
`build_transcript_input(&TranscriptBuildInput)`.
Keep a `&App` wrapper for tests and the main-thread
fallback. Add the ext pre-resolution helper.

Test gate: a unit test asserts the snapshot build
equals the direct `&App` build via insta snapshot.

### Stage 2: worker plus coalescing.

Files: new `bin/tui/src/transcript_worker.rs`,
`bin/tui/src/app.rs`, `bin/tui/src/main.rs`,
`bin/tui/src/render.rs`.

Add the `TranscriptWorker` module. Reuse the
`picker/fuzzy.rs` shape with `std::thread` and `mpsc`.
`App` owns the worker and the result receiver.

Change `App::transcript_lines` to return the stale
cache on a miss instead of building. Record the
desired key and a rebuild flag.

In the main loop before draw, dispatch one request
when the flag is set and no build is in flight.
Call `poll_transcript_worker()` before each draw.
Swap in results that match the current key.

Add the rebuilding indicator. Reuse the working-row
spinner shape from `render.rs` line 1852.

Implement the tail-window fast build for the first
session build and transient states. Disable browse
yank and `gg` until the full build lands.

Test gate: unit tests for stale-while-revalidate,
coalescing, and first-build tail window.

### Stage 3: width-keyed memo.

Files: `bin/tui/src/transcript_worker.rs`,
`bin/tui/src/app.rs`.

Add an LRU cache keyed by `(events_version, width)`
with capacity 4. The worker consults the memo before
building. A hit publishes the cached result
immediately. A miss runs the full build.

Test gate: unit test that a repeated toggle between
two widths hits the memo. Verify the build function
is not called on the second toggle.

### Stage 4: width debounce.

Files: `bin/tui/src/transcript_worker.rs` or
`bin/tui/src/app.rs`.

Add a 75 ms trailing window for width-triggered
misses. A new width event resets the deadline.
Only the final width after the burst triggers a
build. Event-commit misses bypass the debounce.

Test gate: unit test that a burst of width events
produces one build at the settled width.

## Risks and mitigations.

- Memory: two full builds at once. The stale one
  drops on swap. On the 24 MB log that is about 35 MB
  extra, briefly.

- Stale-width flash: the view shows the previous
  width for the build window. The indicator explains
  it. Width changes are brief.

- Transient-state responsiveness: coalescing means
  only the settled state lands. The tail-window fast
  build covers the visible region for fast feedback.

- Ext pre-resolution cost: `O(events)` cache
  lookups on the main thread. Negligible against the
  7 s build.

- Worker panic: the thread dies and the channel
  closes. The UI detects the dead worker via a failed
  send. It falls back to the synchronous main-thread
  build. This is a safety valve, not the steady state.

- `frac_epoch` churn: the Stage 0 fixes removed
  the chronic per-frame bump. Remaining bumps are
  user-triggered, not per-frame.

## Test plan.

- Unit: snapshot fidelity. The snapshot build equals
  the direct `&App` build. Use insta snapshot.

- Unit: coalescing. A burst of requests builds only
  the last. Earlier requests are dropped.

- Unit: memo. A repeated toggle between two widths
  hits the memo. The build function is not called on
  the second toggle.

- Unit: debounce. A burst of width events produces
  one build at the settled width.

- Unit: stale-while-revalidate. Draw returns the last
  good cache while a build is in flight.

- Unit: first-build tail window. The viewport builds
  in under 1 ms. The full build follows.

- Integration: PTY smoke. Run the smoke gate on the
  24 MB log. Browse enter and leave. Scroll past the
  top. No frame over 100 ms during a cache-key miss.

## Open questions.

- The tail-window fast build is the largest sub-change
  in Stage 2. It can ship later if it proves riskier
  than the rest of the stage.

- The memo capacity is 4 by default. It can become
  a config value later.

- The debounce window is 75 ms. It matches the codex
  value and is tunable.
