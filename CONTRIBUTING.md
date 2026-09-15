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

Install the coauthor-guard hooks, once per clone:

```sh
scripts/install-githooks.sh
```

Success: `git config --get core.hooksPath` prints `.githooks`. The
hooks reject `Co-authored-by` trailers the allowlist does not list.
The allowlist is `.githooks/coauthor-allowlist`. It is empty by
default, so no co-author trailer is accepted. See
`docs/coauthor-guard.md` for the policy.

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

`rushi-common` in `bin/tui/Cargo.toml` is a **git dep on the kernel
repo** (`https://github.com/TonyWu20/rushi`, the `rushi-common` package
in `crates/rushi`). The exact kernel rev is pinned in `Cargo.lock`. A
plain `cargo build` fetches it from GitHub, so no sibling kernel
checkout is needed. To move the TUI to a newer kernel rev, run
`cargo update -p rushi-common` (the pin moves to the kernel's current
`main` head) and re-run the Step 6 gates. Do not hand-edit the dep
URL/branch or its lock pin.

## Step 3 — Baseline build and tests

Prove the environment works **before** changing anything:

```sh
cargo build
cargo test -p tui
```

Success: the build is clean and the whole suite is green. The suite holds
42 insta snapshot tests plus roughly 350 logic tests. If the baseline
fails, that is an environment problem. Resolve it before starting feature
work. The extension PTY cases read the kernel's `scripts/ext-fixture`
inputs, so set `KERNEL_ROOT` to the **absolute** path of any kernel
checkout (e.g. after `git clone https://github.com/TonyWu20/rushi`) and
`EXTS_ROOT` to the absolute path of a `rushi-exts` checkout. Without
those, the extension PTY cases print `SKIP` and count as no-ops. Use
absolute paths: `cargo test` runs each test binary with cwd `bin/tui`,
so relative values would resolve against the wrong directory.

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

Only if the maintainer asks does the PTY smoke gate run. It needs a
`rushi-exts` checkout and a kernel checkout (any clone of
`https://github.com/TonyWu20/rushi`, passed as `<kernel-root>`), so it is
not part of the normal flow:

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

No `Co-authored-by` or `Co-Authored-By` trailers in commit messages.
The coauthor-guard hooks reject a trailer the allowlist does not
list. The default allowlist is empty. To allow an identity, add one
line to `.githooks/coauthor-allowlist` and commit that file.

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
- Do not hand-edit the `rushi-common` git dep or its `Cargo.lock` pin.
  To move to a newer kernel rev, run `cargo update -p rushi-common` and
  re-run the gates. Do not bump unrelated dependencies.
- Keep the branch focused: one change per branch.
- If a gate fails, fix the cause. Do not loosen tests, snapshots, or
  clippy to make the gate pass.
- Never add `Co-authored-by` trailers to commit messages. The
  coauthor-guard hooks reject any trailer the allowlist does not
  list.
