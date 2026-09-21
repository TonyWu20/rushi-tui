# TUI feature requests

Slim index. One line per request. Each item links its detail
doc. The checkbox tracks the request state: open, partial, or
shipped. Detail, root cause, spec, and shipped notes live in
the linked doc.

## Original requests

- [x] The `content` field in the `events.jsonl` must be shown in
      full. Never truncate or fold.
- [x] Use `Ctrl + u/d` for scrolling, like vim.
- [x] Mouse scroll support. Shipped: was laggy, uncontrollable,
      and hung the process. A double `q` could not exit.
- [x] Syntax highlighting for tool results. Markdown
      highlighting and rendering for `content`. Shipped: the
      `Read` body drives the shared `CodeHighlighter` from
      `bin/tui/src/highlight.rs` (the same engine the picker
      preview pane uses, `docs/tui-file-picker.md` section 9). The
      fix (2026-09-05) threads the tool call arguments into
      `read_body` so the file path — and therefore the language —
      resolves even though the log stores the path in the call
      argument (`file_path`), not the result value.

## New requests (2026-08-29)

- [x] Truncate `Read` and `Write` tool results. Show `Edit`
      results as a diff. Shipped: the `pi-tool-display` content
      and style port (2026-09-02 pass). Detail:
      `docs/tui-tool-result-truncation.md`.
- [x] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting. Shipped:
      the built-in palette dropped the single gray (commit
      f652b89); the reference renderers and the `Read`
      highlighting shipped in the 2026-09-02 pass. Detail:
      `docs/tui-color-tones.md`.
- [x] The statusline extension should draw a powerline footer.
      Detail: `docs/tui-statusline-powerline.md`.
- [x] Display and control the model's thinking (reasoning)
      block. Shipped: the capture into the log (commit 61cde02);
      the render, the toggles, and the effort control in the
      2026-09-02 pass. Detail: `docs/tui-thinking-block.md`.
- [x] Fix the transient `[malformed log line]` flash on live
      loops. Detail: `docs/tui-malformed-line-flash.md`.

## Pending corrections (2026-08-29)

- [x] Rescope the "never truncate" comment to user and assistant
      messages. Six spots, one per file. Shipped in the
      2026-09-02 pass. Detail:
      `docs/tui-tool-result-truncation.md` (section 4).

## New requests (2026-08-31)

- [x] Show user messages that wait for the busy loop. Render
      them like `pi`'s `steering` and `follow-ups`. Shipped:
      stage 1, the TUI steering block (commit fc51f71); stage 2,
      the loop-side split, in the 2026-09-02 pass. Detail:
      `docs/tui-pending-user-messages.md`.
- [x] Show thinking content behind a toggle, like `pi`.
      Shipped with the thinking block in the 2026-09-02 pass
      (`Ctrl+T` show/hide, `Ctrl+X` collapse/expand). Detail:
      `docs/tui-thinking-block.md`.

## New requests (2026-09-01)

- [x] Show the loop phase while the loop runs. Detail:
      `docs/tui-model-wait-indicator.md`.

## New requests (2026-09-02)

- [x] Full port of `pi-tool-display` for the tool result style:
      the lighter box, the fold/expand control. Shipped in the
      2026-09-02 pass. Detail: `docs/tui-tool-display-port.md`.
- [x] Render the markdown in user and assistant messages
      without the syntax markers. `|` tables draw as proper
      grid tables. Shipped in the 2026-09-02 pass. Detail:
      `docs/tui-markdown-render.md`.
- [x] The TUI colors accept a custom scheme. Use `catppuccin
macchiato` as the first internal color scheme. Shipped in
      the 2026-09-03 pass. Detail:
      `docs/tui-color-scheme.md`.
- [x] Truncate the tool result box overflow, never wrap it.
      Shipped in the 2026-09-02 pass: the cut marks the overflow
      with a trailing ellipsis. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Wrap the bash command text in the box on a narrow
      terminal. Shipped in the 2026-09-02 pass: the `$`
      command line word-wraps to the pane width; the output
      lines keep the truncation. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Draw the `|` tables inside the thinking block as a
      fixed-width grid, like the message tables. Shipped in the
      2026-09-02 pass (`wrap_thinking` in `render.rs`).
- [x] Keep the message table columns at a fixed width. Shipped in
      the 2026-09-02 pass: `table_grid` pads every cell to the
      column width.

## New requests (2026-09-03)

- [x] A scrolling bar at the right edge of the transcript
      pane. It shows the view position over the whole log.
      It appears when the user begins to scroll away from the
      tail, and it stays up in the browsing mode. Highest UX
      priority with the `gg`/`G` jump below: in the current
      TUI a scroll or `Ctrl+U/D` loses the position sense, and
      the walk back to the latest position is confusing and
      tiring. Shipped in the 2026-09-03 pass: the one-column
      bar shows on scroll-back and in browse mode, with the
      thumb, the tail marker, and the cursor marker. Detail:
      `docs/tui-conversation-browsing.md` (section 3).
- [x] A conversation browsing mode over the session log.
      Entry: a double `s`, under the same two conditions as
      the `q q` exit path: the input area is empty and the
      editor is in normal mode. `h j k l` move the cursor on
      the log, `Ctrl+U/D` move half a screen, `<count>j/k`
      move n lines, `<count>h/l` move n columns, `:N` (with
      the `j`/`k` suffix accepted) goes to line N, and
      `gg`/`G` go to the top and the end of the log. While
      active, the line-number gutter shows the absolute
      number at the cursor and relative numbers on the other
      lines. The detail behavior matches the neovim configured
      on this machine. Whether the mode takes a regex search
      on the log is discussed in the detail doc. The
      gutter-side question: left gutter, right bar (section
      4.3 of the detail doc). Highest priority: the bar plus
      `gg`/`G`. Shipped in the 2026-09-03 pass: the mode, the
      gutter, the counts, the `:N` goto, and the stage-2 regex
      search with the highlight and the `N` view restore. Detail:
      `docs/tui-conversation-browsing.md`.
