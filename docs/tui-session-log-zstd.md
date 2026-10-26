# The zstd session log in the TUI

Status: implemented (2026-10-21), issue #39. The TUI reads
session logs through the shared kernel reader
(`rushi_common::logstream::read_log_text`) and tails zstd logs
on decoded lines. The kernel dep is pinned to `0.1.7` in
`bin/tui/Cargo.toml` + `Cargo.lock`.

This is the TUI's share of kernel PR #54
(`TonyWu20/rushi#54`, the zstd session log, decisions D1-D9 and
properties P1-P7 in the kernel `docs/session-log-compression.md`).
Decision D6 selected the consumer model: the rushi-tui repo takes
the zstd stream layer plus the raw-text fallback. A consumer
does not copy the layout logic.

## 1. What the kernel changed

The session log (`events.jsonl`, `tools.jsonl`) is now one zstd
stream: a concatenation of zstd frames.

- P1: `LogLine::commit` writes one frame per commit. The frame
  is the locked write unit; FT-005 holds.
- P2: the format is decided at creation and held for the file's
  life. A new file becomes a zstd stream; a legacy plain-text
  log stays plain for its life. No file mixes both.
- P3: the reader sniffs the four zstd magic bytes.
  `rushi_common::logstream::read_log_text` returns JSONL text
  from either layout.
- P4: a truncated final frame stops the read at the last
  complete frame. The partial line drops at parse time.
- P5/P6/P7: the idle consolidation pass rewrites the stream as
  one level-19 frame under the log-line lock, verifies the
  round-trip, and renames over the original. The decoded text
  is unchanged; writers append frames after it.

## 2. The TUI read sites

Every session-log read site moved to the shared reader:

| Site | Before | After |
| --- | --- | --- |
| `FileSessionPort::read_events` | raw bytes, split on `\n` | `logstream::read_log_text`, split the text |
| `FileSessionPort::log_line_count` | raw byte newline count | decoded-line newline count |
| live tailer (plain logs) | byte offset | unchanged |
| live tailer (zstd logs) | n/a | decoded-line position (section 3) |
| trace read-backs (`tui-trace.jsonl`) | `read_to_string` | `logstream::read_log_text` |
| test read-backs of `events.jsonl` | `read_to_string` | `logstream::read_log_text` |

The in-progress-tail rule is unchanged and now covers both
layouts: a read that races an in-flight append leaves the last
line without its trailing newline; a zstd torn-tail frame shows
the same shape (a partial final line). The tail segment drops
until the next read.

`read_events` fails on a non-UTF-8 plain log: `read_log_text`
is strict, and the kernel's own read sites took the same
contract in PR #54. Per-line malformed JSON still surfaces as
a `MalformedLine` event, not an error.

## 3. The tailer

The plain path is unchanged: byte offset, partial-line carry,
the alignment probe, backpressure by held bytes.

The zstd path tails on a decoded-line position, because the
byte stream is not a line stream:

- `watch` resolves "from the end" in the log's own position
  units: byte offset for a plain log, complete-line count for a
  zstd log.
- Each poll decodes the whole stream when the file length
  changed since the last decode, and emits the complete lines
  past the recorded position. An unchanged length means no new
  complete line: a torn frame stalls progress only while its
  write is in flight, and that write grows the file.
- Backpressure keeps the line position: the position advances
  past a line only once it emitted (or was blank). The next
  poll rereads the stream and picks up where this one stopped.
- The position survives the idle consolidation (P5): the
  rewrite keeps the decoded text byte-identical, so the line
  count does not shrink and nothing re-emits. A true rotation
  (a replacement, shorter log) shrinks the line count and
  resets to the start, like the plain path.
- The layout is sniffed from the head on the first bytes and on
  every rotation. A reappearing zstd file (a Gone-then-recreated
  log) is a new inode: its position state resets and the layout
  re-decides. The plain path keeps its byte position across a
  recreate; its alignment probe handles the new inode. P2 keeps
  the layout stable in between. The zstd cursor carries the line
  position just past the emitted line, the plain cursor the byte
  offset, per the port contract.
- The first creation of a fresh session log is not a
  re-appearance. The watch can attach before the log file
  exists; the first commit creates it. The position state the
  first poll established stands. No `Resumed` item, no reset.
  A reset there would re-decide from zero and re-emit the lines
  the first poll already delivered (the brand-new-session
  double-message report, 2026-10-24). The reset applies only to
  a log that was observed, went Gone, and came back.

Known hole (shared with the plain path): a rotation to a
*longer* file is not detected; the tailer keeps its position
and skips content. Both paths predate this change in that
behavior, and a session log that is replaced by a longer one
under the tailer is an unsupported operation.

## 4. Out of scope

- `.model-stream` (the session-local model stream channel):
  kernel PR #54 does not touch it; the TUI's stream poll stays
  a plain read.
- Arbitrary file previews in the picker: a human browsing a
  zstd log sees frame bytes. A separate concern.
- The `tui-stream-drt` DRT mirror: std-only, no kernel dep,
  no log reads.

## 5. Build

The `zstd` crate (a transitive dep of `rushi-common 0.1.7`)
links `libzstd`. The flake adds `pkgs.zstd` to the package
`nativeBuildInputs`, mirroring the kernel flake. The dev shell
carries `stdenv.cc`, so the plain `cargo build` resolves the
bundled C source.

## 6. Tests

- `read_events` on a zstd stream and on a legacy plain log;
  torn-tail drop (P4).
- `log_line_count` on a zstd stream, torn frame excluded.
- Tailer delivery on a zstd log with the line-position cursor.
- Tailer position across a `consolidate` rewrite (P5/P6): no
  re-emit of the pre-rewrite lines, the post-rewrite frame
  still arrives.
- `read_tail_zstd` disconnects on a dropped receiver.

## Related

- Kernel `docs/session-log-compression.md` — the spec, the
  decisions, and the consumer note.
- `docs/tui-ext-repo-split.md` section 4 item A1 — the kernel
  dep pin rule.
- `docs/INDEX.md` — the repo-state entry for this change.
