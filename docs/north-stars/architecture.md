# Dharma Gates: Architectural North Star

Oct 1, 2026 · @Alex

## Purpose

This document fixes what must stay true about Dharma Gates' architecture as the product grows from a private SSZ demo to a multi-sangha platform. The PRD says what the product promises. This document says which structures keep those promises from being broken by a later change, a new contributor, or a convenient shortcut.

Readers are the founder, any engineer or contractor who joins, and counsel reviewing consent and rights handling. The v1 technical design (Rust workspace, Postgres with pgvector, Axum) is one realization of this direction and can be replaced without touching anything below. A proposal that conflicts with the invariants needs an amendment to this document first, with the reason recorded.

## North star

Dharma Gates returns only what a teacher said under a recorded rights basis, never after an opt-out, exactly as said, with the route to the source and to the giving attached, and its structure makes the opposite impossible rather than merely discouraged.

Six properties follow from that sentence, and every design choice is checked against them.

1. Verbatim by construction. Every word a user reads as a teacher's comes from a stored passage row. No model output is ever displayed as a quotation.
2. Rights before existence. A talk enters the index only with a recorded rights basis, which is the speaker's consent, a verified license or the project owner's recorded mandate, and never after an opt-out or takedown. Without one it cannot enter the index, and it cannot be returned even if a bug let it in.
3. Relevance alone ranks. The ranker cannot see sangha, profile status, upload volume, donation links or partnership, because its input has no place for them.
4. Attribution travels with the text. Teacher, sangha, source moment and donation destination move together as one unit, so no passage can be shown without them.
5. Teachers can leave. Any teacher can have all their words removed in one operation, and the system can demonstrate that nothing remains.
6. Nothing is held that the product has no right to hold. Audio and student voices never reach durable storage, and full transcripts are never displayed.

## Invariants

Each PRD rule is enforced by a mechanism that does not depend on anyone remembering it, and each mechanism has an automated guard. A rule with no row here is a preference, not an invariant.

| PRD rule | Mechanism | Guard |
| --- | --- | --- |
| No indexing without a rights basis and a known rights holder | Indexing accepts only a type built from a cleared talk. Retrieval reads one database view that filters on rights basis and rights holder. | Nightly check that no indexed passage belongs to a talk with no rights basis. Test that indexing a pending talk fails. |
| Passages are verbatim | The model receives passage IDs and returns IDs. Pages are assembled from database rows. | Test that every displayed quotation is a substring of a stored passage. |
| Synthesis states nothing unsupported | The schema has no free-text field. Code drops themes with fewer than two distinct passages. A verifier pass drops unsupported descriptions. | 50-answer audit. Any surviving invented claim fails v1. |
| Relevance alone ranks | The ranker input type has no field for sangha, profile status, upload volume or donation link. | CI test permutes that metadata and asserts identical ordering. |
| Equal split across cited sanghas | Pure function in integer cents. Dedupes by sangha, drops unverified or opted-out recipients, keeps the site line separate. | Property tests: shares sum to total, none negative, order does not matter. |
| No money passes through the product | No payment library in the dependency tree. Donation actions are outbound links. | CI deny-list on payment dependencies. |
| Text only, no audio stored | Audio lives in scratch storage with a lifecycle rule and an explicit delete step. No schema column or bucket prefix holds audio elsewhere. | Test that scratch is empty after a pipeline run. Bucket configuration check. |
| Student voices never indexed or shown | Student segments are dropped inside segmentation, before chunking and embedding. Q&A question fields are null or paraphrased. | Labeled sample scan. Assertion on embedder input. |
| Full transcripts never displayed | Serving queries return passage-length rows only, and the view caps length. | Render test against the length cap. |
| Query logs are read in aggregate | Two database roles. Raw text is reachable only through a function that returns flagged rows. | Permission test on both roles. |
| Teachers can pull their talks | One transaction deletes passages, embeddings, segments and stored transcripts, flips consent and writes the takedown log. | Post-takedown check that counts are zero in every store. |

## Principles

These settle design arguments. When two options both satisfy the invariants, the principles choose between them.

