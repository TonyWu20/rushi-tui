# Handoff — TUI turn-fold re-scope (open items)

For a fresh session. Status date 2026-09-15.

The previous session context became unreliable. It had garbled file
reads and uncertain edit applications. It also once invented a "commit"
instruction the user never gave. Everything below was re-checked against
the working tree on 2026-09-15. Treat any claim not marked verified as
needing a fresh check.

## Ground rules

The user has NOT asked for a commit. Do not commit unless asked.

All work below stays in the working tree.

The turn-fold re-scope is implemented and its own test gate is green.
What remains is one red heavy PTY test, docs, bookkeeping, diff cleanup,
and a clippy pass.

Verify before acting. The code is the source of truth. `docs/` may be
stale.

## Verified state (2026-09-15)

HEAD is `7d623db` ("tui: browse-mode turn fold (L1/L2/L3)"). The
working tree is dirty. Seven non-snapshot files are changed and the 27
`.snap` files are changed.

The changed non-snapshot files are `app.rs`, `color.rs`, `fold.rs`,
`render.rs`, `snapshot_tests.rs`, `transcript_worker.rs`, and
`tests/pty_perf.rs`.

`cargo build` is clean. `cargo test --bin tui` passes 317 tests with 0
failed.

Confirmed present in the tree: `Role::Report` in `color.rs` (builtin and
macchiato `#74c7ec`, key `report`). Also `live_fold_tally()` in `app.rs`
(~line 1402). Also `user_box_rows`, `report_box_rows`, and
`message_box_rows` in `render.rs`. Also `tally_text` in `fold.rs`.

`App.thinking_expanded` is `pub(crate)`. `snapshot_tests.rs` sets it
directly. There is no setter.

## What is done (the re-scope feature)

All of this is in the working tree and covered by the 317 passing tests.

### Tally line

`fold::tally_text` emits `⎿ N steps · bash ×N · read ×N · M msgs`. The
leading `⎿`, the `·` separators, the `×` counts, and `msgs` last. It
renders via `fold_summary_line` in `render.rs`.

### Main-view fold

`fold_input()` in `app.rs` returns a plain `HashSet<u64>`. It is not
gated on `browse.active()`. One shared `turn_fold` set drives both
views. Fold state persists when returning to main.

`BuildKey` and the cache dropped `fold_active`. Invalidation now rides
on `turn_fold_epoch` and `loop_running`.

### Thinking collapsed by default

`App::new` and `attach_external_loop` set `thinking_expanded: false`.

### Final-message box

The idle assistant reply of a completed turn sits in a rounded box. The
box has no title. Only the `Report`-toned border marks it. It differs
from the user box only in border color.

Both share the rounded shape via `message_box_rows`. The thinking block
renders inside the box.

### User-message box

Rounded `Accent` border plus a "User" title. The background fill is
dropped. Only the border remains. `Selection` and `CursorLine` are
unchanged.

### Running turn

Folded by default. The live tally merges into the "Waiting for model"
working row. The format is `Working... · N steps · ... · M msgs`. The
in-transcript spinner row was removed.

## Open items (priority order)

### 1. `tests/pty_perf.rs` is red (the only failing test)

The test is `perf_24mb_background_build_no_freeze`. It is a heavy PTY
test on the 24 MB fixture at
`/home/tony/programming/rushi-tui/sessions/tui-diff-spec-lean`.

#### Root cause (verified)

The test asserts the `building transcript` indicator shows while a
background build runs. It polls the screen every half a second. With
thinking collapsed by default, the full 24 MB build now settles in about
138 ms. That was measured with `TUI_TRANSCRIPT_TRACE`.

The indicator is on screen for far less than the half-second poll step.
So the poll never catches it. This is a real effect of the speedup,
not a bug in the fold feature.

#### Tried so far (all still failing)

Option (a) was the original. A 10 s startup pump plus a 60 s poll. The
first build settles during the startup pump. The poll never sees the
indicator.

Option (b) was a width-resize trigger via `TIOCSWINSZ` to 84 cols. A
resize-triggered build also settles in about 140 ms. Still too short.

Option (c) is the current state of the file. A content-expand trigger
sends `Ctrl+O` and `Ctrl+T` to expand all tool results and thinking.
The toggles do register. The failing screen shows the expanded thinking
text. But the expanded build still settles fast enough that the
half-second poll misses the indicator. Not green.

#### Next step

Make the poll fine-grained. Poll every 50 to 100 ms instead of 500 ms.
Keep the content-expand trigger to widen the window.

The PTY harness `pump` resolves to about 100 ms sampling when idle. The
inner `poll` timeout is 100 ms. That bounds the catch window.

If a fine poll is flaky, reconsider the assertion. The real promise is
that the main loop keeps redrawing while a build runs. Asserting the
frame-diff property plus a settled cache may be more robust than
catching one transient marker.

Verify with `cargo test --test pty_perf` (about 35 to 90 s). Grep the
`--nocapture` output for `building transcript` to confirm it was seen.

### 2. Update `docs/tui-turn-fold.md`

Bring the spec in line with the code. Reconcile these points.

#### Tally format

