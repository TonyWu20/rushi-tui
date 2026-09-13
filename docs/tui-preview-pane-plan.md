# TUI file-picker preview pane: no-truncation, windowed, cancellable

Status: Spec (approved for build).

Last updated: 2026-09-24.

Linked from: `docs/tui_feature_requests_from_human.md` (New requests
2026-09-24, the "picker preview pane truncates file content to 50
lines" entry).

Scope: remove the 50-line truncation of the picker preview pane and
replace the per-frame full-file read + highlight with a windowed,
cancellable model — the same windowed-rendering + background-load
pattern as the main transcript (stage 0 of
`docs/tui-perf-background-build-plan.md`). The full file stays
reachable by scrolling; only the visible window is highlighted per
frame; a load that is still in flight is dropped when the cursor
moves.

## Problem

Today the preview pane is bounded three ways at once:

- `FilePreviewer::new(50)` (`bin/tui/src/render.rs:4068`) caps the
  pane at 50 source lines. The cap is a display limit, not a
  performance guard.
- `FilePreviewer::content` (`bin/tui/src/picker/preview.rs:68`)
  reads the **whole** file (`read_to_string`) and highlights every
  line, then drops all but the first 50 with `.take(max_lines)`.
- `FilePreviewer::header` (`bin/tui/src/picker/preview.rs:51`)
  re-reads the file a second time, per frame, just to count lines.

Both `header` and `content` are called on every frame by
`render_preview` (`bin/tui/src/picker/render.rs:207`). The picker
idle cadence is ~100 ms (`bin/tui/src/main.rs:607`), so the file is
re-read and re-highlighted ~10×/s even when the cursor is still.

The render is already windowed on the *selection* side:
`render_preview` computes a display-row offset from
`preview_scroll` (`bin/tui/src/picker/state.rs:66`, hard-line units)
and draws only `visible_h` rows. What is **not** windowed is the
*highlighting*: the whole file is highlighted before the window is
chosen.

Two user requirements shape the fix:

1. **No truncation.** The full file must be reachable by scrolling.
   "No truncation" does **not** mean rendering the whole file into
   the view at once — it means the full content is reachable via
   scrolling, with only the visible window rendered per frame. The
   `preview_scroll` mechanism already supports this.

2. **No freeze on huge files.** The user sometimes shows
   git-ignored files in the picker (build artifacts, logs,
   datasets). Accidentally hovering a GB-scale file must not freeze
   the TUI. The load must be cancellable: moving the cursor away
   drops the in-flight read.

## Design

The pane gets three layers, in order of cost. Each layer is
independently shippable; layer 1 alone removes the freeze risk.

### Layer 1 — size guard (O(1), kills the freeze)

Add a byte cap constant, e.g. `PREVIEW_MAX_BYTES = 10 * 1024 * 1024`
(10 MiB). In `FilePreviewer::content` and `header`, check
`std::fs::metadata(path).len()` **before** any read. If the file
exceeds the cap, do not read it: return a single placeholder line
(`"(file too large to preview: 5.2G)"`) and let the header show the
byte size. This is one `stat` syscall, no I/O, no highlight, no
freeze. This is what protects the ignored-files use case.

### Layer 2 — async load with cancel (for files under the cap)

For files under `PREVIEW_MAX_BYTES`, the read is fast, but for a
multi-MB source file it is still non-trivial work on the render
thread. Move the read off the render thread:

- On cursor change, dispatch a background read of the current item:
  read the file, split into lines. Store the in-flight result in a
  `pending` slot on the picker state, keyed by the item id.
- Each frame, if the load has settled **and** the cursor is still
  on that item, attach the result. If the cursor moved before it
  settled, drop the in-flight result (cancel) and start a fresh
  dispatch for the new item.
- This is the same dispatch / cancel / settle pattern as the
  transcript worker (`bin/tui/src/transcript_worker.rs`). A
  "loading…" placeholder renders until the read settles.

Cancellation is the core ask: the load does not need to finish if
the user has moved on. The size guard (layer 1) ensures a huge file
never even dispatches a read.

### Layer 3 — windowed highlight (per-frame O(window), not O(file))

Drop the `max_lines` cap entirely so `preview_scroll` spans the
whole file. Per frame, highlight **only the visible window** of
source lines instead of the whole file:

- `content()` takes a window (`preview_scroll` ..
  `preview_scroll + visible_h`, in source-line units, with a small
  margin for wrap expansion) and returns highlighted lines for that
  window only.
- Cache highlighted windows in a small LRU keyed by
  `(path, mtime, line_range, width, palette_level, engine)` so a
  re-scroll to a recently viewed range is a cache hit. This mirrors
  the `BuildMemo` LRU (`transcript_worker.rs`) and the
  `StreamBlockCache` from
  `docs/tui-perf-streaming-incremental-plan.md`.
- The highlighter is stateful (fence / block-comment state carried
  across lines), so a non-contiguous window must carry state from
  the previously highlighted range. See the open question on this.

### Header fix

`header()` must not re-read the file to count lines. Either:

- derive the line count from the one read in `content()` / the
  loaded lines (once settled), or
- drop the exact line count and show the byte size from
  `metadata.len()` only (a `stat`, no read).

The simplest correct choice is the latter until the load settles,
then show the real count from the loaded lines.

## Performance model

Let `F` = total file bytes, `L` = total file lines, `W` =
visible-window source lines (~`visible_h` + wrap margin), `H` =
highlight cost per line.

| Frame type | Before | After |
|---|---|---|
| Hover, cursor still | O(F) read + O(L) highlight × 10/s | O(1) (settled, cached) |
| Hover, cursor still, first window | O(F) read + O(L) highlight | O(F) read once + O(W) highlight |
| Scroll within file | O(F) re-read + O(L) re-highlight | O(W) highlight (cache hit: O(1)) |
| Cursor to new file | O(F') read + O(L') highlight | O(W') highlight, read in background |
| Huge file (> cap) | O(F) read + O(L) highlight, freeze | O(1) `stat`, no read, no freeze |

Per-frame cost is O(W × H), independent of total file size. The
only O(F) left is the one-time background read of the whole file
(bounded by the 10 MiB cap), which never blocks the render thread.

## Test plan

Tests live in `bin/tui/src/picker/preview.rs` (or a new
`preview_tests.rs` mod) under `#[cfg(test)]`, using the PTY harness
(`bin/tui/tests/common/mod.rs`) for the end-to-end cancel case.

1. **`size_guard_skips_read_on_huge_file`** — a file above
   `PREVIEW_MAX_BYTES` returns the too-large placeholder without a
   read (assert via a mock fs or a read counter).
2. **`cursor_move_cancels_in_flight_load`** — dispatch a load,
   move the cursor before it settles, assert the in-flight result
   is dropped and a fresh dispatch starts for the new item.
3. **`windowed_highlight_only_visible_lines`** — assert
   `content()` highlights only the visible window (highlight
   counter), not the whole file.
4. **`window_cache_hit_on_rescroll`** — scroll away and back to a
   recently viewed window; assert the LRU is hit (no re-highlight).
5. **`width_change_invalidates_window_cache`** — resize the pane;
   assert the window cache is rebuilt for the new width.
6. **`scroll_range_covers_full_file`** — with no cap,
   `preview_scroll` reaches the last line of a >50-line file.
7. **`header_does_not_reread_file`** — assert `header()` does not
   call `read_to_string` for the line count.
8. **Perf gate** — a 50 MB fixture; hover and move away; assert no
   frame exceeds the 100 ms budget and no read dispatches
   (size guard).
9. **Snapshot parity** — the layout snapshots for a >50-line
   preview file must be regenerated: the pane now shows more than
   50 lines. Snapshots for ≤50-line files are unchanged.

## File change map

| File | Change |
|---|---|
| `bin/tui/src/picker/preview.rs` | Add `PREVIEW_MAX_BYTES` size guard; add the background read + cancel (a `pending` slot); windowed `content()`; window LRU. |
| `bin/tui/src/picker/render.rs` | `FilePreviewer::new(50)` → `FilePreviewer::new()` (no cap); thread the scroll window and the pending-load state into the previewer call. |
| `bin/tui/src/picker/state.rs` | `preview_scroll` already exists; add the pending-load slot (`Loading` / `Settled` / `None`) and cursor-change detection for the cancel. |
| `bin/tui/src/render.rs` | The `FilePreviewer::new(50)` instantiation site. |
| `bin/tui/src/picker/preview.rs` tests | New test mod. |

## Relationship to the perf plans

This is the picker-side instance of the windowed + background-load
pattern that `docs/tui-perf-background-build-plan.md` (settled
transcript) and `docs/tui-perf-streaming-incremental-plan.md` (live
stream block) establish. The size guard plus async cancel addresses
the "accidental hover on a huge ignored file" case from the feature
request, which is the same class of problem as the thinking
streaming lag: avoid per-frame O(n) work on the main thread.

## Open questions

- **Highlighter state across windows.** The highlighter carries
  fence / block-comment state across lines. A non-contiguous window
  must inherit that state from the last highlighted range, or
  re-highlight from the file start to the window. Decide between
  "carry state" (like `StreamBlockCache.think_hl`) and "re-scan to
  window start".
- **Size guard threshold.** 10 MiB is a starting point. Make it
  configurable via the `tui` config, or keep it a fixed constant.
- **Background read mechanism.** A dedicated thread (like
  `transcript_worker.rs`) versus a per-frame chunked read
  (`BufReader`, N bytes/frame). The thread matches the existing
  worker; the chunked read avoids a new thread.
- **Line-count source.** Estimate from `metadata.len()` for the
  pre-settle header, then show the exact count from the loaded
  lines. Confirm the header is allowed to change text on settle.
