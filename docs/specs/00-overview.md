# Wheel Turning: V1 Technical Specs

Oct 2, 2026 · @Alex

## Goal

v1 is a private, text-only retrieval demo over about 225 SSZ talks (roughly 140 hours, an estimated 8,000 to 15,000 passages) that exists to answer three questions fast: does retrieval return passages a teacher would stand behind, does synthesis stay inside what teachers said, and does anyone follow a dana link. Everything in this spec set serves those three tests and nothing else.

The build is done when a tester can open an invite link, ask a situational question, and get a page with grouped verbatim passages, a verified synthesis line, source links that land on the right moment, a dana panel, and a logged answer, all drawn only from talks with a recorded rights basis: the teacher's permission, a verified license, or the owner's mandate for a teacher who has not opted out. It is also done only when the evaluation harness can run the 50-question set and the claim audit without code changes.

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
| Transcription | Existing captions when present, otherwise whisper large-v3 through `whisper-rs` on the owner's Windows PC | Audio is transient and 140 hours is small | `Transcriber` trait |
| Diarization | pyannote in a Python HTTP sidecar behind a `Diarizer` trait | No mature Rust equivalent | sherpa-onnx in process after the ten-video sample decides it |
| Language model | Claude API through `reqwest`, structured JSON output, temperature 0, model name read from `WT_LLM_MODEL` | Extraction, classification help, synthesis and verification | `LlmClient` trait |
| Hosting | AWS with Terraform: RDS Postgres, ECS Fargate for the web service, S3, Secrets Manager. Transcription runs on the owner's Windows PC, which reaches RDS through an SSM port forward via an on-demand EC2 batch host. | The builder wants AWS skills and the footprint is small | Any Postgres host works, since nothing else is AWS-specific |
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
3. Serving code reads passages only through the `servable_passage` view. A CI test greps for direct reads of the `passage` table outside `wt-index`, the takedown routine and `wt audit`, which looks for passages that must not be served (decision of October 7, 2026; `wt-core/tests/repo_guards.rs`).
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
| M1, AWS baseline | Terraform for network, RDS, S3 buckets, container registry, Fargate service, secrets, and an on-demand batch host | M0 | A deployed `/healthz` answers, and the scratch bucket lifecycle rule is verified by a test object expiring |
| M2, registry, consent, gate | Teacher, sangha and alias registries, consent records, typestate gate, `servable_passage` view, `wt consent`, `wt audit`, takedown skeleton | M0 | Gate and view tests pass, including a talk with no rights basis failing to index and to appear in the view |
| M3, discovery and ingestion | Job queue, source adapters, Vimeo metadata discovery (YouTube once a YouTube source is configured), deduplication, talk classification, review report | M2 | Discovery on the configured channel yields a deduplicated talk list with states up to `awaiting_consent` |
| M3R, rights ledger and mandated talks | Spec 08's source policy and rights lifecycle (`source_policy`, `rights_event`, `permission_request`, `source_exclusion` and the exclusion checks), then Spec 03's mandated talks: the `talks_basis` switch, mandate clearing in classification, `wt teacher opt-out` with suspension, takedown and person and content-hash exclusions, and notice drafts | M3 | Spec 03's mandated-talks acceptance items pass, with the Spec 08 mandated-sources and rights-lifecycle items they rely on |
| M4, transcript pipeline | Media fetch for cleared talks, captions or ASR, diarization, segmentation, glossary correction, extraction, citation resolution, chunking, embedding, indexing, transcript export | M3, and M3R before talks of teachers who have not replied are processed | One cleared talk goes from discovered to ready, and the ten-video segmentation sample report exists |
| M4T, transcript display and review | Spec 03's display levels (`display_level` on teachers and talks, the serving view's filter, `wt teacher display`), Spec 04's review files, reviewed transcript versions and owner exports | M4 | A synthetic talk round-trips through a review file into a reviewed version that a rerun does not overwrite, each display level changes what `servable_passage` returns, and an owner export is written for a teacher at level d |
| Gate A | Python notebook retrieval test over the available cleaned transcripts, rated by the teacher reviewer | M4 | The reviewer rates a majority of test questions faithful and relevant, or the build stops and the PRD is revisited |
| M5, retrieval and answer page | Query parsing, hybrid retrieval, reranking, selection, answer page without synthesis, timestamped links, texts section | Gate A | The 50-question set runs end to end and teacher counts are recorded per answer |
| M6, synthesis | Theme generation, deterministic filters, verifier, answer assembly with themes | M5 | The synthesis validator test suite passes and no invented claim survives on the audit sample |
| M7, dana, access, logging | Dana calculator and panel, invite access, query and answer logging, tester feedback, role-separated log access | M5 | Property tests pass and a tester can follow a dana link from an answer |
| M8, evaluation and hardening | Evaluation harness, review export and import, nightly audit, complete takedown, runbook | M6, M7 | The 50-question run and a 20-answer claim audit complete without code changes, and takedown leaves zero rows in every store |

