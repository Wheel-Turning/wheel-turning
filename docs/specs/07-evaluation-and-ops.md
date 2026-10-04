# Spec 07: Evaluation, testing and operations

Implements M8 and the test layer every earlier milestone relies on. The PRD's pass and fail criteria are only meaningful if the harness can run unchanged on every build, so evaluation is a product feature here, not an afterthought.

| Suite | Tool | Runs | Guards |
| --- | --- | --- | --- |
| Unit and property tests | `cargo test`, `proptest` | Every CI run | Dana split, chunker, glossary, selection |
| Compile-fail tests | `trybuild` | Every CI run | `Talk<Pending>` cannot reach indexing or fetch functions |
| Database tests | `testcontainers` with a pgvector Postgres image | Every CI run | Constraints, serving views, roles, queue claiming, takedown |
| Rendering snapshots | `insta` | Every CI run | Answer page for each notice case and HTML escaping |
| External API contract tests | `wiremock` with synthetic recorded responses | Every CI run | YouTube Data API and Claude API clients |
| Repository guard tests | Plain Rust tests that scan source | Every CI run | No direct `passage` reads in serving code, no audio paths outside scratch, no payment crates, every table with a `talk_id` registered for takedown |
| End-to-end pipeline test | Real worker, real Postgres, real ONNX models, fake ASR and diarizer | Nightly and on demand, behind the `ml-tests` feature | A synthetic talk goes from discovered to a rendered answer |
| Evaluation runs | `wt eval` | On every change to chunking, embeddings, reranking, glossary or synthesis | The PRD pass and fail tests |

Fixtures are synthetic. `scripts/make_fixtures.py` generates fake teachers, sanghas, talk scripts, caption files and short audio with known silence and tones. Real transcripts, audio and credentials never enter the repository, and the guard tests scan for common audio extensions and for strings that look like API keys.

## Evaluation harness

`wt-eval` runs a question set through the same `ask()` function the web route uses, records the result, and supports the reviewer's work. Question files live in `eval/` as TOML.

```toml
# eval/questions_v1.toml  (founder replaces the placeholders)
[[question]]
text = "I keep judging myself during zazen."
tags = ["multi_voice"]

[[question]]
text = "What has Norman Fischer said about grief?"
tags = ["named_teacher", "grief"]
```

The 50-question set follows the PRD. It centers on topics with several voices (compassion, anger, the Heart Sutra, the precepts, repentance, practicing within sangha, discrimination and suffering), and also includes single-teacher topics, a grief question and a named-teacher query for Norman Fischer to test thin coverage. Tags are free text and the report groups results by them.

Commands:

1. `wt eval load --file eval/questions_v1.toml --name practitioner --version 1` loads a set into `eval_set` and `eval_question`. Sets are immutable once loaded, and a changed set gets a new version.
2. `wt eval run --set practitioner@1` runs every question as the reserved tester label `eval`, writes normal answers, and records an `eval_run` holding the build identifier, the git commit, a snapshot of `retrieval.toml` and `pipeline.toml`, and the model names. Eval tester rows are excluded from tester analytics.
3. `wt eval export-review --run <id> --out review.html` writes one self-contained HTML file for the teacher reviewer with no external resources. Each question lists its passages with teacher, link and talk, radio buttons for faithful and relevant with a note field, and a row per claim in each theme with supported, drifting or invented. The file's inline script collects the ratings into a downloadable JSON. It runs locally on the reviewer's machine and is not part of the web app.
4. `wt eval import-ratings --run <id> --file ratings.json --reviewer <name>` writes `eval_rating` and `claim_audit` rows.
5. `wt eval report --run <id>` prints: the share of questions that pass, where a question passes when at least `question_pass_ratio` (default 0.67) of its rated passages are both faithful and relevant; claim-audit counts by verdict; the teacher-count distribution and the number of thin-coverage answers; the outcome of each named-teacher query; stage latency percentiles; and dana event counts. It marks the PRD verdicts: fewer than half of questions passing is a retrieval fail, and any invented claim surviving is a synthesis fail.
6. `wt eval compare --a <run> --b <run>` shows per-question changes between two runs, which is how a tuning change is judged.