- [x] Select-and-yank in the browse mode. The browse mode is
      the natural fit for vim `Visual` mode: select text on
      the rendered transcript and yank it to a register. The
      yanked text is pasteable into the draft (`p` in the
      editor) so the user can quote anything from the
      conversation to ask the agent about it. The `y`
      operator cooperates with the existing browse motions:
      `yw` (word), `y$` (line end), `yG` (last line),
      `<n>yy` (n lines), and the `i` / `a` text objects
      (inside double quotes, single quotes, parentheses,
      square brackets, braces, angle brackets). No new
      dependency: the `vim_editor.rs` motion and text-object
      primitives are reused. The register store is shared
      between the editor and the browse overlay. Shipped
      2026-09-11 (commit `5c82155`): `VisualSel` state,
      `yank_motion`, `yank_linewise`, `visual_yank_range`,
      `raw_yank_text` in `bin/tui/src/browse.rs`; shared
      `registers` on `App` (`bin/tui/src/app.rs`); OSC 52
      host-clipboard write. Detail:
      `docs/tui-conversation-browsing.md` (section 11).
- [x] A file picker on the `@` trigger. The user types `@` in
      the input box. A candidate file list opens. It re-ranks as
      the user types. The picked path inserts into the draft.
      Fuzzy search is on from day 0. The candidate window is a
      reusable completion widget, not a one-off. The display is the
      floating spawned window, chosen for the file content preview
      pane. The preview saves opening the target in a second tmux
      pane or shell session. The float adapts to the terminal width:
      the preview sits on the right in a wide terminal and on the
      bottom in a narrow one. Spec and layout decision committed
      (`docs/tui-file-picker.md`, commit 5b6ac59). Implemented
      2026-09-04: the day-0 scope ships in `bin/tui/src/picker/`
      plus the `@` trigger in `app.rs`. Detail:
      `docs/tui-file-picker.md`. Library research:
      `docs/tui-file-picker-research.md`.

## New requests (2026-09-04)

