# Self-wired `rushi-tui` entry command

Status: implemented (2026-09-22), issue #22. The TUI is now a
self-wired front-end. It is the user-facing entry point on its own,
shares the kernel's config-path resolver, and supervises the loop as
the Tier-1 `rushi` CLI. No kernel launcher subcommand is needed.

This is the TUI side of the 2026-09-20 kernel decision in the kernel
`docs/itches.md`. The kernel keeps no per-front-end launcher
subcommand. The kernel `rushi tui` arm is retired. Kernel PR #30
(commit `10139de`) promoted the config-path resolver into
`rushi_common::paths` so front-ends inherit it. This doc records the
TUI adaptation. The kernel launcher retirement is a kernel-side
change tracked there, not here.

## 1. The three changes

### 1.1 Entry command renamed to `rushi-tui`

The binary was `tui`, launched by the kernel's `rushi tui` arm. The
kernel is retiring that arm. So the TUI is now shipped as a single
`rushi-tui` command and is the entry point on its own.

- `bin/tui/Cargo.toml`: the `[[bin]]` name is `rushi-tui`. The
  package name stays `tui`, so `cargo build -p tui` and the flake
  `cargoBuildFlags = [ "-p" "tui" ]` are unchanged.
- `flake.nix`: the Nix package installs `$out/bin/rushi-tui`. The
  comments and the dev-shell example paths were updated to the new
  name.
- `rushi-tui --help` reports the command name `rushi-tui`.
- The kernel launcher still finds the TUI through the `[tui].binary`
  config key or by putting `rushi-tui` on PATH. Its bare side-by-side
  / PATH fallback looks for the old name `tui` and is part of the
  kernel retirement.

### 1.2 Shared config-path resolver (kernel PR #30)

The TUI no longer keeps a private copy of the 4-step config ladder.
It calls the shared kernel resolver:

- `bin/tui/src/main.rs` `resolve_config_path` now delegates to
  `rushi_common::paths::resolve_config_path`.
- The local `canonicalize_or_raw` and
  `resolve_config_path_for_exe` helpers were deleted. Their Nix
  side-by-side and macOS symlink behavior now lives in the kernel
  `rushi_common::paths` tests. The `pty_smoke`
  `side_by_side_package_layout` test exercises it through the real
  binary.
- `bin/tui/Cargo.toml`: `rushi-common` is now a git dep pinned to
  kernel commit `10139de`. It was a crates.io dep before. That commit
  ships the shared `paths` module.

The resolved priority now matches the kernel by construction. It is
`$CONFIG`, then the `--config` flag, then the Nix side-by-side
`<exe_dir>/../config.toml` with the exe canonicalized, then
`config.toml` in the CWD. Note the precedence change. `$CONFIG` now
outranks `--config`, because the kernel always honors `$CONFIG`
first.

### 1.3 Self-wired loop (the Tier-1 `rushi` CLI)

The TUI supervises the loop without the kernel launcher. When the
config has no `[loop]` section and no `--loop-cmd` flag is passed,
the default loop command is the Tier-1 `rushi` CLI from PATH:

- `bin/tui/src/config.rs`: `default_loop_cmd` returns
  `rushi run <session>` in the `append_session` style.
  `parse_loop_cmd_flag` parses the new `--loop-cmd` override into a
  `LoopCommand`.
- `bin/tui/src/main.rs`: the precedence is the `--loop-cmd` flag,
  then the config `[loop]` section, then the `rushi run` default.

This pins only the Tier-1 `rushi` CLI contract. That is `rushi run
SESSION` and `rushi step SESSION`, both taking the global `--config`.
There is no kernel launcher commit and no private kernel patch.

## 2. Files touched

- `bin/tui/Cargo.toml` — the `[[bin]]` name is `rushi-tui`. The
  `rushi-common` git dep is pinned to kernel `10139de`.
- `bin/tui/src/main.rs` — `#[command(name = "rushi-tui")]`, the
  `--loop-cmd` flag, the shared `resolve_config_path` delegate, and
  the loop-command precedence.
- `bin/tui/src/config.rs` — `default_loop_cmd` and
  `parse_loop_cmd_flag`, plus their unit tests.
- `bin/tui/tests/pty_smoke.rs`, `bin/tui/tests/pty_perf.rs` — the
  `CARGO_BIN_EXE_rushi-tui` env var after the binary rename.
- `bin/tui/src/snapshots/` — 44 insta snapshots renamed from the
  `tui__` prefix to `rushi_tui__`. Insta derives that prefix from the
  binary name.
- `flake.nix`, `.envrc`, `README.md`, `CONTRIBUTING.md`,
  `scripts/tui-capture.py`, `scripts/tui-pty-smoke.py` — the
  `rushi-tui` binary name in comments, examples, and defaults.
- `Cargo.lock` — `rushi-common` now resolves from the kernel git rev.

## 3. Notes and follow-ups

- The kernel launcher (`rushi tui`) still resolves the TUI through
  the `[tui].binary` config key and the old `tui` name on PATH.
  Update the user's `[tui].binary` to the new `rushi-tui` path while
  the kernel launcher is still in use. The kernel retirement is
  tracked in the kernel `docs/itches.md`.
- The `cargo fmt` gate has pre-existing drift in `app.rs`, `ext.rs`,
  `main.rs`, and `vim_editor.rs` on the current toolchain. It fails
  on an untouched HEAD too. This PR formats only the lines it adds.
  The tree-wide reformat is out of scope.
- The Lean DRT gate was skipped because this PR does not touch
  `lean/`.