## Gate A notebook retrieval test

The PRD orders its risks by how cheaply they can be tested, and the cheapest high-impact one is whether retrieval over practitioner-style questions returns faithful passages. Gate A tests that in a notebook over real consented transcripts before any retrieval or interface code is built in Rust.

Inputs:

- `wt export transcripts --format jsonl --out data/export/` writes one JSON object per passage of every consented, `ready` talk: `passage_id`, an opaque `talk_id`, an opaque `teacher_id`, `start_ms` and `text`. It writes no audio and no names. The export directory is gitignored and lives only on the founder's machine.
- A 20-question subset of the practitioner set covering multi-voice topics, single-teacher topics and grief. The size is a default and may change.

Method, in `scripts/notebooks/gate_a_retrieval.ipynb`:

1. Embed the passages with the same bge-m3 model that the Rust service uses and build an in-memory cosine index.
2. Build a BM25 index over the same passages with the `rank-bm25` package.
3. Fuse the two with reciprocal rank fusion at `k = 60`.
4. Rerank the top 40 with bge-reranker-v2-m3 and keep the top 10 with a per-teacher cap of 3.
5. Write `review.html` in the same format `wt eval export-review` produces, so the reviewer uses one interface for both.

Pass rule: a teacher reviewer rates each returned passage faithful and relevant or not, and Gate A passes when more than half of the 20 questions meet the question pass ratio. This matches the PRD's retrieval fail line of fewer than half. The reviewer brief says to rate unflatteringly. Results, with reviewer name, date, count rated and the pass count, go in `docs/gate-a.md`, and M5 does not start until that file records a pass.

The notebook is also the reference for the Rust retrieval build. After M5, a parity script runs the same questions through `wt-retrieve` and reports the overlap of the top-10 passage sets between the two. A mismatch below 0.7 is investigated before M5 is accepted, though it is informational and not a gate.

## Nightly audit checks

`wt audit` runs the checks below, prints a JSON report, and exits nonzero if any check marked fail has findings. `nightly.yml` runs it as a one-off task against the live database. Checks marked warn are listed and do not change the exit code.

| Check | Finding | Severity |
| --- | --- | --- |
| Consent drift | A passage whose talk is not `ready`, not consented, or has an unknown rights holder | Fail |
| Serving parity | The count of rows in `servable_passage` differs from passages of ready, consented talks | Fail |
| Student speech | Any `qa_student` segment, or any transcript JSON span or word attributed to a non-teacher speaker | Fail |
| Audio residue | A scratch directory older than `scratch_resume_ttl_h`, a scratch bucket object older than 24 hours, or an audio-typed object in any other bucket | Fail |
| Takedown completeness | Any row or S3 object remaining for a scope recorded in `takedown_log` | Fail |
| Fixture leak | A talk with `raw.synthetic = true` present when `WT_ENV` is `staging` or `prod` | Fail |
| Mixed embeddings | Passages with more than one `embedding_model` outside a running reindex | Fail |
| Job health | Dead jobs, or running jobs past their lock timeout | Warn |
| Donation links | A sangha cited by a ready talk that is listed but unverified, or verified more than `link_review_days` ago (default 90) | Warn |
| Consent staleness | A consent row last verified more than `consent_review_days` ago (default 90) | Warn |
| Quarantine age | A talk quarantined for more than 7 days | Warn |
| Series metadata | A ready talk with no series whose title matches a series pattern | Warn |

## Takedown procedure and verification

A teacher can pull any talk or all talks at any time. Takedown deletes text, and it is the only route to the `withdrawn` state.

`wt takedown --teacher <name> | --talk <id> --actor <name> [--dry-run] --yes`

Steps:

