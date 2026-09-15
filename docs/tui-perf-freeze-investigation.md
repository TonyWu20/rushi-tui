# TUI performance freeze investigation.

Status: Investigation.
Last updated: 2026-09-13.
Scope: `bin/tui` transcript build and cache.
Basis: user-reported freezes on the 24 MB session log.

## Summary.

The TUI freezes on every cache-key miss.
The miss runs a full transcript build on the render thread.
On the 24 MB log that build takes about 7 seconds.

In focus mode the main loop called `set_focus_block()` each frame.
Each call bumped `frac_epoch`, invalidating the cache.
Every frame was therefore a cache miss.
CPU stayed at 100% and the UI stopped responding.

Width changes cause a one-time freeze.
Entering or leaving browse mode shifts the text width.
Scrolling past the top toggles the position bar.
Each width change triggers a full rebuild.

Two fixes already landed remove the chronic per-frame cost.
The remaining problem is the one-time full build on each miss.
A background build worker is the proposed solution.

## Symptom.

The report covers the `tui-diff-spec-lean` session.
Its log is 23 MB with 7,957 events.
At width 120 the transcript spans about 146,000 lines.
The log path is `../rushi-tui/sessions/tui-diff-spec-lean/events.jsonl`.

What the user saw:

- Focus mode pegs CPU at 100% with no response.
- Click mode freezes about 10 s on browse enter and leave.
- Mouse scrolling freezes about 10 s in click mode.
- Browse plus `gg` freezes once, then in-browse motion is normal.

More observations:

- Idle typing in the input box does not freeze.
- Switching the highlight engine from `syntect` to `tree-sitter` made no change.
- Removing all UI extensions made no difference.

## Root cause.

The transcript cache stores the last full build result.
Its key is a tuple of six fields:
`events_version, width, ext_ver, palette_level, palette, frac_epoch`.

On any key mismatch the cached build is dropped.
The full `build_transcript` then runs synchronously on the render thread.
It blocks the UI until the entire history is re-rendered.

That build is O(all in-memory events).
On the 24 MB log it costs about 7 s in release mode.
Every cache-key miss therefore freezes the UI for that duration.

### Where the time goes.

Measured in release on the 24 MB log at width 120.
The log has 146,149 lines and 7,957 events.

- Prep phase: about 1 ms.
- Per-event loop: about 7,300 ms.
- Texts stringification: about 95 ms.
- Total: about 7,400 ms.

The per-event loop accounts for nearly all the cost.
It wraps each markdown body and assembles the `Line` list.
This step is independent of the highlight engine.

### Triggers that miss the cache.

Each trigger below changes one key field.
On the 24 MB log each costs a one-time freeze of about 7 s.

- Browse enter or leave: gutter and bar shift the width.
- Mouse scroll crossing the top: the position bar toggles.
- Each committed event: `events_version` bumps.

- Palette or scheme change: palette or level field changes.
- Tool fold or expand toggle: expanded height shifts the width.
- Focus before the fix: `frac_epoch` bumped every frame.

Browse `gg` and in-browse scrolling do not miss the cache.
The `scroll` offset is not part of the cache key.
That matches the observation that in-browse motion is normal
after the initial freeze.

Idle typing in the input box does not miss the cache.
The input box state is not part of the cache key.

## What was ruled out.

The user ran two experiments to narrow the cause.

- Switching `syntect` to `tree-sitter` made no difference.
  The highlight engine is a small fraction of build cost.
- Removing all UI extensions made no difference.
  The `ext_ver` field is not the trigger in this setup.

Both results confirm the core per-event build is the cost.

## Already fixed this session.

Two chronic per-frame costs are now eliminated.
Both are verified by the build gate and full test suite.

First fix: per-frame O(n) copy removed.
The draw path no longer copies the full transcript with `to_vec()`.
It no longer rebuilds the `texts` vector on every frame.
It now builds only the visible window from the settled cache slice.

- The `texts` are built once per `build_transcript` call and cached.
- Measured warm frame dropped from about 900 ms to under 1 ms.

Second fix: focus-mode churn guard.
`set_focus_block()` no longer bumps `frac_epoch` unconditionally.
The main loop calls it every frame in focus mode.
Before the guard, every frame was a cache miss.

- A guard now returns early when the focus target is already settled.
- It also checks that no other block is expanded.
- Focus mode no longer pegs the CPU.

## Remaining problem and proposed fix.

The remaining freeze is the one-time synchronous full build.
It runs on the render thread when the cache key changes.
The two fixes above stop per-frame misses.
They do not make the one-time build faster.

Two options are available.

Option A: background build worker.
Build the full transcript in a worker thread from a snapshot.
Keep drawing the last good window meanwhile.
Swap in the result when the worker finishes.

- This makes every trigger above non-blocking.
- It holds two transcripts briefly (about 35 MB extra on this log).
- The view briefly shows the previous width or state.

Option B: per-width cache.
Keep the last few width variants cached.
Repeated browse toggles become instant.
The first build of each new width still hitches.

Option A subsumes option B and fully resolves the symptom.
It is the larger change.
It needs a snapshot of the inputs the build reads.
It also needs a safe swap that respects in-flight animation state.

## Open decision.

Decision (2026-09-13): ship the two surgical fixes now.
Then implement Option A (background build worker) as the
follow-up. Option B (width-keyed memo) is the worker's
fast path. The full fix plan lives in
`docs/tui-perf-background-build-plan.md`.

Ecosystem evidence for the background-build shape:
`docs/tui-large-content-research.md`. No surveyed app freezes its
UI on a content miss. They all pair stale-content rendering with
off-thread rebuilds.

The copy fix and focus guard are already in the tree.
Both are ready to commit.

## Measurements.

All values below are release-mode numbers on the 24 MB log.

- Full `build_transcript`: about 7 to 8 s, run to run.
- Per-event loop: about 7,300 ms, the dominant phase.
- Texts stringification: about 95 ms, negligible.

- Warm per-frame draw after the fix: under 1 ms.
- It was about 900 ms before the fix.
- A per-frame miss before the focus fix: about 7 s.