`⎿ N steps · tool ×N · ... · M msgs`.

#### Main-view fold

The fold applies to the main view too, not only browse. One shared
`turn_fold` set. "Main view unchanged" means the fold state persists
when returning to main.

#### Thinking default

Blocks start collapsed.

#### Final-message panel

A rounded box, no title. Only the `Report` border marks it. The
thinking block is inside the box. The user box keeps its rounded
`Accent` border and "User" title. It has no background fill.

#### Running turn

Folded by default. The live tally merges into the "Waiting for model"
working row. No separate in-transcript spinner row.

### 3. Update `docs/INDEX.md`

Reflect the re-scope in the "Repo state" and status lines. A fresh
session reads this file first.

### 4. Record the three bookkeeping items

Add them to `docs/tui_feature_requests_from_human.md` as pending or
deferred entries. These are user-reported and not implemented.

#### Browse cursor bug

The browse cursor does not paint at the start of a word. For a
hyphenated word it paints on the hyphen. See `bin/tui/src/browse.rs`.

#### `e` motion unregistered in browse

The vim `e` (end of word) motion is not bound while in browse. See
`browse.rs`.

#### Browse with a draft

Allow entering browse with a non-empty draft. Today the double-`s` gate
needs an empty input. Loosen it. See
`docs/tui-conversation-browsing.md` section 4.2.

### 5. Clean up incidental diff churn

An earlier bulk perl and sed cleanup trimmed trailing `────` divider
runs and trailing whitespace in `app.rs`, `color.rs`, and `render.rs`.
This is cosmetic. It pollutes the diff. Restore the original divider
lines and whitespace so the diff holds only intended changes.

Watch for line-merge damage from that same perl pass. Two known spots
joined a comment divider onto the next code line.

`app.rs`: the `// ── join-skip fingerprint` divider merged with the
following doc line. Fixed in-tree. Re-verify.

`render.rs`: the `// ── response-text section` divider merged with the
following `if let`. Re-verify it is a clean comment.

Re-scan all three files for any other comment line running into a code
line.

### 6. Final gate (after the above)

Run the commands in order.

`cargo build`.

`cargo test --bin tui` (expect 317 passing).

`cargo test --test pty_perf` (expect green after item 1).

`cargo clippy`.

Clippy has not been re-run since the re-scope. The dead-code warning on
a setter is already resolved by using the `pub(crate)` field directly.
Re-confirm clippy is clean.

### 7. Commit only when the user asks

Do not commit now. Stage the re-scope as one commit only when asked.
The suggested scope is the turn-fold re-scope feature plus the
regenerated snapshots. Keep the pty_perf fix and the docs together, or
split them per user preference.

## Quick resume commands

```sh
cd /home/tony/programming/rushi-tui-ratatui-widget-based/bin/tui
cargo build
cargo test --bin tui            # expect 317 passing
cargo test --test pty_perf      # the red one, about 35 to 90 s
cargo clippy -p tui
```

## Files and anchors

`bin/tui/src/render.rs`: `user_box_rows` (~192), `report_box_rows`
(~211), `message_box_rows` (~222), `AssistantMessage` arm (~424),
`fold_summary_line`, `working_row`, `rebuilding_row` (~2146).

`bin/tui/src/app.rs`: `fold_input()`, `live_fold_tally()` (~1402),
`BuildKey`, `attach_external_loop`, `App.thinking_expanded` (572,
`pub(crate)`).

`bin/tui/src/color.rs`: `Role::Report` (312, 363, 431, 485, 586).

`bin/tui/src/fold.rs`: `tally_text` (~50), `collapsed_summary`.

`bin/tui/src/transcript_worker.rs`: `BuildKey` (lines 8 to 17, no
`fold_active`).

`bin/tui/src/snapshot_tests.rs`: `snap_assistant_thinking_block`
(~172, sets `app.thinking_expanded = true` directly).

`bin/tui/tests/pty_perf.rs`: the failing test (toggle trigger ~56 to
106).

## Resolution (second session, 2026-09-15)

All open items verified resolved against the working tree:

1. `pty_perf` is green. The 100 ms fine poll plus the content-expand
   trigger was already in tree. Five consecutive runs passed, ~9 s
   each, with the `building transcript` marker caught every time.
2. `docs/tui-turn-fold.md` is re-scoped and reconciled (tally format,
   main-view fold, thinking default, final panel, running turn).
3. `docs/INDEX.md` is updated. Its "clippy is clean" claim was
   corrected to "no new warnings over the pre-existing baseline".
4. The three bookkeeping items are recorded in
   `docs/tui_feature_requests_from_human.md` under "New requests
   (2026-09-15)".
5. No incidental diff churn remains in `app.rs`, `color.rs`, or
   `render.rs`. Both line-merge damage spots are clean.
6. Final gate passes. `cargo build` is clean, `cargo test --bin tui`
   gives 317 passed, `cargo test --test pty_perf` is green, and
   `cargo clippy` shows exactly the 28 pre-existing warnings at
   HEAD. The one warning the re-scope added, an indexed loop in
   `render.rs`, was fixed.
7. Nothing was committed. The user has not asked for one.

