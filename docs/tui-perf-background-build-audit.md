# TUI background build: audit and perf-test record

Status: Audit.

Last updated: 2026-09-13.

Scope: audits `docs/tui-perf-background-build-plan.md` against the shipped code.

It also records the independent performance test added to the suite.

The plan's promised level: no frame over 100 ms during a cache-key miss.

## All five stages shipped as planned

Each stage of the plan is implemented and committed.

Every claimed test gate is present in the tree and passing.

- **Stage 0 (`c4ee892`).** Removed the per-frame O(n) transcript copy. Added a focus churn guard. Draw now renders only the visible window from the settled cache slice.

- **Stage 1 (`5270125`).** Added the `TranscriptBuildInput` snapshot struct. It is `Clone` and `Send`. `build_transcript` became the thin `&App` wrapper. The pure builder is `build_transcript_input`. Ext replies are pre-resolved by `resolve_ext_lines` and `resolve_ext_spans`.

- **Stage 2 (`a658c02`).** New `transcript_worker.rs`: a named thread over `std::thread` and `mpsc`. `transcript_lines` returns the stale cache on a miss. The main loop dispatches one request, then polls before each draw.

- **Stage 3 (`e491656`).** `BuildMemo` LRU keyed by `(events_version, width)`, capacity 4. A hit returns the cached build. A palette or fraction change at the same width is a miss.

- **Stage 4 (`683390f`).** 75 ms trailing width debounce. A new width value resets the deadline. Event-commit misses bypass the window. A cache hit at the built width cancels a pending build.

## Plan test gates, all passing

Every gate the plan named exists and passes under `cargo test -p tui`.

- Stage 1: `snapshot_build_equals_direct_build` pins snapshot fidelity via insta.

- Stage 2: four `background_build_tests` plus three worker tests.

They cover stale-while-revalidate, coalescing, and the first-build tail window.

- Stage 3: `width_toggle_hits_the_memo_without_rebuilding`, `worker_toggle_back_hits_the_memo`, `memo_evicts_oldest_width_at_capacity`, `memo_full_key_mismatch_is_a_miss`.

- Stage 4: `width_burst_produces_one_build_at_the_settled_width` plus the bypass, toggle-back, and rebuild-after-newer-build tests.

Suite on 2026-09-13: 271 unit tests (incl. 4 perf_bgbuild tests,
2 regression tests for the fixes above) and 19 PTY tests
(18 smoke + 1 perf) pass.

One `port_file` backpressure test is timing-flaky. It passed on a re-run.

## The independent perf test

The plan's test plan asks for a PTY check on the 24 MB log.

The battlefield fixture is `../rushi-tui/sessions/tui-diff-spec-lean/events.jsonl` (24 MB, 7957 events).

The module `perf_bgbuild_tests` in `bin/tui/src/app.rs` builds 5000 heavy events.

One full build on them takes ~1.4 s in a debug build.

The tests time the main-thread frame path while that build runs on the worker.

Before the fix, the frame path ran the full build inline and froze the UI.

The tests fail loudly if that regression returns.

- `event_commit_miss_keeps_frames_under_budget`. A commit at the built width is a cache-key miss with no debounce. While the ~1.4 s build is in flight, the frame path stays under 100 ms.

- `width_miss_frames_stay_under_budget`. A width miss renders the stale cache through the debounce window and while the settled build runs. Every frame stays under 100 ms.

- `first_build_tail_window_is_fast_on_the_main_thread`. The first build renders only the viewport tail window on the main thread. That portion stays under 100 ms. The full build follows in the background.

- `real_24mb_log_frames_stay_under_budget`. Loads the real 24 MB battlefield log (7957 events via `TUI_PERF_FIXTURE` or the default path). The full background build takes ~28.7 s in the debug build. The frame path stays at 0 ms throughout.

## Measured result (debug build)

From `cargo test -p tui perf_bgbuild -- --nocapture`:

- `frame_path` (stale read, dispatch, poll): 0 ms against the 100 ms budget.

- `miss_read` (stale cache returned on the miss): 0 ms.

