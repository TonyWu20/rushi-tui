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
- `Summarize the branch`: Close the palette floating window. The TUI sends
  the kernel the branch-summarize primitive (see `Kernel-side changes`). The
  picked event must be on the active path.
  - The kernel summarizes the **abandoned branch**, the events abandoned when
    the fork was made.
  - The summary is appended as a new leaf on the active path.
  - The active-path events are NOT compacted. The abandoned branch stays in
    the log.
  - The summary carries what was done on the other branch and why it was
    abandoned. The current branch's "why" gets context. See the
    `pi-alignment correction` decision at the end of this doc.
- `Summarize with custom prompt`: Close the palette floating window. The user
  writes a prompt in the input box and presses Enter to send it. The prompt
  replaces the default branch-summarize instruction. The user's text is the
  entire instruction.
  - The kernel summarizes the abandoned branch with the user's prompt.
  - The summary is appended as a new leaf on the active path. This is the
    same flow as `Summarize the branch`, with a custom instruction.

## Spec decisions (from discussion)

All open points below were decided by discussion with the author.

### Kernel-side changes (in the `rushi` kernel repo)

- **Correction (2026-09-16).** The original spec of this section was
  wrong. It specified compacting the active prefix up to the picked
  event. That is the opposite of pi's branch summarize. The active
  prefix is the recent work the user keeps. The abandoned branch is
  what gets compressed into context.
- New primitive: a **branch mode of `bin/compact`** (P2: first need,
  inline. No new binary). It reuses the whole auto-compact pipeline:
  `assemble --summary-input`, the `model` call, and the marker append.
  The exact flag spelling is a kernel P6 detail.
  - The summary input covers the abandoned branch events, not the
    active prefix. The range is derived in-kernel from the open span
    of the last `rewind` marker on the active path
    (`rushi_common::rewind`). No new `assemble` flag.
  - It appends a `compaction_summary` marker with a new additive
    optional field `branch_of` (P1b, no `v` bump). That field points
    at the seq of the `rewind` marker whose open span was summarized.
  - `first_kept_seq` is a no-op sentinel on a branch marker: nothing
    is replaced. The active path stays in the context intact.
  - `--prompt` replaces the default instruction. That flag already
    exists.
- Flow: compute the active path up to the picked event using
  `rushi_common::rewind::active_ranges`. Find the last `rewind`
  marker on that path. Its open span `(target_eff, marker_seq)` is
  the abandoned branch. In the tree shape below, that is B, C, D.
  Summarize that span. Append the `compaction_summary` marker at the
  log end, with `branch_of` set to that `rewind` seq. Also write the
  summary to `sessions/<n>/branch-summary/v<N>.md`, mirroring how
  auto-compact writes `handoff/v<N>.md`.
- Projection rule in `bin/assemble` (additive, P1b):
  - Boundary selection is the last `compaction_summary` **without**
    `branch_of`. That is the current last-wins behavior, unchanged.
  - Markers **with** `branch_of` are projected as always-included
    add-on framing (a user-role message, like
    `summary_framing_item`). They never join boundary selection.
    This stops a branch marker clobbering an auto-compact boundary.
  - When the projection input includes file loads (`read_file` /
    `read`), place the `branch_of` marker before the read content.
    The `handoff.md` boundary framing takes that slot when present.
    This lets the model attribute the tool results to the abandoned
    span they came from.
- P1a: no new event type. A branch summary changes no `claim` or
  loop state. It is a projection and TUI effect, which P1a routes to
  a rendering rule. `compaction_summary` plus `branch_of` carries it.
- On an LLM failure, append the existing `compaction_failed` marker.
  No fork happens. No `rewind` marker is appended.
- Two compact primitives coexist. They do different jobs.
  - `bin/compact --up-to <seq> [--prompt]` is manual compaction. It
    compacts the active prefix up to the picked seq. It already
    exists. This flow does not use it.
  - The `bin/compact` branch mode does branch summarize. It
    summarizes the abandoned branch and appends the summary. This
    flow uses it.

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

### Action flows (the four outcomes)

Only `Rewind without summary` forks. It appends a `rewind` marker.
`View-only` changes nothing. It only scrolls the viewport. The two
summarize outcomes do not fork. They spawn the branch-summarize
primitive, which appends a `compaction_summary` leaf on the active
path.

