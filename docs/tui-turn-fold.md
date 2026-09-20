# TUI turn fold

Status: Implemented. The user locked the design on 2026-09-14 and
re-scoped it on 2026-09-15. This doc records the design and every
confirmed decision.

The kernel lives in `bin/tui/src/fold.rs` (pure, no `App`
dependency). The fold state, keys, and cursor remap live on `App`.
The fold-aware build lives in `render.rs`. A folded L3 tool result
renders as a one-line panel header row with no body and no margin
rows.

Expanding it (click, `zo`, `zA`, `zR`) restores the full panel.

The request comes from the feature-request log. Tool steps between
a user message and the final report are too fast to read. Collapse
that noise so the log reads "prompt to outcome". Safety stays gated
by extension permissions, not by forced reading.

## Rescope (2026-09-15)

The fold now applies to the main view, not only browse. One shared
`turn_fold` set on `App` drives both views. The state persists when
you return to the main view. It clears on a session switch.
"Main view unchanged" now means the fold state persists. It no
longer means the main view is always unfolded.

Thinking blocks start collapsed. The final idle assistant message
of a completed turn sits in a rounded panel. The running turn is
folded by default. Its live tally merges into the working row.

## Scope and turn model

The fold applies in both the main view and browse mode. One shared
`turn_fold` set (a `HashSet` of turn seqs) drives both. The set
lives on `App`. It persists across view switches and across
sessions. It clears on a session switch.

The fold applies to every turn, including the in-progress one.

A turn starts at a `user_message` event and ends at the next one
or the log end. The final message is the last `assistant_message`
with content. The kernel guarantees a completed turn ends that way.
An in-progress turn tracks its last assistant message as the live
tail.

## Tally line

The collapsed L1 summary counts the hidden events of the turn.
It leads with a `⎿` marker, then the step count, the tool
histogram, the compact count, the hook histogram, and finally the
message count. The `⎿` leader is prepended at render time in
`fold_summary_line` in `render.rs`. The step count is the hidden
`tool_call` events. The message count is the hidden intermediate
`assistant_message` events and is always last.

```
⎿ 14 steps · read ×5 · bash ×3 · compact ×1 · harness-hook-compact ×2 · 6 msgs
```

The tool tally is a histogram of the `name` field. It reads `name`
from each `tool_call` payload. Tool names sort by count descending.
Ties keep first-appearance order. Extension tools recorded into
`events.jsonl` join the tally with no hard-coded tool list.

The compact count is the number of hidden `compaction_started`
events. Compaction is hook-triggered. The `exhausted.handle` and
`overflow.resolve` windows run the compact hook. The loop records
the result as `compaction_*` events, so the tally counts those
directly.

The hook histogram counts the `hook_applied` `ext_status` markers,
which record the hook triggers that landed. Each entry is keyed by
the command basename (the store-path or bare command name). The
ordering is the same count-descending, first-appearance rule as the
tool histogram. No hard-coded hook list: any hook recorded into
`events.jsonl` joins the tally.

### Tally width overflow

The tally row is a single line. The TUI has no multi-row widget
for it, so wrapping to a second row is not an option. Instead the
tally fits to the available width. When the full text (with the
per-hook breakdown) would exceed the column budget, the compact and
hook fields collapse into one `hook ×<total>` field. The `total`
counts the compact events plus all hook triggers. The step, tool,
and message fields are never truncated.

- Collapsed row: the budget is the transcript width minus the
  gutter and the two-column `⎿ ` leader.
  `build_transcript_input` computes it and passes it to
  `FoldState::collapsed_summary`.
- Live working row: the budget is the row width minus the spinner
  frame, the phase text, and the ` · ` separator.
  `App::live_fold_tally` takes the budget as an `Option`.

Wide terminals (and short hook lists) keep the full
`name ×count` breakdown.

## Fold hierarchy

The fold is a three-level hierarchy. Each level nests in the one
above.

- L1, the turn fold. A collapsed turn shows the user box, a tally
  line, and the final-message panel. Everything between is hidden.

