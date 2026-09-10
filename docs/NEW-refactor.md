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

Leave to agent to help explore.

## Recommended `ratatui` apps to learn

Leave to agent to help explore.
