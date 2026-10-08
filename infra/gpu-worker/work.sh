#!/usr/bin/env bash
# Runs on the GPU machine, started by run.sh: the diarization sidecar, then WORKERS audio workers
# that drain the queue. The database is the PC's, reached through run.sh's reverse tunnel on
# localhost:55433. Transcripts land in ~/wt/data/objects, which run.sh copies back. Audio stays
# in the scratch directory, which the pipeline empties and this script removes (rule 6).
set -uo pipefail
WORKERS=${1:-3}
REPO=$HOME/wt
TOOLS=$HOME/wt-tools
SCRATCH=$HOME/wt-scratch
LOGS=$HOME/wt-logs
TOKEN=$HOME/.wt-hf-token
mkdir -p "$SCRATCH" "$LOGS"
cd "$REPO" || exit 1

export DATABASE_URL=postgres://wt:wt@localhost:55433/wt
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
  rm -rf "$SCRATCH" "$TOKEN"
}
trap cleanup EXIT

[ -s "$TOKEN" ] || {
  echo "no Hugging Face token; run.sh sends it"
  exit 1
}
HF_TOKEN=$(cat "$TOKEN") "$TOOLS/sidecar/bin/python" -m uvicorn app:app \
  --app-dir "$REPO/sidecars/diarize" --host 127.0.0.1 --port 8090 > "$LOGS/sidecar.log" 2>&1 &
SIDECAR=$!
for _ in $(seq 1 180); do
  curl -sf http://127.0.0.1:8090/healthz && break
  sleep 2
done
echo

pids=()
for i in $(seq 1 "$WORKERS"); do
  ./target/release/wt worker --kinds audio --drain --config-dir config > "$LOGS/worker$i.log" 2>&1 &
  pids+=($!)
done
status=0
for pid in "${pids[@]}"; do wait "$pid" || status=1; done
grep -h "segmented\|failed\|error" "$LOGS"/worker*.log | tail -20
echo "workers done, status $status"
exit $status
