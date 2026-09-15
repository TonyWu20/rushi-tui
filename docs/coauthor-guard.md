# Co-author trailer guard

## Purpose

Two commits in the history carry `Co-Authored-By: Claude ...` trailers
from agent sessions the maintainer did not run. This guard blocks
commit messages and pushes that carry a co-author trailer the
allowlist does not list.

## How it works

Two git hooks and one allowlist file, all under `.githooks/`:

- `commit-msg` — runs on every commit. It scans the message for a
  `Co-authored-by` or `Co-Authored-By` trailer line. It blocks the
  commit when the identity is not in the allowlist.
- `pre-push` — runs on every push. It scans each commit being
  pushed. It blocks the push when a commit carries an unlisted
  trailer. It catches `--no-verify` commits, rebases, and
  tool-made commits that skip `commit-msg`.
- `coauthor-allowlist` — the list of accepted identities, one per
  line. Matching uses the `<email>` when one is present,
  lowercased. Else it uses the whole value. An empty allowlist
  (the default) accepts none.
- `_coauthor_check.sh` — shared scanning logic, sourced by both
  hooks.
- `scripts/install-githooks.sh` — sets `core.hooksPath` to
  `.githooks` in the clone. It is local config, so every clone
  runs it once.

## Policy

- Default: no co-author trailers at all. The allowlist is empty.
- To allow an identity, add one line to `.githooks/coauthor-allowlist`
  and commit it. The change is visible in the history.
- The two polluted commits (`683390f`, `8b032c7`) stay in history.
  Rewriting history is a separate owner decision, since it forces a
  push.

## Known limits

- The hooks are per-clone. `core.hooksPath` is local git config and
  does not travel with the repo. A fresh clone without the
  installer has no guard.
- `git commit --no-verify` skips `commit-msg`. The push is still
  blocked by `pre-push` unless that also uses `--no-verify`.
- Server-side pushes bypass both hooks. They include the GitHub web
  UI, other clones without the hooks, and `git commit-tree`
  pipelines. There is no CI in this repo. That is a documented
  trade-off: local gates only.
