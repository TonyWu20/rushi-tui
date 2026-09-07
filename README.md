# rushi-tui

The swappable TUI front-end (Tier-2) of the `rushi` Unix-philosophy
agent harness. This is the dedicated TUI repo from the split described in
`docs/tui-ext-repo-split.md`.

## Contents
- `bin/tui/` — the TUI binary (Ratatui + crossterm; the extension host
  in `src/ext.rs` discovers and supervises external extension processes).
- `bin/tui-stream-drt/` — the std-only Rust mirror of the Lean DRT spec
  (the production side of the differential-random-testing gate).
- `lean/` — the TUI half of the Lean DRT backstop: `TuiStreamSpec`,
  `TuiViewportSpec`, and the `TuiStreamDrt` model executable.
- `scripts/` — `tui-pty-smoke.py` (the PTY acceptance gate),
  `tui-capture.py`, `tui-stream-drt-inputs.sh`.
- `docs/` — the TUI / ui-extension / goal-UX docs.

## Building (bootstrap: local path dep, no remote yet)
`rushi-common` is a sibling **path dep** on the kernel checkout
(`../rust-unix-harness/crates/rushi`), so build from a checkout where the
kernel sits next to this repo:

```
cargo build          # builds tui + tui-stream-drt (+ kernel rushi-common via path dep)
cargo test -p tui
cd lean && lake build TuiStreamSpec TuiViewportSpec TuiStreamDrt   # 3 TUI modules + the TuiStreamDrt DRT model exe
```

## Running the PTY smoke against a separate exts checkout
```
EXTS_ROOT=../rushi-exts python3 scripts/tui-pty-smoke.py target/debug/tui <kernel-root>
```

At hosting time, the `rushi-common` path dep flips to a pinned git dep on
the kernel repo (docs/tui-ext-repo-split.md section 4, item A1).
