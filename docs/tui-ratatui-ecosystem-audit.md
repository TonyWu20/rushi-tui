# TUI ratatui Ecosystem Audit

Status: Active
Last updated: 2026-09-28
Scope: `bin/tui`, `bin/tui-stream-drt`, `crates/tui-highlight`
Basis: `docs/NEW-refactor.md` (the refactor decision) and
`docs/tui_feature_requests_from_human.md` (the feature index)

---

## 1. Modules Already Leveraging the Ecosystem

These modules use ecosystem libraries correctly and need no change.

| Module | Library | What it does |
|---|---|---|
| `markdown.rs` | `ratatui-markdown` (git fork, ratatui 0.30 compat) | Renders message content to styled `Line`s via `MarkdownRenderer`. Maps the active `Palette` through a `RichTextTheme` impl. |
| `tool_display.rs` | `ansi-to-tui ^8` | Converts ANSI-colored bash output to ratatui `Text` at frame rate. |
| `picker/fuzzy.rs` | `frizbee ^0.13` | Fuzzy ranking in a background worker thread. The picker and command palette both share this ranker. |
| `crates/tui-highlight/` | `tree-sitter` + 13 language grammars | Grammar-based syntax highlighting. Isolated in its own crate so TUI source changes never trigger a grammar rebuild (must-do #1 and #2 from NEW-refactor). |
| Core rendering | `ratatui ^0.30` | All widget rendering, layout, and terminal I/O. |
| Image decoding | `image ^0.25` | Decodes base64 payloads to pixel data. |

Verdict: no gaps.

---

## 2. Declared but Unused Dependency

### `ratatui-image ^11` (must-do #3 gap)

`bin/tui/Cargo.toml` declares:

```toml
ratatui-image = { version = "11", default-features = false,
                   features = ["crossterm", "image-defaults"] }
```

No source file in `bin/tui/src/` imports `ratatui_image`. Instead,
`image_render.rs` (155 lines) hand-rolls half-block Unicode conversion
using the `image` crate directly: it decodes pixels, maps pixel pairs
to `▀ ▄ █` with fg/bg colors, and emits `BodyRow` vectors.

**Action**: Replace `image_render.rs` with `ratatui-image`'s
`ImageView` widget. `ratatui-image` supports sixel, kitty, iterm2, and
unicode-halfblock protocols. The halfblock path is equivalent to what
`image_render.rs` does but handles protocol negotiation, downscaling,
and color-mapping for us. Removing `image_render.rs` drops ~155 lines
and the direct `image` dependency (if nothing else uses it).

---

## 3. Per-Module Hand-Rolled vs. Ecosystem

Each section below names the hand-rolled mechanism, the ecosystem
library that could replace or complement it, and a verdict.

### 3.1 Scroll / Viewport Management

**Files**: `app.rs` (lines 236-570: `scroll`, `viewport`,
`scroll_up`, `scroll_down`, `half_page`, tail-follow logic),
`render.rs` (`draw_position_bar`, lines 2416-2455)

**Hand-rolled pieces**:
- `scroll: usize` — offset from tail. 0 means "follow the tail".
- `half_page()` — computes `(viewport - 1) / 2` for Ctrl+U/D.
- Tail-follow: when `scroll == 0` the viewport is pinned to the
  newest lines. Any user scroll breaks the pin.
- `draw_position_bar` — one-column scrollbar with thumb, tail marker,
  and cursor marker, drawn character-by-character.

**Ecosystem options**:
- `tui-scrollview ^0.6` (452k downloads, ratatui 0.30): provides
  `ScrollView` + `ScrollViewState` for a bounded viewport over a
  large buffer. It tracks scroll offset, supports programmatic
  scrolling, and can be told to follow the tail. However, the
  browse-mode state machine (cursor tracking, scrolloff margins,
  saved views, `:N` goto, regex search highlight) is custom and
  would still be hand-rolled on top. `tui-scrollview` replaces the
  "how many lines are scrolled from the top" plumbing but not the
  browse-mode semantics.
- ratatui built-in `Scrollbar` widget: could replace
  `draw_position_bar`'s character-by-character rendering. The custom
  bar adds a tail marker (▼) and cursor marker (▶) that the built-in
  scrollbar does not, so full replacement is not straightforward.

**Verdict**: **Partial adopt.** Migrate the scroll offset plumbing to
`tui-scrollview` to get the viewport abstraction, clamping, and
follow-tail semantics for free. Keep the browse-mode state machine
custom. Consider `ratatui::widgets::Scrollbar` for the position bar if
the custom markers are dropped. Otherwise keep `draw_position_bar`.

### 3.2 Floating / Overlay Windows

**Files**: `float.rs` (120 lines), `picker/render.rs`,
`palette/render.rs`

**Hand-rolled pieces**:
- `compute_float_layout` — centers a 60% box, computes interior
  regions for list / input / preview panes, switches orientation at
  80-column and 50-column thresholds.
- Picker and palette both call this and draw their own `Block`
  borders, list items, input bar, and preview pane.

**Ecosystem options**:
- `tui-overlay ^0.1.2` (jharsono/tui-overlay): composable overlay
  primitives — drawers, modals, popovers, toasts. Provides the
  centering, sizing, and stacking logic that `float.rs` hand-rolls.
  0.30-compatible.
- `tui-popup`: a simpler Popup widget.
- `tui-dialog`: single-line text input dialog.

**Verdict**: **Low-priority adopt.** `float.rs` is 120 lines and
works. `tui-overlay` would add configurability (multiple stacked
overlays, drawer slide-in) that the app does not need today. Adopt
only if a second overlay layer (e.g., a toast notification system)
appears.

### 3.3 Spinner / Working Indicator

**Files**: `render.rs` lines 1785-1840

**Hand-rolled pieces**:
- `WORKING_SPINNER_FRAMES` — 10 braille frames.
- `spinner_frame(now)` — pure function of wall-clock time, returns
  the frame char. No animation state in `App`.

**Ecosystem options**:
- `ratatui-cheese ^0.7` — spinner, help, tree, paginator, list
  widgets. The spinner widget would replace the 10-frame array.
- `throbber-widgets-tui` — throbber (loading indicator) widget.

**Verdict**: **Keep hand-rolled.** The spinner is 10 string literals
plus a modulus. No library justifies the dependency.

### 3.4 Color / Theme System

**Files**: `color.rs` (845 lines), `palette/` (4 files, ~615 lines)

**Hand-rolled pieces**:
- `Level` enum (`Rgb` / `C256` / `C16`) with `detect()` that
  parses `COLORTERM` and `TERM` env vars.
- `lower()` / `lower_style()` — quantize RGB to 256 or 16-color.
- `Role` enum (~30 named roles: `PlainText`, `Accent`, `DiffAdded`,
  `ToolBoxBgSuccess`, `ThinkingTag`, etc.).
- `Palette` struct: built-in (pi dark theme), `catppuccin_macchiato`,
  custom hex map, overlay.
- `parse_scheme_color` — hex parsing to `Color`.

**Ecosystem options**:
- `termprofile ^0.2` — detects terminal color/styling support.
  Replaces `Level::detect()` and `from_cfg()`.
- `coolor ^1.1` — color format conversion (hex, HSL, CSS names).
  Replaces `parse_scheme_color` and parts of `lower()`.
- `color-to-tui ^0.3` — parse CSS/hex colors to
  `ratatui::style::Colors`. Complements `coolor`.
- `opaline ^0.4` — token-based theme engine with 20 built-in themes
  and a theme-selector widget. Could replace the entire `Role` +
  `Palette` system. However, the current role system is tightly
  coupled to the pi theme alignment (one role per pi theme token),
  and `opaline`'s token set would need to be mapped to the
  `Role` enum. This is a large refactor.

**Verdict**: **Incremental adopt.**
1. Adopt `termprofile` to replace the `COLORTERM`/`TERM` detection
   block in `color.rs`. Low risk, small diff.
2. Adopt `coolor` for hex-to-`Color` conversion. Small diff.
3. `opaline` is a larger architectural change. Defer unless a
   second built-in theme (beyond macchiato) is added.

### 3.5 Diff Rendering (Edit / Write Tool Results)

**Files**: `tool_display.rs` lines 87-315 (`DiffView`,
`parse_diff_view`, `diff_layout`, `body_rows`), lines ~330-400
(split/unified diff row rendering)

**Hand-rolled pieces**:
- `DiffView` enum: `Auto` / `Split` / `Unified`.
- `diff_layout(width)` — `Auto` picks `Split` at ≥ 60 columns
  (2 × `DIFF_SPLIT_MIN_WIDTH`), `Unified` below.
- Split layout: two side-by-side panes with a divider column.
  Unified: single pane with `+` / `-` prefixes.
- `+N -M` stats line computed from the diff text.

**Ecosystem options**:
- `clankerdiff-ratatui ^0.1.8` — embeddable, repository-agnostic
  diff review widget. Provides `DiffDocument` snapshots, `DiffReviewEvent`
  routing, and a `DiffView` widget. Depends on `clankerdiff-core`,
  `clankerdiff-markdown`, `clankerdiff-syntax`, `clankerdiff-theme`,
  `similar`, `unicode-segmentation`, `unicode-width`.
  However, the TUI receives **pre-computed** diff lines from the
  kernel. The TUI does not compute diffs. It renders them.
  `clankerdiff-ratatui` expects to receive diff documents and render
  them interactively (with scroll, jump-to-hunk, etc.), which is a
  different usage model. The hand-rolled renderer is simpler for the
  "render pre-computed diff lines in a fixed box" use case.
- `ftdv` and `diff-tui` are standalone apps, not embeddable libraries.

**Verdict**: **Keep hand-rolled.** The diff content arrives pre-computed
from the kernel. The TUI's job is to lay out fixed-height diff
panes inside a tool-result box. `clankerdiff-ratatui` would add
interactive diff review (scroll, hunk navigation) that the TUI does
not need. Revisit if the "vertical split when wide, horizontal when
narrow" request (open item) grows into interactive diff viewing.

### 3.6 Fuzzy Search / Picker / Command Palette

**Files**: `picker/` (5 files, ~1500 lines), `palette/` (4 files,
~680 lines)

**Hand-rolled pieces**:
- `PickerState` — cursor, scroll, visible count, file-scope cycling
  (default → git-ignored → hidden).
- `render_picker` — list rendering with cursor highlight, path
  abbreviation (`abbreviate_path`), preview pane.
- `render_palette` — two-pane float: fuzzy command list (left) +
  help text / option picker (right).
- `FloatLayout` geometry from `float.rs`.

**Ecosystem options**:
- `frizbee ^0.13` — already used for fuzzy matching. ✓
- `tui-widget-list ^0.15` — list widget with scrolling, selection,
  and key handling. Could replace the hand-rolled list rendering in
  `render_picker` and `render_palette`. However, the custom preview
  pane, path abbreviation, scope cycling, and two-pane palette layout
  are app-specific and would still be hand-rolled.
- `tui-dialog` — single-line text input dialog. Not applicable. The
  input bar is custom (shows `@query` with key hints).

**Verdict**: **Keep hand-rolled.** `frizbee` already handles the
fuzzy ranking. The list rendering is tightly coupled to the preview
pane, scope cycling, and path abbreviation. Extracting to
`tui-widget-list` would not reduce complexity.

### 3.7 Vim Editor

**Files**: `vim_editor.rs` (4244 lines)

**Hand-rolled pieces**: Full vim modal editor — normal/insert/
visual/visual-line/command-line modes, motions (hjkl, w/b/e,
0/$, gg/G, / and ? search), operators (d, c, s, y, ~, .),
text objects (i/aw, i"a, i(, i[, i{, i<), registers, undo/redo,
multi-line editing, `@` token insertion.

**Ecosystem options**:
- `tui-textarea ^0.7` (2.5M downloads) — basic multi-line text
  editor. No vim modality.
- `ratatui-code-editor ^0.0.6` — tree-sitter code editor widget.
  No vim modality.
- `edtui` — "vim-inspired editor widget" listed in awesome-ratatui.
  Not a full vim modal editor with registers and text objects.

**Verdict**: **Keep hand-rolled.** No library provides full vim
modal editing with registers, text objects, and undo/redo. The 4244
lines are the core differentiator.

### 3.8 Extension IPC / UI Framework

**Files**: `ext.rs` (4314 lines)

**Hand-rolled pieces**: Extension host lifecycle, JSONL-over-stdio
IPC protocol, capability negotiation, `invoke` / `notify` /
`transform` operations, command items for the palette.

**Ecosystem options** (for extension-side UI, not the core):
- `tui-realm` — Elm/React-style framework for building extension UIs.
- `widgetui` — bevy-like widget system.
- `rat-salsa` — event queue with tasks, timers, focus, dialogs.
- `ratatui-input-manager` — Elm-style declarative input handlers.
- `ratatui-interact` — interactive components with focus and mouse.
- `malevich ^1.21` — terminal plotting (line, bar, heatmap).

**Verdict**: **N/A for the core TUI.** The extension IPC protocol is
custom by design (JSONL on stdio). The listed libraries are options
for **extension developers** building UIs inside extension processes.
No change needed in `ext.rs`.

### 3.9 Stream Rendering

**Files**: `render.rs` `stream_block_lines` (line 1877),
`app.rs` `pump_stream_pacing`, `main.rs` event loop

**Hand-rolled pieces**:
- FIFO pace queue: arriving deltas are buffered and released at
  `max(1, backlog/15)` chars per frame (~60 FPS).
- `stream_block_lines` renders the live thinking + text window.
- `done` event drains the queue in one shot.

**Ecosystem options**: None. This is app-specific pacing logic.
`ratatui-markdown`'s hybrid scroll system is for scrolling rendered
content, not for pacing a live stream.

**Verdict**: **Keep hand-rolled.** The pacing queue is a small,
well-tested piece of app logic.

---

## 4. Per-Feature-Request Library Mapping

Maps each open (unshipped) request from
`docs/tui_feature_requests_from_human.md` to the ecosystem library
that addresses it, if one exists.

### 4.1 Markdown table rendering: `|` disambiguation and cell truncation (OPEN)

> "The current markdown table rendering of the messages cannot
> correctly distinguish if `|` is used as the table column marker or
> written as part of the text or code, e.g. `.map(|e| ...)`"
> "The tabulated information is lost in TUI because we do not wrap
> lines exceeding the width."

**Root cause (two sub-bugs in `highlight.rs` `table_grid`)**:

1. `is_table_row` checks `starts_with('|') && pipes >= 2`. A code
   line like `|x| y => x` in prose or inside an un-fenced code block
   is misidentified as a table row, so closures and other pipe-heavy
   code get swallowed into a spurious grid table.
2. The `clamp` closure inside `table_grid` truncates any cell wider
   than its column width with a trailing `…`. No wrapping is
   performed. Information in wide cells is silently lost.

**Why `ratatui-markdown` fixes both**:

- Its parser recognises GFM table syntax: a table is a header row
  plus a `|---|---|` separator plus body rows. A single `|` inside
  a paragraph or code fence does not trigger table parsing. This
  eliminates the misidentification.
- `render_table` calls `wrap_styled_spans_to_width` for every cell.
  Cell content wraps to the allocated column width and the row
  height grows to fit the tallest cell. No data is lost.
- Column width allocation is proportional to content, targeting ~3
  lines per column, with a floor of `min_widths` and proportional
  shrink/expansion to fit the available pane width.

**Current hand-rolled path** (`highlight.rs` lines 779–905):
`is_table_row` → `table_cells` → `is_table_separator` →
`table_grid`. The `table_grid` function computes
`widths = min(max_content, even_share)` then clamps each cell with
`…`. No wrapping. No multi-line cells.

**Action**: Route the thinking-block and ext-path table rendering
through `ratatui-markdown`'s `render_table` (already a dependency).
Remove `is_table_row` / `table_cells` / `table_grid` from
`highlight.rs` once all call sites in `render.rs` are migrated.
The `|` disambiguation and the truncation are fixed together.

### 4.2 Browse mode: model updates must not flush to tail (OPEN)

> "When in browse mode, updates from model response should not flush
> the screen to the latest position of the conversation."

**Library that addresses it**: None directly. This is an app-level state
bug. The `Browse::sync` method receives `grew` via
`app.take_events_grew()` (render.rs line 3142) and uses it to
compute `pure_growth = total > last_total && grew && h == last_h`.
When `pure_growth` is true the view stays put. The likely bug path:
when `EVENTS_CAP` causes old events to be drained, the rendered
`total` can stay the same or even shrink even though new events
arrived, making `pure_growth` false and triggering the re-centre
branch (`follow_view`). Inspect the `total` vs `last_total` values
at the moment the flush is observed to confirm.

**Action**: Audit the `events_grew` / `total` interaction with
`EVENTS_CAP` in `app.rs`. If the cap causes `total` to stop growing,
the `pure_growth` heuristic fails. No library change needed.

### 4.3 Select-and-yank in browse mode (OPEN)

> Select text in rendered transcript, yank to a register, paste
> into the draft with `p`. `yw`, `y$`, `yG`, `<n>yy`, `i`/`a`
> text objects.

**Library that addresses it**: None. No TUI library provides vim
visual-mode selection with registers and text objects. The
`Browse` state machine already has `VisualSel`, `yank_reg`,
`yank_pending`, `obj_prefix` fields — the selection and yank
mechanism is partially implemented. The remaining work is:
1. Render the visual selection highlight in `render.rs`.
2. Wire the yank → register → `Editor::paste` path.
3. Implement the text objects on top of rendered lines.

**Action**: Pure hand-rolled work in `browse.rs` and `render.rs`.
No library applicable.

### 4.4 Input box VISUAL / V-LINE selection highlight bug (OPEN)

> "In VISUAL / V-LINE the draft shows only the inverted block on the
> cursor cell. The chars between the anchor and the cursor are not
> shaded."

**Library that addresses it**: None. This is a rendering bug in
`vim_editor.rs` — the `display_rows` method must emit the selected
span with a background color for every cell between the anchor and
the cursor. Char-visual spans across wrapped rows. Line-visual
shades whole display rows.

**Action**: Fix `vim_editor.rs`. Expose `visual_range` (currently
private) to the renderer. No library applicable.

### 4.5 tool:edit diff always shows +0 -0 (OPEN — bug)

> "Bug: `tool:edit` results always show `diff +0 -0`."

**Library that addresses it**: None. This is a data bug in the
kernel or the diff-text parsing in `tool_display.rs`. The `+N -M`
count is computed from the diff text lines. If the kernel sends the
diff without `+`/`-` prefixes (or in a different format), the parser
counts zero.

**Action**: Debug the diff text the kernel sends for `edit` results.
No library involved.

### 4.6 tool:edit vertical split (wide) / horizontal split (narrow) (OPEN)

> "tool:edit shows diff in vertical split when terminal is wide,
> horizontal split when terminal is narrow."

**Current state**: `DiffView::Auto` already switches between
`Split` (side-by-side) and `Unified` (stacked) at a width
threshold of 60 columns (`DIFF_SPLIT_MIN_WIDTH * 2`). This is the
inverse of the request: the request wants **vertical** (stacked /
unified) when wide and **horizontal** (side-by-side / split) when
narrow. The current `Auto` does the opposite.

**Library that addresses it**: `clankerdiff-ratatui` provides
interactive diff views with layout options, but the TUI's diff
content is pre-computed and rendered in a fixed-height box. The
fix is to invert the `Auto` threshold logic in
`ToolDisplay::diff_layout`. No library needed.

**Action**: Invert the threshold in `diff_layout`: `Split` when
width < threshold, `Unified` when width ≥ threshold. Or add a
config option to control the preference.

### 4.7 Simplify live stream into main content area (OPEN)

> "Stream the model response into the main content area instead of
> a pinned block that grows and collapses. `Ctrl+T` should collapse
> and expand all thinking blocks, including the live-streaming one."

**Library that addresses it**: None. This is an app-architecture
change: the live stream block is currently a pinned region between
the transcript and the input box. Moving it into the transcript
means the stream content scrolls with the transcript and the
`Ctrl+T` toggle applies to it like any other thinking block.

**Action**: Refactor `stream_block_lines` to emit lines into the
transcript buffer instead of a separate pinned region. The
`ratatui-markdown` hybrid scroll system is not relevant here.

---

## 5. Shipped Features That Already Use Ecosystem Libraries

For completeness, the shipped items that are backed by ecosystem
libraries:

| Feature | Library |
|---|---|
| Markdown rendering in messages | `ratatui-markdown` (git fork) |
| ANSI-colored bash output | `ansi-to-tui ^8` |
| Fuzzy search in picker / palette | `frizbee ^0.13` |
| Syntax highlighting (Read, Write, thinking) | `tree-sitter` via `tui-highlight` |
| Image decoding | `image ^0.25` (hand-rolled halfblock. See §2) |
| Color scheme: catppuccin macchiato | Hand-rolled (see §3.4) |
| Stream pacing | Hand-rolled (see §3.9) |

---

## 6. Action Items Summary

Priority ordered by impact-to-effort ratio:

1. **HIGH** — Fix the markdown table rendering (§4.1). Route
   thinking-block and ext-path table rendering through
   `ratatui-markdown`'s `render_table`. Fixes both the `|`
   misidentification in code and the cell-truncation (no wrapping)
   in one change. Removes the hand-rolled `is_table_row` /
   `table_cells` / `table_grid` / `md_line` paths.
2. **HIGH** — Replace `image_render.rs` with `ratatui-image`
   (§2). The dependency is already declared but unused. Drop the
   `image` crate if nothing else uses it.
3. **MEDIUM** — Fix the `+0 -0` diff bug (§4.5). Data-layer fix,
   no library.
4. **MEDIUM** — Invert or parameterize the diff `Auto` threshold
   (§4.6). One-line logic change.
5. **MEDIUM** — Fix browse-mode tail-flush bug (§4.2). Fix the
   `grew` flag in `app.rs`.
6. **MEDIUM** — Fix VISUAL/V-LINE selection highlight in
   `vim_editor.rs` (§4.4). Expose `visual_range`, shade the span.
7. **LOW** — Adopt `termprofile` for terminal color detection and
   `coolor` for hex parsing (§3.4). Small, isolated diffs in
   `color.rs`.
8. **LOW** — Migrate scroll plumbing to `tui-scrollview` (§3.1).
   Moderate refactor. Keep browse-mode state machine custom.
9. **LOW** — Implement select-and-yank in browse mode (§4.3).
   Hand-rolled. No library.
10. **LOW** — Move live stream into transcript (§4.7). App
    architecture change.
11. **DEFER** — `tui-overlay` for float layout (§3.2). Adopt only
    when a second overlay layer appears.
12. **DEFER** — `opaline` for theme tokens (§3.4). Large refactor.
    Defer until a second built-in theme is added.
13. **N/A** — `ratatui-cheese` spinner (§3.3). 10 lines. No value.
14. **N/A** — `clankerdiff-ratatui` for diff (§3.5). The TUI
    renders pre-computed diffs. The library's interactive model
    does not fit.
15. **N/A** — `tui-realm` / `widgetui` / `rat-salsa` /
    `ratatui-input-manager` / `ratatui-interact` / `malevich`
    (§3.8). These are extension-side libraries, not core TUI.

---

## 7. Gap 2 Deep-Dive: tui-scrollview 60 FPS Smoothness

Question: can `tui-scrollview` give stable 60 FPS scrolling with no
spikes or lag, matching the feel of the buffered/paced response
stream?

### 7.1 Render model (verified from source)

`tui-scrollview ^0.6` owns a `Buffer` of full content size. The
per-frame path:

- `render_visible_area` copies only the visible window into the frame
  buffer. Cost is O(visible_rows × cols), one `Cell` clone per cell.
  A 24×100 viewport is ~2400 clones/frame. Negligible CPU. Zero heap
  allocation per frame.
- Content rebuild (via `render_widget` / `render_stateful_widget`)
  happens only when content changes, not per scroll frame. Storing the
  `ScrollView` and rebuilding on content change keeps scroll-only
  frames cheap.
- `ScrollViewState` is a small `Copy` struct (offset, size,
  page_size). `is_at_bottom()` covers the tail-follow check.

Verdict: the render side is 60 FPS safe. It is algorithmically
equivalent to our current approach (slicing a cached `Vec<Line>`
into the visible window). `tui-scrollview` does not make scrolling
faster than what we already do. It adds a stable state object,
a built-in scrollbar, and `is_at_bottom()`.

### 7.2 Where the actual 60 FPS gap is

Smooth scroll is not decided by the widget. Two things control it:

1. **Frame-loop tick rate.** `main.rs` ticks at 16 ms only while
   `app.stream_live() || app.is_animating()`; otherwise it ticks at
   100 ms (10 FPS). A plain user scroll (wheel, Ctrl+U/D, browse
   motion) with no active stream or animation runs at 100 ms, so
   scrolling feels choppy today. To match the paced-stream feel,
   the loop must tick at 16 ms while a scroll is active. Add a
   "recently scrolled" flag (analogous to `stream_live` /
   `is_animating`) that holds the 16 ms cadence for a short window
   after the last scroll input.
2. **Input-to-offset mapping.** `ScrollViewState` offers `scroll_up` /
   `scroll_down` (1 row) and `scroll_page_up` / `scroll_page_down`
   (1 page). It does not interpolate. For "buffered and played"
   smoothness the app must coalesce wheel deltas and ease the offset
   toward a target across frames, then call `render` each frame.
   `tui-scrollview` provides none of that; it is a viewport, not an
   animator.

### 7.3 Terminal I/O ceiling

`ratatui` diffs the frame buffer against the previous frame and emits
escape sequences only for changed cells. On a scroll, the changed
region is the visible window, so terminal write volume stays small.
The remaining physical limit is the terminal emulator's rasterizer
(kitty, alacritty, wezterm). All of those handle 60 FPS text scroll
comfortably.

### 7.4 Recommendation

- Adopt `tui-scrollview` for the viewport/state (low risk). It is not
  the source of smoothness but removes hand-rolled offset math and
  gives `is_at_bottom()`.
- To actually reach 60 FPS during scroll, change the frame-loop
  gate in `main.rs` to include a recently-scrolled condition, and add
  wheel-delta coalescing plus offset easing in the app. That work
  lives in `main.rs` / `app.rs`, not in `tui-scrollview`.
- Verify with the PTY smoke gate (`scripts/tui-pty-smoke.py`) after
  the change, and add an insta snapshot only if the visible layout
  changes.
