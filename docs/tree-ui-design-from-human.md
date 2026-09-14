# Session log tree ui design

The `rushi` kernel has implemented a rewind/tree feature.
It supports going back to any event logged on the `events.jsonl` session log file.
We just need to implement the TUI interface to use it.

## Draft design

### Entry point

The command palette shows `tree`. In the TUI, type `<Esc>` enters `Normal` mode, type
`:tree<Enter>` to enter the `tree` interface.

### Interface

Reuse the preview pane + fuzzy search features.

In the command palette floating window, type anything to fuzzy search the wanted
event(s).

The events are displayed in oneline mode. Event type is tagged in the beginning
of the event.
No wrapping needed. Show `...` when
the message is too long to be displayed in the current window size.

The preview pane shows the preview of the full content of the event.

After branch is created by an attempt of `tree`, the events shown in the palette
floating window are indented according to the depth level of tree.
Example:

```
<user message> ...
<assistant> ...
<tool:read> ...
<tool:...> ...
  |_<user message> ...
  |_<assistant> ...
  |_<tool:...> ...
  |_<assistant> ...
      |_<assistant> ...
      |_<user message> ...
  |_<assistant> ...
<assistant> ...
...

```

On selection, shows hint of 4 options:

- View-only
- Rewind without summary
- Summarize the branch
- Summarize with custom prompt

Target behaviors:

- `View-only`: Close the palette floating window. No rewind, no fork, no `rewind`
  marker. The full active path is kept; the TUI only scrolls the main viewport
  to the picked event so it is visible. This is navigation of the existing log,
  not a change to the conversation.
- `Rewind without summary`: Close the palette floating window, directly go to the picked event. TUI updates the rendering, only
  show history up to the picked event. Off-path events are dropped from
  the main transcript. The rewind marker stays as the fork-boundary line.
  Branch visibility is owned by the `tree` palette and its preview.
- `Summarize the branch`: Close the palette floating window, the TUI sends command to `rushi` kernel to compact the context up
  to the selected event.
- `Summarize with custom prompt`: Close the palette floating window, user write
  prompt in the input box, press Enter to send. The `rushi` replaces the default
  compact instruction with the user's prompt to compact the context up to the
  selected event. (`rushi`'s `bin/compact` might need to add a CLI flag to accept
  the custom prompt content)

## Spec decisions (from discussion)

All open points below were decided by discussion with the author.

### Kernel-side changes (in the `rushi` kernel repo)

- `bin/compact` gains a `--up-to <seq>` flag. Today the compact always cuts at
  the end of the log. The flag lands the compact boundary on the picked
  event, so the prefix up to that seq becomes the summary input. It reuses
  the `assemble --up-to` name.
- `bin/compact` gains a `--prompt <text>` flag. It replaces the default
  compaction instruction wholesale. The six-section format is not merged in.
  The user's text is the entire instruction. The flag forwards to
  `bin/assemble --summary-input`, which accepts the same flag.

### Palette behavior

- Every event is a valid pick. The kernel clears all pending state on a
  `rewind` marker (`bin/claim` clears `pending_tool_calls`,
  `pending_follow_ups`, `pending_approval_request`). It derives the owed
  state from the target. A `tool_result` target in `on` mode sets
  `awaiting_model`. Every other target sets `idle`. So a mid-step pick
  does not trigger the loop. No dimming, no filtering.
- Row rendering: one line per event. The event type tags the front. No
  wrapping. Long messages truncate with `...`.
- Tree indent (after any fork exists): box-drawing `└─` marker, 3 spaces
  per level. Top-level rows carry no marker. Level 1 rows start with
  `└─ `. Level 2 rows start with three spaces plus `└─ `. Each deeper
  level adds three more spaces. The top of the active branch stays left
  aligned.
- Selection swaps the floating window content. The event list clears. The
  same window shows the four options. Up/down moves the choice.
  Enter confirms. Esc returns to the event list.
- `mode` follows the target type. A `user_message` target uses `before`
  (the message waits in the input box, unsent). Every other target uses
  `on`.

### Action flows (the three fork outcomes)

`View-only` is the fourth option but does not fork: it appends no `rewind`
marker and spawns no `bin/compact`; it only scrolls the viewport. The three
outcomes below all fork.

