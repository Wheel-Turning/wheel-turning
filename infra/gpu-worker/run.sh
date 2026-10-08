#!/usr/bin/env bash
# From the PC: sets up a GPU machine and drains the audio queue on it (Spec 01, GPU access).
#
#   infra/gpu-worker/run.sh ubuntu@<address> [workers]
#
# The machine is one the owner rented or launched, reached by SSH with the owner's key. This
# script copies the repository's committed files to it, runs setup.sh, sends the Hugging Face
# token over SSH into a file only the owner can read, and runs work.sh with a reverse tunnel so
# the machine reaches the PC's database on its own localhost:55433; the database is never
# exposed. Every two minutes, and at the end, it copies the transcripts back into data/objects.
# Delete the machine afterwards: it is billed by the hour.
set -euo pipefail
HOST=${1:?usage: run.sh user@host [workers]}
WORKERS=${2:-3}
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
step "token"
grep '^HF_TOKEN=' "$MAIN/.env" | cut -d= -f2- | tr -d '\r\n' |
  "${SSH[@]}" "$HOST" 'umask 077 && cat > ~/.wt-hf-token'

sync_objects() {
  "${SSH[@]}" "$HOST" 'cd ~/wt && if [ -d data/objects ]; then tar -cf - data/objects; fi' |
    tar -x -C "$MAIN" 2> /dev/null || true
}
(while sleep 120; do sync_objects; done) &
SYNC=$!
trap 'kill $SYNC 2> /dev/null || true' EXIT

step "work: $WORKERS workers"
"${SSH[@]}" -o ExitOnForwardFailure=yes -R 55433:localhost:55433 "$HOST" \
  "bash ~/wt/infra/gpu-worker/work.sh $WORKERS" || echo "work.sh ended with an error"
step "copy transcripts back"
sync_objects
step "done: delete the machine now"
