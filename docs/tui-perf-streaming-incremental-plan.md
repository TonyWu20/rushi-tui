# TUI streaming live-block: incremental render plan

Status: Spec (approved for build).

Last updated: 2026-09-13.

Scope: eliminate the per-frame O(n) cost of rendering the live
stream block (thinking + response text) when the content grows to
a large chunk. The fix is an incremental cache that re-wraps only
newly appended lines and keeps the stateful code highlighter alive
across frames. This subsumes the "keep the highlighter alive"
optimisation (#2) as a prerequisite.

## Problem

`stream_block_lines` (`bin/tui/src/render.rs:1972`) is called on
every draw. While a response is streaming the main loop ticks at
16 ms (about 60 FPS, `main.rs:604`). Each call does:

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
}
```

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
/// `prev_lines` are the already-wrapped lines for the prefix.
/// `hl`, `in_fence`, `fence_lang` carry the state from the
/// previous call. Returns the new lines for the suffix plus the
/// updated state.
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
fn stream_block_lines(app, width, max_body_lines) -> Vec<Line> {
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
        cache = Some(StreamBlockCache::default());
    }

    // ── thinking section ──────────────────────────────
    let mut think_lines: Vec<Line> = Vec::new();
    if app.thinking_shown() && !buf.reasoning.is_empty() {
        let joined = thinking_text(Some(&buf.reasoning_values())).unwrap_or_default();
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
            think_lines = cache.think_lines.clone();
        } else {
            // Full rebuild (first call, invalidation, or re-order).
            let (lines, in_fence, fence_lang) =
                wrap_thinking_full(&joined, wrap_w, palette, style,
                                   &mut cache.think_hl,
                                   cache.think_in_fence,
                                   cache.think_fence_lang.as_deref());
            cache.think_lines = lines.clone();
            cache.think_in_fence = in_fence;
            cache.think_fence_lang = fence_lang;
            cache.think_src = joined;
            think_lines = cache.think_lines.clone();
        }
    }

    // ── response-text section ─────────────────────────
    let text_lines: Vec<Line> = if buf.text.is_empty() {
        Vec::new()
    } else if cache.text_src == buf.text {
        cache.text_lines.clone()          // unchanged: reuse
    } else {
        let lines = wrap_markdown_p(&buf.text, wrap_w, palette, prose);
        cache.text_src = buf.text.clone();
        cache.text_lines = lines.clone();
        cache.text_lines.clone()
    };

    // Sliding window, tool_args, header — same as today.
    …
}
```

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

## Performance model

Let `T` = total thinking chars, `D` = new chars this frame,
`W` = total response-text chars.

| Frame type | Before (current) | After (incremental) |
|---|---|---|
| Streaming (D > 0) | O(T) highlight + O(W) markdown parse | O(D) highlight + O(W) markdown parse (text still re-parses) |
| Idle (D = 0, W unchanged) | O(T) + O(W) | O(1) (cache hit, no work) |
| Idle (D = 0, W growing) | O(T) + O(W) | O(T) cached + O(W) re-parse |
| Width change | O(T) + O(W) | O(T) + O(W) (full rebuild, same as before) |

For a 50 KB thinking block at 60 FPS, the before cost is
~50 KB × 60 = 3 MB/s of tree-sitter work. After, it is
O(D × 60) where D is the per-frame delta (a few hundred chars).
The idle cost drops from O(T+W) to O(1).

The response-text path still re-parses on every frame where the
text changed (pacing releases chars every frame). A true
incremental markdown parser is a future improvement; the
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
   - Start with reasoning id `"a"`. Add reasoning id `"a"` more
     text (append — fine, incremental). Then add a new reasoning
     id `"b"` that sorts before `"a"`. The joined text changes
     at a position before the prefix end, so `starts_with` fails
     → full rebuild. Assert output matches a full rebuild.

6. **`clear_stream_drops_cache`**
   - Build the cache, call `clear_stream()`, assert
     `stream_block_cache` is `None`.

### Perf gate (in `perf_bgbuild_tests` or a new mod)

7. **`streaming_thinking_per_frame_stays_under_budget`**
   - Create 200 KB of thinking text (with code fences).
   - Simulate 60 frames: each frame appends ~3 KB (1/60 of total),
     calls `stream_block_lines`, times it.
   - Assert every frame is under 100 ms (the plan's frame budget).
   - Also assert the *average* per-frame time is < 10 ms,
     proving the incremental path is not doing O(total) work.

8. **`idle_frame_is_o1`**
   - After the cache is warm and no new deltas arrive, a frame
     (cache hit, no text change) should complete in < 2 ms.

### Integration / snapshot

9. **Existing insta snapshots** for the stream block must stay
   green. The incremental path must produce byte-identical output
   to the current full-rebuild path for the same input.

## File change map

| File | Change |
|---|---|
| `bin/tui/src/app.rs` | Add `StreamBlockCache` struct + `stream_block_cache` field. Init in `App::new`. Clear in `clear_stream`. Expose `stream_block_cache_mut()` for the render fn. |
| `bin/tui/src/render.rs` | Split `wrap_thinking` into `wrap_thinking_full` (non-incremental, used by settled builds) and `wrap_thinking_delta` (incremental). Rewrite `stream_block_lines` to use the cache. |
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
