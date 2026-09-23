# TUI feature requests

Slim index. One line per request. Each item links its detail
doc. The checkbox tracks the request state: open, partial, or
shipped. Detail, root cause, spec, and shipped notes live in
the linked doc.

From 2026-09-03 on, the entries that outgrew one line keep their
detail in the dated docs `docs/tui-feature-requests-<date>.md`
(one section per entry, slugged); the index line stays one
request plus the pointer.

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
      pane, showing the view position over the whole log.
      Detail:
      `docs/tui-feature-requests/2026-09-03.md` (scroll-bar).

- [x] A conversation browsing mode over the session log
      (double-`s` entry, `hjkl` + counts + `:N` + `gg`/`G`,
      the line-number gutter, and the regex search).
      Detail:
      `docs/tui-feature-requests/2026-09-03.md` (browse-mode).

- [x] Select-and-yank in the browse mode (`Visual`-style
      operators and text objects, yankable into the draft).
      Detail:
      `docs/tui-feature-requests/2026-09-03.md` (select-yank).

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

- [x] Syntax-highlight the picker preview pane, sharing the
      highlighter with tool-result rendering.
      Detail:
      `docs/tui-feature-requests/2026-09-04.md` (picker-code-highlight).

- [x] Redesign session navigation; the design landed in the
      `:` command palette (`:b` / `:bn` / `:bp`).
      Detail:
      `docs/tui-feature-requests/2026-09-04.md` (session-navigation).

- [x] A `:` command palette in normal mode.
      Detail:
      `docs/tui-feature-requests/2026-09-04.md` (command-palette).

- [x] Recall and edit pending user messages (`Alt+Up` /
      `:edit-queue`, the `user_message_retract` event).
      Detail:
      `docs/tui-feature-requests/2026-09-04.md` (pending-message-recall).

## New requests (2026-09-06)

- [x] Accept `ctrl+z` the standard keybinding that send our tui to
      background jobs in the shell. Shipped: `Ctrl+Z` sends SIGTSTP,
      the shell backgrounds the TUI; `fg` resumes it. The terminal is
      restored before suspend and re-initialised on resume.

- [x] A `|` line counts as a table only when a `|---|`
      separator row follows it.
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (table-pipe-not-table).

- [x] Stream rendering of the model response.
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (streaming-response).

- [x] No collapse jump at the thinking to text transition
      (the thinking tail and the text share one window).
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (thinking-text-transition).

- [x] Buffer the response text and render it at a smooth,
      steady frame rate (paced FIFO queue).
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (stream-pacing).

- [x] Drop the `assistant` / `tool:` markers and the message
      indent; wrap user messages in a rounded titled panel.
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (message-panel-redesign).

- [x] When in browse mode, updates from model response should
      not flush the screen to the latest position of the
      conversation.
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (browse-pin).

- [x] The `@` picker cycles the file scope (git-ignored,
      hidden) with `Ctrl+I`/`Tab` (now `Ctrl+T`).
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (picker-file-scope).

- [x] Abbreviate long `@` picker paths with `...` so the
      tail stays visible.
      Detail:
      `docs/tui-feature-requests/2026-09-06.md` (picker-path-abbreviation).

## New requests (2026-09-07)

- [x] Bug: `tool:edit` results always show `diff +0 -0`. Evidence session:
      `sessions/goal-ux-impl`. Fixed: the diff stat row now computes
      `added`/`removed` from the positional diff of the `before` and
      `after` line lists in `edit_body` (`bin/tui/src/tool_display.rs`),
      so a changed file shows its real line counts instead of `+0 -0`.

- [x] `tool:edit` shows a split diff when the terminal is
      wide, a unified diff when narrow.
      Detail:
      `docs/tui-feature-requests/2026-09-07.md` (diff-split-layout).

- [ ] The input box does not highlight the whole visual
      selection in `VISUAL` / `V-LINE`.
      Detail:
      `docs/tui-feature-requests/2026-09-07.md` (visual-selection-highlight).
      Open.

## New requests (2026-09-14)

- [x] Tool-result panel: the tool name in a purple, bold
      font; the border lines dropped.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (tool-panel-style).

- [x] A background-filled margin row above the panel header
      and below the last body row.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (tool-panel-margin).

- [x] No `tool:` prefix on external tool calls (goal tools
      get compact labels).
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (no-tool-prefix).

- [x] The tool-result panel spans the full transcript width.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (tool-panel-full-width).

- [x] The panel header names what the tool acted on (the
      `read` file path, the goal summary/reason); the
      redundant `ok` is dropped.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (tool-panel-header-target).

- [x] Compact tool labels are scoped to the kernel's own
      tools; the extension display stack is removed.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (compact-label-scope).

- [x] Simplify the live stream: the in-progress response
      streams inline in the transcript tail; `Ctrl+T` covers
      live-thinking blocks too.
      Detail:
      `docs/tui-feature-requests/2026-09-14.md` (live-stream-simplify).

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

- [ ] Observation: the text inside a markdown table renders
      as raw text, with no syntax highlighting.
      Detail:
      `docs/tui-feature-requests/2026-09-12.md` (table-cell-highlight).
      Open.

