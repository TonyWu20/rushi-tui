# Tree UI phase 2: navigation, filter, rows, pane

Status: Implemented. Approved and shipped 2026-09-15. Refined 2026-09-16: tag colors (item 1), the kitty protocol enables `Ctrl+Shift+P` (item 4), and `Tab` replaces the typed text (item 5). Refined 2026-09-17: the tree-pane preview renders tool events through the transcript's tool display instead of `\n`-escaped JSON (item 2, below).
Parent: `docs/tree-ui-design-from-human.md` ("Follow-up spec decisions
(2026-09-15)"). Its open points are all resolved in this doc.
Related: `docs/tui-preview-pane-plan.md` (the windowed pane model),
`docs/tui-file-picker.md` (picker keys, P9),
`docs/tui-command-palette.md` (the palette key table).

Phase 2 ships the 2026-09-15 follow-up decisions as five build
items. The standing `Tab` reservation is item 5.

## Work items and build order

| # | Item | Main files |
|---|---|---|
| 1 | Prettified tool rows | `bin/tui/src/app.rs` |
| 2 | Pane parse + highlight + full scroll | `app.rs`, `palette/preview.rs` |
| 3 | Event type filter | `palette/state.rs`, `app.rs` |
| 4 | Navigation and focus | `float.rs`, `palette/state.rs`, `picker/state.rs`, `main.rs` |
| 5 | `Tab` completion | `palette/state.rs`, `picker/state.rs`, `app.rs` |

Build in this order. Each item ships and tests on its own.

## Key contract

One contract across every float window: the file picker, the
session list, the tree event list, `TreeOptions`, and any future
float.

### Movement and wrap

| key | action |
|---|---|
| `Ctrl+J` / `Down` | down one row, wrap to first at the last |
| `Ctrl+K` / `Up` | up one row, wrap to last at the first |
| `PgDn` / `PgUp` | page down / up, wrap at both ends |
| `Home` / `End` | jump to first / last row |

Wrap is an inherent property of every float list, not a per-call
behavior. The state machine wraps the index in the ring instead of
saturating it. Plain `j` and `k` are released to the query, so they
no longer move the cursor.

### Focus and scroll

| key | action |
|---|---|
| `Ctrl+Shift+P` | toggle focus between the list and the preview |
| `BackTab` | same as `Ctrl+Shift+P`, the legacy fallback |
| `Ctrl+U` / `Ctrl+D` | half-page scroll of the focused pane |

- Default focus is the entry list.
- The focused preview pane gets a green border (`Success` role).
- Unfocused panes keep the current `Status` border.
- The list scroll step is half the visible rows, minimum one.
- The preview scroll step is the existing `PREVIEW_PAGE` constant.
- The toggle is a no-op while the preview pane is hidden.

### Query and filter keys

- Plain `j` and `k` are released to the palette query.
- `Ctrl+F` cycles the event-type filter in the tree stage only.
- `Tab` completes the highlighted fuzzy item in every float window.
- `Ctrl+T` cycles the picker file scope. `Ctrl+I` stays bound
  for terminals that report it distinctly. `Tab` no longer cycles
  it.

## 1. Prettified tool rows

Today a `bash` row reads `<tool:bash> {"command":"ls -la",...}`
with compact JSON. The new rows:

| row | new label |
|---|---|
| `bash` call | `bash <arguments.command>` |
| `read` / `edit` / `write` call | `<tool> <arguments.file_path>` |
| `tool_result` | `<name> <status> <first result line>` |
| custom tool | raw, unchanged |

- Built-in tools drop the angle brackets and the `tool:` prefix.

- `file_path` falls back to `path` when absent. This mirrors
  `tool_display.rs`, which reads `file_path` then `path` from the
  call arguments.

- Result rows resolve the tool name from the call `id`. Reuse the
  `call_details` map pattern (`App::call_details` in `app.rs`).
  When the id is unknown, fall back to the bare `tool` tag.

- Status is `ok` or `err` from the result's `is_error`.

- The first result line is truncated with the existing
  `truncate_one_line` helper in `app.rs`.

- Custom and unknown tools keep today's raw `<tool:name>` rows.

- Row tags are classified and colored (human feedback 2026-09-16):
  `user` / `retract` rows take the `Accent` tone, `assistant` rows
  the `Report` tone, tool rows the `ToolName` tone. Every other
  class keeps the plain label. The cursor row keeps its accent
  highlight and is not tag-colored.

Functions to change:

- `tree_row_label`, `tree_event_tag`, and `tree_event_preview` in
  `app.rs` produce the new label shapes.
- `tree_event_items` threads `call_details` into the label builder.
- The existing row-label tests in `app.rs` gain cases for each
  shape.

## 2. Preview pane: parse, highlight, fully scrollable

- Add `jaq-core = "3.1.1"` and `jaq-json = "2.0.3"` to
  `bin/tui/Cargo.toml`. `jaq` is the CLI crate and is not the
  dependency. `jaq-core` is the filter engine. `jaq-json` is the
  JSON value, reader, and writer half of the toolchain.

- Tool calls parse the raw `arguments` JSON with
  `jaq_json::read::parse_single`.
- Tool results parse the raw `value` JSON the same way.
- Parsed values pretty-print with the `jaq_json::write` printer,
  two-space indent (`Pp { indent: Some("  "), ... }`).
- On a parse failure, the pane shows the raw text. It never
  crashes.

- Highlighting is unconditional. There is no raw-JSON toggle key.
  This supersedes the "single key toggles raw JSON" paragraph in
  the parent doc for the tree pane.

- User and assistant message text use the markdown highlight pass.
- Parsed JSON uses the JSON highlight pass.
- Both paths run on the tree-sitter `tui-highlight` engine, the
  same engine the transcript uses. The registry resolves `json`
  and `markdown` (`tui_highlight::resolve_lang`).

- Drop the 600-character cap in `tree_event_body`. The pane
  scrolls the full content, clamped to the content length.

- Per frame, highlight only the highlighted item's body. Cache the
  result per event `seq`, per
  `docs/tui-preview-pane-plan.md` (windowed highlighting, LRU
  bound).

Functions to change:

- `tree_event_body` in `app.rs` drops the cap and returns the full
  source text: the message text, or the compact JSON.

- `PaletteItem` gains a preview-kind flag: JSON or Markdown.
  `palette/preview.rs` dispatches on it.

- `palette/preview.rs` gains the tree-pane pipeline: parse,
  pretty-print, highlight, and cache.

- `render_preview_pane` in `palette/render.rs` keeps the windowed
  scroll and consumes the styled lines.

- The "Enter offers the four options" suffix that
  `tree_event_items` appends to the help stays as a plain line
  after the highlighted body.

- Refinement (2026-09-17): tool-call / tool-result tree events no
  longer re-serialize their JSON. The original pipeline re-serialized
  the compact record through `jaq_json::write::Pp`. That escapes
  string values, so result text showed as raw `\n`-escaped JSON.
  The pane now reuses the transcript's tool display
  (`tool_display.rs::body_rows`). This surfaces the pretty tool
  result even when the transcript folds tool calls (`fold.rs`).

  Concretely:
  - `PaletteItem` gains `tool_payload: Option<ToolPayload>`
    (`{ name, is_call, value, call_args, err }`). A new
    `PreviewKind::Tool` routes tool events to the tool pipeline.
  - `tool_call` events render their arguments as a readable listing
    (`tool_display.rs::call_args_rows`). Scalar args show as
    `key: value` rows. Multi-line string values (`content`,
    `old_string`, `new_string`) show as indented blocks.
  - `tool_result` events render through `body_rows`, with the call's
    arguments resolved through the call `id`
    (`App::call_details`). They use `expanded = true` (the pane
    scrolls); per-tool caps still apply.
  - A custom / unknown result with no `text` field falls back to the
    pretty-JSON pipeline so the pane is never empty.
  - Pure-thinking assistant events (empty `content`, a `reasoning`
    array that carries text) now show the thinking block in the
    preview pane instead of an empty body. `tree_event_body`
    falls back to `render::thinking_text` when `content` is empty.
    The tree row also gains a one-line preview: `thinking: <first
    line>` under the usual `<assistant>` tag, so a thinking-only
    entry is no longer a bare `<assistant>` row. An assistant event
    with real content still previews the content, never the marker.
  - The `TreePreviewCache` key gains the pane width
    (`"<seq>:<width>"`). The tool body layout is width-dependent.

## 3. Event-type filter

The filter is a new field on `PaletteState`. The tree stage owns
it. It resets on stage exit and on palette close.

| state | keeps |
|---|---|
| `full` | every event |
| `user` | `user_message` + `user_message_retract` |
| `assistant` | `assistant_message` |
| `tool` | `tool_call` + `tool_result` |
| `user+assistant` | user plus assistant |

- `Ctrl+F` cycles `full → user → assistant → tool →
  user+assistant → full`.
- The filter narrows candidates before fuzzy ranking. It ANDs with
  the query.
- The active value shows in the input-bar hint, e.g. `[f: tool]`.
- The session sub-list is unaffected (human decision 2026-09-15).
- `Ctrl+F` binds only in the `TreeList` stage. Other stages leave
  it unhandled.

State to add:

- `PaletteState` gains a `TreeFilter` field with a `next()` cycle
  through the five states.
- `press` maps `Key::CtrlF` to a new `PaletteAction::CycleFilter`
  in the tree stage. The caller rebuilds the ranked list, as for
  `Query`.
- `tree_event_items` takes the filter and skips out-of-filter
  events before ranking.

## 4. Navigation and focus

State machine changes:

- A shared `Focus` enum (`List` / `Preview`) lives in
  `bin/tui/src/float.rs`.
- `PaletteState` and `PickerState` each gain a `focus` field.
  Default is `List`.
- `Key::CtrlShiftP` is a new variant in `app.rs`.
- `main.rs::key_input` routes a shift-held `Ctrl+P` press to
  `CtrlShiftP`. Without shift it stays `CtrlP`. Today both land
  on `CtrlP`, so the split is needed for the toggle.
- `BackTab` is already mapped in `main.rs`. It is the legacy
  fallback. Where `Ctrl+Shift+P` arrives as byte 0x10 it is
  indistinguishable from `Ctrl+P`.
- The TUI enables the kitty keyboard protocol at startup
  (`PushKeyboardEnhancementFlags` with
  `DISAMBIGUATE_ESCAPE_CODES`, in `main.rs::TermGuard`). Capable
  terminals then report the shift modifier, and
  `main.rs::key_input` accepts the protocol-form uppercase
  codepoint (`Ctrl+P` as `Char('P')`) for every control binding.
  `BackTab` stays the fallback for legacy terminals. Shipped with
  an end-to-end PTY test
  (`csi_u_ctrl_shift_p_toggles_preview_focus`).
- The palette drops `Char('j')` and `Char('k')` from its move arm.
  Those letters type into the query.
- `move_down`, `move_up`, `page_down`, and `page_up` wrap in both
  state machines.
- `PgDn` at the last row lands on the first row. `PgUp` at the
  first row lands on the last.
- `PaletteState::move_up` gains a `count` parameter for the wrap.
  Today it saturates without one.
- `Ctrl+U` / `Ctrl+D` dispatch on the focus field. The list steps
  half of the visible rows, minimum one, wrapping like the step
  keys. The preview steps `PREVIEW_PAGE` lines.
- The `TreeOptions` option cursor keeps its existing wrap. It
  moves with `Ctrl+J` / `Ctrl+K` and the arrows.
- The file picker adopts the same wrap and focus contract.

Render changes:

- `render_preview_pane` in `palette/render.rs` paints the preview
  border with `Success` when the preview has focus. Otherwise it
  keeps `Status`.
- The picker renderer in `picker/render.rs` does the same.
- The input-bar hint in `palette/render.rs` reflects the new keys,
  the active filter, and the focused pane.
- In `app.rs`, the picker host-key arm swallows `BackTab` today.
  Drop it from the arm so it reaches the picker state machine
  while the picker is open.
- The palette dispatch already funnels every key to
  `palette_state.press`.
  No app-level change is needed for `BackTab` or `CtrlShiftP`.

## 5. `Tab` completion

`Tab` completes the highlighted item. The completion replaces the
typed filter text with the item's full text, mirroring the
picker's `@<path>` replacement model. The window stays open, so
typing continues after the completion.

| window | `Tab` replaces the typed text with |
|---|---|
| file picker | `@<path>` in the draft |
| session list | the full session name |
| tree list | the row label |
| root list | the command label |
| `TreeOptions` | no-op (human decision 2026-09-15) |

- In a sub-stage the goto prefix is kept: the filter typed after
  the prefix is replaced, e.g. `tree sha` + Tab on `bash make -j4`
  yields `tree bash make -j4` (human decision 2026-09-16).
- `Enter` still commits and closes, as today.
- The picker's `press` splits the current `CtrlI | Tab` arm.
  `Tab` completes. `Ctrl+T` cycles the scope. `Ctrl+I` stays
  bound for terminals that report it distinctly.
- A new `PickAction::Complete` and `PaletteAction::Complete` carry
  the selected index to `app.rs`, which does the text edit. The
  state machines stay crossterm-free.
- `BackTab` is reserved for the focus toggle, not completion.

## Pins and resolutions

- The `jaq-core` pin is `3.1.1`, with `jaq-json` at `2.0.3`.
  Both are verified against crates.io as of 2026-09-15. The parse
  entry point is `jaq_json::read::parse_single`. The pretty printer
  is `jaq_json::write::Pp` with two-space indent. `jaq-std` is not
  needed until user-typed jaq queries land.

- `Tab` and `Ctrl+I` share byte 0x09 in standard terminals.
  Crossterm reports both as `Tab`.
  Resolved (human decision 2026-09-15): the scope cycle moves to
  `Ctrl+T` in the picker. `Ctrl+H` was rejected: it is the
  backspace byte in most terminals. `Ctrl+I` stays bound for
  terminals that report it distinctly. The main-view and browse
  `Ctrl+T` thinking-block toggle is untouched: the picker consumes
  the key while open.

## Test plan

- State machine tests: wrap on step and page keys, focus toggle,
  filter cycle, completion text per stage.
- `app.rs` tests: each prettified row shape, including the custom
  raw row and the `file_path` to `path` fallback.
- Parse tests: valid JSON, a JSON-encoded string value, and bad
  JSON falling back to raw text.
- Snapshot tests: the green focused border, the filter hint in the
  input bar, and highlighted pane lines.
- Gate: `cargo test -p tui` plus the PTY smoke script.

## Docs to touch when building

- `docs/tui-file-picker.md` P9: `Tab` no longer cycles the scope.
  The scope cycle key is `Ctrl+T`, with `Ctrl+I` where terminals
  report it distinctly.
- `docs/tui-command-palette.md` key table: add the new bindings.
- `docs/INDEX.md`: mark this doc Implemented on ship.
- `docs/tui_feature_requests_from_human.md`: flip the five open
  entries to shipped with notes.
