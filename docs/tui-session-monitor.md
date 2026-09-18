# Session monitor (working / idle / blocked) — discussion

Status: OPEN (2026-09-17 discussion with the user. No implementation
yet). This doc records the requirement, the feasibility findings, and
the placement decision space. No code is written until the user signs
off.

## 1. Requirement

The user runs multiple `rushi` sessions, one per tmux pane, across
projects. Projects use different working directories and possibly
different `config.toml` files. The demand: a way to know the progress
of **all** `rushi` sessions in a glimpse, as a coarse three-state
status.

| Status | Meaning |
|---|---|
| `working` | the session's loop process is alive |
| `idle` | the loop is not running |
| `blocked` | `idle` and the last meaningful event on the log is an error-class event |

Two candidate designs were discussed.

- **Design A — TUI tab row**: a tab row inside the TUI listing all
  sessions, the current one highlighted, each with its status.
- **Design B — standalone monitor program**: a pure program that finds
  session directories, checks the loop, tails `events.jsonl`, and
  prints a dashboard. Invoked from `tmux display-popup` or the shell.
  A Discord push is a possible later sink (out of scope now).

## 2. Necessity

Needed. Today the TUI shows status for its own active session only.
That is the `[working]` bit and the `model working · Ns` row in
`docs/tui-working-status.md`, driven by the `loop_phase` `wait` and
`tools` markers. There is no cross-session view anywhere. No command
in `bin/rushi` or the TUI answers "which of my N sessions is
working, idle, or blocked".

## 3. Availability (what the codebase already provides)

The status is fully derivable from on-disk session artifacts. No
cross-process TUI communication is needed.

- **`working` vs `idle`**: the kernel loop holds an exclusive
  `flock` on `sessions/<n>/.loop.lock` for its whole life. It also
  writes its pid to `sessions/<n>/loop.pid` after the lock. See
  `acquire_lock` in `rust-unix-harness/bin/rushi/src/run_loop.rs`.

  A non-blocking `flock` probe answers "is a live loop holding this
  session". The TUI already does exactly this in
  `rushi-tui/bin/tui/src/port_file.rs` via `loop_lock_is_free`,
  `group_alive`, and `pid_is_loop`.

  A dead loop's stale `loop.pid` is safe. The kernel releases the
  lock when the process dies.

- **`blocked`**: `events.jsonl` is append-only and complete. The
  loop writes a terminal `error` event right before stopping on a
  failure. That is `append_terminal_error` in
  `rust-unix-harness/bin/rushi/src/step/logio.rs`.

  Other error-class events exist too. A `tool_result` with
  `is_error` is usually transient, since the loop routes it back to
  the model and continues. `compaction_failed`,
  `context_exhausted`, and an `assistant_message` with `stop_reason`
  of `error` or `aborted` also mark a bad end.

  The `ext_status` markers such as `loop_phase`, `model_thinking`,
  and `run.refire` are noise. They must be skipped when finding the
  last meaningful event.

- **Typed event parsing**: `rushi_common::event::parse_event` is in
  the kernel crate and is already a pinned git dep of the TUI repo.
  A monitor can reuse it.
- **Write side precedent**: `bin/user` in the kernel appends a
  `user_message` and runs the loop. The monitor is the read-side
  mirror of that.

## 4. Placement options and recommendation

| Option | Fits the "all sessions" demand? | Notes |
|---|---|---|
| **A. TUI tab row** (in `rushi-tui` `bin/tui`) | No | One TUI instance is bound to one `config.toml` and one `[paths] sessions_root`. It can only list its own sessions, not the cross-pane set. It also duplicates what the tmux panes already show and raises the user's objection that `new-session` would silently reuse the launching config for every project. |
| **B. Per-TUI UI extension** (rushi-exts, JSONL protocol) | No | Extensions are spawned by one TUI and receive only that TUI's context. A global dashboard crosses TUI instances, and the protocol has no cross-instance channel. An extension could scan the filesystem itself, but then it is just a monitor wearing the protocol's lifecycle for no benefit. |
| **C. Standalone monitor binary** | Yes | A pure read tool. It probes `.loop.lock`, tails `events.jsonl`, and prints a table. It works from `tmux display-popup`, the shell, and later a Discord notifier. It takes session roots explicitly on the command line, so there is no silent config assumption. |

