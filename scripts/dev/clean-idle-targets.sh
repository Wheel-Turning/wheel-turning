#!/usr/bin/env bash
# Frees disk by clearing the Rust build folder (target/) of every worktree of this repository
# that has not been built in the last $IDLE_HOURS hours (default 12). Each worktree keeps its own
# target/, 20 to 120 GB, and parallel sessions filled the owner's disk with them (October 12,
# 2026). A target/ is only a cache: the next build in that worktree recreates it.
#
#   scripts/dev/clean-idle-targets.sh [--dry-run]
#
# The main checkout is never touched, since its target/ holds the binaries the local worker and
# alpha run from (target/main-cli-*, target/alpha).
set -euo pipefail
IDLE_HOURS=${IDLE_HOURS:-12}
DRY=${1:-}
main=$(git worktree list --porcelain | awk '/^worktree /{print substr($0, 10); exit}')
freed=0
while IFS= read -r wt; do
  [ "$wt" = "$main" ] && continue
  t="$wt/target"
  [ -d "$t" ] || continue
  # A build touches files near the top of target/; anything newer than the cutoff means active.
  if [ -n "$(find "$t" -maxdepth 3 -newermt "-${IDLE_HOURS} hours" -print -quit 2>/dev/null)" ]; then
    echo "active   $wt"
    continue
  fi
  kb=$(du -sk "$t" 2>/dev/null | cut -f1)
  if [ "$DRY" = "--dry-run" ]; then
    echo "would clear $wt ($((kb / 1048576)) GB)"
  else
    (cd "$wt" && cargo clean -q) && echo "cleared  $wt ($((kb / 1048576)) GB)"
  fi
  freed=$((freed + kb))
done < <(git worktree list --porcelain | awk '/^worktree /{print substr($0, 10)}')
echo "total $((freed / 1048576)) GB"
