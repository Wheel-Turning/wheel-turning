# Spec 05: Retrieval, synthesis and dana

Implements M5 (retrieval and answer assembly), M6 (synthesis) and the dana calculator of M7. A question comes in and a persisted, renderable `AnswerView` comes out. Rules come from the PRD's answer experience and ranking sections, and tunable values live in `config/retrieval.toml`.

The query path, in order:

1. Validate and normalize the query, then detect a named teacher.
2. Embed the query, and run full-text and vector search over `servable_passage`.
3. Fuse the two result lists with reciprocal rank fusion.
4. Rerank the fused candidates with the cross-encoder and drop those below a relevance threshold.
5. Select passages with per-teacher caps, diversity and a source ceiling.
6. In standard mode, draft themes, filter them deterministically and verify them.
7. Assemble the `AnswerView` and compute the dana panel.
8. Persist the query log, answer and answer passages, then render.

## Query parsing and named-teacher detection

1. Trim, collapse whitespace, strip control characters. Reject queries shorter than 3 or longer than 500 characters with a friendly error.
2. Build an Aho-Corasick automaton over every `teacher_alias`, lowercased, matched on word boundaries with longest match winning. It is built at startup and rebuilt when the registry changes.
3. An alias shorter than 4 characters, or one that appears in `config/common_words.txt`, is ignored unless a title word (roshi, sensei, rev., dr.) immediately precedes it. This keeps ordinary words from being read as names.
4. Exactly one distinct teacher matched sets mode `NamedTeacher(teacher_id)`. The matched alias text is removed from the string used for embedding and full-text search, so passages are not favored for merely mentioning the name.
5. Two or more distinct teachers matched run standard mode, and the page shows a one-line notice that named-teacher search takes one teacher at a time. Zero matches run standard mode.
6. The original query text is what the log and the page show.

## Hybrid retrieval, reranking and selection

Defaults below go in `config/retrieval.toml`. Numeric values are starting points to calibrate on the 50-question set before the first tester session.

```toml
[retrieval]
fts_top_k = 60
vector_top_k = 60
rrf_k = 60
fuse_top_k = 40            # candidates sent to the reranker
hnsw_ef_search = 100
rerank_min_score = 0.30    # sigmoid of the reranker logit; calibrate on the eval set
rerank_timeout_s = 10

[selection]
target_teachers = 3
per_teacher_cap = 3
max_passages = 10
max_sources = 8            # distinct talks in one answer
mmr_lambda = 0.7
same_talk_min_gap_s = 60   # passages closer than this in one talk count as near-duplicates
named_min_passages = 2
named_max_passages = 8
named_per_talk_cap = 2
```

Two input types keep the ranker honest. Everything before selection works on `RankInput`, which has only a passage id, text and embedding. Selection works on `SelectInput`, which adds teacher id, talk id, start time and the relevance score. Neither has a field for sangha, profile status, upload volume or donation link, and sangha details are joined only when the answer is assembled.

Steps in `wt-retrieve`:

1. Full-text search. Content words from the query, minus stopwords, are joined with OR into a `to_tsquery('english', ...)` expression, since practitioner questions are long and an AND query returns nothing. Rank by `ts_rank_cd`, take `fts_top_k`.
2. Vector search. Embed the query, run `set local hnsw.ef_search = 100`, and order by cosine distance, taking `vector_top_k`. Both searches read `servable_passage` and nothing else. In named mode both add `teacher_id = $1`.
3. Fusion. Reciprocal rank fusion with score the sum of `1 / (rrf_k + rank)` across the two lists. Keep the top `fuse_top_k`.
4. Rerank. Score each query and passage pair with the cross-encoder, apply a sigmoid, and drop anything below `rerank_min_score`. If the reranker times out, fall back to the fused order with the threshold dropped and record `rerank_degraded` in the log. If rerank latency is too high on the target host, lower `fuse_top_k` first, then switch to the smaller reranker.
5. Selection in standard mode. Walk the surviving candidates by score. In a seed round, take the best candidate from each distinct teacher in score order, up to `target_teachers`. Then fill remaining slots by MMR, choosing the candidate that maximizes `mmr_lambda * score - (1 - mmr_lambda) * max_cosine_to_selected`. Skip a candidate that breaks `per_teacher_cap`, would add a ninth distinct talk, or lies within `same_talk_min_gap_s` of an already selected passage in the same talk. Stop at `max_passages`.
6. The system never pads. If fewer than three teachers have candidates above the threshold, the answer has fewer teachers and says how many it found. The count is stored in `query_log.teacher_count`.
7. Selection in named mode. Take the top `named_max_passages` by score with MMR and `named_per_talk_cap`. If fewer than `named_min_passages` survive, the answer says how many passages from that teacher were found, or that none were, and never substitutes another teacher.
8. No soft preference for a minimum number of sanghas is implemented in v1. If the PRD's optional test is run later, it needs `sangha_id` added to `SelectInput`, a flag default off, and a visible disclosure on any answer where it overrides relevance.

## Synthesis and verifier