- [x] Syntax-highlight the picker preview pane and share the
      highlighter with tool-result rendering (follow-up flagged in
      `docs/tui-file-picker.md` section 9: "Code highlight is a later
      add"). Implemented 2026-09-04: `bin/tui/src/highlight.rs` now
      exposes a reusable `CodeHighlighter` plus `language_from_path`
      and a one-shot `highlight_text_lines` entry point. The picker
      preview pane (`picker/preview.rs`) and the `Read` tool-result
      body (`tool_display.rs` `read_body`) both drive it through the
      shared `Palette`/`Role` system. Unknown languages and binary
      files render plain. No new dependencies: a hand-rolled
      per-language tokenizer. Tree-sitter was evaluated and deferred:
      C-FFI grammar builds are disproportionate for a preview pane and
      an inline tool-result body. Revisit if highlight quality
      demands it.
- [x] Redesign session navigation. The `Tab` / `Shift+Tab`
      bindings were freed from unconditional session cycling so
      they can serve the file picker (path completion). The
      `CycleSessions` action and `cycle_target` helper remain in
      `app.rs` as the seam for a redesigned session navigator.
      The new design should: (1) not hijack keys the user expects
      for text editing or picker completion; (2) support listing
      all sessions, not just next/prev; (3) be reachable in one
      key press. The design now lives in the `:` command palette:
      `:b` opens a fuzzy session list, `:bn` / `:bp` cycle next
      and previous. Shipped with the `:` command palette in the
      2026-09-05 pass (commit 156c01e): `:b` opens the fuzzy
      session list, `:bn` / `:bp` cycle next and previous.
      Detail: `docs/tui-command-palette.md` (section 7);
      `bin/tui/src/app.rs` `Action::CycleSessions`.
- [x] A `:` command palette in normal mode. The user types `:`
      in normal mode. A floating two-pane window opens: the left
      pane lists commands and settings with fuzzy filtering, the
      right pane shows help text, option pickers, and session
      metadata. Built-in commands cover toggles, the effort
      setter, session buffers (`b`, `bn`, `bp`), `new-session`,
      `edit-queue`, open editor, and quit. Extension-provided
      commands join the list through a new `commands` cap and
      `invoke` op on the extension protocol. The window reuses
      the `picker/` fuzzy ranker and floating layout. Shipped in
      the 2026-09-05 pass (commit 156c01e): the two-pane float
      on the `picker/` ranker, the built-ins (`toggle-tools`,
      `toggle-thinking`, `expand-thinking`, `thinking-level`,
      `b`/`bn`/`bp`, `new-session`, `edit-queue`, `e`, `q`),
      and the extension `commands` cap with the `invoke` op
      (`ext.rs` `command_items`). Detail:
      `docs/tui-command-palette.md`.
- [x] Recall and edit pending user messages. `Alt + Up` pulls
      every pending message into the editor in one shot. The
      user edits the combined text and sends it. The log stays
      append-only: a new `user_message_retract` event cancels the
      originals, and the loop skips retracted ids. `:edit-queue`
      in the `:` palette is the deliberate entry point for the
      same flow. Shipped in the 2026-09-05 pass (commit
      156c01e): `Alt+Up` and `:edit-queue` both run
      `Action::RecallQueue`, which pulls the pending queue into
      the editor and emits the `user_message_retract` events;
      `pending_user_messages` filters the retracted ids so the
      loop skips them. Detail:
      `docs/user-message-editing.md`.

## New requests (2026-09-06)

- [x] Accept `ctrl+z` the standard keybinding that send our tui to
      background jobs in the shell. Shipped: `Ctrl+Z` sends SIGTSTP,
      the shell backgrounds the TUI; `fg` resumes it. The terminal is
      restored before suspend and re-initialised on resume.
- [x] The current markdown table rendering of the messages cannot correctly
      distinguish if `|` is used as the table column marker or written as part of the
      text or code, e.g. the closure syntax in Rust `.map(|e| ...)`/`.unwrap_or(|e| ...)`.
      Shipped: `is_table_block_start` (`bin/tui/src/highlight.rs`) now requires the
      `|---|` separator row below a `|`-prefixed line before treating it as a table;
      a `|` line with no separator (a closure in code or prose) renders as plain
      text. Pinned by the `snap_table_pipe_not_a_table` snapshot. Detail:
      `docs/tui-ratatui-ecosystem-audit.md` section 4.1, bug 1.
- [x] Stream rendering of the model response. Shipped 2026-09-13: the
      `harness` loop owns `sessions/<n>/.model-stream`, the `model` binary writes one
      JSON line per SSE delta, and the TUI polls the file each frame to render a live
      block that settles into the transcript on the `assistant_message` event.
      See `docs/tui-streaming-response.md` and `lean/TuiStreamSpec.lean`.
      2026-09-06 audit: the TUI and harness consumer paths checked out; the single
      defect was producer-side — `bin/model` buffered the whole SSE body before
      writing any channel line, so the live block showed nothing while the response
      was in flight. Fixed with the incremental `SseParser` in
      `bin/model/src/main.rs`; in-flight growth is proven by
      `bin/model/tests/stream_channel.rs` and re-verified against the live endpoint.
      Lean spec clean (`scripts/lean-gate.sh`, zero `sorry`).
- [x] The thinking→text transition flickered: when the response text
      began, the thinking block collapsed from its full grown height to a
      fixed 2-line summary, so the block shrunk ~16 rows and the view
      jumped. Shipped 2026-09-13: the thinking tail and response text now
      share one sliding window (`stream_block_lines`, `bin/tui/src/render.rs`)
      — the oldest thinking rows slide out as text grows instead of a sudden
      collapse, so the block height never shrinks at the transition.
      Tests: `stream_block_thinking_and_text_share_the_window`,
      `stream_block_height_never_shrinks_when_text_starts`.
- [x] Buffer the response text and render it at a smooth, steady frame rate
      instead of jumping in whole-delta bursts. Shipped 2026-09-13: the TUI
      now queues arriving deltas into a FIFO pace queue and releases
      `max(1, backlog/15)` characters per frame (`App::pump_stream_pacing`,
      `bin/tui/src/app.rs`); the main loop ticks at ~60 FPS (16 ms poll)
      while a response is in flight and falls back to 100 ms when idle.
      `done` drains the queue in one shot so the final text settles promptly.
      Tests: `stream_pacing_*`, `clear_stream_drops_the_pace_queue`.
      Note: both changes are binary-side; the running TUI must be restarted
      (rebuild `target/release/tui`) to pick them up.
- [x] Remove `assistant`, `tool:xxx` markers. Remove the indent of assistant and
      user messages. Wrap the user message with the same color background of tool
      results, and
      a `Block::bordered().border_type(BorderType::Rounded).title("User")` block.
      Shipped in the 2026-09-14 pass. The `user_message` body now renders in a
      rounded bordered panel (`render.rs` `user_box_rows`). The panel is titled
      `User` and sits on the tool-result panel background. It uses
      `tool_display::box_bg`, the `ToolBoxBgSuccess` role. The panel spans
      the transcript width. The border and title read in the accent tone.
      The `user` marker and the 12-column content gutter are gone. The panel
      interior and the assistant body start at the left edge. The
      `assistant` marker is dropped too. The optional `(n tool calls)` count
      still notes the actions that follow. The live stream block header drops
      the `assistant` marker as well. The `tool:` prefix was already gone with
      the 2026-09-14 tool-name pass. It now shows the bare purple
      `tool_name` role.
      Tests: `user_box_tests` (`user_box_has_title_and_background`,
      `empty_user_box_is_three_rows`,
      `assistant_message_has_no_marker_or_gutter`). The layout snapshots were
      regenerated.
- [x] When in browse mode, updates from model response should not flush the
      screen to the latest position of the conversation.
      Shipped: `Browse::sync` pins the view on any model-driven tail
      change, stream growth and the settle shrink alike. The cursorline
      holds through the whole streaming lifecycle. Detail:
      `bin/tui/src/browse.rs` `sync`, tests `stream_pin_tests`.
- [x] The `@` picker respects `.gitignore` by default, but sometimes the
      human needs to point at ignored files or directories (e.g. a
      specific session in `@sessions`). Shipped 2026-09-06: `Ctrl+I`
      in the picker cycles the file scope — default (hidden and
      git-ignored excluded) → show git-ignored → also show hidden →
      back to default on the third press. `FileScope` in
      `bin/tui/src/picker/items.rs`, the state cycle in
      `bin/tui/src/picker/state.rs` (P9). In a standard terminal
      `Ctrl+I` is the same byte as `Tab` (0x09), so the picker binds
      `Tab` to the cycle too. Detail: `docs/tui-file-picker.md`.
      Updated 2026-09-15: `Tab` was reassigned to completion; the
      scope cycle is now `Ctrl+T`, with `Ctrl+I` kept for terminals
      that report it distinctly.
- [x] When a file path in the `@` picker result list is too long to
      fit the column, trim the leading parent directory levels and
      replace them with `...` so the tail of the path stays visible
      (e.g. `.../a/b/src/app.rs`), with the preview pane on or off.
      Shipped 2026-09-06: `abbreviate_path` in
      `bin/tui/src/picker/render.rs` — the list column keeps the
      largest suffix of the path that fits the column budget, and
      the budget follows the column width, which differs with the
      preview pane on or off (P10 in `docs/tui-file-picker.md`).

## New requests (2026-09-07)

- [x] Bug: `tool:edit` results always show `diff +0 -0`. Evidence session:
      `sessions/goal-ux-impl`. Fixed: the diff stat row now computes
      `added`/`removed` from the positional diff of the `before` and
      `after` line lists in `edit_body` (`bin/tui/src/tool_display.rs`),
      so a changed file shows its real line counts instead of `+0 -0`.
- [x] `tool:edit` shows diff in vertical split when terminal is wide, horizontal
      split when terminal is narrow. Shipped: `DiffView::Auto` picks
      `Split` (two-area, side-by-side) when the pane width is at least
      `2 * DIFF_SPLIT_MIN_WIDTH` (60 cols) and falls back to `Unified`
      (stacked) below that. The `DIFF_SPLIT_MIN_WIDTH` constant and
      `diff_layout` live in `bin/tui/src/tool_display.rs`.
- [ ] Bug: the input box does not highlight the whole visual
      selection. In `VISUAL` / `V-LINE` the draft shows only the
      inverted block on the cursor cell; the chars between the
      anchor and the cursor are not shaded, so the selection is not
      visible as a block. Fix: expose the visual range from
      `VimEditor` (`visual_range` is private today), shade the
      selected span with a background in the editor-box render
      (char-visual spans its chars across wrapped display rows;
      line-visual shades whole display rows), and add a test. Not
      shipped.

## New requests (2026-09-14)

- [x] Show the tool name of a tool-result panel in a purple, bold
      font, and drop the white border lines around the panel; the
      `tool:<name>` title prefix is omitted (the name stands alone).
      Shipped in the 2026-09-14 pass: the panel is the lighter
      background only (no `┌─┐│└─┘` border runs); its header row
      names the tool in the new purple `tool_name` color role,
      bold, followed by the result status. Detail:
      `docs/tui-tool-display-port.md` (section 4, the box, and P1).
- [x] Follow-up (2026-09-14): add one background-filled margin row
      above the panel header and one below the last body row. The
      result panel reads as a lighter band with breathing room. The
      body height budget is `height-1`. The two freed border rows
      become the top and bottom margins. Shipped: `box_rows` emits a
      top margin row, the header row, the body rows, and a bottom
      margin row.
- [x] Follow-up (2026-09-14): no `tool:` prefix on external tool
      calls. The raw `tool:<name> {json}` call line is replaced.
      Shipped: the `tool_call` line shows the bare name in the
      purple `tool_name` role. The goal tools get compact labels
      (the `summary` or `reason` argument). They merge into the
      result panel when the result follows. The raw JSON call line
      no longer appears.
- [x] Follow-up (2026-09-14): the tool-result panel spans the full
      transcript width. Shipped: the transcript now uses the full
      layout-cell width. The panel reaches the right edge. A stale
      two-column reservation from the bordered-box era left a dead
      column on the right. The position bar still owns its own
      rightmost column when shown.
- [x] Follow-up (2026-09-14): the tool-result panel header shows what
      the tool acted on, after the tool name. The `read` result names
      the file path it read. A goal result shows its summary or
      reason. The redundant `ok` status word is dropped. The panel
      background color already signals the outcome. Shipped: the
      `ToolResult` pass passes the compact label to `box_rows` after
      the name. `result_status` returns empty on success. A new
      snapshot `snap_tool_result_read_shows_path_in_header` pins the
      path in the header.
- [x] Follow-up (2026-09-14): compact tool labels are scoped to the
       kernel's own tools. `compact_tool_label` is gone for extension
       tools: the `goal_complete`/`goal_blocked` labels (from
       `rushi-exts/goal-app`, not the kernel) and the `list`/`find`
       arms are removed, and the orphaned `search`/`list` display stack
       went with them (`SearchMode`, `ToolDisplay.search_mode`, the
       `search` `[tui.tool_display]` key; `list` was the only caller)
       along with the `list` arm in the JSON-detection `known` match.
       Call lines now carry the tool name only: no `tool:` prefix and
       no raw args JSON (the full JSON stays the yank source). The
       `read` result panel header keeps the file it read as a dim
       label after the name, the one argument shape the kernel owns.
       Snapshot `snap_tool_result_read_shows_path_in_header` pins the
       path in the header; the `snap_pending_approval_banner` call line
       is now the bare `bash` name; the 31 layout snapshots were
       regenerated against the new top-only session frame.
- [x] Simplify the live stream: stream the model response into the
      main content area instead of a pinned block that grows and
      collapses. `Ctrl+T` should collapse and expand all thinking
      blocks, including the live-streaming one. Shipped 2026-09-14:
      the in-progress model response now renders inline in the
      transcript tail instead of a pinned layout cell between the
      transcript and the working row; no dedicated layout cell is
      allocated, so the layout no longer shifts on start/complete.
      The `Ctrl+T` toggle covers both settled and live-streaming
      thinking blocks. Detail: `docs/tui-streaming-simplify.md`
      (section 3).

## New requests (2026-09-12)

- [x] Restore markdown rendering in the thinking block. After the
      `ratatui-markdown` migration the body used plain word-wrap.
      Now `wrap_thinking` runs prose lines through `md_line`. The
      tree-sitter fence and table-grid paths are unchanged.

- [x] Add a `ThinkingTag` color role for the `thinking` label.
      Expanded: `#c6a0f6`. Folded: `#8087a2`.
      `Palette::thinking_tag(expanded)` picks the right one.

- [x] Recolor the `Thinking` role to `#8087a2`. Updated the
      `Level::thinking` fallback, the macchiato scheme, and the
      color alignment doc.

- [ ] Observation (2026-09-14): the text inside a markdown table
      renders as raw text, with no syntax highlighting. The grid
      itself (the fixed-width `table_grid` pass) draws, but each
      cell's content passes through unprocessed — `table_cells`
      (`bin/tui/src/highlight.rs`) pipe-splits the row and
      `wrap_cell_text` word-wraps the raw string, so a cell never
      goes through `md_line` (inline markdown) or the code
      highlighter the way the surrounding prose does. Reported by
      the user while reading a rendered reply; unrelated to the
      streaming-perf work tracked elsewhere. Open.

- [x] Observation (2026-09-14): the picker preview pane truncates
      file content to 50 lines (`FilePreviewer::new(50)` in
      `bin/tui/src/render.rs`). Shipped 2026-09-15: the 50-line cap
      is gone. `FilePreviewer::new` now takes a shared
      `Arc<Mutex<WindowCache>>` and renders only the visible window
      (`bin/tui/src/picker/preview.rs`, commit `7fc8a54` "tui:
      windowed, cancellable, size-guarded picker preview pane"); a
      layer-1 byte cap guards against huge files, and the background
      load is cancellable. Detail: `docs/tui-preview-pane-plan.md`.

## New requests (2026-09-13)

- [x] Preview-pane text in the picker and palette floats was
      truncated at the pane width instead of wrapping.
      Fixed: `wrap_hard_lines` (in `render.rs`) pre-wraps each
      hard line to the pane's inner width, so content reflows on
      terminal resize. Detail:
      `docs/tui-ratatui-ecosystem-audit.md` (§4.8).

- [x] Also fixed a latent off-by-2 in the picker preview pane
      where the two border rows were not subtracted from the
      visible height. Detail:
      `docs/tui-ratatui-ecosystem-audit.md` (§4.8).

- [x] Fix browse-mode search typing. While the search line is
      open, typing `s` armed the double-`s` exit instead of
      entering the query. The exit arm is now suppressed while
      the line is typing.

- [x] Pin the browse view on streaming growth. The live tail
      growth flushed the viewport and re-centered it. The stream
      growth now pins the view like a settled event. Shipped
      2026-09-13: the settle shrink pins too, so the cursorline
      holds through the whole streaming lifecycle. Tests:
      `stream_pin_tests` in `bin/tui/src/browse.rs`.
- [x] No cap on the number of replayed events. The session start
      must stay reachable in browse mode and the `tree` list.
      Shipped 2026-09-13: the `TRANSCRIPT_EVENT_CAP`,
      `EVENTS_CAP`, `MAX_LOG_READ_BYTES`, and `SCROLL_CAP` limits
      are gone. `read_events` replays the whole log and the
      transcript renders every in-memory event. Detail:
      `docs/tui-conversation-browsing.md` section 4.6.

- [x] Yank returns the raw source (message bodies and thinking
      blocks) instead of the rendered text.
      Superseded: the 2026-09-13 entry recorded "not implemented,
      user conceded to rendered-text yank", but the line-level raw
      source map (`line_raw`, `bin/tui/src/app.rs`) and the
      browse-mode raw yank path (`browse.rs` `complete_yank` /
      `raw_yank_text`) landed in commit `5c82155` + `0e1fa5f`
      (2026-09-11), so the feature is in the tree. Shipped: the
      transcript cache carries a per-line raw source map
      (`line_raw`, `bin/tui/src/app.rs`), and the browse-mode yank
      (`browse.rs` `complete_yank` / `raw_yank_text`) prefers the
      raw source over the rendered text, falling back to the
      rendered text when no raw mapping is available. The register
      store is shared with the editor. Detail:
      `docs/tui-conversation-browsing.md` (section 11.3).

## New requests (2026-09-15)

- [x] The browse cursor does not paint at the start of a word.
      For a hyphenated word it paints on the hyphen.
      Shipped 2026-09-15 (two parts):
      1. Hyphen word treatment: the browse word motion now runs
         under a hyphen-folding word class (`WordClass::Browse`).
         A hyphenated word like `foo-bar` is one word. `w` / `b`
         / `e` land on a word start or word end, never on the
         hyphen. The editor keeps the plain vim class
         (`WordClass::Editor`). Detail: `WordClass` in
         `bin/tui/src/vim_editor.rs`; the motion in
         `bin/tui/src/browse.rs`. Tests: `word_motion_tests`,
         `word_class_tests`. (5dcef90 only shipped this part.)
      2. Caret painting: the caret cell was dropped whenever the
         cursor col sat exactly at a span boundary — col 0 or a
         word start whose first char begins its own span (the
         usual case for `w` / `b` targets) — because
         `caret_spans` treated "rest == 0" as "caret already
         drawn" instead of "caret belongs on this span's first
         char". Fixed 2026-09-15: `caret_spans`
         (`bin/tui/src/render.rs`) now places the caret on the
         first character of a span when the cursor sits on a span
         boundary, and paints the block on a space cell when the
         cursor sits one past the last character (line end).
         `fg_override` (search/match highlighting) also applies
         to the end-of-line block now. Tests: `caret_span_tests`
         in `bin/tui/src/render.rs` (col 0, span boundary,
         mid-span regression, line end, empty span).
- [x] The vim `e` (end of word) motion is not registered while
      in browse mode. Shipped 2026-09-15: `e` is now in the
      browse key table (`char_key`). It is also in the `y`
      operator list and `browse_motion_range` (`ye`). It uses
      the browse word class. The status hint gained `ye end`.
      Detail: `bin/tui/src/browse.rs`. Tests:
      `e_lands_on_the_word_end`, `ye_yanks_the_whole_hyphenated_word`.
- [x] Allow entering browse mode with a non-empty draft. Today
      the double-`s` gate needs an empty input. Loosen it.
      Shipped 2026-09-15: `browse_gate_open`
      (`bin/tui/src/app.rs`) dropped the empty-draft condition.
      The double-`s` now works in normal mode with a held
      draft. The quit gate (`q q`) still needs an empty draft.
      Tests: `browse_gate_tests` (`bin/tui/src/app.rs`).
- [x] Float lists wrap at both ends.
      `Ctrl+J`/`Ctrl+K` and arrows move the cursor.
      `Ctrl+Shift+P` toggles list/preview focus, and `Ctrl+U/D`
      scrolls the focused pane.
      The focused preview border is green. Detail:
      `docs/tree-ui-design-from-human-phase-2.md`.
      Shipped 2026-09-15: the `Focus` enum lives in
      `bin/tui/src/float.rs`. The wrap and focus logic lives in
      `bin/tui/src/palette/state.rs` and
      `bin/tui/src/picker/state.rs`. `BackTab` is the legacy
      fallback for the focus toggle. Tests: `move_down_wraps_from_
      last_to_first`, `focus_toggle_keys_reach_the_state_machine`,
      `the_focused_preview_pane_border_is_green`.
- [x] `Tab` completes the highlighted fuzzy item in every float
      window: file picker, session list, tree event list, command
      palette. The picker scope cycle moves to `Ctrl+T`;
      `Ctrl+I` stays bound where terminals report it distinctly.
      Detail: `docs/tree-ui-design-from-human-phase-2.md`.
      Shipped 2026-09-15: `PickAction::Complete` and
      `PaletteAction::Complete` carry the pick to `app.rs`, which
      inserts the text and keeps the window open. `TreeOptions`
      is a no-op. `Ctrl+T` cycles the scope, `Ctrl+I` stays.
      Tests: `tab_completes_the_highlighted_item`,
      `ctrl_t_cycles_the_scope`, `ctrl_i_still_cycles_the_scope`,
      `tab_is_a_noop_in_tree_options`.
- [x] `Ctrl+F` cycles the tree event type filter: full, user,
      assistant, tool, user+assistant. Detail:
      `docs/tree-ui-design-from-human-phase-2.md`.
      Shipped 2026-09-15: `TreeFilter` (five-state cycle) lives on
      `PaletteState` and the tree stage owns it. It resets on stage
      exit and close. The active value shows in the input-bar hint
      as `[f: <label>]`. The session sub-list is unaffected.
      Tests: `filter_cycles_through_the_five_states`,
      `filter_key_binds_only_in_the_tree_stage`,
      `filter_resets_on_stage_exit_and_close`,
      `the_event_type_filter_narrows_the_candidates`.
- [x] Prettify tree tool rows. `bash` shows the command, and
      `read`/`edit`/`write` show the `file_path`. Result rows show
      name, status, and the first result line. Custom tools stay
      raw. Detail: `docs/tree-ui-design-from-human-phase-2.md`.
      Shipped 2026-09-15: `tree_row_label`, `tree_event_tag`, and
      `tree_event_preview` in `bin/tui/src/app.rs`. Built-ins drop
      the brackets and the `tool:` prefix. `file_path` falls back
      to `path`. Result names resolve through `App::call_names`.
      Custom tools keep the raw row. Tests: `tree_prettify_tests`.
- [x] The tree preview pane parses tool JSON with `jaq-core` and
      highlights messages and JSON unconditionally (tree-sitter
      engine). The pane scrolls fully. Detail:
      `docs/tree-ui-design-from-human-phase-2.md`,
      `docs/tui-preview-pane-plan.md`.
      Shipped 2026-09-15: `bin/tui/src/palette/preview.rs` runs
      the jaq parse, two-space pretty-print, and `tui-highlight`
      pipeline. A parse failure falls back to the raw text. The
      body cache is `TreePreviewCache` (LRU). The 600-char cap is
      dropped; the pane scrolls. Tests: `palette::preview::tests`
      (parse, fallback, cache) and `snap_tree_event_pane_lines`.
      Refined 2026-09-17: tool events no longer re-serialize JSON
      (the `\n`-escaped-text complaint). The pane now renders them
      through the transcript's tool display: results via
      `tool_display.rs::body_rows`, calls via the new
      `tool_display.rs::call_args_rows` (readable argument
      listing, multi-line values as indented blocks). Custom
      results without a `text` field fall back to the pretty-JSON
      pipeline. `PaletteItem` gained `tool_payload` plus
      `PreviewKind::Tool`; the cache key gained the pane width.
      Detail: `docs/tree-ui-design-from-human-phase-2.md`
      (item 2 refinement). Tests: `a_bash_result_renders_real_
      lines_not_escaped_json`, `a_tool_call_renders_its_
      arguments_as_real_lines`,
      `a_custom_result_without_text_falls_back_to_pretty_json`,
      `the_tool_cache_keys_by_event_seq_and_width`, plus the
      updated `snap_tree_event_pane_lines`.
      Also: pure-thinking assistant events (empty content, a
      `reasoning` array with text) now show the thinking block in
      the preview pane instead of an empty body — `tree_event_body`
      falls back to `render::thinking_text`. The tree row itself
      shows a `thinking: <first line>` one-liner under the
      `<assistant>` tag (`tree_event_preview`); content-bearing
      rows are unchanged. Tests:
      `a_pure_thinking_assistant_event_shows_its_thinking_block`,
      `a_pure_thinking_assistant_row_shows_a_thinking_one_liner`,
      `an_assistant_row_with_content_keeps_the_content_preview`.

## New requests (2026-09-16)

- [x] The `wait` loop-phase state covers two sub-phases of the
      model call, and the TUI shows `waiting for model · Ns` for
      both: the sent request pending on the server (no response
      data yet), and the response streaming back (deltas arriving
      through the session's `.model-stream`). Add one more loop-
      phase status, `working`, beyond [`idle`, `tools`, `wait`],
      to distinguish the two cases: `wait` is the request pending,
      `working` is the response streaming. Proposal: `working` as
      the new status. Decision 2026-09-16: the marker source is a
      TUI-side rendering rule, no kernel change. Per the
      refinement policy (`docs/refinement-policy.md`, P1a
      condition 3 and the P4 pre-test) a TUI-only effect gets a
      rendering rule instead of a protocol change. While the loop
      phase is `wait`, the TUI shows `working` when the session's
      `.model-stream` is non-empty (deltas have arrived) and keeps
      `wait` while it is empty (the request is still pending). The
      kernel emits no `working` marker. The label `working` is
      kept as proposed: the `running-unknown` fallback is an
      internal state name (`PhaseState::RunningUnknown`, `bin/tui/
      src/render.rs`), shown as `[running]` / `Working...`, and the
      user has never seen it surface (2026-09-16). Detail:
      `docs/tui-working-status.md` (the open-request design),
      `docs/tui-model-wait-indicator.md` (the base contract),
      `docs/tui-streaming-response.md`.
      Shipped 2026-09-16: `PhaseState::Working` plus the `working`
      state-table row are TUI-derived in `bin/tui/src/render.rs`
      (`phase_state` reads the last `loop_phase` value and the
      session stream buffer; `[working]` bit; `model working · Ns`
      row). No kernel change. Tests: `working_status_tests` in
      `bin/tui/src/render.rs`.

## New feedback (2026-09-16)

- [x] Color the tree row tags by class: user, assistant, and
      tool rows each get a distinct fg color.
      Shipped 2026-09-16: `PaletteItem.tag_fg` carries the role;
      user/retract rows take the `Accent` tone, assistant rows the
      `Report` tone, tool rows the `ToolName` tone (the same
      identity colors as the transcript panels). The cursor row
      keeps its accent highlight. Detail:
      `docs/tree-ui-design-from-human-phase-2.md` item 1.
      Tests: `the_row_tag_fg_classifies_user_assistant_and_tool`,
      `tree_row_tags_carry_the_class_fg_color`.
- [x] `Ctrl+Shift+P` must actually toggle list/preview focus; on a
      legacy terminal it arrived as plain `Ctrl+P` and toggled the
      preview pane instead.
      Shipped 2026-09-16: `main.rs` now enables the kitty keyboard
      protocol (`PushKeyboardEnhancementFlags` with
      `DISAMBIGUATE_ESCAPE_CODES`, popped on exit) so capable
      terminals report the shift modifier, and `key_input` accepts
      both the legacy lowercase and the protocol-form uppercase
      codepoint. `BackTab` remains the legacy fallback.
      Detail: `docs/tree-ui-design-from-human-phase-2.md` item 4.
      Tests: `key_input_tests` (protocol-form cases) and the PTY
      case `csi_u_ctrl_shift_p_toggles_preview_focus`.
- [x] In the command palette, `Tab` should replace the typed
      query with the highlighted item, like the file picker does,
      not append to it.
      Shipped 2026-09-16: `PaletteState::apply_complete` replaces
      the typed filter portion (after the goto prefix in a
      sub-stage) with the item's text; the root stage replaces the
      whole query. Detail:
      `docs/tree-ui-design-from-human-phase-2.md` item 5.
      Tests: `tab_replaces_the_typed_query_in_the_root_stage`,
      `tab_replaces_the_filter_after_the_goto_prefix`,
      `tab_replaces_the_tree_filter_with_the_row_label`.

## New requests (2026-09-17)

- [ ] A cross-session status dashboard: see `working` / `idle` /
      `blocked` for all rushi sessions at a glance, across tmux panes
      and projects. Open discussion of placement (TUI tab row vs
      standalone monitor binary vs UI extension) and the blocked
      detection rule. Detail: `docs/tui-session-monitor.md`.

## New requests (2026-09-19)

- [x] Add vim's `ge` motion (backward to the end of the previous
      word) to both the input box (vim editor) and browse mode,
      mirroring neovim's `ge`. In browse it is entered as `g` then
      `e` (the `g` prefix is shared with `gg`), and a `yge` yank form
      yanks through the previous word's end.
      Shipped 2026-09-19: a shared `word_end_backward` primitive
      (plus a `WORD_end_backward` variant for `gE`) in
      `bin/tui/src/vim_editor.rs`; `ge` / `gE` wired into the
      editor's normal-mode and visual-mode `g`-prefix blocks and the
      operator-motion `g` block. Browse reuses the same primitive
      under `WordClass::Browse` (the hyphen joins a word) through
      `browse_ge_range` in `bin/tui/src/browse.rs`; plain `ge` is
      wired into the browse key table and `yge` into the `y`
      operator. The operator range is inclusive, like `e`. Detail:
      `docs/vim-editor-design.md` (editor `ge` / `gE`),
      `docs/tui-conversation-browsing.md` (browse `ge` / `yge`, the
      section 4.4 key table and the section 11.4 yank table).
      Tests: `word_class_tests` in `vim_editor.rs`
      (`ge_lands_on_the_end_of_the_previous_word`,
      `counted_ge_steps_word_by_word`, `ge_crosses_lines_backwards`,
      `ge_stops_at_a_blank_line_boundary`,
      `ge_word_classes_split_the_hyphen_differently`,
      `ge_uppercase_lands_on_the_end_of_the_previous_word`) and
      `word_motion_tests` in `browse.rs`
      (`ge_lands_on_the_end_of_the_previous_word`,
      `ge_folds_the_hyphenated_word_in_browse`,
      `counted_ge_steps_word_by_word_in_browse`,
      `stale_g_prefix_cancels_before_plain_e`,
      `ge_crosses_lines_in_browse`,
      `yge_yanks_through_the_previous_word_end`).

## New requests (2026-09-20)

- [x] The fold-view tally (the `⎿` summary line between the user
      message and the final assistant message) does not show the
      number of compact events triggered in the turn. Since compact
      is triggered by the hook mechanism, add the hook trigger times
      and hook names to the tally.
      Decision 2026-09-20 (the user's overflow choice): when the
      tally overflows the terminal width, truncate all hook results
      into a single field, `hook ×<N times>`. The alternative,
      wrapping to a second row, was rejected: the current TUI does
      not support a widget occupying more than one row.
      Shipped 2026-09-20: `tally_parts` in `bin/tui/src/fold.rs`
      counts `compaction_started` events (shown as `compact ×N`) and
      histograms the `hook_applied` `ext_status` markers by the
      hook command basename (shown as `name ×N`), joined between
      the tool histogram and the message count. `tally_text_fit`
      collapses the compact and hook fields into one `hook ×<total>`
      field when the full text exceeds the column budget; the
      collapsed-turn row (`FoldState::collapsed_summary` with a
      width budget) and the live working-row tally
      (`App::live_fold_tally`) both fit to their row width. Detail:
      `docs/tui-turn-fold.md` (the "Tally line" section, updated).
      Tests: `tally_counts_compact_events`,
      `tally_shows_hook_names_and_counts`,
      `tally_shows_compact_with_no_tools`,
      `tally_fit_keeps_hook_names_when_they_fit`,
      `tally_fit_collapses_to_single_hook_field_when_over_budget`,
      `collapsed_summary_collapse_to_hook_total_when_over_budget`
      in `bin/tui/src/fold.rs`.
- [x] The input box's `r` (replace one char) motion should get its
      own mode: only `R` transit the input-box title to
      `[REPLACE]`, while `r` still showed `[NORMAL]`; a more
      suitable display is `[r-PENDING]`, like `[d-PENDING]` for a
      pending operator. Related conflict: while `r` was pending, the
      replacement char typed next (e.g. `s`) was caught by the
      double-`s` browse-mode hint instead of completing the
      replace. Shipped 2026-09-20: a new `Mode::ReplaceChar` state
      (`bin/tui/src/vim_editor.rs`), entered when `r` is pressed in
      normal mode; the box title shows `[r-PENDING]` through
      `editor_mode_label`. Because the state is no longer
      `Mode::Normal`, `browse_gate_open` (the double-`s` gate in
      `app.rs`) stays closed while the replacement is pending, so
      `s` (or any other char) reaches the editor and completes the
      `r` replace instead of arming the browse hint. A non-printable
      (Esc, Ctrl-C, ...) cancels back to normal. Detail:
      `docs/vim-editor-design.md` (the `ReplaceChar` mode).
      Tests: `replace_char_tests` in `vim_editor.rs` and
      `s_after_r_completes_the_replace_not_the_browse_gate` in
      `app.rs` (`browse_gate_tests`).
- [x] The PR#16 fold tally's hook-trigger counters are inflated by
      no-op transforms, making them read as "model calls" rather
      than meaningful hook activity. Investigation (2026-09-21):
      the tally counts `hook_applied` `ext_status` markers
      (`bin/tui/src/fold.rs`). The kernel writes one per hook that
      *returned* `transform` at `model.before`
      (`bin/rushi/src/step/model.rs` in rust-unix-harness, issue
      #19 fix); true no-op responses (`{}` → no decision,
      `crates/rushi/src/hooks.rs` `fire_one`) already produce no
      markers. The inflation comes from the two installed
      extensions' deliberate "always trigger" design:
      `harness-hook-goal-arm` must re-emit the byte-stable
      idempotent transform on every model call while a goal is
      open (P17 keeps the provider prefix cache warm — the
      tool filter must keep running), and
      `harness-hook-simple-english` unconditionally emits a
      transform with its static rule-summary fragment. Both
      produce a `hook_applied` marker every call even when the
      applied request is byte-identical to the original.
      Follow-up (same day, user observation): even in a session
      that never used goal mode, goal-arm marked *every* step
      (`sessions/tree-compact-revise`: 248 markers, no goal
      state files). That case is NOT a `{}` no-op: this
      `config.toml` registers `goal-tools/` under
      `extension_tool_paths`, so the goal tool schemas are in the
      base request of every model call, and the D1 filter strips
      them every call — a genuine per-call transform. A
      byte-equality kernel filter (option c below) therefore
      does NOT silence this case; only the goal-open
      steady-state case. To silence goal-less sessions the goal
      tools would have to leave the always-on tool manifest and
      be advertised by the hook only while a goal is open (the
      inverse D1 filter). That needs kernel support, because the
      `route` stage resolves tool calls via the same manifest
      paths. Simple-english is different: its fragment is the
      extension's core mechanism (the model must see the rules
      every turn), so its ×N is by design and cannot be made
      lazy. Options considered:
      (a) extensions stop the lazy mechanism — rejected: the
      goal hook's always-transform is required by the protocol,
      and pushing diff-logic into every extension author is
      fragile; (b) TUI filters no-ops — impossible: the log
      carries no original-vs-transformed request, only the kernel
      holds both at apply time; (c) kernel records `hook_applied`
      only when the applied request actually differs from the
      original — one cheap semantic `serde_json::Value ==`
      compare in `apply_model_before_transform` (serde_json
      without `preserve_order` → BTreeMap, allocation-free,
      key-order-insensitive, matching wire bytes). (c) is cheap
      but only fixes the goal-open steady-state case; the
      goal-less-session case needs goal tools to come off the
      always-on manifest (hook-advertised instead), which is a
      kernel + goal-app change. Decision pending: whether to
      implement the kernel-side no-op filter, and whether to also
      skip the `hook.model.before` decision marker on no-op
      transforms, not just `hook_applied`. Also pending: accept
      "requests the hook mutated" as the counter's documented
      semantics, or pursue the hook-advertised-goal-tools design.
      Note a pre-existing separate wart
      (out of scope): with multiple transforming hooks on
      `model.before`, `fold_decision` applies only the first
      hook's payload while issue #19's loop marks every hook
      that returned `transform`.
      Decision 2026-09-21 (user): bear with the current counters
      for the moment. Accept the "requests the hook mutated"
      semantics as-is; no kernel, extension, or TUI change now.
      The option list above stays on file if the user wants to
      revisit later.
      Note (same day, follow-up question): `harness-hook-no-find-grep`
      (a `tool.before` gate hook) is invisible to the tally by
      design. It never returns `transform`, so the kernel never
      writes a `hook_applied` marker for it. Its `block` decisions
      are also not logged as markers at all: the tool step calls
      `log_hook_window` with an empty decision
      (`bin/rushi/src/step/tool.rs` line 75 in
      rust-unix-harness), so no `hook.tool.before` decision
      marker exists. The only trace of a block in the log is the
      synthesized failed `tool_result` carrying the reason
      (e.g. "BARE grep DETECTED..."). If the tally should ever
      cover gate blocks, the kernel must first record per-hook
      tool.before decisions with the hook command name, and the
      tally would then read that marker.
      Filing done 2026-09-21: option (c) is kernel issue
      `TonyWu20/rushi#24` (label `enhancement`, "model.before: skip
      `hook_applied` markers when the transform did not change the
      request"). The two parked follow-ups (goal tools off the
      manifest; per-hook `tool.before` decision markers) are listed
      in that issue's "Out of scope" section. They get their own
      issues only when picked up.