M5 starts in parallel with Gate A's ratings (owner's decision of October 9, 2026), so the owner can run a personal alpha on the answer page; Gate A must still pass before any tester other than the owner sees it, and a failed Gate A stops M5 as before. M4T can proceed in parallel with Gate A; the transcript page itself is part of M5 (decision of October 7, 2026). M1 can proceed in parallel with M2 and M3. M3R follows M3's discovery and classification (decision of October 5, 2026): until it lands the system is consent-only, so M4 can start on talks of teachers who have granted consent. M6 and M7 can proceed in parallel once M5 is done.

## Spec index

Each spec below is a tab here and a file under `docs/specs/` in the repository. Read the overview first, then the spec for the milestone in hand.

| Spec | Covers | Milestones |
| --- | --- | --- |
| 01 Platform | Local environment, configuration keys, AWS resources in Terraform, CI and deployment | M0, M1 |
| 02 Data model | Full schema, enums, the serving view, database roles | M0, M2, and every later milestone |
| 03 Ingestion | Talk state machine, source adapters including YouTube, deduplication, classification, the consent gate, the job queue, mandated talks | M2, M3, M3R |
| 04 Transcript pipeline | Transcription, diarization, segmentation, glossary, extraction, citation resolution, chunking, embedding, indexing | M4 |
| 05 Retrieval and answers | Query parsing, hybrid retrieval, reranking, selection, synthesis, validation, answer assembly, dana | M5, M6, M7 |
| 06 API and web | Routes, invite access, page structure, logging, feedback | M5, M7 |
| 07 Evaluation and ops | Tests, evaluation harness, the notebook gate, audit, takedown, observability | M8, and the tests of every milestone |

## Blockers outside the code

Several PRD open items gate real data, not software. The code is built so each one fails closed, which means the system does less while an item is open and never does something unsafe.

