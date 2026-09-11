# Refactor decision

I decide to steer the `rushi-tui` to leverage the rich `ratatui` ecosystem better.
TUI development affects the user experience. We should be well structured and
learn from the best practices of `ratatui` ecosystem early, before everything
is too late.

## Must-dos from human

1. Drop `syntect`. Use Rust tree-sitter. Split the TUI crate, draw a clean boundary on TUI crate and
   the syntax highlight support. So changes irrelevant to syntex highlight
   never triggers a tree-sitter rebuild.

2. Addition to point #1: Divide the TUI crate further. Use the workspace.
   Boost development iteration time: current structure makes `cargo build
--release --bin tui` hang at the last step for over 2 mins, even when the
   other deps are readily cached.

3. Image support from `ratatui-image`. The `rushi` kernel has added image
   read support.

## Recommended `ratatui` libraries: widget/frameworks/utilities

Source: https://github.com/ratatui/awesome-ratatui (fetched 2026-09). ratatui 0.30.2 is modularized into `ratatui-core` and `ratatui-widgets` sub-crates.

### Core-enhancing libraries (in-process, performance-critical)

These run inside the TUI process. They render at 60 FPS. They touch the
transcript, the layout, and the frame loop directly.

1. **ratatui-markdown ^0.3** — markdown rendering to styled `ratatui::text::Line`s: headings, lists, code blocks, blockquotes, tables, inline formatting. Also provides collapsible JSON/TOML trees and a hybrid scroll system. The TUI renders markdown for the majority of transcript content, so this is the highest-leverage library for the core. Note: currently tracks `ratatui ^0.29`; a 0.30-compatible release is needed before adoption. https://crates.io/crates/ratatui-markdown
2. **ratatui-image ^11** — sixel/halfblock image rendering. Directly implements the new image-output requirement. In-process so streaming image output stays smooth. https://crates.io/crates/ratatui-image
3. **tui-scrollview ^0.6** — bounded viewport over a large buffer. The right primitive for long scrolling transcripts. Replaces the current manual scroll offset. https://crates.io/crates/tui-scrollview
4. **ansi-to-tui ^8** — converts ANSI-colored text into ratatui `Text`. Fits code boxes that display raw tool output. In-process so bash output colors render at frame rate. https://github.com/ratatui/ansi-to-tui
5. **tui-overlay ^0.1.2** — drawers, modals, popovers, toasts. Core UI chrome that must render inline with the frame loop. 0.30-compatible.
6. **ratatui-cheese ^0.7** — spinner, help, tree, paginator, list widgets. Core UI widgets used in the main pane. 0.30-compatible.
7. **ratatui-macros ^0.7** — declarative widget macros. ratatui 0.30.2 itself depends on this. Reduces boilerplate in the core render path.
8. **opaline** — token-based theme engine with built-in themes. Replaces the hand-rolled `color.rs` palette with a token system.
9. **termprofile**, **coolor**, **color-to-tui** — terminal color detection and conversion. In-process so capability probing stays fast at startup.

### Extension-building libraries (out-of-process, IPC)

These run in extension processes. They communicate with the core via
JSONL on stdio. They add capability without touching the frame loop.

1. **malevich** — terminal plotting: line charts, bar charts, heatmaps.
   Useful for agents that do data analysis and produce reports. An
   extension process renders a plot and ships it to the core as a
   base64 image or half-block payload. The user reads the chart in the
   transcript without leaving the TUI. https://github.com/eyfein/malevich
2. **tui-realm** — Elm/React-style ratatui framework. Good for building
   structured extension UIs with unidirectional data flow. An extension
   that needs a small interactive panel (file picker, diff viewer) can
   use the realm pattern without pulling the whole framework into the core.
3. **widgetui** — bevy-like widget system for ratatui and crossterm.
   Useful for building complex multi-panel extension UIs. The extension
   owns its own widget tree and ships rendered frames to the core.
4. **rat-salsa** — event queue with tasks, timers, focus, dialogs.
   An extension that needs async work (a long build, a network call)
   can use the salsa event loop pattern and report progress via IPC.
5. **ratatui-input-manager** — Elm-style declarative update handlers
   for crossterm. An extension that owns a custom input area (search
   box, filter) can use this to keep input handling declarative.
6. **ratatui-interact** — interactive components with focus and mouse
   support. An extension that shows a clickable tree or a sortable
   table can use the interact primitives inside its process.

## Recommended `ratatui` apps to learn

### Core patterns (in-process render loop, viewport, frame budget)

1. **gitui** — the reference ratatui event-driven render loop. It also
   implements diff and list views. Study the event loop, the frame
   budget, and how it keeps the UI responsive under load. https://github.com/extrawurst/gitui
2. **Yazi** — list plus viewport with async I/O and image previews.
   The closest analog to our scrollback plus image output. Study the
   viewport scroll model and how it keeps image rendering in-process. https://github.com/sxyazi/yazi

### Extension patterns (composable views, out-of-process capability)

3. **xplr** — event-driven architecture with composable, hackable
   views. It models how to split a TUI into independent view modules
   that communicate through a shared state bus. The pattern maps
directly to our extension IPC design. https://github.com/sayanarijit/xplr
4. **Livediff** — real-time terminal diff monitor. It gives a concrete
   diff-view implementation to study. A diff extension can reuse the
   same live-update pattern. https://github.com/SoCkEt7/Livediff
5. **bottom** — cross-platform system monitor with panes and sparkline
   widgets. Study how it composes independent panes that update at
different rates. The pattern maps to a monitoring extension that
   streams into a reserved pane. https://github.com/ClementTsang/bottom
6. **atuin** — shell history TUI with fuzzy search and SQLite backend.
   A search extension can use the same fuzzy-matching plus database
   pattern to index transcript history across sessions. https://github.com/atuinsh/atuin
