# TUI working status

Status: shipped (2026-09-16). The `working` state is derived in
`bin/tui/src/render.rs` (`PhaseState::Working`, `phase_state`,
`phase_bit`, `working_row_text`). There is no kernel change and
no new `ext_status` marker. The base contract is
`docs/tui-model-wait-indicator.md` (section 2, the state table,
and section 4, the failure modes). The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-16 item,
closed with a Shipped note).

## 1. Purpose

The `wait` state covers two sub-phases of a model call. The TUI
shows `waiting for model · Ns` for both. The first is the sent
request pending on the server, with no response data yet. The
second is the response streaming back, with deltas arriving
through the session's `.model-stream`
(`docs/tui-streaming-response.md`). This request adds a
`working` state: `wait` is the request pending, and `working` is
the response streaming.

## 2. Marker source (decision 2026-09-16)

No kernel change. The `ext_status` marker vocabulary stays
`wait`/`tools`. The kernel emits no `working` marker. Per the
refinement policy (`docs/refinement-policy.md`, P1a condition 3
and the P4 pre-test), a TUI-only effect gets a rendering rule
instead of a protocol change. The TUI already observes both
inputs: the last `loop_phase` value (the log) and the session's
`.model-stream`. The `working` state is derived, not emitted.

## 3. Label (decision 2026-09-16)

The state is named `working` as proposed. The earlier collision
note is withdrawn: `running-unknown` is the internal
`PhaseState::RunningUnknown` name only. Its user-visible strings
are `[running]` and `Working...`, and the loop emits
`wait`/`tools` markers in practice, so that fallback has never
surfaced for the user.

## 4. Rendering rule

One new row in the base contract's state table, derived at draw
time:

| State | Condition | Title bit | Working row |
|---|---|---|---|
| `working` | loop running, last marker `wait`, and the session stream buffer is open (`App::stream_buf()` is `Some`: at least one `.model-stream` delta line was read) | `[working]` | `model working · Ns` |

- The derivation applies only inside `wait`. A missing marker, or
  a value outside `wait`/`tools`, stays `running-unknown`
  regardless of the stream buffer.
- `working` persists for the whole in-flight window: from the
  first delta to settle. It includes the window after the `done`
  line where the stream is complete but the `assistant_message`
  has not landed. It ends when `clear_stream` runs (the settle
  event, the file deletion, or the error/cancel path).
- The timer keeps the `wait` marker timestamp: `N` counts the
  whole model call from the marker, not from the first delta. The
  P5 span-format rules of the base contract apply unchanged. The
  number never resets at the `wait` to `working` transition.
- The stale-file case is safe: the derivation needs the running
  bit plus the `wait` marker. A dead loop with an undeleted
  stream file is `idle`.

## 5. Open point

- Row label (decided 2026-09-16): the proposed `model working · Ns`
  is used, to pair with `waiting for model · Ns`. The one-word
  alternative `working · Ns` was not taken.

## 6. Tests

- running + marker `wait` + open stream buffer: bit `[working]`,
  row `model working · Ns`.
- running + marker `wait` + no stream file: the `wait` row is
  unchanged.
- The `done` line is read but not settled: still `working`.
- Settle (`assistant_message`) clears the buffer: the state falls
  back to the marker-derived one (`wait` until the next marker,
  `tools`, or `idle`).
- running + no marker + open stream buffer: `running-unknown`.
- Running bit clear + a stale stream file: `idle`.
- A restart onto a running session with a non-empty stream file:
  `working` on the first draw.
- Timer continuity: `N` during `working` counts from the `wait`
  marker. The P5 format holds.

On ship: the base contract's state table gains the `working`
row. `PhaseState` gains the variant. `phase_state`, `phase_bit`,
and `working_row_text` in `bin/tui/src/render.rs` gain the arm.
No kernel diff. The e2e marker test is unchanged.

## Properties

Lean-style invariants for this spec. One property per
non-trivial invariant. Each property is observable: given an
input, an output guarantee.

P1. working-state: given the loop is running, the last
    `loop_phase` value is `wait`, and the stream buffer is open,
    observe the title bit reads `[working]` and the working row
    shows `model working · Ns`.
P2. wait-pending: given the loop is running, the last value is
    `wait`, and no stream file exists, observe the state stays
    `wait` and the row keeps `waiting for model · Ns`.
P3. done-window: given the `done` line is read but the
    `assistant_message` has not landed, observe the state stays
    `working`.
P4. settle-fallback: given the settle event clears the stream
    buffer, observe the state falls back to the marker-derived
    one.
P5. unknown-marker: given the loop is running, no marker (or a
    value outside `wait`/`tools`), and an open stream buffer,
    observe the state stays `running-unknown`.
P6. stale-file: given the running bit is clear and a stale
    stream file exists, observe the state is `idle`.
P7. timer-continuity: given the `wait` marker is 30 s old and
    the state is `working`, observe the row shows the span from
    the marker, not from the first delta.

## Verification

Each property maps to its proof. `proven` means the cited test
exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | working-state | `working_state_shows_the_bit_and_the_row` in `bin/tui/src/render.rs` (state derivation, bit, row text); `a_restart_onto_a_running_session_shows_working` covers the restart-onto-running-session first-draw case | proven |
| P2 | wait-pending | `wait_marker_without_a_stream_buffer_keeps_the_wait_row` and `a_missing_stream_file_keeps_the_wait_state` in `bin/tui/src/render.rs` | proven |
| P3 | done-window | `the_done_line_keeps_the_state_working` in `bin/tui/src/render.rs` | proven |
| P4 | settle-fallback | `settle_clears_the_buffer_and_falls_back_to_the_marker` in `bin/tui/src/render.rs` | proven |
| P5 | unknown-marker | `an_unknown_marker_stays_unknown_with_an_open_buffer` in `bin/tui/src/render.rs` | proven |
| P6 | stale-file | `a_stale_stream_buffer_is_idle_when_the_loop_stops` in `bin/tui/src/render.rs` | proven |
| P7 | timer-continuity | `the_working_timer_counts_from_the_wait_marker` in `bin/tui/src/render.rs` (30 s span from marker; P5 format at 90 s) | proven |

## Gate

The acceptance commands. All must exit 0 on ship.

```
cargo build
cargo test
scripts/cache-e2e.sh
```