| Blocker | Owner | What the code does while it is open |
| --- | --- | --- |
| Speaker consent, Kanshin first | Founder | Gate refuses to fetch, transcribe or index any talk without a recorded rights basis: granted consent, a verified license, or the owner's mandate for a teacher who has not opted out. Development runs on synthetic fixtures. |
| Rights holder per talk | Founder | `rights_holder` null or `unknown` blocks the cleared state and indexing |
| Counsel review of YouTube caption ingestion, text-only storage, excerpt display, dana routing | Founder with counsel | Third-party fetching (`yt-dlp`) is disabled by default behind `WT_ALLOW_THIRD_PARTY_FETCH`. Metadata comes from the official YouTube Data API. Media comes from SSZ storage or the channel owner's authorized route. |
| Language-model provider terms (resolved: zero retention is not available, and the standard commercial terms apply, owner's decision of October 9, 2026) | Founder | `WT_LLM_REAL_DATA_OK` defaults to false, and any call carrying real transcript text is refused until the owner sets it. It is the owner's switch, no longer waiting on a retention agreement. |
| Teacher reviewer scheduled with an unflattering-ratings brief | Founder | Gate A cannot be marked passed without at least one reviewer's recorded ratings |
| SSZ permission note and speaker-release line | Founder | No code effect in v1. Recorded as a checklist item in the runbook. |

A manual look at ten SSZ videos to check whether files are trimmed is an inspection by a person. It produces notes, not stored text, and does not need the gate.

Licensed corpus. The PRD includes other Soto sanghas' talks in v1, under a license or the owner's mandate. Soto sanghas that publish talks under a license the project may rely on can be added beside SSZ talks, cleared by license evidence instead of speaker consent. The rules, commands and tests are in the Licensed sources section of the ingestion spec. Two items need a person: reading each sangha's license statement before approving it, and counsel's answer on noncommercial and no-derivatives variants, which the build rejects by default.

Texts and reference assets. Foundational texts such as the Dhammapada and the Platform Sutra, and background material such as glossaries and lineage charts, are specified in 08 Texts and reference. Texts are searchable in their own section and never count as teachers. Reference assets feed the pipeline and are never shown as results.

## Feature backlog

Items that are decided but deliberately not built or enabled in v1.

1. Automatic sending of permission requests, teacher notices and follow-ups from a project domain email address. Until then, the CLI drafts every request and notice, a person sends it by hand, and `wt rights sent` records the date and schedules the follow-up. When it is built, sending goes through the same ledger so nothing changes in the records.
2. Domain-owned mailboxes at wheelturning.org, for example rights@ and hello@, so letters, replies and alerts use the project's own address. Until then, alerts reach the owner's personal address through an AWS notification.
3. Email alerts for new Rights and removal requests from a domain address, which replace the AWS notification stopgap.
4. Public per-teacher status on the rights page, if teachers ask for it. The page lists sources only for now.
5. Resume the paused downstream queue (owner's decision of October 12, 2026). The worker was stopped so the alpha has the CPU while beta work lands, leaving 1,904 `index`, 431 `resolve` and 33 `extract` jobs queued, mostly San Francisco Zen Center talks from the October 10 transcription run. The 29 talks among them that were already served were moved from `ready` back to `resolving`, so nothing half-indexed is searchable; their index jobs set them `ready` again. To resume, run `wt worker --kinds classify,correct,extract,resolve,index,questions` with a CLI built from current main, at idle priority while testers use the site (about 4 to 7 hours of CPU). 17 of the extract jobs fail on a truncated transcript file and need re-transcription, not a retry.

## Post-beta candidates

Owner's notes of October 12, 2026, refined in discussion. None is built before beta testing; the order follows user interviews after beta. An item marked "PRD change" reverses a current PRD rule and needs that change written before it is built.

Answers and search
1. Query variations. The triage call (Spec 05, Sensitive questions) also returns two or three rephrasings of the question, each searched as another fused list; the page and the log show only the original. Kept only if the evaluation set shows better relevance.
2. Lower latency. Make vector search use the HNSW index through the serving view's filters, run stages in parallel, cache repeated questions, and consider a GPU-hosted reranker. The index fix may move before beta.
3. Live progress while a question runs. Real stage counts (passages searched, candidates, shortlisted, selected) and plain stage names (reading the question, writing variations, searching the talks, weighing excerpts, writing the summary), streamed with server-sent events. Each stage is announced once to screen readers, nothing animates under reduced motion, and without JavaScript the page waits as now.
4. A fuller summary, in the manner of Consensus (PRD change). A synthesized answer with inline citations, author and date, linking to each talk, so a reader gets the idea without reading every excerpt. Shape, from a Consensus research answer read on October 12, 2026: a one-sentence headline answer first; a short framing paragraph; sections for where the teachers converge, where they differ, and points to keep in mind (few talks on the topic, one sangha dominating, context a quote leaves out); and a closing sum-up. Every citation marker points to a verbatim quotation the code takes from the cited excerpt, as Consensus ties each marker to a quoted line of its paper, so a reader can check each claim at once. Consensus's evidence-strength meter does not carry over to teachings; the answer states instead how many teachers and talks it draws on. It must not put words in a teacher's mouth: each sentence cites the excerpts that support it and passes the verifier, describes what is said in a talk ("in a 2011 talk, Weitsman says...") rather than what a teacher believes, and the verbatim excerpts stay below. Consensus also reports its funnel (papers retrieved, eligible, included), which item 3 adopts.
5. Follow-up questions and threads. A box at the bottom of an answer asks a follow-up in the same thread; the follow-up is rewritten into a standalone question with the thread's context. Threads keep the tester's privacy mode, and a back-to-this-question link sits on each answer.
6. One row of links under each excerpt (Listen, Read, Cite, Give, Report), with Cite opening that excerpt's MLA citation and a copy button, beside the References list.

Rights and removal
7. Verified removal requests (PRD change). A request names the requester, an email at their organization, the organization (a sangha or publisher) and the request type, and the requester confirms the email through a link before the request is acted on, so a malicious request cannot take material down unchecked. Needs domain email (items 1 to 3 above).
8. Rights-holder likelihood. After email confirmation, signals (an email domain matching the sangha's site, a name matching a registered teacher or a ledger contact, an earlier permission) route a request: a strong match is acted on at once, a weak one goes to the owner. A low score never refuses a request on its own.

Discovery
9. Search engines and answer engines. Not for the private demo. For a public launch: the About, teacher and sangha pages and talk pages a teacher approves are indexable; excerpts and answers stay out of indexes unless a teacher opts in.
10. Corpus search for researchers, rights holders and teachers. Talks only, with filters for teacher, sangha, date, duration, series and works cited, respecting display levels, so a teacher or rights holder can see everything of theirs that is here.
11. CSV export of a talk search: citation fields, internal talk ids and source links, never transcript text, rate limited, with Spec 03's rule on ShareAlike sources.

Accounts and support (PRD change)
12. Accounts through federated sign-in only (Google and Apple first, others if interviews ask), with saved threads in a left-hand pane, an explicitly opted-in newsletter, and management of recurring support to the site. Needs a privacy policy and real account deletion. The invite links can stay for beta.
13. Support for the site. Money given to the site goes through a hosted payment page (a processor's checkout link), never through code in this repository, which keeps CLAUDE.md rule 7 intact. It is called support, not donations, until a nonprofit or fiscal sponsor exists, and the page says gifts are not tax-deductible until then. Gifts to sanghas stay outbound links as now.
14. A "Keep the site running" page linked from the header: the site's support links first; a suggested amount with an expandable explanation of how it is calculated (fixed costs for hosting and development allocated per question on a rolling monthly volume, plus the cost of the reader's own use); the site's monthly operating cost, the cost of the reader's use, a sustaining level for growth, total support received and the current deficit; a plain statement that the site is free and nothing is expected; and below, a search for any sangha's giving links. Needs monthly cost data from AWS billing and model spend.

Accessibility
15. Close every remaining accessibility item: automated axe and contrast checks in CI and the manual passes in Spec 06. The first cohort's screen-reader and keyboard-only testers are a beta requirement already.

Small
16. Return to the top of the page from each answer.

## Decisions recorded October 3, 2026

- Naming: the repository is `Wheel-Turning/wheel-turning`, the command is `wt` and variables start with `WT_`. The first Claude Code pass renames the earlier `dg` names, and the Platform spec records it.
- Legal entity: the owner as an individual for now. Accounts are opened in the owner's name. Operating personally also means personal exposure to any rights claim, so counsel should weigh in, and a nonprofit or fiscal sponsor sooner rather than later is worth discussing.
- Domain: `wheelturning.org`, with v1 at `demo.wheelturning.org`.
- SSZ sources: Vimeo holds every talk and supplies the listening link. Apple Podcasts and Spotify carry only some, so a matching feed episode can supply audio, and the rest needs a Vimeo access token or enabled downloads from SSZ. Set out in the ingestion spec.
- Source copies: the index keeps each work's whole text as units. Legal pages are stored separately for metadata and rights, and original files are deleted after the proofread for mandated sources.
- Excerpt limits: talk passages and text units up to 250 words, with fewer neighbors for mandated editions, set out in the texts spec.
- OCR proofreading: 24 units per edition, and automatic range checks.
- Language-model cap: 25 dollars a month, with 60 in the first ingestion month, set out in the transcript pipeline spec.
- Question privacy: a tester chooses whether questions are kept, and can delete them, set out in the web spec.
- Alerts: an AWS notification to the owner's address for removal requests, accessibility reports and failures.

Development machine: a Windows PC with WSL 2, an Intel Arc integrated GPU and 16 GB of RAM, and no Mac or NVIDIA card. The Windows notes in the platform spec and the hardware section in the transcript pipeline spec set the consequences: Unix-style tooling inside WSL 2, line-ending rules for the repository, a transcription worker built natively on Windows, and a measured decision between running locally and renting a GPU machine.
