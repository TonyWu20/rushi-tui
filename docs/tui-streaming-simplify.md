# TUI live stream into the main content area

Status: Open. A simplification of the pinned live-stream block
(`docs/tui-streaming-response.md`). Raised 2026-09-14.
Not work for that session.

## 1. What to change

Stream the model response into the main content area. This replaces
the pinned live-stream block. That block sits between the transcript
and the model-status row. It grows and collapses while a response is
streaming.

## 2. Why

Two problems, from daily use.

- The live block does not scroll. When a streamed response outgrows
  it, the early lines fall off the top. Those lines cannot be
  reached. They are often the ones that grabbed attention.

- The block grows when the agent starts a response. It collapses
  when the response completes. Each transition shifts the transcript
  layout. The cursorline position in scroll and browse mode drifts on
  every transition.

## 3. Proposal

Two changes.

- Stream the whole main content area. Render the in-progress
  response where it will settle, in the transcript. Not in a
  separate pinned block. With no separate grow and collapse region,
  the cursor no longer drifts on the start and complete transitions.

- Use one thinking toggle. `Ctrl+T` collapses and expands all
  thinking blocks. This includes the live-streaming one. Today the
  settled blocks and the live block have separate visibility paths.
  Unify them.

## 4. Scope

- Design doc only. Not implemented this session.

- Supersedes the pinned-block render in
  `docs/tui-streaming-response.md` when it lands.

- The stream file stays unchanged.

- The `harness` and `bin/model` producer side stays unchanged.

- The log contract stays unchanged.
