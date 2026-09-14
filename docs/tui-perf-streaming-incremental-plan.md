# TUI streaming live-block: incremental render plan

Status: Implemented (built and tested).

Last updated: 2026-09-15.

Scope: eliminate the per-frame O(n) cost of rendering the live
stream block (thinking + response text) when the content grows to
a large chunk. The fix is an incremental cache that re-wraps only
newly appended lines and keeps the stateful code highlighter alive
across frames. This subsumes the "keep the highlighter alive"
optimisation (#2) as a prerequisite.

## Problem

`stream_block_lines` (`bin/tui/src/render.rs:1972`) is called on
every draw. While a response is streaming the main loop ticks at
16 ms (about 60 FPS, `main.rs:605`). Each call does:

1. `wrap_thinking` — creates a **new** `CodeHl` (tree-sitter or
   builtin) and re-highlights **every** hard line in the entire
   accumulated thinking text. Cost: O(n) where n = total thinking
   chars so far.
2. `wrap_markdown_p` → `render_markdown_lines` — re-parses and
   re-renders the **entire** response text with a fresh
   `MarkdownRenderer`. Cost: O(n) where n = total response chars.

As the thinking block grows to, say, 50 KB of code with fences,
each 16 ms frame pays ~50 KB of tree-sitter highlighting plus the
markdown re-parse. Frames stretch, the UI stutters, and CPU pegs.

The settled transcript cache (`transcript_cache`, stage 0 of the
background-build plan) already solves this for the settled log.
The live stream block is a separate, uncached path.

## Design

### Invariant

The live stream block cache stores, for each logical section
(thinking, response text), the raw source prefix that produced the
cached wrapped lines plus the wrapped lines themselves. On a new
call:

- If the source has not changed (same prefix), return the cached
  lines — no re-wrap, no re-highlight, no re-parse.
- If the source has grown (prefix + new suffix), wrap and highlight
  **only the new suffix** and append to the cached lines.
- If the source has changed non-incrementally (width change,
  palette change, collapse/expand toggle, engine swap, session
  switch), invalidate the cache and rebuild from scratch.

The thinking path is fully incremental because `wrap_thinking`
processes one hard line at a time with a stateful `CodeHl`. The
response-text path uses a whole-document markdown parser
(`render_markdown_lines`), so it gets a "cache-if-unchanged"
optimisation (skip the re-parse when the text is identical to the
last frame) but not true incremental parsing. A streaming
markdown parser is a future improvement, out of scope here.

### Data structures

Add to `app.rs` (near the existing `transcript_cache` field):

```rust
/// Incremental cache for the live stream block
/// (docs/tui-perf-streaming-incremental-plan.md).
///
/// Holds the wrapped output for the thinking and response-text
/// sections plus the source prefix that produced them. On each
/// call to `stream_block_lines`, only the appended suffix is
/// re-wrapped. The stateful `CodeHl` persists so code fences
/// that span the append boundary stay coherent.
pub(crate) struct StreamBlockCache {
    // ── thinking section ────────────────────────────────
    /// The raw joined thinking text that `think_lines` was
    /// produced from. On a new call, if the current joined text
    /// starts with this string, only the suffix is wrapped.
    think_src: String,
    /// Wrapped, highlighted lines for `think_src`.
    think_lines: Vec<ratatui::text::Line<'static>>,
    /// The persistent code highlighter, carried across calls so
    /// block-comment state in fences stays open.
    think_hl: crate::tool_display::CodeHl,
    /// Fence state at the end of `think_src` (whether we are
    /// inside a code fence and what language tag was opened).
    think_in_fence: bool,
    think_fence_lang: Option<String>,

    // ── response-text section ───────────────────────────
    /// The response text that `text_lines` was produced from.
    /// `render_markdown_lines` is a whole-document parser, so we
    /// can only skip the re-parse when the text is unchanged.
    text_src: String,
    /// Rendered lines for `text_src`.
    text_lines: Vec<ratatui::text::Line<'static>>,

    // ── config snapshot for invalidation ────────────────
    /// The width, palette level, highlight engine, and
    /// thinking_expanded flag at the time the cache was built.
    /// A change to any of these invalidates the cache.
    width: usize,
    palette_level: crate::color::Level,
    engine: crate::tool_display::HighlightEngine,
    thinking_expanded: bool,

    // ── join-skip fingerprint ───────────────────────────
    /// Cheap way to detect that the `reasoning` map is unchanged
    /// without re-joining it. `think_reasoning_keys` is the sorted
    /// id set; `think_reasoning_len` is the total char length
    /// across all values. If both match the previous frame, the
    /// O(T) join is skipped (see "Join-skip optimisation").
    think_reasoning_keys: Vec<String>,
    think_reasoning_len: usize,
}

impl StreamBlockCache {
    /// Construct the cache for the active highlight engine.
    /// `CodeHl` is engine-dependent and has no `Default`, so the
    /// cache must be built with the engine it will use.
    fn new(engine: crate::tool_display::HighlightEngine) -> Self {
        // `think_hl` starts fresh; all other fields start empty /
        // default and are filled on the first build.
        Self {
            think_src: String::new(),
            think_lines: Vec::new(),
            think_hl: crate::tool_display::CodeHl::new(engine),
            think_in_fence: false,
            think_fence_lang: None,
            text_src: String::new(),
            text_lines: Vec::new(),
            width: 0,
            palette_level: crate::color::Level::DEFAULT,
            engine,
            thinking_expanded: false,
            think_reasoning_keys: Vec::new(),
            think_reasoning_len: 0,
        }
    }
}
```

Note: `StreamBlockCache` is `!Send` (it owns `CodeHl`, which may
hold a tree-sitter parser). That is fine — it lives on the main
thread alongside `App`.

Add to `App`:

```rust
stream_block_cache: Option<StreamBlockCache>,
```

Initialised to `None` in `App::new()`. Cleared in
`App::clear_stream()` (which is called when the stream settles
into the transcript) and on session switch.

### Refactor `wrap_thinking`

Currently `wrap_thinking(text, wrap_w, palette, style, engine)`
creates a fresh `CodeHl` internally and processes the entire text.
Split it into two functions:

```rust
/// Wrap an **incremental suffix** of thinking text.
///
/// `hl`, `in_fence`, `fence_lang` carry the state from the
/// previous call (the already-wrapped prefix lines live in the
/// cache, not as a parameter). Returns the new lines for the
/// suffix plus the updated fence state.
pub(crate) fn wrap_thinking_delta(
    suffix: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
    hl: &mut CodeHl,
    in_fence: bool,
    fence_lang: Option<&str>,
) -> (Vec<Line<'static>>, bool, Option<String>)
```

The body is the same loop as today's `wrap_thinking`, but:

- starts from the passed `hl` / `in_fence` / `fence_lang`
  instead of creating fresh state
- processes only `suffix` (the newly appended text)
- returns the updated `in_fence` and `fence_lang` so the next
  call can continue

The existing `wrap_thinking` becomes a thin wrapper that calls
`wrap_thinking_delta` with empty prefix state (or is kept for
settled-transcript builds, which are non-incremental).

### Refactor `stream_block_lines`

```
struct StreamBlockView<'a> {
    header:      Vec<Line<'static>>,   // 1 line, built fresh
    think:       &'a [Line<'static>],  // borrowed from cache
    text:        &'a [Line<'static>],  // borrowed from cache
    tool_args:   Vec<Line<'static>>,   // a few lines, built fresh
    cursor_span: Option<Span<'static>>, // overlay on last body line; None when done
}

fn stream_block_lines<'a>(app: &'a App, width, max_body_lines)
    -> StreamBlockView<'a> {
    buf = app.stream_buf()?;
    cache = app.stream_block_cache_mut();

    // Invalidation check.
    let needs_invalidate = !cache
        .map_or(false, |c|
            c.width == width
            && c.palette_level == app.palette().level()
            && c.engine == *app.tool_display().highlight_engine
            && c.thinking_expanded == app.thinking_expanded()
        );

    if needs_invalidate {
        cache = Some(StreamBlockCache::new(app.tool_display().highlight_engine));
    }

    // Record the config snapshot AFTER the decision, so the next
    // frame's `needs_invalidate` check reads the current values.
    // Without this, the sentinel `width = 0` from `new()` would
    // never match and the cache would rebuild every frame.
    cache.width = width;
    cache.palette_level = app.palette().level();
    cache.engine = *app.tool_display().highlight_engine;
    cache.thinking_expanded = app.thinking_expanded();

    // ── thinking section ──────────────────────────────
    let think_lines: &[Line] = if app.thinking_shown() && !buf.reasoning.is_empty() {
        // Join-skip: fingerprint the reasoning map cheaply.
        // `StreamBuf.reasoning` is a `HashMap<String, String>`.
        // If the key set and total length are unchanged, the joined
        // string is unchanged, so skip the O(T) join entirely.
        let mut cur_keys: Vec<String> =
            buf.reasoning.keys().cloned().collect();
        cur_keys.sort();
        let cur_len: usize =
            buf.reasoning.values().map(|s| s.len()).sum();

        if cur_keys == cache.think_reasoning_keys
            && cur_len == cache.think_reasoning_len
        {
            // Fingerprint unchanged: borrow the cached lines.
            &cache.think_lines[..]
        } else {
            // Re-join (O(T)) only when the map actually changed.
            let joined = buf.reasoning_text();
            cache.think_reasoning_keys = cur_keys;
            cache.think_reasoning_len = cur_len;

            if joined.starts_with(&cache.think_src) {
                // Incremental: wrap only the suffix.
                let suffix = &joined[cache.think_src.len()..];
                let (new_lines, in_fence, fence_lang) =
                    wrap_thinking_delta(suffix, wrap_w, palette, style,
                                         &mut cache.think_hl,
                                         cache.think_in_fence,
                                         cache.think_fence_lang.as_deref());
                cache.think_lines.extend(new_lines);
                cache.think_in_fence = in_fence;
                cache.think_fence_lang = fence_lang;
                cache.think_src = joined;
            } else {
                // Full rebuild (first call, invalidation, or re-order).
                let (lines, in_fence, fence_lang) =
                    wrap_thinking_full(&joined, wrap_w, palette, style,
                                       &mut cache.think_hl,
                                       cache.think_in_fence,
                                       cache.think_fence_lang.as_deref());
                cache.think_lines = lines;
                cache.think_in_fence = in_fence;
                cache.think_fence_lang = fence_lang;
                cache.think_src = joined;
            }
            &cache.think_lines[..]
        }
    } else {
        &[]
    };

    // ── response-text section ─────────────────────────
    let text_lines: &[Line] = if buf.text.is_empty() {
        &[]
    } else if cache.text_src == buf.text {
        &cache.text_lines[..]          // unchanged: borrow, no clone
    } else {
        let lines = wrap_markdown_p(&buf.text, wrap_w, palette, prose);
        cache.text_src = buf.text.clone();
        cache.text_lines = lines;
        &cache.text_lines[..]
    };

    // Build the view: borrow the big cached sections, and build the
    // small fresh pieces (header, tool_args) as owned lines. The
    // cursor is a span overlay (cursor_span), not a stored line.
    // The sliding-window / tool_args / header logic is unchanged.
    StreamBlockView {
        header, think: think_lines, text: text_lines, tool_args,
        cursor_span,
    }
}
```

The single call site (render.rs:3558) consumes the value three
ways: `.len()` at 3559, index access at 3620, and `.iter()` at
3651. `StreamBlockView` implements all three over the header,
think, text, tool_args order, so no flat `Vec` is needed. The
browse/yank path at 3651 is unaffected. On an idle frame the
borrowed slices cost nothing.

One subtlety: the cursor is a span overlaid on the last body line
(render.rs:2140). `iter()` and `get()` must yield that last line
with the cursor span applied. This keeps the 3651 stringification
byte-identical to today's flat `Vec`. When the body is empty the
cursor is its own line instead.

### Invalidation rules

| Trigger | Effect |
|---|---|
| `width` changes (terminal resize) | full rebuild of both sections |
| `palette_level` changes (palette switch) | full rebuild |
| `highlight_engine` changes | full rebuild + new `CodeHl` |
| `thinking_expanded` toggles | thinking section rebuilds (collapsed = 0 lines, expanded = full); text section unaffected |
| `thinking_shown` toggles | no rebuild; just show or hide the cached lines |
| `clear_stream()` / session switch | drop the entire `StreamBlockCache` |
| reasoning id re-order (new id appears before existing ones) | thinking section full rebuild (prefix no longer matches); text section unaffected |

### What is NOT cached

- **Partial tool-call args** (`buf.tool_args`): always re-rendered
  from scratch. They are a handful of lines, not worth caching.
- **Header row** (`" …"` / `" · done"`): one line, trivial.

### `clear_stream` integration

`App::clear_stream()` (called when the stream settles into the
transcript via the `assistant_message` event) must set
`stream_block_cache = None`. This releases the cached lines and
the `CodeHl` (which holds a tree-sitter parser) so memory returns
to the baseline.

### Join-skip optimisation (in scope)

The wrapped-line cache still recomputes the source join every frame.
`StreamBuf::reasoning_text()` sorts the `reasoning` keys and joins
the values, which is an O(T) copy even on a pure cache hit.
The fix fingerprints the `reasoning` map so the join is skipped
when nothing changed.

Two new cache fields track the key set and total value length:

```rust
think_reasoning_keys: Vec<String>,   // sorted id set
think_reasoning_len: usize,          // total chars across all values
```

In `stream_block_lines`, compare the fingerprint before joining.
On a match, skip the join and reuse the cached lines.
On a mismatch, re-join once, refresh the fingerprint, then run
the prefix-vs-suffix check as before.

The fingerprint is exact for the streaming path.
Reasoning values grow only via `push_str`, and the id set only
grows within a stream. The pair (sorted keys, total length)
changes only when the map changes. The check costs O(k log k)
where k is the number of reasoning ids, usually 1 to 2.

### Clone avoidance: borrowed slices (superseded)

Superseded at build. See "Build deviations".

The wrapped-line cache still copies O(T) memory on every frame.
`stream_block_lines` returns an owned `Vec<Line<'static>>`, so a
cache hit clones the whole vector. Each `Line` owns a `Vec<Span>`,
and each `Span` owns its content string. That copy happens every
idle frame for no benefit.

**Decision: return a `StreamBlockView<'a>` with borrowed slices.**

`StreamBlockView` borrows the big `think` and `text` sections
from the cache as `&'a [Line<'static>]`. The lifetime `'a` is tied
to `&'a App`. The small fresh pieces (header, tool_args) are owned `Vec`s built
per frame. The cursor is a span overlay on the last body line. On
a cache hit the big sections cost nothing. The caller renders the
pieces in order at render.rs:3558.

Considered and rejected:

- **`Rc<Vec<Line<'static>>>` in the cache.** Cloning the `Rc`
  on a hit is O(1), but the caller still calls `rc.to_vec()`,
  paying the O(T) copy again. Not a fix on its own.
- **Per-line `Cow<'a, Line<'static>>`.** Adds indirection.
  A section is either fully fresh or fully cached, so per-line
  ownership adds no benefit over a plain slice.

The incremental-append path still builds a new `Vec` from old
lines plus the new delta, at O(T+D). That copy is unavoidable
with a `Vec`. Structural sharing (a persistent deque) would
remove it, but that is out of scope. The dominant win is
removing the per-frame O(T) clone on idle frames.

### Residual caveats

The prefix check `starts_with` on the joined thinking string
succeeds only when the growing value is last in sorted-key order.
If an earlier id gains text, the join changes at a non-suffix
position. The `starts_with` check then fails, so the thinking
section does a full O(T) rebuild. That is correct but not
incremental. The common streaming case (one growing reasoning
item) stays incremental.

`wrap_thinking_delta` processes hard lines, so the suffix must
start at a hard-line boundary. This holds today because deltas
append to whole reasoning values via `push_str`. Each value's hard
lines are separated by a newline. Document the assumption so a
future mid-line edit path cannot silently corrupt wrapping.

## Performance model

Let `T` = total thinking chars, `D` = new chars this frame,
`W` = total response-text chars, `k` = reasoning id count.

| Frame type | Before (current) | After (incremental) |
|---|---|---|
| Streaming (D > 0) | O(T) highlight + O(W) markdown parse | O(D) highlight + O(W) markdown parse + O(k log k) fingerprint |
| Idle (D = 0, W unchanged) | O(T) + O(W) | O(k log k) fingerprint + O(1) borrow (no join, no clone) |
| Idle (D = 0, W growing) | O(T) + O(W) | O(k log k) fingerprint + O(W) re-parse |
| Width change | O(T) + O(W) | O(T) + O(W) (full rebuild, same as before) |

For a 50 KB thinking block at 60 FPS, the before cost is
~50 KB × 60 = 3 MB/s of tree-sitter work. After, highlighting is
O(D × 60) where D is the per-frame delta (a few hundred chars).
The join-skip fingerprint reduces the idle cost to O(k log k).
The borrowed-slice return eliminates the per-frame O(T) clone.

The response-text path still re-parses on every frame where the
text changed (pacing releases chars every frame). A true
incremental markdown parser is a future improvement. The
`cache-if-unchanged` optimisation at least avoids the re-parse
on frames where no new text was released.

## Test plan

All tests live in `bin/tui/src/render.rs` (or a new
`bin/tui/src/stream_cache_tests.rs` mod) under `#[cfg(test)]`.

### Unit tests

1. **`incremental_thinking_matches_full_rebuild`**
   - Feed a 10 KB thinking text in 10 chunks of 1 KB.
   - After each chunk, assert the cached `think_lines` equals
     what a full `wrap_thinking_full` of the same prefix would
     produce.
   - Final: the full cache equals the full rebuild of the whole
     text.

2. **`thinking_code_fence_spans_delta_boundary`**
   - Build a thinking text where a code fence opens in chunk 1
     and closes in chunk 2.
   - After chunk 1, the cache is in-fence with the right lang.
   - After chunk 2, the fence is closed and the line after the
     fence is highlighted as prose, not code.

3. **`width_change_invalidates_cache`**
   - Build at width 80, then call at width 120.
   - Assert the cache was rebuilt (line count differs, or a
     flag indicates a fresh build).

4. **`text_unchanged_returns_cache`**
   - Same text, two consecutive calls. Second call returns the
     same lines without re-parsing (assert via a mock or a
     side-channel counter on `render_markdown_lines`).

5. **`reasoning_reorder_triggers_full_rebuild`**
   - Start with reasoning ids `"a"` and `"b"`. Append more text to
     `"b"` (the last in sort order) — incremental, `starts_with`
     holds. Then append text to `"a"` (earlier in sort order): the
     joined string changes at a non-suffix position, so
     `starts_with` fails → full rebuild. Assert output matches a
     full rebuild.

6. **`clear_stream_drops_cache`**
   - Build the cache, call `clear_stream()`, assert
     `stream_block_cache` is `None`.

7. **`join_skip_on_unchanged_reasoning`**
   - Build the cache with reasoning text, then call again with
     the same `reasoning` map (no new deltas).
   - Assert `reasoning_text()` was not called (via a counter or
     spy). The cached `think_lines` are returned as-is.

8. **`join_triggers_on_reasoning_change`**
   - Build the cache, then append to a reasoning value.
   - Assert the fingerprint mismatch causes a re-join and the
     incremental path picks up the new suffix.

### Perf gate (in `perf_bgbuild_tests` or a new mod)

9. **`streaming_thinking_per_frame_stays_under_budget`**
   - Create 200 KB of thinking text (with code fences).
   - Simulate 60 frames: each frame appends ~3 KB (1/60 of total),
     calls `stream_block_lines`, times it.
   - Assert every frame is under 100 ms (the plan's frame budget).
   - Also assert the *average* per-frame time is < 10 ms,
     proving the incremental path is not doing O(total) work.

10. **`idle_frame_is_cached`**
   - After the cache is warm and no new deltas arrive, a frame
     (cache hit, no text change) should complete in < 2 ms.
     With the join-skip and borrowed-slice optimisations, the
     residual cost is O(k log k) fingerprint comparison. No
     highlighting, join, or markdown re-parse runs on the hot
     path.

### Integration / snapshot

11. **Existing insta snapshots** for the stream block must stay
   green. The incremental path must produce byte-identical output
   to the current full-rebuild path for the same input.

## File change map

| File | Change |
|---|---|
| `bin/tui/src/app.rs` | Add `StreamBlockCache` struct (with `think_reasoning_keys` / `think_reasoning_len` fingerprint fields) + `stream_block_cache` field + `StreamBuf::reasoning_text()` helper (joins the sorted `reasoning` map). Init in `App::new`. Clear in `clear_stream`. Expose `stream_block_cache_mut()` for the render fn. `StreamBlockCache` is `!Send` (owns `CodeHl`) — fine, lives on the main thread. |
| `bin/tui/src/render.rs` | Split `wrap_thinking` into `wrap_thinking_full` (non-incremental, used by settled builds) and `wrap_thinking_delta` (incremental). Rewrite `stream_block_lines` to use the cache and return a `StreamBlockView<'a>` (borrowed `think` / `text` slices + fresh `header` / `tool_args` + `cursor_span` overlay on the last body line). Update the call site at render.rs:3558: the view implements `.len()` / index / ordered `.iter()`, and the caller overlays the blinking cursor span on the last body line. |
| `bin/tui/src/tool_display.rs` | No change. `CodeHl` is already stateful and `Send`-free (lives on the main thread). |
| `bin/tui/src/main.rs` | No change. |
| `bin/tui/src/render.rs` tests | New test mod or extend existing. |

## Relationship to the perf plan

This is the streaming-side complement to
`docs/tui-perf-background-build-plan.md` (which covers the
settled-transcript path). Together they bound the per-frame cost
of both the settled and the live view.

The "keep the highlighter alive" idea (#2) is a prerequisite of
this plan and is folded in: `StreamBlockCache` owns the
`CodeHl`, so it persists across frames by construction.

## Build deviations

Departures from the design above, made during the build. All are
covered by the tests in `render::stream_cache_tests`.

### The view holds `Rc`, not borrowed slices

The "Clone avoidance" section decides on a `StreamBlockView`
with borrowed slices tied to `&'a App`. It rejects `Rc` up
front. The stated reason is a caller-side `to_vec()` copy.
The build changes that decision.

`draw()` interleaves `&mut App` calls between its uses of the
view. A view that borrows `App` would not coexist with those.
The view therefore holds `Rc<Vec<Line<'static>>>` for the
settled `think` and `text` sections.

The caller consumes those lines directly via `iter()`, `Index`,
and `len()`. No `to_vec()` copy runs. An idle frame pays one
O(1) refcount bump. That keeps the clone-avoidance goal on the
hot path.

The cache lives in `App`, so mutating it makes the build
signature `fn stream_block_lines(app: &mut App, ...)`, not the
spec's `&'a App`.

### The held line is rewrapped, not cached

The spec cache stores a settled source prefix and rewraps only
the appended suffix. The build splits the joined thinking text
at the last `\n`. The settled half feeds `think_lines`. The
in-progress held line is rewrapped fresh on each change.

The held line is partial and grows every frame, so it reflows
every frame anyway. Rewrapping it fresh bounds the work by the
partial line, not the total. Treating it as settled would
leave stale wraps behind. Tests 1, 2, 5, and 8 prove
byte-identity with a full rebuild at every prefix.

### The perf gate runs the Builtin engine

The perf-gate tests run on the Builtin highlight engine. The
tree-sitter highlighter re-parses its whole buffer on each line,
so its per-frame cost stays O(total). The Builtin engine isolates
the cache mechanism this plan targets.

## Open questions

- **Streaming markdown parser**: the response-text path still
  re-parses the full text on every frame where text changed. A
  true incremental markdown parser (or a line-level cache in
  `render_markdown_lines`) would make the text path incremental
  too. Out of scope for this pass; note it as a follow-up.
- **Cache size cap**: the cached `think_lines` / `text_lines`
  grow with the stream. For a very long stream (hours of
  reasoning) the cached `Vec<Line>` could be large. Consider a
  sliding cap: keep only the last `max_body_lines + margin`
  lines in the cache, since the window logic only shows the tail.
  The `CodeHl` state still tracks the full document, so the
  highlighter must see every line even if we drop old wrapped
  output. This is a memory optimisation, not a correctness issue.

## Independent verification tests

A second, independent test module `render::stream_cache_independent_tests`
(`bin/tui/src/render.rs`) verifies the mechanism on top of the
plan's own `stream_cache_tests`. It drives the cache through the
public `App` API (`set_stream_buf`, `press`, `clear_stream`) rather
than poking cache fields, so it catches wiring regressions as well.

What it covers:

- Byte-identity of the cached thinking against a fresh full rebuild
  at *irregular* chunk boundaries (1 %, 13 %, 29 %, ... splits),
  including splits inside code fences and on newline boundaries.
- Fence-state coherence across a delta boundary (open in chunk 1,
  close in chunk 2; the post-fence line is prose).
- The held-line / settled split: a held-only delta keeps the
  settled `Rc` (no merge); a completed hard line merges into a new
  `Rc`.
- A non-suffix reasoning-id change forces a full rebuild that still
  matches a fresh full rebuild.
- Invalidation matrix: width change, palette-level change, the
  `Ctrl+T` (thinking_expanded) toggle, the `Ctrl+X` (thinking_shown)
  toggle, and `clear_stream()`.
- Idle-frame zero work: the reasoning-join and markdown-parse
  counters are unchanged and the `Rc`s are identical across
  consecutive idle frames.

Two A/B performance gates quantify the benefit (Builtin engine, to
isolate the cache mechanism from tree-sitter's full-buffer re-parse):

- Streaming: 60 frames of ~800 KB. The pre-plan full-rebuild path
  is at least 2x the total cost of the incremental path
  (measured ~2.4x here).
- Idle frame: a warm idle frame is at least 10x cheaper than a
  full rebuild + re-parse (measured ~0 ms vs ~118 ms here).

The module prints its measured timings (`indep_streaming`,
`indep_idle`) so a reader can see the actual ratio on their
machine.