- The two summarize outcomes need the loop to be idle. They append
  log events.
- Browsing and searching stay allowed mid-step. `View-only` is
  allowed mid-step too because it changes no state.
- When a fork or a summarize is requested with the loop busy, the
  hint line shows "loop busy, wait for the step".

- `View-only`: close the palette floating window. No `rewind` marker,
  no fork, no kernel process, no `mode` change. The full active path
  stays intact and the main viewport scrolls to the picked event so
  it is visible. This is navigation of the existing log. Nothing is
  dimmed or masked.
- `Rewind without summary`: close the palette, go to the picked
  event. The TUI re-renders the active path. Off-path events are
  dropped from the main transcript. The rewind marker stays as the
  fork-boundary line. Branch visibility is owned by the `tree`
  palette and its preview pane.
- `Summarize the branch`: close the palette. Spawn
  `bin/compact <session> --branch` as a child process. No `rewind`
  marker is appended. The status line shows a compacting indicator
  while it runs. On the `compaction_summary` marker the TUI re-renders
  and the summary leaf appears on the active path. On
  `compaction_failed` it shows the error in the status line.
- `Summarize with custom prompt`: close the palette, enter a
  transient compact-prompt state. The input box shows a hint that the
  next Enter submits the custom summarize instruction. Esc cancels
  back to normal input. On Enter, spawn
  `bin/compact <session> --branch --prompt <text>`. No `rewind`
  marker is appended.

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
  list as "pending kernel". No-op until the kernel's `bin/compact`
  branch mode lands. The existing `bin/compact --up-to`/`--prompt`
  flags have compact-prefix meaning. That is the opposite of the
  pi-aligned branch summarize above, so they are not wired into this
  flow. The TUI help strings in `tree_option_items`
  (`bin/tui/src/app.rs`) were reworded 2026-09-16 to the branch-
  summarize flow.
- Tree indentation (`└─`, 3 spaces per level, post-fork) is now
  rendered. The `TreeList` stage draws the event log through a
  `tui-treelistview` table instead of a flat list.
- `bin/tui/src/palette/tree_model.rs` builds the `SessionTree` model.
  Branch depth mirrors the kernel `rewind_active_ranges` semantics.
  A branch is rooted at its rewind marker's `target_seq`, the
  rewound event. The branch block is the marker row and the events
  after it, up to the marker that next rewinds back to its target
  or earlier.
  - The abandoned tail (rows between the target and the marker)
    keeps its own depth. The marker row and the new events indent
    one level below the target row. The branch block renders right
    under the rewound event.
  - A fork is a marker whose abandoned tail holds no marker row.
  - A re-entry keeps the target's depth. Its abandoned tail held a
    complete nested branch. The new events continue the target's
    own branch.
  - A marker whose target sits outside the in-memory window falls
    back to the trunk.
- A hidden trunk root holds every depth-0 row. Top-level rows render
  at level 0 with no marker. Each deeper level adds one `└─` marker
  and three spaces.
- The type filter plus fuzzy query keeps a match and its ancestor
  path visible. Navigation, ring wrap, and the preview pane follow
  the selected tree row through the `TreeListViewState`.

### Follow-up spec decisions (2026-09-15)

Feedback on the shipped View-only and Rewind-without-summary modes.
Recorded 2026-09-15. Nothing below is built yet.

#### Navigation and focus (all float windows)

- Wrap-around is inherent to every float list.
  `Up`/`Down` at the first item jumps to the last, and at the last
  item jumps to the first.

- Applies to the file picker, the session list, the tree event list,
  and every future float.

- Movement keys are `Ctrl+J` / `Ctrl+K` and the arrows.
  Plain `j` / `k` are released back to query typing.
  This unblocks queries that contain `j` or `k`.

- Focus model, option B. `Ctrl+Shift+P` toggles focus between the
  entry list and the preview pane. `Ctrl+U` / `Ctrl+D` half-page
  scroll the focused pane. The focused preview pane gets a green
  border.

- The file picker adopts the same contract.
  One scheme across the floats, not two.

#### `Tab` reserved for completion

- `Tab` completes the highlighted fuzzy-matched item.
  It applies to the file picker, the session list, the tree event
  list, and the root command list.

- Completion inserts the item text and keeps the window open.
  The user keeps typing to narrow further.

