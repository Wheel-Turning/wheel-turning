#!/usr/bin/env bash
# Runs on the GPU machine, started by run.sh: the diarization sidecar, then WORKERS audio workers
# that drain the queue. The database is RDS, reached through run.sh's reverse tunnel to the PC's
# forward; run.sh writes its URL and name to ~/.wt-database, which this script removes.
# Transcripts land in ~/wt/data/objects, which run.sh copies back. Audio stays in the scratch
# directory, which the pipeline empties and this script removes (rule 6), and in
# the copy of the owner's storage files run.sh sends to ~/wt/data/storage, which this script
# removes too.
set -uo pipefail
WORKERS=${1:-3}
REPO=$HOME/wt
TOOLS=$HOME/wt-tools
SCRATCH=$HOME/wt-scratch
LOGS=$HOME/wt-logs
TOKEN=$HOME/.wt-hf-token
DATABASE=$HOME/.wt-database
mkdir -p "$SCRATCH" "$LOGS"
cd "$REPO" || exit 1

[ -s "$DATABASE" ] || {
  echo "no database settings; run.sh sends them"
  exit 1
}
DATABASE_URL=$(sed -n 's/^DATABASE_URL=//p' "$DATABASE")
WT_DATABASE_NAME=$(sed -n 's/^WT_DATABASE_NAME=//p' "$DATABASE")
export DATABASE_URL WT_DATABASE_NAME
export WT_LOG=info
export WT_SCRATCH_DIR=$SCRATCH
export WT_MODEL_LICENSES=$REPO/docs/model-licenses.md
export WT_WHISPER_CLI=$TOOLS/whisper.cpp/build/bin/whisper-cli
export WT_WHISPER_MODEL=$TOOLS/models/ggml-large-v3-turbo.bin
export WT_DIARIZE_URL=http://127.0.0.1:8090
unset ANTHROPIC_API_KEY WT_ENV WT_DB_HOST

# shellcheck disable=SC2329 # run by the trap
cleanup() {
  [ -n "${SIDECAR:-}" ] && kill "$SIDECAR" 2> /dev/null
  rm -rf "$SCRATCH" "$TOKEN" "$DATABASE" "$REPO/data/storage"
}
trap cleanup EXIT

[ -s "$TOKEN" ] || {
  echo "no Hugging Face token; run.sh sends it"
  exit 1
}
# A whisper-cli that aborts here aborts on every job and burns their attempts (October 9: 833
# jobs dead in eight minutes on two hosts), so a machine that fails this does no work.
if ! "$WT_WHISPER_CLI" -m "$WT_WHISPER_MODEL" -f "$TOOLS/whisper.cpp/samples/jfk.wav" -np -nt \
  > "$LOGS/smoke.log" 2>&1; then
  echo "whisper-cli smoke test failed:"
  tail -20 "$LOGS/smoke.log"
  exit 4
fi
echo "whisper-cli smoke test ok"

HF_TOKEN=$(cat "$TOKEN") "$TOOLS/sidecar/bin/python" -m uvicorn app:app \
  --app-dir "$REPO/sidecars/diarize" --host 127.0.0.1 --port 8090 > "$LOGS/sidecar.log" 2>&1 &
SIDECAR=$!
healthy=0
for _ in $(seq 1 180); do
  curl -sf http://127.0.0.1:8090/healthz && healthy=1 && break
  sleep 2
done
echo
if [ "$healthy" = 0 ]; then
  echo "diarization sidecar never became healthy:"
  tail -20 "$LOGS/sidecar.log"
  exit 5
fi

pids=()
for i in $(seq 1 "$WORKERS"); do
  ./target/release/wt worker --kinds audio --drain --config-dir config > "$LOGS/worker$i.log" 2>&1 &
  pids+=($!)
done
# Circuit breaker: a worker whose last jobs all failed, with no success since, is on a broken host
# or GPU. Stop it before it spends more attempts; its jobs go back to the queue for other hosts.
FAILS=${WT_WORKER_MAX_FAILS:-3}
(
  while sleep 30; do
    for i in "${!pids[@]}"; do
      kill -0 "${pids[$i]}" 2> /dev/null || continue
      since=$(awk '/job done/ { n = 0; next } /job failed|job dead/ { n++ } END { print n + 0 }' \
        "$LOGS/worker$((i + 1)).log")
      if [ "$since" -ge "$FAILS" ]; then
        echo "worker$((i + 1)): $since failures in a row, stopping it"
        kill "${pids[$i]}"
      fi
    done
  done
) &
BREAKER=$!
status=0
for pid in "${pids[@]}"; do wait "$pid" || status=1; done
kill "$BREAKER" 2> /dev/null
grep -h "segmented\|failed\|error" "$LOGS"/worker*.log | tail -20
echo "workers done, status $status"
exit $status
