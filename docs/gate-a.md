# Gate A record

Spec 07, "Gate A notebook retrieval test". M5 does not start until this file records a pass.

Status: **not yet rated.**

## How a run is recorded

1. Export the passages: `wt export transcripts --format jsonl --out data/export/`.
2. Run `scripts/notebooks/gate_a_retrieval.ipynb` over the questions in `eval/gate_a_questions.toml`. It writes `data/gate-a/<run>/review.html` and `results.json`. Both quote real transcripts and stay out of git.
3. Send `review.html` to the teacher reviewer with the brief at its top. It works offline in a browser, names each passage's teacher and talk with a link to the source, and saves `ratings-<run>.json` when the reviewer presses Save ratings.
4. Put the ratings file in the run's directory, then run the notebook's scoring cell. Paste the block it prints below.

A question passes when at least 67 percent of its rated passages are both faithful and relevant. A passage counts as rated only when both answers are given. The gate passes when more than half of the questions pass.

## Runs

None rated yet.