1. Make the wrong thing unrepresentable. Prefer a type, a schema constraint or a missing field over a runtime check, and a runtime check over a code review comment. The ranker cannot be biased by sangha if it never receives sangha.
2. Models propose, code disposes. Language models may classify, extract, rank candidates and draft theme descriptions. They never produce text that is displayed as a teacher's words, and every model output passes through deterministic validation before it reaches a page.
3. One consistency boundary. Transcripts, passages, embeddings, consent state and the takedown log live in one database so a takedown or a consent change is a single transaction. A second store is added only when measured load demands it, and it must be rebuildable from the first.
4. Every stage is idempotent and keyed by content hash. A glossary fix, a better chunker or a new embedding model means rerunning cheap, safe jobs, never hand repair.
5. The corpus is the product, the ranker is replaceable. Retrieval components sit behind narrow interfaces so embedding models, rerankers and search engines can change without schema or page changes.
6. Boring storage, explicit seams. Postgres, object storage and plain HTTP are enough for the foreseeable scale. Complexity is spent on the trust boundaries, not on infrastructure.
7. Lineage, sangha and teacher are data, not code. Widening from Soto to other lineages, and adding sanghas, is a registry change and a filter change. No sangha is named in a branch of code.
8. Measure what the product claims. Teacher breadth, synthesis drift, dana link follow-through and thin-coverage responses are logged as first-class metrics because the PRD's pass and fail criteria depend on them.

## Target architecture

A talk moves down through four zones, and the consent gate is the first thing any talk meets.

In short: consent gate → scratch zone (audio and video, only during transcription and diarization) → processing zone (text only) → core store (cleared, segmented text) → serving zone (reads the core store through one view and writes only query logs).

Rust carries every stage that enforces an invariant or sits on the serving path: domain types, the gate, glossary correction, segmentation post-processing, citation resolution, chunking, retrieval, selection, synthesis validation, the dana calculator, the web server and the admin command line. Python is confined to evaluation notebooks and, unless sherpa-onnx proves adequate, a diarization sidecar. Embedding and reranking run locally through ONNX Runtime.

Stages run as idempotent jobs from a Postgres queue. Postgres with pgvector and built-in full-text search is the only searchable store. Tantivy or Qdrant may sit behind the retrieval interface if the corpus passes a few million passages, and either would be derived from Postgres and rebuildable.

## Trust and data boundaries

Data moves through four zones behind one gate, and each crossing has one owner and one rule. Nothing is pulled or transcribed until the consent gate passes, so talks without a rights basis exist only as metadata. Audio and video exist only in the scratch zone. The processing zone holds text only. Cleared, segmented text lives in the core store. The serving zone reads the core store through one view and writes only query logs.

The audio boundary sits at the end of transcription and diarization. Scratch storage has a lifecycle rule that expires objects within a day, and the pipeline deletes explicitly on success and on failure. Nothing downstream has a place to put audio.

The consent boundary sits at the front of the pipeline, before any media is pulled, and is checked again at indexing by a type that only a cleared talk can satisfy. It is the same code path for a v1 SSZ talk and a v2b partner upload. Student segments are removed at the segmentation stage, before chunking and embedding.

The model-provider boundary governs what leaves the infrastructure. Embedding and reranking run locally so retrieval never sends transcripts out. Extraction, classification assistance, synthesis and verification call a hosted model, and only cleared talks reach those calls. The notice to each teacher names this step, and zero-retention terms are confirmed on the provider account before the first speaker's transcript is sent. Counsel reviews this alongside caption ingestion and dana routing.

The log boundary is two database roles. Analysts see aggregate views. Raw query text is readable only through a function that returns queries a tester has flagged, which makes the stated policy mechanically true.

The removal boundary is the takedown operation. It reaches every store that can hold a teacher's words: passages, embeddings, segments, stored transcripts, caches and search indexes. Any new store added later must register with the takedown routine before it ships, and the post-takedown check enumerates stores from the same registry.

## Evolution path

The architecture grows by adding capabilities behind the same two chokepoints: the consent gate on the way in, and the ranker's relevance-only input on the way out. Each stage below adds something new and leaves the invariants untouched.

| Stage | What changes in the architecture | Trigger |
| --- | --- | --- |
| v1, private demo | One Postgres, one web binary, batch pipeline run by the founder. Takedown is a command-line script. Citations are a hand-listed set. Access is invite-only. | Passes retrieval faithfulness and synthesis integrity on the 50-question set |
| v1.5, follow-up | Conversation state anchored to the same passage IDs. Follow-up answers go through the same validator and cannot introduce text that is not a stored passage. | Retrieval has proven out |
| v2a, teacher track | Teacher identity with permissions scoped to their own talks: review how their words appear, pull talks, supply corrections. Context cards for cited texts. | v1 reviewers confirm the excerpts are ones they would stand behind |
| v2b, sangha track | Accounts and claimed profiles that unlock corrections, context and archive upload and nothing else. Partner uploads enter through the same consent gate plus a review step. Public takedown tooling. Lineage widens by registry tag. A dedicated search engine joins behind the retrieval interface if volume requires it. | Readiness test at roughly 15 to 25 Soto centers and a few thousand segmented talks, with at least three teachers in most answers |
| v3, gifts and texts | Single-gift split through a payment connector for claimed recipients, as a separate service with its own boundary. Licensed translations and translator-credited excerpts, controlled by the `work.excerptable` flag. | Counsel review of tax and money-transmission questions, and signed translation licenses |

