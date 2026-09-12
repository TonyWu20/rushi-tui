# Documentation Index

Authoritative entry point for any agent starting a new session in
this repo. Read this first. It tells you what exists, what is
shipped, and what is next.

## Repo state (2026-09-11)

**Working.** `bin/tui` is the swappable Ratatui TUI front-end for
the `rushi` kernel. It compiles and runs against a sibling
`rushi-common` path dep. The PTY smoke gate is
`scripts/tui-pty-smoke.py`.

**Split.** This repo is the TUI half of the split described in
`tui-ext-repo-split.md`. The kernel lives in
`../rust-unix-harness`; UI extensions live in `../rushi-exts`.
This repo has no build-time dependency on either.

**Tests.** The suite now centres on 25 insta snapshot tests
(`bin/tui/src/snapshot_tests.rs`) that capture the rendered
terminal grid for each major state. Pure-logic tests remain for
event parsing, config, port-file tailing, fuzzy matching, and the
extension-host lifecycle. Method and conventions:
`tui-insta-snapshot-testing.md`.

## Doc inventory

| Doc | Status | Last updated | Purpose |
| --- | --- | --- | --- |
| `tui.md` | Historical | 2026-09-07 | Original TUI proposal: coupling contract, `SessionPort`, wire format. Superseded by shipped code. |
| `tui-plan.html` | Historical | 2026-09-07 | HTML implementation plan with mermaid diagrams for the first TUI build. |
| `NEW-refactor.md` | Implemented | 2026-09-11 | Decision to drop syntect for tree-sitter and adopt the ratatui widget ecosystem. This repo is the result. |
| `tui-ratatui-ecosystem-audit.md` | Active | 2026-09-28 | Per-module and per-feature audit of hand-rolled vs. ratatui-ecosystem libraries. Action items with priorities. |
| `ui-extension.md` | Active | 2026-09-07 | UI extension design: protocol, host lifecycle, trust model. The contract for `../rushi-exts`. |
| `ui-extension-plan.md` | Plan | 2026-09-07 | Staged build plan for the extension mechanism. |
| `tui-extension-design-review.md` | Review | 2026-09-07 | Adversarial review of `ui-extension.md`: missing `notify` op, `transform` reply correlation. |
| `tui_extension_design_questions_from_human.md` | Draft | 2026-09-07 | Open design questions on extension categories and openness. |
| `tui-conversation-browsing.md` | Implemented | 2026-09-07 | Scroll, position bar, browse mode, search. The browsing contract. |
| `tui-conversation-browsing-review.md` | Review | 2026-09-07 | Spec review of the browsing doc: YAGNI check, scope verification. |
| `tui-file-picker.md` | Implemented | 2026-09-07 | `@` file picker and auto-completion widget. |
| `tui-file-picker-research.md` | Investigation | 2026-09-07 | Library research: frizbee, television, telescope.nvim. |
| `tui-command-palette.md` | Superseded | 2026-09-08 | `:` command palette. Now implemented in `bin/tui/src/palette/`. |
| `tui-color-scheme.md` | Implemented | 2026-09-07 | Color scheme selection: named schemes, custom schemes, capability detection. |
| `tui-color-tones.md` | Implemented | 2026-09-07 | Text-tone roles and the gray-abuse fix. |
| `tui-color-pi-alignment.md` | Implemented | 2026-09-07 | Case-by-case colour alignment with the pi TUI. |
| `tui-streaming-response.md` | Implemented | 2026-09-07 | Live model-response streaming into the TUI transcript. |
| `tui-model-wait-indicator.md` | Implemented | 2026-09-07 | Working-row spinner and timer during model wait. |
| `tui-thinking-block.md` | Implemented | 2026-09-07 | Reasoning/thinking block capture and display. |
| `tui-thinking-level-input-box.md` | Implemented | 2026-09-07 | Thinking-level control in the input-box border. |
| `tui-pending-user-messages.md` | Implemented | 2026-09-07 | Queued user messages rendered while the loop is busy. |
| `tui-malformed-line-flash.md` | Implemented | 2026-09-07 | Root-cause and fix for the transient malformed-line flash. |
| `tui-markdown-render.md` | Superseded | 2026-09-08 | Original markdown-render spec. Now in `highlight.rs`. |
| `tui-tool-display-port.md` | Implemented | 2026-09-07 | Port of pi-tool-display: boxed tool results, fold/expand, presets. |
| `tui-tool-result-truncation.md` | Implemented | 2026-09-07 | Truncation and diff layout for tool results. |
| `tui-statusline-powerline.md` | Implemented | 2026-09-07 | Powerline footer rendering. |
| `tui-syntax-highlighting.md` | Implemented | 2026-09-07 | tree-sitter highlight engine in `highlight.rs`. |
| `user-message-editing.md` | Spec | 2026-09-10 | Queue-recall and retract event. `:edit-queue` palette entry. Not yet built. |
| `vim-editor-design.md` | Implemented | 2026-09-10 | Vim modal input: modes, motions, operators, registers. |
| `goal-ux.md` | Spec | 2026-09-08 | Goal UX: prompt template, TUI status, `goal pause` / `goal clear`. |
| `goal-ui_feedback_from_human.md` | Draft | 2026-09-10 | Human-reported goal UI bugs and feature requests. |
| `tree-ui-design-from-human.md` | Draft | 2026-09-11 | Draft for the rewind/tree browse UI. |
| `tui_feature_requests_from_human.md` | Active | 2026-09-10 | Slim index of all TUI feature requests with ship status. |
| `tui-ext-repo-split.md` | Historical | 2026-09-08 | The repo-split execution record. Marked STALE for the re-pointing step. |
| `tui-insta-snapshot-testing.md` | Implemented | 2026-09-11 | The insta-snapshot test method: harness, snapshot set, determinism rules. |

## Status legend

- **Active** — standing reference. Read before touching the area.
- **Implemented** — the system exists in code. The doc is the design
  record. Update it when behaviour changes.
- **Spec** — approved to build. The next work item.
- **Plan** — staged work breakdown of an approved spec.
- **Proposal** — under consideration. Do not build until approved.
- **Draft** — early exploration. May change shape.
- **Review** — critique of another doc. Not a specification.
- **Investigation** — research that surveys external code. Not a spec.
- **Historical** — records a past decision. Not a current spec.
- **Superseded** — replaced by a newer doc. Kept for history.

## Reading order for a new session

1. This file (`INDEX.md`).
2. `tui.md` — the original coupling contract and `SessionPort`
   abstraction.
3. `ui-extension.md` — the extension protocol and host lifecycle.
4. `tui-conversation-browsing.md` — the browsing contract
   (scroll, position bar, browse mode).
5. `NEW-refactor.md` — why the crate is split and which ratatui
   widgets are in use.
6. `tui-insta-snapshot-testing.md` — how to add and run snapshot
   tests for any new or changed UI behaviour.
7. The spec doc matching the feature you are working on (check the
   Status column above).
8. `tui_feature_requests_from_human.md` — what is still open.

## Maintenance rules

- Update the Status column when a doc changes state.
- Date the "Last updated" column on every edit.
- When a doc is superseded, mark it "Superseded" and leave it in
  place. Do not delete.
- Keep this file under 150 lines. If the inventory outgrows that,
  split into sub-indexes by topic and keep only the state summary
  here.