- The file picker's `Tab` scope-cycle binding is removed.
  Scope cycling stays on `Ctrl+I` only.

- Open: `Tab` in the `TreeOptions` stage has no query to complete
  into. No-op or commit is undecided.

#### Event-type filter in the tree list

- A type filter cycles on a key press.
  Cycle: `full → user → assistant → tool → user+assistant → full`.

- Recommended key: `Ctrl+F`.
  Bare `f` types into the fuzzy query, so it cannot be the trigger.

- `tool` keeps both `tool_call` and `tool_result`.
  `user` includes `user_message_retract`.

- The filter narrows candidates before fuzzy ranking.
  It combines with the query in AND semantics.
  The active filter shows in the input-bar hint.

- The filter resets when the tree stage is left or the palette
  closes.

- Open: whether the same filter applies to the session sub-list.

#### Prettified tool entries

- The `bash` row shows `bash <arguments.command>`.

- The `read` / `edit` / `write` rows show
  `<tool> <arguments.file_path>`.
  A missing `file_path` falls back to `path`.

- The `tool_result` row shows the tool name, the status, and the
  first line of the result text.

- Built-in tools drop the angle brackets and the `tool:` prefix.
  Custom tools stay raw for the moment.

#### Preview pane: parse and highlight, fully scrollable

- Tool call and tool result JSON is parsed with `jaq-core`.
  `jaq` is the CLI crate. `jaq-core` is the library crate.

- Parsed values are pretty-printed.
  On parse failure, the pane shows the raw text.

- The pane reuses the repo syntax highlighter unconditionally.
  User and assistant text use the markdown pass.
  Parsed JSON uses the JSON highlight.

- The engine is the tree-sitter `tui-highlight` engine, the same
  engine the transcript uses.

- The raw-JSON toggle in the design doc is superseded for the tree
  pane. The highlight is always on.

- The pane is fully scrollable.
  The 600-character body cap is dropped.
  Windowed rendering follows `docs/tui-preview-pane-plan.md`.

#### Resolved open points (2026-09-15)

All four open points closed by decision.

- Legacy terminals: `BackTab` is the fallback focus toggle.
  `Ctrl+Shift+P` may arrive as plain `Ctrl+P` there, and
  `BackTab` covers it.

- `Tab` in the `TreeOptions` stage is a no-op. `Enter` commits.

- The type filter is tree-list only. The session sub-list is
  unaffected.

- The `jaq-core` version is pinned to `3.1.1`, with `jaq-json` at
  `2.0.3`. Verified against crates.io 2026-09-15. The parse entry
  point and pretty printer are recorded in the phase-2 spec.

- Open point (agent, spec pass), resolved by the human 2026-09-15:
  `Tab` and `Ctrl+I` share byte `0x09` in standard terminals.
  The picker scope cycle moves to `Ctrl+T`. `Ctrl+H` was rejected
  as the backspace byte. `Ctrl+I` stays bound for terminals that
  report it distinctly.

The full build spec is
`docs/tree-ui-design-from-human-phase-2.md`.

#### Implementation order and tests

- Order: prettified rows, pane parse + highlight, type filter,
  navigation and focus, `Tab` completion.

- Unit tests cover wrap, focus, filter cycling, and completion.
  Snapshot tests cover the tree palette states.

## Spec decision: pi-alignment correction for branch summarize (2026-09-16)

This decision supersedes the original `Summarize the branch` /
`Summarize with custom prompt` design. It was learned from pi.

### The mistake

The original design compacted the active prefix up to the picked
event. In the tree shape below, that reads as compacting `A, E, F`.
That is the recent active work the user keeps. It also drops the
abandoned branch `B, C, D` entirely. That loses the context that makes
a fork useful.

```
A -> B -> C -> D   (old leaf, abandoned)
  |
   > E -> F         (target, on the active path)
Common ancestor: A
```

### The pi design, adopted here

`Summarize the branch` summarizes the **abandoned branch** (the
sibling that shares the common ancestor), not the active prefix. The
summary of the other branch carries what and how things were done
there. It acts as the "why" context for the current branch. After the
operation:

```
A -> B -> C -> D
  |
  -> E -> F -> [summary of B, C, D]   (new active leaf)
```

The active-path events `E, F` are kept. The abandoned branch is
compressed into a single summary leaf appended to the active path.

### Consequences for the spec

