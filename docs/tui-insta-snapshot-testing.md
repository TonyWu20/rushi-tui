# TUI insta-snapshot testing

Status: Implemented. The snapshot suite lives in
`bin/tui/src/snapshot_tests.rs` with its `.snap` files in
`bin/tui/src/snapshots/`. This doc is the contract for the
method, so a reader can add, run, and review snapshots without
reverse-engineering the harness.

## 1. Purpose

The TUI test suite leaned on hundreds of small assertion tests
that poked internal state (a scroll index, a mode flag, a buffer
string). Most of them tested the inside of the widget, not the
screen a user actually sees. The suite is now steered toward
insta-snapshot tests that capture the concrete rendered output of
each user-visible state, and use those snapshots as the regression
gate.

This follows the official ratatui recipe for snapshot testing
(<https://ratatui.rs/recipes/testing/snapshots/>). The method is:
drive the `App` to a state, render one frame onto a `TestBackend`,
and snapshot the resulting terminal grid.

## 2. The method

Three pieces make it work. No new runtime code enters the binary.
Everything below is `#[cfg(test)]`.

| Piece | Where | Job |
|---|---|---|
| `TestBackend` | ratatui | An in-memory `Backend`. `Terminal::draw` writes the frame into its buffer instead of a real tty. |
| `render()` helper | `snapshot_tests.rs` | Wraps one `draw` call at a fixed `(w, h)` and returns the grid as a `String`. |
| `insta::assert_snapshot!` | the `insta` crate | Diffs the grid string against the stored `.snap`. A miss creates a `.snap.new` for review. |

The `render` helper is the only place that touches `draw`. Every
snapshot test builds its `App` state, calls `render`, and asserts
the grid. Because the grid is a plain string, a snapshot diff is
readable line by line: a border that moved, a label that changed,
a row that wrapped. That is the "concrete behavior" the suite now
guards.

## 3. The harness

`bin/tui/src/snapshot_tests.rs` holds four shared helpers and the
25 snapshot tests. The helpers:

- `empty_host()` — builds an `ExtHost` with no extension processes
  over a throwaway `TuiConfig` and `TempDir`. This is the host the
  frame draws against. It keeps the snapshot free of live extension
  replies.
- `render(app, host, w, h)` — one `Terminal::draw` at the requested
  size, returns `term.backend().to_string()`.
- `app_with_session(events)` — an `App` with one active session.
- `ev(json)` — parse one wire JSON line into an `Event`.

Snapshot tests are grouped by feature with `──` section comments:
main screen, editor states, browse, picker, palette, color
schemes, tool-display presets, and session naming.

## 4. The snapshot set

The first set captures the concrete behavior of each major state.
A reader adding a feature adds one test here, runs it once to
write the baseline, and commits the `.snap`.

- `snap_empty_app` — no session, the naming prompt.
- `snap_session_with_conversation` — user + assistant transcript.
- `snap_running_loop_indicator` — an attached running loop, the
  spinner row. The spinner frame is time-dependent, so the test
  masks the braille frame to `[SPINNER]` before asserting (section
  6.2).
- `snap_pending_approval_banner` — an open `approval_request` with
  the `[y allow] [n deny] [e edit]` row.
- `snap_pending_user_messages` — queued `follow` messages.
- `snap_tool_result_collapsed` / `snap_tool_result_expanded` — the
  same result block in its two fold states.
- `snap_error_event` — an `error` event in the transcript.
- `snap_long_transcript_scrolled_back` — a long log scrolled up,
  the position bar.
- `snap_narrow_terminal` / `snap_wide_terminal` — layout at 60x20
  and 120x40.
- `snap_editor_insert_mode` / `_normal_mode` / `_multiline_draft` /
  `_search_command_line` / `_visual_selection` / `_w_motion` — the
  editor in its input modes and after a motion.
- `snap_browse_mode_active` — the double-`s` overlay on a long log.
- `snap_picker_open` — the `@` file picker after typing a query.
- `snap_palette_open` / `snap_palette_dark` / `snap_palette_light`
  — the `:` palette and two color levels.
- `snap_tool_display_balanced` / `_verbose` — the tool-display
  presets.
- `snap_pending_name_input` — the new-session name entry.

## 5. Running and reviewing

`cargo-insta` is the driver. `insta` is a dev-dependency of `tui`.

- `cargo insta test -p tui` — run the suite. A miss writes a
  `.snap.new` next to the baseline.
- `cargo insta review` — open the pending `.snap.new` files and
  accept or reject each diff.
- `cargo test -p tui` — plain run, asserts against the committed
  baselines with no review step.

A new snapshot is created by running the one test with
`INSTA_UPDATE=always` (or `cargo insta test` then `cargo insta
review`), committing the accepted `.snap`, and adding the test to
this doc's list in section 4.

## 6. Determinism

A snapshot is only a gate if it is stable across runs. Three rules
keep the set stable.

### 6.1 Fixed size

Every `render` call names its `(w, h)`. The grid is a function of
the app state and the size only. No wall clock, no tty, no
extension process. A test that needs a different viewport names it
in the call (`snap_long_transcript_scrolled_back` uses 80x30).

### 6.2 Time-dependent content

The running-loop spinner frame is derived from the wall clock
(`WORKING_SPINNER_FRAMES` cycles on `timestamp_millis`). A raw
snapshot of that row would fail on the next run. `snap_running_loop_indicator`
therefore masks the ten braille frames to `[SPINNER]` before
asserting, so the snapshot pins the layout and the label, not the
frame.

### 6.3 Async state, polled not slept

The fuzzy picker and the extension host settle their state on a
background worker. Their tests must not race the worker with a
fixed `std::thread::sleep`, which is the old cause of the flaky
runs. Instead they poll the published state until it is settled
for the query they sent, with a deadline. See
`picker/fuzzy.rs` (polls `Snapshot::settled && Snapshot::query`)
and `ext.rs::status_reply_bound_marks_the_row_stale` (polls
`poll_status` against a 5 s deadline). A fixed sleep here is a
latent flake. A poll with a deadline is not.

## 7. What was removed, what stayed

The `bin/tui` crate fell from 671 tests to 180.
The `tui-highlight` and `tui-stream-drt` crates keep their own
tests and were not part of this steering. The removed set is the
rendering and state-machine assertion modules, now covered by the
section 4 snapshots:

- `render.rs`, `vim_editor.rs`, `app.rs`, `browse.rs`,
  `tool_display.rs` — frame, editor, app-state, and browse
  assertions.
- `picker/{state,render,preview,items}.rs`,
  `palette/{state,render,preview,items}.rs` — the overlay state
  machines and their render helpers.
- `float.rs`, `image_render.rs`, `color.rs`, `highlight.rs`,
  `main.rs`, `editor.rs` — float, image, color, and highlight
  assertions, and the main-entry test module.

The kept set is logic that a grid snapshot cannot express. A
snapshot shows the screen. It does not show what a parser
accepted or a matcher ranked. These stay as plain `#[test]`:

- `event.rs` — wire JSON event parsing.
- `config.rs` — `TuiConfig` parsing.
- `port_file.rs` — port-file tailing.
- `picker/fuzzy.rs` — the fuzzy-rank algorithm.
- `ext.rs` — the extension-host process and reply lifecycle.
- `render.rs::cursor_span_tests` — the cursor-span and thinking-
  wrap helpers, tested in isolation.

A test that asserts a value the screen does not show belongs in
the kept set. A test that asserts how the screen looks belongs in
the snapshot set. When a change touches both, update the affected
`.snap` in the same commit.