Synthesis exists to say how approaches differ in one short line, never to speak for a teacher. It runs only in standard mode. Passages always render, so a synthesis failure removes the line and nothing else.

```rust
pub struct SynthesisInput { pub query: String, pub passages: Vec<(PassageRef, String)> }  // refs P1..Pn in score order
pub struct ThemeDraft { pub description: String, pub passage_refs: Vec<String> }
pub struct SynthesisOutput { pub themes: Vec<ThemeDraft> }
pub struct VerifierOutput { pub supported: bool, pub unsupported_part: Option<String> }
```

The generation prompt, stored in `crates/wt-synth/prompts/synthesize.md`, must state these rules to the model:

- You receive a question and numbered passages from different talks. Return at most four themes.
- Each theme is one sentence of at most 30 words describing a difference or a shared emphasis in how the passages approach the question.
- Each theme cites at least two passage numbers that support every part of the sentence.
- State only what the passages say. Do not add advice, interpretation, quotations or names of teachers.
- Return the structured output and nothing else.

Deterministic filters then run in this order, and each drop is counted and logged with its reason:

1. The output matches the schema, and at most `max_themes` (default 4) remain.
2. Every cited ref exists in the offered set. Duplicate refs are collapsed.
3. At least two distinct passages support the theme.
4. The description is one sentence of at most 30 words.
5. It contains no quotation marks and no teacher alias from the registry.
6. Grounding. After stopword removal and simple stemming, at least `min_grounding_ratio` (default 0.4) of its content words appear in the union of its cited passages. This ratio is tuned on the claim audit and is a coarse guard against drift, not a proof.

Each surviving theme goes to the verifier, a second model call that sees only the theme and its cited passages. The verifier prompt, in `crates/wt-synth/prompts/verify.md`, says a theme is supported only if every distinct assertion in it is stated or directly entailed by at least one cited passage, and that a claim of difference requires the cited passages to show different emphases. A theme the verifier does not mark supported is dropped. Verifier calls run in parallel. `WT_LLM_MODEL_VERIFY` may name a different model and defaults to `WT_LLM_MODEL`.

Outcomes:

- Zero themes survive: the page omits the synthesis line.
- A generation or verifier call fails or exceeds `synthesis_timeout_s` (default 20): the page omits the synthesis line and the log records `synthesis_failed`.
- The code, not the model, maps refs back to passage ids, and the rendered theme shows its passages as footnote chips.
- Every drop with its reason is written to the job log so the 50-answer integrity audit can read it.

The model receives passage text, so `DataClass::Real` rules from Spec 04 apply. In named-teacher mode no synthesis call is made.

## Answer assembly

The assembler turns selected passage ids into the structure the page renders. All passage text comes from `servable_passage` rows, never from a model.

```rust
pub struct AnswerView {
    pub answer_id: AnswerId, pub query: String, pub mode: AnswerMode,   // Standard | NamedTeacher
    pub teacher_count: u8, pub notice: Option<Notice>,
    pub themes: Vec<ThemeView>,                 // description plus passage ids
    pub groups: Vec<TeacherGroup>,
    pub talks_cited: Vec<TalkRef>, pub texts: Vec<TextRef>,
    pub dana: DanaPanel,
}
pub struct TeacherGroup { pub teacher: TeacherRef, pub sangha: SanghaRef, pub passages: Vec<PassageView> }
pub struct PassageView {
    pub id: PassageId, pub runs: Vec<TextRun>,   // plain text and footnote markers, in order
    pub talk: TalkRef, pub start_ms: i32, pub source_link: String, pub give: Option<GiveLink>,
}
pub enum Notice { ThinCoverage { teachers: u8 }, NoneFound, NamedNone { teacher: String }, NamedFew { teacher: String, found: u8 }, MultiNamed }
```

Rules:

1. Groups are ordered by the best passage score in the group, and passages inside a group by score. The page does not reorder them.
2. `source_link` for YouTube is `https://www.youtube.com/watch?v=<external_id>&t=<seconds>s`, for any other source `<source_url>#t=<seconds>`. Seconds are `max(0, start_ms - link_lead_in_ms) / 1000` with `link_lead_in_ms` defaulting to 2000.
3. `give` exists only when the sangha is `listed`, `donation_verified` and has a donation URL. Otherwise the passage shows attribution with no give button. The link target is the app route `/give/<answer_id>/<sangha_id>`, which logs the click and redirects (Spec 06).
4. Footnotes come from `servable_citation`. Resolved and probable citations become numbered markers at their span offsets, numbered in order of appearance across the answer. Splitting text into runs at span offsets happens in code, and the template escapes everything.
5. Two lists close the page. Talks cited holds each talk's title, teacher, date, series and part, and a link to its start. Texts referenced holds each distinct work and locator with a confidence badge of resolved, probable or unresolved, and a link-out when `locator_url` exists. v1 shows no excerpts of any text, so `work.excerptable` is unused until v3.
6. Texts never count toward the teacher count and never enter the dana split.
7. The answer is persisted as `answer.payload` holding mode, counts, theme text with passage ids, and ordered passage ids, never passage text. A permalink re-renders through the serving views, and a passage that no longer serves renders as a one-line withdrawn marker.

