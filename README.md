# rushi-tui

The swappable TUI front-end (Tier-2) of the `rushi` Unix-philosophy
agent harness. This is the dedicated TUI repo from the split described in
`docs/tui-ext-repo-split.md`.

## Contents
- `bin/tui/` — the TUI binary `rushi-tui` (Ratatui + crossterm).
  The extension host in `src/ext.rs` discovers and supervises
  external extension processes. `rushi-tui` is the self-wired entry
  point. It resolves its config through the shared kernel resolver and
  supervises the loop as the Tier-1 `rushi` CLI. The default is
  `rushi run <session>`. Override it with `--loop-cmd` or the config
  `[loop]` section.
- `bin/tui-stream-drt/` — the std-only Rust mirror of the Lean DRT spec
  (the production side of the differential-random-testing gate).
- `lean/` — the TUI half of the Lean DRT backstop: `TuiStreamSpec`,
  `TuiViewportSpec`, and the `TuiStreamDrt` model executable.
- `scripts/` — `tui-pty-smoke.py` (the PTY acceptance gate),
  `tui-capture.py`, `tui-stream-drt-inputs.sh`.
- `docs/` — the TUI / ui-extension / goal-UX docs.

## Contributing

`CONTRIBUTING.md` — step-by-step from a fresh clone to an open PR, written
for first-time contributors and their coding agents.

## Building
`rushi-common` is a **git dep** on the kernel repo
(`github.com/TonyWu20/rushi`, pinned by rev in `bin/tui/Cargo.toml`).
The kernel crate is the `rushi-common` package in `crates/rushi`. A
plain clone builds with no sibling checkouts:

```
cargo build          # builds the rushi-tui + tui-stream-drt binaries (+ kernel rushi-common via git dep)
cargo test -p tui
cd lean && lake build TuiStreamSpec TuiViewportSpec TuiStreamDrt   # 3 TUI modules + the TuiStreamDrt DRT model exe
```

## Running the PTY smoke against a separate exts checkout
```
EXTS_ROOT=../rushi-exts python3 scripts/tui-pty-smoke.py target/debug/rushi-tui <kernel-root>
```

To track a newer kernel rev: bump the `rev` in `bin/tui/Cargo.toml`
(the `rushi-common` git dep) and re-run the gates
(docs/tui-ext-repo-split.md section 4, item A1).
