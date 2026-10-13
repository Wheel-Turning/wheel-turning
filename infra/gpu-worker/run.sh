#!/usr/bin/env bash
# From the PC: sets up a GPU machine and drains the audio queue on it (Spec 01, GPU access).
#
#   infra/gpu-worker/run.sh ubuntu@<address> [workers] [hours]
#
# `hours` stops the workers after that long, so a run cannot pass its share of the spending cap.
#
# The machine is one the owner rented or launched, reached by SSH with the owner's key. This
# script copies the repository's committed files to it, runs setup.sh, sends the Hugging Face
# token over SSH into a file only the owner can read, and runs work.sh with a reverse tunnel so
# the machine reaches the PC's database on its own localhost:55433; the database is never
# exposed. Every two minutes, and at the end, it copies new and changed transcripts back into
# data/objects, each replaced whole (see sync_objects).
# Delete the machine afterwards: it is billed by the hour.
set -euo pipefail
HOST=${1:?usage: run.sh user@host [workers] [hours]}
WORKERS=${2:-3}
HOURS=${3:-0}
MAIN=$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
# The key the provider was given; ~/.ssh/wt-gpu unless WT_GPU_SSH_KEY names another.
KEY=${WT_GPU_SSH_KEY:-$HOME/.ssh/wt-gpu}
SSH=(ssh -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=30)
[ -f "$KEY" ] && SSH+=(-i "$KEY")
step() { echo "=== $(date +%H:%M:%S) $*"; }

step "copy the repository"
git archive HEAD | "${SSH[@]}" "$HOST" 'mkdir -p ~/wt && tar -x -C ~/wt'
step "set up"
# Exit 3: the driver was installed and the machine must reboot first.
code=0
"${SSH[@]}" "$HOST" 'bash ~/wt/infra/gpu-worker/setup.sh' || code=$?
if [ "$code" = 3 ]; then
  step "reboot for the driver"
  "${SSH[@]}" "$HOST" 'sudo reboot' || true
  sleep 30
  until "${SSH[@]}" -o ConnectTimeout=10 "$HOST" true 2> /dev/null; do sleep 10; done
  "${SSH[@]}" "$HOST" 'bash ~/wt/infra/gpu-worker/setup.sh'
elif [ "$code" != 0 ]; then
  exit "$code"
fi
# Route (c): the files a sangha gave the owner, under data/storage, so the machine's workers can
# copy matched ones into scratch (Spec 03, the storage adapter). work.sh removes the machine's copy
# when it exits; the originals on the PC are the owner's, deleted by the owner after the run.
if [ -d "$MAIN/data/storage" ] && [ -n "$(ls -A "$MAIN/data/storage")" ]; then
  step "copy the storage files ($(du -sh "$MAIN/data/storage" | cut -f1))"
  tar -C "$MAIN" -cf - data/storage | "${SSH[@]}" "$HOST" 'mkdir -p ~/wt && tar -x -C ~/wt'
fi
step "token"
grep '^HF_TOKEN=' "$MAIN/.env" | cut -d= -f2- | tr -d '\r\n' |
  "${SSH[@]}" "$HOST" 'umask 077 && cat > ~/.wt-hf-token'

# Each sync unpacks into a staging folder of its own, and only then moves new or changed files
# into data/objects, each with a rename. Unpacking straight into data/objects rewrote every
# transcript in place every two minutes, so a local job could read one half written and die on
# it (October 11, 2026). A rename replaces a file whole, and an unchanged file is not touched.
# An interrupted transfer moves nothing; the next sync sends everything again.
STAGE="$MAIN/data/objects.incoming/$(printf '%s' "$HOST" | tr -c 'A-Za-z0-9.' '_')"
sync_objects() {
  rm -rf "$STAGE" && mkdir -p "$STAGE"
  if ! "${SSH[@]}" "$HOST" 'cd ~/wt && if [ -d data/objects ]; then tar -cf - data/objects; fi' |
    tar -x -C "$STAGE" 2> /dev/null; then
    return 0
  fi
  [ -d "$STAGE/data/objects" ] || return 0
  (cd "$STAGE/data/objects" && find . -type f) | while IFS= read -r f; do
    src="$STAGE/data/objects/$f"
    dst="$MAIN/data/objects/$f"
    cmp -s "$src" "$dst" && continue
    mkdir -p "$(dirname "$dst")"
    # A file a reader holds open on Windows cannot be replaced; it is sent again next sync.
    mv -f "$src" "$dst" 2> /dev/null || true
  done
}
(while sleep 120; do sync_objects; done) &
SYNC=$!
trap 'kill $SYNC 2> /dev/null || true' EXIT

step "clear what an earlier round left"
# A killed round can leave workers, the sidecar on 8090 and its tunnel's listener on 55433 behind:
# they hold GPU memory, and the tunnel below would fail to bind. The brackets keep pkill from
# matching this command line itself.
"${SSH[@]}" "$HOST" "pkill -f 'wt [w]orker'; pkill -f 'uvicorn [a]pp:app'; \
  sudo fuser -k 55433/tcp 8090/tcp; true" > /dev/null 2>&1 || true

step "work: $WORKERS workers"
LIMIT=()
[ "$HOURS" != 0 ] && LIMIT=(timeout "${HOURS}h")
"${LIMIT[@]}" "${SSH[@]}" -o ExitOnForwardFailure=yes -R 55433:localhost:55433 "$HOST" \
  "bash ~/wt/infra/gpu-worker/work.sh $WORKERS" || echo "work.sh ended with an error"
step "copy transcripts back"
sync_objects
step "done: delete the machine now"
