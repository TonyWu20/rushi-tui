# `loop.meta` identity record in the TUI probe and stop

Status: implemented (2026-10-05), issue #31. The TUI's external
loop probe and stop prefer the kernel's `loop.meta` identity
sidecar when it is present. The session argument accepts an
explicit session dir. This is the TUI's share of the kernel's
single-source-of-identity fix (kernel issue #44, shipped in
kernel PR #46).

## 1. The observed bug

The episode is in kernel issue #44 (2026-10-04, the tv-rushi
workflow). `tv rushi-sessions` `send_message` starts a detached
loop on an idle session. The starter is
`setsid rushi run <abs-session-dir> <msg>`. After that, a TUI
attached to that session cannot get the loop status.

Two verified mechanisms:

- Linux: the TUI's `loop_identity_ok` needs the bare session name
  as a *whole argument* in the loop's `/proc/<pid>/cmdline`. A
  loop started with the absolute session dir never passes that
  check. A live pid plus a held lock still probes idle.
  `stop_external_loop` then no-ops.
- Both OSes: a consumer probes the session dir *it* resolved
  (its own config ladder, its own CWD). When that dir differs
  from the dir the external starter used, the probe checks a free
  lock and reports idle forever.

## 2. The kernel's side (kernel PR #46)

The loop process writes a flat TOML record beside the bare
`loop.pid`:

```toml
pid = 12345
session = "issue-29"
dir = "/abs/sessions/issue-29"
binary = "rushi"
started = 1758790200
```

The lock stays the authority for liveness. `loop.meta` is the
authority for identity. The bare `loop.pid` stays for old
readers. `rushi-common` exposes the reader and a name-or-dir
match in its `loop_meta` module.

The record is per-dir, not a global index. It closes the
identity half: given a dir, who is the live loop here. It does
not close the discovery half: given a bare name and a CWD, which
dir is the session. The discovery half is consumer-side work.
This issue covers the TUI's part of both.

## 3. The TUI's two halves

### 3.1 Identity half (the needed change)

When `<dir>/loop.meta` exists, `external_loop_pid` and
`stop_external_loop` prefer it. The lock-first order is
unchanged:

- the lock check still gates the probe. A free lock means no
  live loop. The probe returns `None` without reading the
  record.
- the recorded pid is the liveness fact. The probe checks it
  with a `kill -0` on the resolved process group
  (`group_alive`). The recorded pid may be a group member, not
  the leader. The group resolves through `getpgid`, as the
  fallback path already does.
- the recorded canonical name-or-dir is the identity fact.
  `matches_name_or_dir` matches the probe target against the
  recorded `session` name or the recorded `dir` (or its
  canonical form). That replaces the Linux whole-arg cmdline
  check in `loop_identity_ok` and `cmdline_names_session` for
  the record path.
- when the record is absent, the probe and stop fall back to
  the current bare pid path. The fallback uses the bare
  `loop.pid` plus the cmdline (Linux) or binary-name (macOS)
  identity check. Old loops still work.
- a corrupt or unreadable record degrades to the same
  fallback. A corrupt record must not masquerade as an
  identity. The fallback keeps its own identity check, so it
  stays safe.

The stop path kills the record's pid group. That holds even
when the record's pid differs from the bare `loop.pid` value.

### 3.2 Discovery half (complementary)

The CLI takes a bare session name (`session: Option<String>`
in `main.rs`). `session_dir` now accepts an explicit session
dir:

- an absolute path is used as-is. It is a session dir the
  caller resolved, not a name to join under the configured
  sessions root.
- everything else keeps the current rule. It joins under
  `sessions_root`. It rejects `..` components and the empty
  id.

The tv-rushi `open` action hands the discovered dir to the TUI
through this argument. That closes the wrong-dir case, which
`loop.meta` alone does not close. The kernel's `run` command
accepts the abs-dir form. A TUI-spawned loop for an explicit
dir passes the dir to the loop command. The kernel expects
that form.

## 4. Where the code lives

- `bin/tui/src/port_file.rs`: the `LoopMeta` record type, the
  `read_loop_meta` reader, and `matches_name_or_dir`. The
  storage detail stays inside the port. No path or file name
  is referenced above the port (docs/tui.md section 10,
  guardrail 3).
- `bin/tui/src/port_file.rs` `external_loop_pid` and
  `stop_external_loop`: the record-preferred probe and stop.
- `bin/tui/src/port_file.rs` `session_dir`: the explicit-dir
  mapping.
- `bin/tui/src/main.rs`: the session-argument doc. The
  argument was already `Option<String>`. The validation moved
  from "reject absolute" to "use absolute as-is".
- `bin/tui/src/port.rs`: the `SessionPort::session_dir`
  contract note.

## 5. The reader is local, not from `rushi-common`

The TUI pins `rushi-common` to the crates.io release `0.1.5`
(issue #22). That release predates kernel PR #46. The
published crate has no `loop_meta` module. The flake is
hostable: it builds against the crates.io kernel only, with no
raw source-tree input (docs/tui-ext-repo-split.md section 4).
So the TUI keeps its own reader for the record. It matches the
kernel's field set and match semantics.

The record contract is the flat five-field TOML above. A
missing field or a torn read is a corrupt record. It degrades
to the fallback. When the kernel publishes the `loop_meta`
module in a newer release, this type can move to the shared
crate. The local reader then deletes.

## 6. Test plan (issue #31)

- Producer: `external_loop_pid_matches_a_loop_started_with_an_abs_dir`
  spawns a live group. Its command line carries the absolute
  session dir, and it holds the session lock. The test asserts
  the legacy cmdline check fails for the bare name. It then
  asserts the probe claims the group via the record. It also
  probes with the abs dir itself (the discovery half). The
  stop path kills the group.

- Fallback: the existing no-record tests pin the bare pid
  path. These are `external_loop_pid_*`, `stop_external_loop_*`,
  and `pid_is_loop_*`. `loop_meta_absent_reads_as_none` pins
  the absent-record case.

- Stop: `stop_external_loop_uses_the_meta_pid_not_the_bare_pid_file`
  records the group member in the record and the group leader
  in the bare `loop.pid`. The stop kills the record's pid and
  reports it.

- Discovery: `session_dir_accepts_an_explicit_abs_dir_and_rejects_traversal`
  pins the as-is mapping and the traversal rejection.
  `read_events_reads_from_an_explicit_abs_dir` and
  `append_event_creates_the_dir_for_an_explicit_session_dir`
  pin that the TUI opens a session from an explicit dir.

All of these run on the host OS. The record reader and the
lock-held-in-process probe are OS-agnostic. The spawned-group
tests are Linux-only. They match the existing loop-group
tests.

## 7. Policy mapping (kernel refinement policy, P6)

- P0: the tv-to-TUI episode reproduces. A loop started with an
  absolute dir probes idle in the TUI. Its stop no-ops.
- P4: unblocks the tv-plus-TUI workflow. It removes the manual
  re-attach step where a human restarts the TUI from the
  loop's working directory.
- P3: the identity fact already appears in the probe and the
  stop path. Each re-derives it from the cmdline. This change
  makes both read one recorded fact.