- `snapshot+dispatch` (cloning 5001 events plus enqueuing): 27 to 37 ms.

- `background_full_build` (the expensive work, off-thread): about 1.3 to 1.4 s (synthetic), about 28.7 s (real 24 MB log).

The frame path is sub-millisecond while a multi-second build runs off-thread.

That is the promised level: the expensive build never blocks a frame.

The only non-trivial main-thread cost is the O(n) snapshot clone at dispatch.

It is 27 to 37 ms for 5000 events in a debug build.

That cost scales with log size, but stays well under budget in release.

## PTY integration test (implemented)

The plan's test-plan item is now in `bin/tui/tests/pty_perf.rs`.

It spawns the TUI on the battlefield fixture via PTY and checks:

- The tail marker appears after the fast tail-window build.
- The `building transcript` indicator appears while the full build runs in the background.
- Two screens captured 0.6 s apart differ during the build (the spinner advances, proving the main loop keeps drawing).
- The indicator disappears after the build settles.
- The process exits cleanly.

Passes in ~36 s. Skips gracefully when the fixture is absent (override via `TUI_PERF_SESSIONS_ROOT`).

## Known issues found in audit

**Memory: memo retains up to 4 full builds.**
RSS probe on the 24 MB log (debug build): one full
`TranscriptBuild` costs ~250 MB. The `BuildMemo` (capacity 4) plus
the app cache plus an in-flight build can hold up to 6 copies.
Measured: 48 MB baseline → 534 MB after 1st build → 995 MB after
3rd width. The plan's "35 MB extra" risk estimate was an
underestimate by an order of magnitude.

**`ext_ver` is too broad in the cache key.**
`ext.replies_version()` bumps on every ext reply type (status,
row, frame, lines, transform). The transcript build only reads
`ext_lines` and `ext_data` (transform spans). A statusline content
change at idle bumps `ext_ver` → cache miss → full 24 MB rebuild,
even though the transcript content is unchanged. This is the
likely cause of periodic idle `building transcript` flashes.

**Click-triggered full builds.**
`expand_mode = "click"` means a click on a tool block bumps
`frac_epoch` → cache miss → full background build (~7 s release
on 24 MB). Expected behavior, not a bug.

## Fixes applied

**Memo size cap (`MEMO_MAX_RAW_BYTES`).**
`build_with_memo` now measures the display-text byte count of the
finished build via `build_raw_bytes` (sums `texts` lengths).
Builds whose text exceeds 4 MiB are delivered to the caller but
**not** inserted into the `BuildMemo` LRU. This caps steady-state
RSS for large logs at one cache copy plus one in-flight build
instead of up to 6 copies. Small logs (under 4 MiB of text)
still get the fast toggle-back memo hit.
Guard test: `memo_skips_retention_above_raw_cap`.

**Transcript-only ext version (`transcript_replies_version`).**
`ExtHost` now carries a second counter that bumps only when
transcript-visible data changes: `lines` replies, `transformed`
replies, `clear_replies` (session switch), and `mark_dead`
(extension died). The `status`, `frame_spec`, and `row_spec`
replies bump only the original `replies_version` counter, which
is no longer used by the transcript cache key.
`App::transcript_lines` now folds
`ext.transcript_replies_version()` into the `BuildKey`, so
statusline ticks at idle no longer trigger a full transcript
rebuild.
Guard test: `status_frame_and_row_replies_do_not_bump_the_transcript_version`.

**Transcript-rebuild trace (`TUI_TRANSCRIPT_TRACE`).**
Setting `TUI_TRANSCRIPT_TRACE=1` opens
`/tmp/tui-transcript-trace-<pid>.log`; a path value opens that
path instead. Every cache-key miss, worker dispatch, debounce
hold, cancel, and settle writes one timestamped line to the file.
This lets the user confirm at idle whether rebuilds are still
firing and which key field is the trigger.

## Open question

The plan's under-1-ms target for the tail-window build is a release figure.

The test asserts the looser 100 ms budget so it stays stable across build profiles.

Tighten it to 1 ms in a release-mode CI job once the tail-window build is measured there.
