#!/usr/bin/env bash
# From the PC: sets up a GPU machine and drains the audio queue on it (Spec 01, GPU access).
#
#   infra/gpu-worker/run.sh ubuntu@<address> [workers] [hours]
#
# `hours` stops the workers after that long, so a run cannot pass its share of the spending cap.
#
# The machine is one the owner rented or launched, reached by SSH with the owner's key. This
# script copies the repository's committed files to it, runs setup.sh, sends the Hugging Face
# token and the database URL over SSH into files only the owner can read, and runs work.sh with a
# reverse tunnel so the machine reaches the database through the PC (Spec 11, PB1.1). The
# database is RDS, reached on the PC through the batch-host forward on localhost:5433 (runbook
# §1); the machine sees it on its own localhost:5433, and the database is never exposed. The URL
# is the PC's WT_GPU_DATABASE_URL, or DATABASE_URL in .env, as wt_pipeline_login with
# sslmode=require, and WT_DATABASE_NAME (rds-v1) goes with it so the workers refuse any other
# database. WT_GPU_DB_PORT changes the port. Every two minutes, and at the end, it copies new
# and changed transcripts back into data/objects, each replaced whole (see sync_objects).
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
# Only the files of talks with a queued audio job are sent, so a rerun for a few talks does not
# upload the whole folder again. Set WT_STORAGE_ALL=1 to send everything.
if [ -d "$MAIN/data/storage" ] && [ -n "$(ls -A "$MAIN/data/storage")" ]; then
  if [ "${WT_STORAGE_ALL:-0}" = 1 ]; then
    step "copy the storage files ($(du -sh "$MAIN/data/storage" | cut -f1))"
    tar -C "$MAIN" -cf - data/storage | "${SSH[@]}" "$HOST" 'mkdir -p ~/wt && tar -x -C ~/wt'
  else
    LIST=$(mktemp)
    # Each storage source's root, from the [[source]] blocks of config/sources.toml.
    declare -A ROOT
    while read -r name root; do ROOT[$name]=$root; done < <(awk -F' *= *' '
      /^\[\[source\]\]/ { name = ""; root = "" }
      $1 == "name" { gsub(/"/, "", $2); name = $2 }
      $1 == "root" { gsub(/"/, "", $2); root = $2; if (name != "") print name, root }
    ' "$(git rev-parse --show-toplevel)/config/sources.toml")
    docker exec wt-m3-ssz psql -U wt -d wt -tAc \
      "select distinct (sv.raw->'storage'->>'source') || ' ' || (sv.raw->'storage'->>'key')
       from job j join talk t on t.id = j.talk_id join source_video sv on sv.id = t.source_video_id
       where j.kind = 'audio' and j.state = 'queued' and sv.raw ? 'storage'" |
      tr -d '\r' | while read -r source key; do
        [ -n "${ROOT[$source]:-}" ] && [ -f "$MAIN/${ROOT[$source]}/$key" ] && echo "${ROOT[$source]}/$key"
      done > "$LIST"
    if [ -s "$LIST" ]; then
      step "copy $(wc -l < "$LIST") storage files for queued talks"
      tar -C "$MAIN" -cf - -T "$LIST" | "${SSH[@]}" "$HOST" 'mkdir -p ~/wt && tar -x -C ~/wt'
    fi
    rm -f "$LIST"
  fi
fi
step "token"
grep '^HF_TOKEN=' "$MAIN/.env" | cut -d= -f2- | tr -d '\r\n' |
  "${SSH[@]}" "$HOST" 'umask 077 && cat > ~/.wt-hf-token'
step "database"
DB_PORT=${WT_GPU_DB_PORT:-5433}
DB_URL=${WT_GPU_DATABASE_URL:-$(grep '^DATABASE_URL=' "$MAIN/.env" | cut -d= -f2- | tr -d '\r\n')}
DB_NAME=${WT_DATABASE_NAME:-$(grep '^WT_DATABASE_NAME=' "$MAIN/.env" | cut -d= -f2- | tr -d '\r\n')}
case "$DB_URL" in
  *"@localhost:$DB_PORT/"* | *"@127.0.0.1:$DB_PORT/"*) ;;
  *)
    echo "the database URL must name localhost:$DB_PORT, the PC's forward to RDS"
    exit 2
    ;;
esac
[ -n "$DB_NAME" ] || {
  echo "WT_DATABASE_NAME is unset; set it to the database's name (rds-v1)"
  exit 2
}
printf 'DATABASE_URL=%s\nWT_DATABASE_NAME=%s\n' "$DB_URL" "$DB_NAME" |
  "${SSH[@]}" "$HOST" 'umask 077 && cat > ~/.wt-database'

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
# A killed round can leave workers, the sidecar on 8090 and its tunnel's listener behind:
# they hold GPU memory, and the tunnel below would fail to bind. The brackets keep pkill from
# matching this command line itself.
"${SSH[@]}" "$HOST" "pkill -f 'wt [w]orker'; pkill -f 'uvicorn [a]pp:app'; \
  sudo fuser -k /tcp 8090/tcp; true" > /dev/null 2>&1 || true

step "work: $WORKERS workers"
LIMIT=()
[ "$HOURS" != 0 ] && LIMIT=(timeout "${HOURS}h")
"${LIMIT[@]}" "${SSH[@]}" -o ExitOnForwardFailure=yes -R "" "$HOST" \
  "bash ~/wt/infra/gpu-worker/work.sh $WORKERS" || echo "work.sh ended with an error"
step "copy transcripts back"
sync_objects
step "done: delete the machine now"
