# Contributing to rushi-tui

Step-by-step guide for a first-time contributor (and the coding agent that
drives them) from a fresh clone to an open pull request. Follow the steps in
order. Each step says what to run and what success looks like. If a step
fails, stop and fix that step before moving to the next one.

## Repo facts

| Item | Value |
| --- | --- |
| Upstream | `ssh://git@github.com/TonyWu20/rushi-tui` |
| PR base branch | `main` (the ratatui-widget-based work is merged) |
| Build | Nix flake (`flake.nix`). It is hostable: it fetches the `rushi` kernel from GitHub, so no sibling checkouts are needed |
| CI | None. There is no `.github/`. All gates run locally, so the PR description must include the gate evidence |

Keep this file's instructions literal: exact commands, exact success
criteria. When something is ambiguous, stop and ask the maintainer.

## Step 0 — Preflight (human + agent)

The human needs a GitHub account (call it `<YOU>`) with:

- git auth set up (an SSH key, or a token for HTTPS remotes),
- the `gh` CLI logged in (`gh auth status`),
- Nix installed.

The agent verifies the toolchain before starting:

```sh
command -v git gh rg nix
gh auth status
```

The first `nix develop` fetches `nixpkgs`, the kernel flake, and the Lean
toolchain, so it can take a while. That is normal, not a failure.

## Step 1 — Fork and clone

The human forks `TonyWu20/rushi-tui` into their own account (GitHub web UI:
Fork). The fork is named `rushi-tui` under `<YOU>`.

```sh
git clone ssh://git@github.com/<YOU>/rushi-tui
cd rushi-tui
git remote add upstream ssh://git@github.com/TonyWu20/rushi-tui
```

`origin` is your fork (you push branches here). `upstream` is the
maintainer's repo (you fetch and open PRs against it).

## Step 2 — Dev shell

```sh
nix develop        # or: direnv allow, if you use direnv
```

Success: `cargo`, `clippy`, `lake`, `lean`, `rushi`, `z3` are on PATH, and

```sh
cargo --version
```

prints a version.

Do **not** "fix" the `rushi-common` path dep in `bin/tui/Cargo.toml`. It
points at a sibling kernel checkout on purpose. That is bootstrap state
from `docs/tui-ext-repo-split.md` section 4, item A1. The flake rewrites
it in `patchPhase` for the Nix build. A plain `cargo build` inside the dev
shell uses the sibling layout the flake provides.

## Step 3 — Baseline build and tests

Prove the environment works **before** changing anything:

```sh
cargo build
cargo test -p tui
```

Success: the build is clean and the whole suite is green. The suite holds
42 insta snapshot tests plus roughly 350 logic tests. If the baseline
fails, that is an environment problem. Resolve it before starting feature
work. Without a sibling `rushi-exts` checkout, the extension PTY tests
skip themselves.

## Step 4 — Feature branch

```sh
git fetch upstream
git checkout -b <type>/<short-kebab-name> upstream/main
```

Example: `git checkout -b tui/statusline-blink upstream/main`.

## Step 5 — Implement, with tests

Where code lives:

- `bin/tui/src/` — the TUI (Ratatui).
- `crates/tui-highlight/` — the tree-sitter highlight engine.
- `bin/tui-stream-drt/` + `lean/` — the DRT mirror and its Lean specs.

Rules:

- Record design decisions in `docs/` and keep the `docs/INDEX.md` table in
  sync. Bookkeeping lives in the repo.
- UI behaviour: follow `docs/tui-insta-snapshot-testing.md`.
  - Add the snapshot test.
  - Write the baseline: `INSTA_UPDATE=always cargo test -p tui <name>`,
    or `cargo insta test` + `cargo insta review`.
  - Commit the test and the `.snap` in the same commit.
  - No `std::thread::sleep` in tests. Poll published state against a
    deadline (section 6.3). Mask wall-clock content (section 6.2).
- Lean side: `cd lean && lake build TuiStreamSpec TuiViewportSpec TuiStreamDrt`.

## Step 6 — The gates

All of these must be green before you push:

```sh
cargo build
cargo test -p tui
cargo clippy --workspace --all-targets
cargo fmt -p tui -p tui-highlight -p tui-stream-drt --check
```

The bar is zero clippy warnings across the workspace (see `docs/INDEX.md`).
A failing gate means fix the cause. Do not weaken the test to pass.

Only if the maintainer asks does the PTY smoke gate run. It needs two
sibling checkouts (`rushi-exts` and the kernel), so it is not part of the
normal flow:

```sh
EXTS_ROOT=../rushi-exts python3 scripts/tui-pty-smoke.py target/debug/tui <kernel-root>
```

## Step 7 — Commit

Subject-line style, matching the existing history:

```
<scope>: imperative summary
```

Scopes in use: `tui`, `docs`, `ext`, `lean`, `clippy`. Examples from the
history:

- `tui: windowed, cancellable, size-guarded picker preview pane`
- `docs: add cwd field and RUSHI_CWD to extension protocol`
- `clippy: clear the workspace-wide warning backlog`

Small, focused commits. Commit `Cargo.lock` if dependencies changed. Never
stage `sessions/`, `target/`, `result`, `.direnv/`, or `config*.toml`.
They are gitignored. Check with `git status` that only intended paths are
staged.

## Step 8 — Push and open the PR

```sh
git push -u origin <branch>
```

Then open the cross-fork PR against upstream:

```sh
gh pr create --repo TonyWu20/rushi-tui --base main --head <YOU>:<branch> \
  --title "<scope>: <summary>" \
  --body "$(cat <<'EOF'
What: one to three lines on what changed and why.
Docs: link to the docs/ entry for the design decision, or "none".
Gates (all run locally, no CI):
  cargo build            -> ok
  cargo test -p tui      -> N passed
  cargo clippy --workspace --all-targets -> 0 warnings
  cargo fmt -p tui -p tui-highlight -p tui-stream-drt --check -> clean
Snapshots: list any .snap files added or updated, and why.
EOF
)"
```

Without `gh`, open the same PR in the browser:

```
https://github.com/TonyWu20/rushi-tui/compare/main...<YOU>:<branch>?expand=1
```

The maintainer re-runs the gates when reviewing. The body above is the
evidence that saves a round-trip.

## Definition of done

Run this checklist before opening the PR:

- [ ] `cargo build` clean
- [ ] `cargo test -p tui` green (snapshots committed, no `.snap.new` left)
- [ ] `cargo clippy --workspace --all-targets` zero warnings
- [ ] `cargo fmt -p tui -p tui-highlight -p tui-stream-drt --check` clean
- [ ] `docs/` and `docs/INDEX.md` updated wherever a decision was made
- [ ] `git status` shows no gitignored cruft staged
- [ ] PR opened against the right base branch, with the gate evidence in the body

## Guardrails for the agent

- Never silently decide. When two plausible approaches exist, ask the
  human which one to take.
- Never "fix" the bootstrap `rushi-common` path dep or bump unrelated
  dependencies.
- Keep the branch focused: one change per branch.
- If a gate fails, fix the cause. Do not loosen tests, snapshots, or
  clippy to make the gate pass.
