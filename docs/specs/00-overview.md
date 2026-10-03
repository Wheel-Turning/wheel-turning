<!-- Exported from the "Dharma Gates: V1 Technical Specs" doc (https://claude.ai/code/artifact/68e63636-e0f6-43ee-ab79-ccc03b76946d), tab "Overview and build plan", on 2026-10-02. The doc is the source of truth until this file is edited in the repo. -->
# Wheel Turning: V1 Technical Specs

2026-10-02 · @Alex

## Goal

v1 is a private, text-only retrieval demo over about 225 SSZ talks (roughly 140 hours, an estimated 8,000 to 15,000 passages) that exists to answer three questions fast: does retrieval return passages a teacher would stand behind, does synthesis stay inside what teachers said, and does anyone follow a dana link. Everything in this spec set serves those three tests and nothing else.

The build is done when a tester can open an invite link, ask a situational question, and get a page with grouped verbatim passages, a verified synthesis line, source links that land on the right moment, a dana panel, and a logged answer, all drawn only from talks whose speakers have consented. It is also done only when the evaluation harness can run the 50-question set and the claim audit without code changes.

This document set is written to be executed by Claude Code. Each spec tab maps to a file under `docs/specs/` in the repository, states what it implements, and ends with acceptance criteria written as checkable items. Where the PRD or the architectural north star already fixes a rule, the spec points to it and does not restate the reasoning.

## Locked decisions and defaults

Claude Code builds to these defaults without asking. Each has an escape hatch behind a trait or a config key, so changing one later does not ripple through the system. Pin exact versions in `Cargo.lock` and lock files at build time and do not rely on the version names below.

| Area | Default | Reason | Escape hatch |
| --- | --- | --- | --- |
| Language | Rust stable pinned in `rust-toolchain.toml`, one Cargo workspace | Invariants are enforced in types | Python only for notebooks, the model export script and the diarization sidecar |
| Database | PostgreSQL 16 with pgvector (HNSW, cosine) and built-in full-text search. Access through sqlx with compile-time checked queries and offline metadata committed. | One consistency boundary for consent and takedown at this corpus size | `Retriever` trait allows Tantivy or Qdrant later |
| Web | Axum, Askama templates, htmx, plain CSS, no JavaScript build step | Polished interface is out of scope for v1 | None needed |
| Job queue | A Postgres table with `FOR UPDATE SKIP LOCKED` and a small custom worker | No extra infrastructure | `apalis` if the custom worker grows |
| Embeddings | bge-m3, 1024 dimensions, int8 ONNX through `ort`, run locally | Transcripts do not leave the infrastructure for retrieval | `Embedder` trait allows a hosted API |
| Reranker | bge-reranker-v2-m3 in ONNX through `ort`, with a smaller model as a latency fallback | A per-passage relevance threshold is needed | `Reranker` trait |
| Transcription | Existing captions when present, otherwise whisper large-v3 through `whisper-rs` on a batch host | Audio is transient and 140 hours is small | `Transcriber` trait |
| Diarization | pyannote in a Python HTTP sidecar behind a `Diarizer` trait | No mature Rust equivalent | sherpa-onnx in process after the ten-video sample decides it |
| Language model | Claude API through `reqwest`, structured JSON output, temperature 0, model name read from `WT_LLM_MODEL` | Extraction, classification help, synthesis and verification | `LlmClient` trait |
| Hosting | AWS with Terraform: RDS Postgres, ECS Fargate for the web service, S3, Secrets Manager. GPU work runs on an on-demand EC2 batch host or the founder's machine. | The builder wants AWS skills and the footprint is small | Any Postgres host works, since nothing else is AWS-specific |
| Tester access | Invite tokens issued by the CLI, stored hashed, exchanged for a session cookie | Accounts are out of scope | Identity-aware proxy later |
| Units | Money in integer US cents. Media offsets in integer milliseconds. Timestamps in UTC. | Avoids rounding and timezone bugs | None |

## Repository layout and conventions

The repository is a single Cargo workspace. The layout below is the target; milestones create crates as they need them.

```text
wheel-turning/
  Cargo.toml                workspace
  rust-toolchain.toml
  CLAUDE.md                 the conventions below, read by Claude Code
  deny.toml                 cargo-deny: bans payment and audio-storage crates
  docker-compose.yml        local Postgres with pgvector
  docs/specs/               one file per spec tab
  crates/
    wt-core/                domain types, ids, errors, typestate, config
    wt-db/                  sqlx pool, migration runner, repositories, views
    wt-registry/            teachers, aliases, sanghas, glossary, consent records
    wt-ingest/              SourceAdapter trait, YouTube and storage adapters, dedupe
    wt-asr/                 Transcriber trait, caption parsers, whisper-rs
    wt-segment/             Diarizer client, segment labels, student-speech removal
    wt-extract/             LlmClient, schemas, extraction, citation resolution
    wt-ml/                  Embedder and Reranker traits, ort implementations
    wt-index/               chunker, embedding, index writer
    wt-retrieve/            hybrid search, rerank, selection
    wt-synth/               synthesis, validator, verifier
    wt-dana/                split calculator
    wt-api/                 Axum app, templates, invite auth
    wt-cli/                 the wt binary: commands and worker
    wt-eval/                evaluation harness
  migrations/               sqlx migrations
  sidecars/diarize/         Python FastAPI service
  scripts/                  export_models.py, notebooks
  infra/terraform/
  fixtures/                 synthetic transcripts only
  .github/workflows/
```

These rules go into `CLAUDE.md` verbatim.

1. Read the matching spec tab and the PRD section before writing code. If the two conflict, stop and ask. Do not resolve it silently.
2. No `unwrap` or `expect` outside tests. Library crates use `thiserror`, binaries use `anyhow`.
3. Serving code reads passages only through the `servable_passage` view. A CI test greps for direct reads of the `passage` table outside `wt-index` and the takedown routine.
4. The ranker input type has no field for sangha, profile status, upload volume or donation link. Do not add one.
5. A language model never produces text that is displayed as a quotation. It returns passage IDs and JSON only.
6. Audio exists only under the configured scratch directory, and the pipeline deletes it explicitly on success and failure. No schema column or bucket holds audio.
7. No payment or card-handling crate may enter the dependency tree. `cargo deny` enforces this.
8. Every pipeline stage is idempotent and keyed by content hash. Rerunning a stage on unchanged input is a no-op.
9. Tests make no network calls. External services sit behind traits and tests use fakes. Postgres tests use a real container.
10. Fixtures are synthetic text. Real transcripts, audio and credentials are never committed.
11. A milestone ends with CI green and its acceptance items ticked in the spec file.

Checks that CI and every milestone run: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `cargo sqlx prepare --check`, `cargo deny check`.

## Build milestones

Milestones are ordered so the riskiest PRD hypotheses are tested before the interface exists. Gate A sits between M4 and M5 and is a hard stop: if retrieval on real transcripts fails there, M5 onward does not start.

| Milestone | Deliverable | Depends on | Done when |
| --- | --- | --- | --- |
| M0, foundation | Workspace, CI, local Postgres with pgvector in Docker, migration runner, config loader, structured logging | None | CI is green and `wt db migrate` applies every migration to an empty database |
| M1, AWS baseline | Terraform for network, RDS, S3 buckets, container registry, Fargate service, secrets, and an on-demand GPU batch host | M0 | A deployed `/healthz` answers, and the scratch bucket lifecycle rule is verified by a test object expiring |
| M2, registry, consent, gate | Teacher, sangha and alias registries, consent records, typestate gate, `servable_passage` view, `wt consent`, `wt audit`, takedown skeleton | M0 | Gate and view tests pass, including an unconsented talk failing to index and to appear in the view |
| M3, discovery and ingestion | Job queue, source adapters, YouTube metadata discovery, deduplication, talk classification, review report | M2 | Discovery on the configured channel yields a deduplicated talk list with states up to `awaiting_consent` |
| M4, transcript pipeline | Media fetch for cleared talks, captions or ASR, diarization, segmentation, glossary correction, extraction, citation resolution, chunking, embedding, indexing, transcript export | M3 | One consented talk goes from discovered to ready, and the ten-video segmentation sample report exists |
| Gate A | Python notebook retrieval test over the available cleaned transcripts, rated by the teacher reviewer | M4 | The reviewer rates a majority of test questions faithful and relevant, or the build stops and the PRD is revisited |
| M5, retrieval and answer page | Query parsing, hybrid retrieval, reranking, selection, answer page without synthesis, timestamped links, texts section | Gate A | The 50-question set runs end to end and teacher counts are recorded per answer |
| M6, synthesis | Theme generation, deterministic filters, verifier, answer assembly with themes | M5 | The synthesis validator test suite passes and no invented claim survives on the audit sample |
| M7, dana, access, logging | Dana calculator and panel, invite access, query and answer logging, tester feedback, role-separated log access | M5 | Property tests pass and a tester can follow a dana link from an answer |
| M8, evaluation and hardening | Evaluation harness, review export and import, nightly audit, complete takedown, runbook | M6, M7 | The 50-question run and a 20-answer claim audit complete without code changes, and takedown leaves zero rows in every store |

M1 can proceed in parallel with M2 and M3. M6 and M7 can proceed in parallel once M5 is done.

## Spec index

Each spec below is a tab here and a file under `docs/specs/` in the repository. Read the overview first, then the spec for the milestone in hand.

| Spec | Covers | Milestones |
| --- | --- | --- |
| [01 Platform](01-platform.md) | Local environment, configuration keys, AWS resources in Terraform, CI and deployment | M0, M1 |
| [02 Data model](02-data-model.md) | Full schema, enums, the serving view, database roles | M0, M2, and every later milestone |
| [03 Ingestion](03-ingestion.md) | Talk state machine, source adapters including YouTube, deduplication, classification, the consent gate, the job queue | M2, M3 |
| [04 Transcript pipeline](04-transcript-pipeline.md) | Transcription, diarization, segmentation, glossary, extraction, citation resolution, chunking, embedding, indexing | M4 |
| [05 Retrieval and answers](05-retrieval-and-answers.md) | Query parsing, hybrid retrieval, reranking, selection, synthesis, validation, answer assembly, dana | M5, M6, M7 |
| [06 API and web](06-api-and-web.md) | Routes, invite access, page structure, logging, feedback | M5, M7 |
| [07 Evaluation and ops](07-evaluation-and-ops.md) | Tests, evaluation harness, the notebook gate, audit, takedown, observability | M8, and the tests of every milestone |

## Blockers outside the code

Several PRD open items gate real data, not software. The code is built so each one fails closed, which means the system does less while an item is open and never does something unsafe.

| Blocker | Owner | What the code does while it is open |
| --- | --- | --- |
| Speaker consent, Kanshin first | Founder | Gate refuses to fetch, transcribe or index any talk whose teacher lacks a granted consent record. Development runs on synthetic fixtures. |
| Rights holder per talk | Founder | `rights_holder` null or `unknown` blocks the cleared state and indexing |
| Counsel review of YouTube caption ingestion, text-only storage, excerpt display, dana routing | Founder with counsel | Third-party fetching (`yt-dlp`) is disabled by default behind `WT_ALLOW_THIRD_PARTY_FETCH`. Metadata comes from the official YouTube Data API. Media comes from SSZ storage or the channel owner's authorized route. |
| Language-model provider terms with zero retention confirmed | Founder | `WT_LLM_REAL_DATA_OK` defaults to false, and any call carrying real transcript text is refused until it is set. Extraction and synthesis run on fixtures only. |
| Teacher reviewer scheduled with an unflattering-ratings brief | Founder | Gate A cannot be marked passed without at least one reviewer's recorded ratings |
| SSZ permission note and speaker-release line | Founder | No code effect in v1. Recorded as a checklist item in the runbook. |

A manual look at ten SSZ videos to check whether files are trimmed is an inspection by a person. It produces notes, not stored text, and does not need the gate.

Licensed corpus. The PRD lists open-license material and other sanghas as out of scope for v1, and this build amends that. Soto sanghas that publish talks under a license the project may rely on can be added beside consented SSZ talks, cleared by license evidence instead of speaker consent. The rules, commands and tests are in the Licensed sources section of the ingestion spec. Two items need a person: reading each sangha's license statement before approving it, and counsel's answer on noncommercial and no-derivatives variants, which the build rejects by default.