Tuning keys under `[answer]` and `[synthesis]` in `config/retrieval.toml`: `link_lead_in_ms = 2000`, `max_themes = 4`, `min_grounding_ratio = 0.4`, `synthesis_timeout_s = 20`.

## Dana calculator

The PRD's dana mechanic is a pure function in `wt-dana`. It handles no money, performs no I/O and has no payment dependency.

```rust
pub struct Recipient { pub sangha_id: SanghaId, pub name: String, pub url: Option<String>,
                       pub url_template: Option<String>, pub verified: bool, pub listed: bool }
pub struct DanaInput { pub gift_total_cents: i64, pub site_cents: i64, pub recipients: Vec<Recipient>, pub tiebreak_seed: u64 }
pub struct DanaLine { pub sangha_id: SanghaId, pub name: String, pub cents: i64, pub link: String }
pub struct DanaSplit { pub lines: Vec<DanaLine>, pub site_cents: i64, pub total_outlay_cents: i64, pub excluded: Vec<SanghaId> }
pub fn split(input: &DanaInput) -> Result<DanaSplit, DanaError>;
```

Behavior:

1. The donor enters a gift total, which is divided among sanghas, and an optional separate site amount. The site line is a visible choice and is never taken out of the sangha total.
2. Deduplicate recipients by `sangha_id`. Remove any that are not `listed`, not `verified`, or have no URL, and report them in `excluded`.
3. Sort the remaining recipients by `sangha_id`, then rotate the list by `tiebreak_seed mod n`.
4. Compute `base = total / n` and `rem = total % n` in integer cents. The first `rem` recipients in the rotated order get one extra cent. The caller passes a seed derived from the answer id, so the extra cent does not always land on the same sangha.
5. Reject negative amounts and totals above `max_total_cents` (default 10,000,000) with `DanaError`. With no eligible recipient the split has no lines, and the panel shows a plain note.
6. A link is the sangha's donation URL, or its template with `{amount_cents}` or `{amount}` (dollars with two decimals) substituted after URL-encoding. Registry import validates that a template is HTTPS and has the same host as the donation URL, and the calculator re-checks that before substituting.

Property tests with `proptest`:

- The sum of line amounts equals the gift total whenever at least one recipient is eligible.
- Every share is `floor(total/n)` or one more, so the largest and smallest differ by at most one cent.
- No share is negative.
- Permuting the input recipient order with the same seed gives the same set of sangha and amount pairs.
- Duplicates of one sangha collapse to one line.
- Ineligible sanghas never appear in lines and always appear in `excluded`.
- The site amount never changes any sangha share.

## Acceptance criteria

A synthetic fixture corpus with planted relevant passages across at least five fake teachers and four fake sanghas backs the automated tests. Real transcripts are never in the test suite.

M5, retrieval and assembly

- [ ] Full-text-only, vector-only and fused retrieval each return the planted relevant passages for the fixture queries, and fusion beats either alone on a mixed query.
- [ ] Retrieval code reads only `servable_passage`. A test connects as `dg_web` and confirms a direct read of `passage` is denied, and a grep test fails on any other table name in `wt-retrieve` queries.
- [ ] Selection never pads. Fixtures with candidates from one, two and four teachers produce exactly that teacher count, and the count is stored.
- [ ] Per-teacher cap, source ceiling, near-duplicate gap and MMR each have a test that fails when the rule is removed.
- [ ] Named mode filters to the named teacher, suspends the three-teacher target, produces the `NamedNone` and `NamedFew` notices from fixtures, and never returns another teacher's passage.
- [ ] The ranking invariance test shuffles sangha, donation, profile and volume fields in the fixture database and asserts identical passage order out of retrieval and selection.
- [ ] Assembly tests cover source link formats for both platforms, the lead-in offset, no give button for unlisted or unverified sanghas, footnote numbering across passages, and texts excluded from the teacher count.
- [ ] The 50-question set runs end to end on the target host, and per-stage timings are recorded in the eval run. Latency is reported and not a pass or fail.

M6, synthesis

- [ ] Each deterministic filter has a fixture that it drops and a fixture that it passes.
- [ ] A `FakeLlm` verifier that rejects a theme drops it, and the drop reason lands in the log.
- [ ] A theme containing a teacher alias or a quotation mark is dropped before the verifier is called.
- [ ] A generation timeout and a verifier failure each leave a fully rendered answer without a synthesis line.
- [ ] Named mode makes no synthesis call.
- [ ] On the claim-audit sample, no theme that a reviewer marks invented survives the filters and verifier.

M7, dana calculator

- [ ] All property tests in the dana section pass with at least 1,000 cases each.
- [ ] URL template validation rejects a non-HTTPS template and a template whose host differs from the donation URL.
- [ ] `cargo deny` still passes with no payment dependency in `wt-dana`.
