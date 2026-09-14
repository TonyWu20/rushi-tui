# TUI turn fold

Status: Implemented. The user locked the design on 2026-09-14.
The kernel lives in `bin/tui/src/fold.rs` (pure, no `App`
dependency), with the fold state, keys, and cursor remap on `App`
and the fold-aware build in `render.rs`. A folded L3 tool result
renders as a one-line panel header row (`header_row` in
`tool_display.rs`): name, label, status — no body, no margin rows.
Expanding it (click, `zo`, `zA`, or `zR`) restores the full panel.
This doc records the design and every confirmed decision.

The request comes from the feature-request log. Tool steps between
a user message and the final report are too fast to read. Collapse that
noise so the log reads "prompt to outcome". Safety stays gated by
extension permissions, not by forced reading.

## Scope and turn model

The fold is browse-mode only. The main view always shows the full
transcript. The fold state lives on `App` and persists across browse
sessions. On entering browse, the view starts fully folded.

The fold applies to every turn, including the in-progress one.

A turn starts at a `user_message` event and ends at the next one or
the log end. The final message is the last `assistant_message` with
content. The kernel guarantees a completed turn ends that way. An
in-progress turn tracks its last assistant message as the live tail.

## Fold hierarchy and summary line

The fold is a three-level hierarchy. Each level nests in the one above.

- L1, the turn fold. A collapsed turn shows the user box, a summary
  line, and the final assistant box. Everything between is hidden.

- L2, the expanded-turn default. An open turn shows the intermediate
  assistant messages. Every tool result folds to a one-line header.

- L3, the per-result fold. Each tool result expands or collapses on its
  own, by click, `za` under the cursor, or `zA` for the whole turn.

The L1 summary line counts the hidden events. It reads like
`14 steps, 6 msgs, read x5, bash x3, edit x2`. The step count is the
hidden `tool_call` events. The msg count is the hidden intermediate
messages.

The tally is a histogram of the `name` field. It reads
`name` from each `tool_call` payload. Extension tools recorded into
`events.jsonl` join the tally with no hard-coded tool list.

An in-progress turn shows a braille spinner instead of final counts.
It reuses `WORKING_SPINNER_FRAMES` in `render.rs`. The spinner and the
tally update live while the loop runs. No partial assistant text shows.
The snapshot harness masks braille to `[SPINNER]` to keep tests
deterministic.

## Keys and default state

All keys are browse-scoped. The `z` prefix is unused in browse today.

| Key | Action |
| --- | --- |
| `za` | toggle the fold under the cursor |
| `zo` | open the fold under the cursor |
| `zc` | close the fold under the cursor |
| `zA` | toggle every tool-result fold in the turn under the cursor |
| `zR` | open all folds, all turns, all levels |
| `zM` | close all folds, all turns, all levels |
| `zj` | jump the cursor to the next turn top |
| `zk` | jump the cursor to the previous turn top |
| click | toggle the L3 result under the cursor |

`za` acts on the innermost fold under the cursor. A collapsed turn line
toggles L1. A tool-result header toggles that L3 result.

The default on entering browse is all folded, the `zM` state. The state
lives on `App` and persists across browse sessions. A fresh session
starts all folded.

## Confirmed decisions

The user confirmed every item on 2026-09-14. None is open.

- The fold applies in all loop states, running or idle.
- The collapsed line uses the detailed tally.
- An in-progress turn shows a spinner and live tally, no partial text.
- A turn cannot end on a contentless tool-call message. The kernel
  forbids it.
- The fold is browse-only. The main view stays unchanged.
- The fold state is app-level and persists across browse sessions.
- The default on entering browse is all folded.
- `zM` and `zR` reset all levels at once.
- `zj` and `zk` jump between turn tops only.
- Extension tools join the tally through the event `name` field.

## Build and test plan

- Turn segmentation is a pure pre-pass over the event vec. It splits
  on `user_message` and records the final message index per turn.
- The fold-aware build feeds only the browse layout. The main transcript
  cache stays unfolded.
- A fold-state change rebuilds the browse layout and remaps the cursor
  onto the containing fold.
- The build cache key includes the fold state, so the streaming cache
  cannot serve stale folded lines.
- L3 reuses the per-`tool_id` expand state in `app.rs` (`block_fracs`,
  `block_targets`). `zA` toggles every id in the current turn.
- Cursor-to-turn mapping runs through `event_line_starts` plus the turn
  partition.

Snapshots: one test per state. All folded. L2 open with results
folded. One L3 open.

The in-progress spinner line. A tally with an extension tool
name. The cursor remap after a fold change.