- All three outcomes fork. Each pick appends a `rewind` marker at the
  picked event. `reason` is `tui_pick`. The two summarize outcomes
  also run `bin/compact`.
- The loop must be idle for the three fork actions. Browsing and searching stay
  allowed mid-step, and `View-only` is allowed mid-step too because it
  changes no state. When the loop is busy and a fork is requested, the
  hint line shows "loop busy, wait for the step".
- Order: append the `rewind` marker first, then run the compact. If the
  compact fails, the fork still stands. The user can retry the compact or
  continue un-compact.

- `View-only`: close the palette floating window. No `rewind` marker, no fork,
  no compact, and no `mode` change. The full active path stays intact and the
  main viewport scrolls to the picked event so it is visible. This is
  navigation of the existing log; nothing is dimmed or masked.
- `Rewind without summary`: close the palette, go to the picked event. The TUI re-renders
  the active path. Off-path events are dropped from the main transcript. The
  rewind marker stays as the fork-boundary line. Branch visibility is owned
  by the `tree` palette and its preview pane.
- `Summarize the branch`: append the rewind marker, spawn
  `bin/compact --up-to <picked_seq>` as a child process. The status line
  shows a compacting indicator while it runs. On the `compaction_summary`
  marker the TUI re-renders. On `compaction_failed` it shows the error in
  the status line. The fork stands either way.
- `Summarize with custom prompt`: close the palette, enter a transient
  compact-prompt state. The input box shows a hint that the next Enter
  submits the custom compaction instruction. Esc cancels back to normal
  input. On Enter, append the rewind marker, then spawn
  `bin/compact --up-to <picked_seq> --prompt <text>`.

### Preview pane

- The preview pane shows a pretty view per event kind. User messages show
  text. Assistant messages show text. Tool calls show args. Tool results
  show output. With the event list focused, a single key toggles the raw
  JSON of the picked event for debugging. Search typing never triggers
  the toggle.

### Rewind transcript masking (hide, not dim)

- With a rewind marker in the log, the main transcript shows the active
  path only. Abandoned-branch events are dropped from it entirely. They
  are not rendered dimmed. Branch visibility is owned by the `tree`
  palette and its preview pane.
- The `rewind` marker event is the exception. It stays rendered as the
  fork-boundary line. It shows even when its seq is off the active path
  and even when it sits in a collapsed turn's span.
- The marker line is kept only if it helps find the marker in the
  fuzzy palette. The transcript line and the palette row share the
  exact wording "rewound to seq N (mode)". What the user sees is what
  they search. The row also shows the marker's own log seq as its
  `#N` hint.
- This is a user decision from the discussion. The dimming approach was
  rejected. The user said the dim does not work. Option A, full
  removal, is what the user wanted all along.
- Deferred follow-up: improve the tree palette and preview UI for branch
  visibility and filtering. That work is a separate task.

### Confirmed behaviors

- On a `before`-mode user-message pick, the TUI loads that message text
  into the input box, unsent. This matches the kernel design doc
  (`rewind-fork-design.md` section 1).

### Implementation status (TUI side)

- **View-only** — wired end to end. `tree` Goto item → `TreeList` stage
  (fuzzy-ranked event rows, one line each, type-tagged, truncated with
  `...`) → Enter → `TreeOptions` stage with the four options. Committing
  `View-only` closes the palette and sets a one-shot scroll target; the
  next draw pins the picked event's first line at the top of the
  viewport. No marker appended, no state change; allowed mid-step.
- **Rewind without summary** — wired end to end. Committing it appends a
  `rewind` marker (`reason` `tui_pick`) via the port. The active-path
  mask (kernel `active_ranges`) drops abandoned-branch events from the
  transcript. The rewind marker stays as the fork-boundary line. A
  `user_message` target uses `before` mode and restores the message to the
  input box unsent. Every other target uses `on`. A busy loop blocks the
  commit with the "loop busy, wait for the step" hint.
- **Summarize the branch / with custom prompt** — shown in the option
  list as "pending kernel"; committing flashes that the kernel
  `bin/compact --up-to [--prompt]` flags are pending. No-op until the
  kernel side lands.
- Tree indentation (`└─`, 3 spaces per level, post-fork) is not yet
  rendered; rows are flat until it is implemented.