The PRD runs v2a and v2b as separate tracks. This table assumes v2a serves teachers and v2b serves sanghas, which is an inference from the PRD's user table, so correct it if the tracks were drawn differently.

What never changes across stages: the six properties, the invariants table, the single consistency boundary for consent and takedown, and the rule that the ranker has no input for money, partnership or profile status. A stage that needs one of these to bend is a stage that needs this document amended first.

## Anti-goals

The architecture deliberately will not do the following, even if a later request makes it tempting.

- Generate answers in a teacher's voice, or merge several teachers into one synthesized answer.
- Display a model-written paraphrase as if it were a quotation.
- Apply any ranking signal other than relevance, including a soft boost for the founder's own sangha.
- Hold, move or route money. The single-gift payment split is a v3 candidate that needs counsel and a separate service with its own boundary.
- Store audio or video, or show a full transcript.
- Index or display student speech in any form.
- Train or fine-tune models on teachers' words.
- Add a second system of record. Caches and search engines are derived and rebuildable.
- Open the demo beyond its private testers before counsel has reviewed the mandate model.
- Hard-code a sangha, lineage or teacher into application logic.

## Fitness functions

These checks run continuously so the architecture drifts loudly, not silently. The first five protect structure. The last four protect the PRD's measured claims and feed its pass and fail decisions.

| Check | Asserts | When it runs |
| --- | --- | --- |
| Ranker invariance | Shuffling sangha, profile, volume and donation metadata leaves result order unchanged | Every CI run |
| Consent drift | No indexed passage belongs to a talk with no rights basis, an opt-out or an unknown rights holder | Nightly |
| Takedown completeness | After a takedown, zero rows for that teacher remain in any store in the registry | CI on fixtures, and after every real takedown |
| Audio residue | Scratch is empty after each pipeline run, and no audio object exists outside scratch | Nightly |
| Stage reproducibility | Rerunning a stage on unchanged input yields identical content hashes | Every CI run |
| Retrieval quality | The 50-question set, rated by the teacher reviewer, does not score worse than the last accepted run | Any change to chunking, embeddings, reranking or glossary |
| Synthesis drift | Share of themes the verifier drops, plus the claim-by-claim review of sampled answers | Weekly during v1 |
| Teacher breadth | Distribution of answers with one, two, and three or more teachers, and count of thin-coverage responses | Weekly, feeds v2b corpus sizing |
| Sangha spread | Distribution of distinct sanghas per answer, so concentration in the dana split is visible | Weekly |

## Open decisions and review

None of these decisions touches an invariant. Each one has a trigger that forces it.

| Decision | Options | Trigger |
| --- | --- | --- |
| Speaker diarization | Python sidecar with pyannote, or sherpa-onnx in Rust | Result of the ten-video segmentation sample |
| Embedding location | Local through ONNX, or a hosted API | Consent wording is final and provider terms are confirmed |
| Synthesis verifier | Second language-model call, or a local entailment model | First claim-by-claim review of 20 answers |
| ASR hosting | Decided October 4, 2026: the owner's Windows PC, with the OpenVINO encoder on its Intel GPU, reaching the database through an SSM port forward | Decided; see docs/asr-benchmark.md |
| Tester access | Invite-link allowlist, or an identity-aware proxy | Before the first tester sees the page |
| Model provider terms | Zero-retention agreement and its scope | Before any speaker's transcript is sent for extraction |

This document is reviewed at each roadmap gate and whenever a change touches a row of the invariants table. Amendments are dated and state which invariant moved and why.

## Amendment log

October 3, 2026. Property 2 changed from consent before existence to rights before existence. The project owner decided that recorded talks from teachers who have not replied are used by default, that each teacher can leave at any time, and that a takedown is permanent. What moved: the basis for clearing a talk can now be the owner's recorded mandate, alongside speaker consent and a verified license. What did not move: the consent gate remains the only path into the index and is enforced in the type system and the database, student speech is never stored, the page never claims permission that was not given, a teacher's opt-out removes their passages from serving at once, and an excluded teacher, work or file can never be loaded again. A single database setting turns mandated talks off without deleting anything, so the previous behavior is one command away. The v1 spec set carries the mechanics. The PRD's consent section needs the matching rewrite.