- The kernel primitive is a branch mode of `bin/compact`, not a new
  binary and not the `--up-to`/`--prompt` manual compact. See
  `Kernel-side changes`.
- `Summarize with custom prompt` replaces the default summarize
  instruction with the user's text. It targets the same abandoned
  branch.
- The summarize outcomes do not fork. They append no `rewind` marker.
  They append a `compaction_summary` marker on the active path. That
  marker carries the summary text.
- The picked target must be on the active path. The abandoned branch
  is derived from the last `rewind` marker on that path.

### Decided (2026-09-16, via `refinement-policy.md`)

These decisions apply the kernel's `refinement-policy.md`
(`rust-unix-harness/docs/refinement-policy.md`) to this flow.

- **Two compaction primitives coexist.** `bin/compact --up-to`/`--prompt`
  is manual prefix compaction and stays as is. The tree summarize
  options use a new branch mode of `bin/compact`.
- **Binary shape (P2 rule of three).** No new `bin/branch-compact`
  binary. `bin/compact` gains a `--branch` mode. This is the first
  need, so do it inline. The shared pipeline and the TUI caller both
  justify an inline mode. The exact flag spelling is a kernel P6
  detail.
- **Event type (P1a).** No new event type. A branch summary changes
  no `claim` or loop state. It is a projection and TUI effect. P1a
  routes such effects to a rendering rule, not a new type.
- **Reuse the marker (P1b).** Reuse `compaction_summary` with a new
  additive optional `branch_of` field (no `v` bump). It points at the
  `rewind` marker whose open span was summarized. `first_kept_seq` is
  `1`, a no-op sentinel. The active path stays intact. The summary is
  a pure add-on.
- **Versioned markdown artifact (P1b).** Like the auto-compact handoff,
  the branch summary is written to disk as a versioned, human-readable
  markdown file. The marker still carries `version`, `parent_version`,
  and `diverge_seq` (from `handoff_version_meta`) for a stable
  identity. `bin/assemble` projects only the highest version per
  `branch_of`, which is the folding rule.
- **Own `branch-summary/` namespace, not `handoff/`.** The branch
  summary goes to `sessions/<n>/branch-summary/v<N>.md`, not to
  `handoff/v<N>.md` or `handoff.md`. That `handoff` path is the
  auto-compact continuation handoff, and its P3 invariant (`handoff.md`
  equals the latest compaction handoff) must stay intact. `bin/assemble`
  reads the branch file and falls back to the marker's inline `summary`.
- **Branch selection.** For multiple forks, summarize the latest active
  branch. That is the branch active just before the current branch.
  In log terms, the open span of the last `rewind` marker on the
  active path.
- **Range selection (P0/P3/P4).** Derive it in-kernel. The abandoned
  range is the open span of the last `rewind` marker on the active
  path. Compute it with the existing `rushi_common::rewind` module.
  No new `assemble` flag.
- **Re-run semantics.** Accumulate by folding into a rolling summary
  via the existing `COMPACT_INSTRUCTIONS_UPDATE` path. Effort from
  every abandoned branch survives in the rolling summary.

### Open kernel detail (P6, for the `bin/compact` branch mode)

- **Branch-summary file, written by `bin/compact`.** The loop's
  `write_handoff` writes the continuation handoff to
  `handoff/v<N>.md` + `handoff.md`. A branch marker must not write
  there, or the P3 invariant breaks. `bin/compact --branch` instead
  writes `branch-summary/v<N>.md` (N from `handoff_version_meta`) and
  leaves the `handoff/` dir untouched. `bin/assemble` reads that file
  for the framing and falls back to the inline `summary`.
- **DAG edge for a branch marker.** `handoff_version_meta` sets
  `diverge_seq` to the parent boundary's `first_kept_seq`. For a
  branch marker, set `diverge_seq` to `branch_of` instead. That is
  the fork point. The DAG edge then reads as this branch summary
  diverged from the main line at the fork.
- **Coexistence is already handled.** Boundary selection is the last
  `compaction_summary` without `branch_of`. Branch markers project as
  add-on framing. They never clobber an auto-compact boundary. No
  last-wins edge case remains.
- **Filed 2026-09-16.** This kernel delta is tracked as
  `github.com/TonyWu20/rushi` issue #22. The TUI summarize outcomes
  stay "pending kernel" until that lands.