1. A dry run prints the count of rows and objects per store and changes nothing. A real run requires `--yes`.
2. One database transaction, in this order: delete `citation` and `answer_passage` rows for the affected passages through the cascade from `passage`, delete `passage`, `segment` and `transcript` rows, strip from `answer.payload` every theme whose passage ids intersect the removed set, set `eval_rating.note` to null for the removed passage ids, set the talks to `withdrawn` with a null `content_hash`, and for a teacher scope insert a `withdrawn` row in `speaker_consent`. Insert the `takedown_log` row with the per-store counts. Serving stops the moment this commits.
3. After commit, delete the S3 objects under `transcripts/<talk_id>/` and `citations/<talk_id>.json`, then list the prefixes to confirm they are empty. If deletion fails, the `takedown_log` row stays as the record of what is left and `wt takedown --finish` retries every logged scope that still has objects. The nightly audit fails until nothing remains.
4. The stub `talk` and its `source_video` row remain, so a later discovery run does not recreate the talk.

Verification uses a `StoreRegistry` in `wt-core`, a list of every place a teacher's words can live: database tables, S3 prefixes, local cache directories and any future search index. Takedown and the audit both iterate it. A repository guard test fails the build if a table with a `talk_id` column or a new storage location is not registered, which keeps takedown complete as the system grows.

Backups are the honest limit. RDS automated backups retain deleted rows until they expire after seven days. The runbook says to tell a teacher this, and after any database restore, `wt takedown --replay` reapplies every recorded takedown so removed talks cannot return.

## Observability and runbook

Logs are structured JSON through `tracing`. Request logs carry a request id, tester id, route, status and latency. They never carry query text, passage text, invite tokens, cookies or secrets. A test issues a request with a distinctive query string and asserts that string appears in no captured log line.

Log-based metrics in CloudWatch cover ask latency, `synthesis_failed`, `rerank_degraded`, 5xx responses, dead jobs and audit failures. Alarms are defined in Spec 01.

The runbook is `docs/runbook.md`, written as the work is built, and it contains these procedures:

1. Start and stop the batch host, run `wt worker --kinds audio`, and confirm the scratch directory and bucket are empty afterward.
2. Add a teacher, set aliases, record a consent reply with its evidence reference, and set the rights holder.
3. Handle a takedown request: confirm identity, run a dry run, run the takedown, send the teacher the counts and the seven-day backup note. Target the same day.
4. Respond to a blocking quality failure: quarantine, repair, re-check, release, and record the decision.
5. Rotate secrets and invite tokens.
6. Restore the database from a snapshot and then run `wt takedown --replay`.
7. Change the embedding model or glossary and run `wt reindex`, then an evaluation run, then compare.
8. Before enabling real transcripts with the language model: confirm the provider's zero-retention terms and set `WT_LLM_REAL_DATA_OK`.
9. Release checklist: audit passes, evaluation run attached, Gate A recorded, consent coverage report reviewed, disclosure copy matches the logging code.

## Acceptance criteria

- [ ] A full `wt eval run` of the 50-question set completes without code changes, and `wt eval report` prints every metric listed above.
- [ ] `wt eval export-review` produces a standalone HTML file that works offline, and `wt eval import-ratings` loads its output into `eval_rating` and `claim_audit`.
- [ ] A 20-answer claim audit imports and the report counts supported, drifting and invented claims.
- [ ] Each audit check has a test with a failing fixture that makes `wt audit` exit nonzero and a passing fixture that does not, and warn-level checks do not change the exit code.
- [ ] Takedown on a fixture teacher leaves zero rows and zero objects in every registered store, including S3 emulated by MinIO in tests.
- [ ] Takedown strips affected themes from stored answers and nulls affected rating notes.
- [ ] A takedown stub blocks re-ingest of the same source video, and `wt takedown --replay` reapplies all recorded scopes after a simulated restore.
- [ ] The `StoreRegistry` guard test fails when a table with a `talk_id` column is added without registration.
- [ ] The log scrubber test passes.
- [ ] `docs/runbook.md` exists with every procedure above.
- [ ] `nightly.yml` runs `wt audit` in the deployed environment and fails the workflow on a failing check.
- [ ] `docs/gate-a.md` records a pass, with a named reviewer, before the M5 branch merges.
