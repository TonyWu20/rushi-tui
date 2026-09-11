# TUI tool display port

Status: shipped (2026-09-03 pass). The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).
See section 4 for what shipped and where.

## 1. Request

Full port of `pi-tool-display` for the tool result style. Wrap
every tool result in a lighter-colored box. Add the fold/expand
control, just like the extension.

Today: the built-in render (`bin/tui/src/render.rs`) and the
reference renderer (`ui_extensions-demos/tool_result/tool_result.sh`,
Rust port `ext-rs/tool_result-rs`) paint the result body as
flat lines. No box. No fold. No expand.

Reference: `pi-tool-display` (github.com/MasuRii/pi-tool-display,
v0.5.0, pinned rev `91cef758`), installed via
`~/programming/pi-config/flake.nix`.

## 2. Parts to port

- Box: each result sits in a rounded box. A light background.
  One-cell padding. The extension renders its compact output
  inside that box.
- Fold: long output collapses to a preview. The preview shows
  the first lines only. A muted hint states the remainder and
  the key, like `... (173 more lines • Ctrl+O to expand)`.
- Expand: one global key toggles every collapsed block to the
  full output. Pi's key is `app.tools.expand`, default
  `Ctrl+O`. The expanded preview caps at
  `expandedPreviewMaxLines` (4000).
- Per-tool limits: `previewLines` 8 for read,
  `bashCollapsedLines` 10, `diffCollapsedLines` 24. Output
  modes: `hidden` / `summary` / `preview` for read and bash.
  Presets `opencode`, `balanced`, `verbose`.
- Config: `config.json` plus a settings modal, like the
  extension.

## 3. Relation to the truncation request

The 2026-08-29 request
(`docs/tui-tool-result-truncation.md`) owns the content layer:
truncation of `Read` and `Write` results and the `Edit` diff.
This doc owns the style layer: the box, the fold/expand
control, and the remaining render behavior. The result is a
full port of the extension.

## 4. Shipped (2026-09-03 pass)

The port lives in `bin/tui/src/tool_display.rs` plus the
`ToolResult` render pass in `bin/tui/src/render.rs`:

- **Box**: each result sits in a lighter panel with the box-state
  background role (`tool_box_bg_success` on success, the
  `tool_box_bg_error` role on failure; docs/tui-color-pi-
  alignment.md: the pi `toolSuccessBg` / `toolErrorBg` values).
  No border lines: the 2026-09-14 user pass dropped the white
  rounded-corner border. The panel is the lighter background only.
  The two freed border rows became one margin row above the header
  and one below the last body row. These margin rows are
  background-filled, so the panel reads as a band with breathing
  room. The header row names the tool in the purple `tool_name`
  accent, bold. A `read` result adds the file it read as a dim label
  after the name. Other tools keep the bare name. The
  status is the red bold accent on an error. A success shows no
  status word, the panel background already signals the outcome.
  No separate header line sits above the panel.
- **Full width**: the panel spans the full transcript width. The
  2026-09-14 follow-up dropped a stale two-column right reservation,
  so the band reaches the right edge. The position bar still owns
  its own rightmost column when shown.
- **Call/result merge**: a bash tool_call whose result follows
  drops its own line: the result box body opens with the
  `$ <command>` line, so the separate call line would repeat the
  command. A call without a result yet keeps its line: the
  command is the only view of a running tool.
- **Fold**: long output collapses to the preview. The muted hint
  states the remainder and the key, `... (N more lines •
  Ctrl+O to expand)`.
- **Expand**: `Ctrl+O` toggles every collapsed block to the
  full output, capped at `expanded_max_lines` (4000).
- **Per-tool limits**: `preview_lines` 8 (read, the
  generic tool), `bash_collapsed_lines` 10, `diff_collapsed_lines`
  24. Output modes, the extension names: `hidden` / `summary` /
  `preview` for read and bash. `hidden` shows no body. `summary`
  keeps the line summary line.
- **Presets**: `opencode`, `balanced`, `verbose`, the extension
  values. An override switches the effective preset to
  `custom`.
- **Config**: `[tui] tool_display` in `config.toml` (docs/
  tui.md section 13.2). The extension's settings modal is not
  built: the TUI config is the toml file, edited by the user or
  by the `Ctrl+L`-style in-place edits.
- **Diff layout**: `Edit` results render as a diff (before and
  after). `diff_view` `auto` switches split at
  `DIFF_SPLIT_MIN_WIDTH` (120 columns), `unified` and `split`
  force a layout. The added and removed lines color through the
  `diff_added` / `diff_removed` roles (the pi `toolDiffAdded` /
  `toolDiffRemoved` values, docs/tui-color-pi-alignment.md). The
  JSON and bash bodies syntax-highlight inside the box
  (docs/tui-color-tones.md section 4): the JSON tokens color
  through the `syntax*` roles, the pi scope mapping.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. result-box: given a finished tool result, observe a lighter panel with no border lines and the success or error background role. The header row names the tool in the purple `tool_name` accent (bold) with the status. The top and bottom margin rows are background-filled. The panel spans the full transcript width.
P2. call-merge: given a bash `tool_call` whose result follows, observe the call line drop and the box open with the `$ <command>` line. A call with no result keeps its own line.
P3. fold: given a result longer than the tool preview cap, observe a collapsed preview with a muted `N more lines` hint naming `Ctrl+O`.
P4. expand: given a collapsed block, observe `Ctrl+O` open every block to the full output capped at `expanded_max_lines`.
P5. preset-override: given a per-tool override in `[tui.tool_display]`, observe the effective preset become `custom` and the named per-tool limits apply.
P6. external-call: given an external `tool_call` such as a goal tool, the call line shows only the bare name. It has no `tool:` prefix and no arguments. The kernel does not special-case extension tools, so the call line keeps its own row and the result panel renders on its own.
P7. result-header: given a tool result, the panel header shows the tool name. A `read` result adds the file it read as a dim label after the name. A success shows no status word, the panel background signals the outcome.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | result-box | `panel_has_no_border_lines` / `header_names_the_tool_in_purple_bold` in `bin/tui/src/tool_display.rs`; `snap_tool_result_collapsed` in `bin/tui/src/snapshot_tests.rs` | proven |
| P2 | call-merge | `bash_call_merges_into_the_result_box` in `bin/tui/src/render.rs` | proven |
| P3 | fold | `read_preview_folds_to_the_preview_lines` in `bin/tui/src/tool_display.rs`; `long_tool_result_folds_to_the_preview_cap` in `bin/tui/src/render.rs` | proven |
| P4 | expand | `expand_overrides_the_hidden_and_summary_modes` in `bin/tui/src/tool_display.rs` | proven |
| P5 | preset-override | `preset_and_mode_parsing`, `preset_value_tables` in `bin/tui/src/tool_display.rs` | proven |
| P6 | external-call | `snap_pending_approval_banner` in `bin/tui/src/snapshot_tests.rs` | proven |
| P7 | result-header | `snap_tool_result_read_bare_name_header` in `bin/tui/src/snapshot_tests.rs` and `result_status` in `bin/tui/src/render.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