**Recommendation: C, the standalone monitor binary.**

Where it lives: the kernel repo (`rust-unix-harness`), as a sibling
of `bin/user`. Proposal: `bin/monitor`, or `bin/status` (the name
is open). Rationale: the session-dir format is kernel-owned.
`rushi-common` (typed events) is a kernel crate. `bin/user` is
already the write-side sibling. The TUI repo and the exts repo stay
untouched.

Consequence for the user's two-way choice: the monitor core belongs
to neither of the two options. It is a standalone tool in the kernel.
If the user later wants it inside the TUI, a thin extension or tab
row can render the monitor's output. The status-derivation logic
should not be duplicated into the TUI binary.

## 5. Status-derivation rule (proposed, needs sign-off)

Per session directory, in order.

1. Probe `sessions/<n>/.loop.lock` with `flock(LOCK_EX, LOCK_NB)`.
   If held, the status is `working` and we are done. If free, the
   session is an `idle` candidate and we continue. An optional
   cross-check is the `loop.pid` liveness test, like `pid_is_loop`.
2. Read the tail of `events.jsonl` (a bounded read is enough, since
   the log is append-only). Skip `ext_status` lines.
3. Let `L` be the last non-`ext_status` event.
   - `idle` and `L` is error-class, the status is **`blocked`**.
   - `idle` otherwise, the status is **`idle`**. This includes a
     fresh session with no meaningful events.

**Open decision D1, the error-class set.** Minimum: `error`, the
terminal marker the loop writes before stopping. Candidate
additions: `tool_result` with `is_error` true, `compaction_failed`,
`context_exhausted`, and `assistant_message` with `stop_reason` in
{`error`, `aborted`}. A failing `tool_result` is usually transient,
so counting it would make `blocked` mean "ended or stuck at a tool
error". That may be broader than intended.

**Known coarseness, accepted for a glimpse view.** A session parked
on an `approval_request` still has its loop alive. The run loop
keeps claiming `awaiting_approval`. So it reads `working`, not a
separate `awaiting-approval` state.

## 6. CLI shape (sketch, not a contract)

```
monitor [options] <sessions_root>...

Options:
  --config <path>    read [paths] sessions_root from a config.toml
  --watch [SECS]     poll and redraw (for tmux display-popup)
  --json             machine-readable output
```

Output: one row per session. Each row shows the `name`, the
`status` (`working` / `idle` / `blocked`, color-coded), the last
event ts and kind, and, for `blocked`, the error message. Multiple
`sessions_root` arguments cover the multi-project case. Each root is
scanned independently, so different projects' session sets can be
combined in one glimpse without a shared config.

## 7. Explicitly out of scope (for now)

- A Discord push or two-way control. The `user` binary plus
  `tmux send-keys` path exists for `Claude Code` today. The rushi
  analog is easy but is a later sink for this monitor's output.
- A TUI tab row (design A). Only if the user wants the monitor's
  table rendered inside the TUI later.
- Finer phases inside `working` (`wait`, `tools`, streaming). Those
  belong to the in-TUI indicator in `docs/tui-working-status.md`,
  not to a glimpse dashboard.

## 8. Open decisions (awaiting the user)

- **D0** placement: standalone binary in the kernel repo
  (recommended), or one of the user's two original options (the TUI
  tab row, or a UI extension).
- **D1** the error-class set for `blocked` (section 5).
- **D2** the binary name (`monitor` vs `status`), and whether a
  `rushi <subcommand>` front-end is wanted, or a bare binary is
  enough.
- **D3** where the design doc moves if the binary lands in the
  kernel (copy it, or move it to `rust-unix-harness/docs/`).
