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

On selection, shows hint of 3 options:

- No summary
- Summarize the branch
- Summarize with custom prompt

Target behaviors:

- `No summary`: Close the palette floating window, directly go to the picked event. TUI updates the rendering, only
  show history up to the picked event.
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
  same window shows the three outcome options. Up/down moves the choice.
  Enter confirms. Esc returns to the event list.
- `mode` follows the target type. A `user_message` target uses `before`
  (the message waits in the input box, unsent). Every other target uses
  `on`.

### Action flows (all three outcomes)

- All three outcomes fork. Each pick appends a `rewind` marker at the
  picked event. `reason` is `tui_pick`. The two summarize outcomes
  also run `bin/compact`.
- The loop must be idle for all three actions. Browsing and searching stay
  allowed mid-step. When the loop is busy, the hint line shows "loop
  busy, wait for the step".
- Order: append the `rewind` marker first, then run the compact. If the
  compact fails, the fork still stands. The user can retry the compact or
  continue un-compact.

- `No summary`: close the palette, go to the picked event. The TUI re-renders
  the active path. Masked events stay visible but dimmed, per the kernel
  marker rendering.
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

### Confirmed behaviors

- On a `before`-mode user-message pick, the TUI loads that message text
  into the input box, unsent. This matches the kernel design doc
  (`rewind-fork-design.md` section 1).