- L2, the expanded-turn default. An open turn shows the
  intermediate assistant messages. Every tool result folds to a
  one-line header by default. The fold applies in both views.

- L3, the per-result fold. Each tool result expands or collapses
  on its own. A click, `za` under the cursor, or `zA` for the whole
  turn sets its per-block fraction. A fraction of 0.5 or more
  renders the full body. Below that renders the one-line header.

## Final message panel

The idle assistant reply of a completed turn sits in a rounded box.
The box has no title. Only the `Report`-toned border marks it.
The border color is distinct from the `Accent` user-box border.
The thinking block renders inside the box. The panel is built by
`report_box_rows` in `render.rs`.

The user box keeps its rounded `Accent` border and its `User`
title. It has no background fill. Only the border remains. Both
boxes share the rounded shape via `message_box_rows`.

## Thinking block default

Thinking blocks start collapsed. `App::new` sets `thinking_expanded`
to `false`. A collapsed block renders a one-line label. `Ctrl+T`
expands the full reasoning text. `Ctrl+X` hides or shows the block
entirely.

## Running turn

The in-progress turn is folded by default. It shows the user box
and the live tail, not the full intermediate steps. The live tally
merges into the working row instead of a separate in-transcript
spinner line. There is no separate in-transcript spinner row.

The working row reads the phase label plus the live tally, for
example:

```
Working… · 3 steps · bash ×2 · 1 msg
```

## Keys and default state

The `z` fold keys act while browse is active. The state they set
persists in the main view.

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
| `Ctrl+O` | global tool expand/collapse toggle |
| `Ctrl+T` | expand or collapse thinking blocks |
| `Ctrl+X` | show or hide thinking blocks |

`za` acts on the innermost fold under the cursor. A collapsed turn
line toggles L1. A tool-result header toggles that L3 result.

The default on entering browse is all folded, the `zM` state. The
state lives on `App` and persists across browse sessions. A fresh
session starts all folded.

## Confirmed decisions

The user confirmed the original design on 2026-09-14, the re-scope
on 2026-09-15, and the tally extension on 2026-09-20.

- The fold applies in all loop states, running or idle.
- The fold applies in the main view and browse mode alike.
- The collapsed line uses the detailed tally with the `⎿` leader.
- The in-progress turn is folded. Its live tally merges into the
  working row. No in-transcript spinner line.
- A turn cannot end on a contentless tool-call message. The kernel
  forbids it.
- The fold state is app-level. It persists across view switches
  and sessions.
- The default on entering browse is all folded.
- Thinking blocks start collapsed.
- The final idle reply sits in a title-less `Report` panel.
- `zM` and `zR` reset all levels at once.
- `zj` and `zk` jump between turn tops only.
- Extension tools join the tally through the event `name` field.
- The tally now carries a compact count and a hook trigger
  histogram (2026-09-20). `compact ×N` counts the hidden
  `compaction_started` events. Each `hook_applied` marker adds a
  `name ×N` entry keyed by the command basename.
- On overflow the tally collapses the compact and hook fields into
  one `hook ×<total>` field. The TUI has no multi-row widget for the
  tally row, so no second-row wrap (2026-09-20 decision).

## Build and test plan

- Turn segmentation is a pure pre-pass over the event vec. It
  splits on `user_message` and records the final message index per
  turn.
- The fold-aware build feeds both the main and browse layouts. One
  shared open-turn set drives the build.
- A fold-state change rebuilds the layout and remaps the cursor
  onto the containing fold.
- The build cache key carries `turn_fold_epoch` and `loop_running`.
  It does not carry a separate fold-active flag.
- L3 reuses the per-`tool_id` expand state in `app.rs`
  (`block_fracs`, `block_targets`). `zA` toggles every id in the
  current turn.
- Cursor-to-turn mapping runs through `event_line_starts` plus the
  turn partition.

Snapshots: one test per state. All folded. L2 open with results
folded. One L3 open. The in-progress turn folded. The
final-message panel.
