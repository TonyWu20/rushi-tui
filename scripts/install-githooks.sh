#!/bin/sh
# - Installs the coauthor-guard git hooks for this clone.
# - Points core.hooksPath at .githooks/ so git runs the versioned hooks.
# - Run it once per clone: sh scripts/install-githooks.sh
# - The hooks block co-author trailers that are not in .githooks/coauthor-allowlist.

set -eu

cd "$(dirname -- "$0")/.."

for f in .githooks/commit-msg .githooks/pre-push .githooks/_coauthor_check.sh
do
  if [ ! -f "$f" ]
  then
    echo "install-githooks: missing $f" >&2
    exit 1
  fi
  chmod +x "$f"
done

git config core.hooksPath .githooks

echo "coauthor-guard active: $(git config --get core.hooksPath)"
echo "Allowlist: .githooks/coauthor-allowlist (empty = no co-author trailers allowed)"