- [x] The picker preview pane no longer caps file content at
      50 lines (windowed, cancellable rendering).
      Detail:
      `docs/tui-feature-requests/2026-09-12.md` (picker-preview-windowing).

## New requests (2026-09-13)

- [x] Preview-pane text wraps at the pane width instead of
      truncating.
      Detail:
      `docs/tui-feature-requests/2026-09-13.md` (preview-pane-wrap).

- [x] Also fixed a latent off-by-2 in the picker preview pane
      where the two border rows were not subtracted from the
      visible height. Detail:
      `docs/tui-ratatui-ecosystem-audit.md` (§4.8).

- [x] Fix browse-mode search typing. While the search line is
      open, typing `s` armed the double-`s` exit instead of
      entering the query. The exit arm is now suppressed while
      the line is typing.

- [x] Pin the browse view on streaming growth and on the
      settle shrink.
      Detail:
      `docs/tui-feature-requests/2026-09-13.md` (browse-pin-on-settle).

- [x] No cap on the number of replayed events.
      Detail:
      `docs/tui-feature-requests/2026-09-13.md` (no-event-cap).

- [x] Yank returns the raw source instead of the rendered
      text.
      Detail:
      `docs/tui-feature-requests/2026-09-13.md` (raw-yank).

## New requests (2026-09-15)

- [x] The browse cursor paints at the start of a word
      (hyphen-folding word class, the caret-span fix).
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (browse-caret).

- [x] The vim `e` motion is registered in browse mode (plus
      the `ye` yank form).
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (browse-e-motion).

- [x] Browse mode opens with a held draft (the double-`s`
      gate dropped the empty-draft condition).
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (browse-gate-draft).

- [x] Float lists wrap at both ends; `Ctrl+Shift+P` toggles
      list/preview focus; the focused preview border is green.
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (float-wrap-focus).

- [x] `Tab` completes the highlighted item in every float
      window; the picker scope cycle moves to `Ctrl+T`.
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (tab-complete).

- [x] `Ctrl+F` cycles the tree event type filter.
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (tree-filter).

- [x] Prettify tree tool rows (the command, the `file_path`,
      the result status and first line).
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (tree-row-prettify).

- [x] The tree preview pane parses tool JSON with
      `jaq-core`, highlights unconditionally, and scrolls.
      Detail:
      `docs/tui-feature-requests/2026-09-15.md` (tree-preview-jaq).

## New requests (2026-09-16)

- [x] A `working` loop-phase status distinct from `wait`
      (the request pending vs. the response streaming).
      Detail:
      `docs/tui-feature-requests/2026-09-16.md` (working-status).

## New feedback (2026-09-16)

- [x] Color the tree row tags by class (user, assistant,
      tool rows each get a distinct fg color).
      Detail:
      `docs/tui-feature-requests/2026-09-16.md` (tree-tag-colors).

- [x] `Ctrl+Shift+P` toggles list/preview focus on legacy
      terminals (the kitty keyboard protocol).
      Detail:
      `docs/tui-feature-requests/2026-09-16.md` (ctrl-shift-p).

- [x] In the command palette, `Tab` replaces the typed query
      with the highlighted item, not appended to it.
      Detail:
      `docs/tui-feature-requests/2026-09-16.md` (palette-tab-replace).

## New requests (2026-09-17)

- [ ] A cross-session status dashboard: see `working` / `idle` /
      `blocked` for all rushi sessions at a glance, across tmux panes
      and projects. Open discussion of placement (TUI tab row vs
      standalone monitor binary vs UI extension) and the blocked
      detection rule. Detail: `docs/tui-session-monitor.md`.

## New requests (2026-09-19)

- [x] Add vim's `ge` motion (backward to the end of the
      previous word) to the input box and browse mode (plus
      the `yge` yank form).
      Detail:
      `docs/tui-feature-requests/2026-09-19.md` (ge-motion).

## New requests (2026-09-20)

- [x] The fold-view tally shows the compact-event and
      hook-trigger counts.
      Detail:
      `docs/tui-feature-requests/2026-09-20.md` (fold-tally-hooks).

- [x] The input-box `r` motion gets its own mode: only the
      pending replace shows `[r-PENDING]`.
      Detail:
      `docs/tui-feature-requests/2026-09-20.md` (replace-char-mode).

- [x] The fold tally's hook-trigger counters are inflated by
      no-op transforms. Decision 2026-09-21: bear with the
      counters for now. The kernel no-op filter is filed as
      `rushi#24` for later. Detail:
      `docs/tui-feature-requests/2026-09-20.md` (tally-hook-inflation).

## New requests (2026-09-23)

- [x] The tree palette gets fold/unfold and jump keys that reuse
      the browse-mode `z` fold arm: `za` toggles the branch under
      the cursor, `zo`/`zc` open and close it, `zR`/`zM` open and
      close every branch, and `zj`/`zk` jump to the next and
      previous branch starting point (the rewind marker row).
      Detail:
      `docs/tui-feature-requests/2026-09-23.md` (tree-fold-jump).