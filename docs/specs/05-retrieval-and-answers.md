# Spec 05: Retrieval, synthesis and dana

Implements M5 (retrieval and answer assembly), M6 (synthesis, now the summary) and the dana calculator of M7. A question comes in and a persisted, renderable `AnswerView` comes out. Rules come from the PRD's answer experience and ranking sections, and tunable values live in `config/retrieval.toml`.

The query path, in order:

1. Validate and normalize the query, then detect a named teacher.
2. Embed the query, and run full-text and vector search over `servable_passage`.
3. Fuse the two result lists with reciprocal rank fusion.
4. Rerank the fused candidates (a Haiku-class model on the web service since October 15, 2026; the cross-encoder elsewhere) and drop those below a relevance threshold.
5. Select passages with per-teacher caps, diversity and a source ceiling.
6. In standard mode, draft the summary, filter its sentences deterministically and verify them.
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
hnsw_ef_search = 1000     # pgvector's maximum; keeps exact-search recall
rerank_min_score = 0.30    # sigmoid of the reranker logit; calibrate on the eval set
rerank_timeout_s = 10

[ranking]                  # owner's decision of October 15, 2026; config/retrieval.toml holds the values in use
ranker = "model"           # or "onnx", the cross-encoder
batch = 10
max_chars = 1500

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

1. Full-text search. Content words from the query, minus stopwords, are stemmed as the passages were, and lexemes found in more than `fts_max_lexeme_share` (0.10) of passages are left out (migration `0045_common_lexemes.sql` reads them from the table's statistics, once the collection holds at least 10,000 passages; owner's request of October 15, 2026). When every lexeme is that common, the least common stays. The rest are joined with OR into a `to_tsquery('simple', ...)` expression, since practitioner questions are long and an AND query returns nothing. Rank by `ts_rank_cd`, take `fts_top_k`. Words like know (52% of passages), practice (29%) and feel (17%) had made every question rank most of the collection, 1.1 to 1.6 s on RDS and 5 s cold. Over 40 eval questions, the ranking model scored 7 or more on 1,132 fused candidates with them left out, against 999 with every word kept, and the query's median time fell from 479 ms to 115 ms on a copy of the data.
2. Vector search. Embed the query, run `set local hnsw.ef_search = 1000` with a strict-order iterative scan, and order by cosine distance on the half-precision cast `embedding::halfvec(1024)`, which `passage_embedding_half_hnsw` indexes (migration `0043_passage_halfvec_index.sql`, owner's request of October 15, 2026), taking `vector_top_k`. The index is a third of the full-precision one's size (372 MB against 1,117 MB for 143,223 passages), so it stays in the database's cache; the full one was read from disk on most beta questions, 2 to 7 s. Over 119 eval questions at ef_search 1000 its recall@60 against an exact search was 0.984 mean (0.986 for the full-precision index) at a warm median of 72 ms. The web service runs a vector search for each of 24 practice phrases at startup and one every five minutes, so a quiet hour does not let the index go cold. The planner misjudges how many passages the view serves, so the search also turns sorting and JIT off for its own transaction to keep the scan on the HNSW index. At 1000 the index keeps recall at the level of an exact search (recall@60 against exact: 0.995 mean, 0.967 worst over the 20 Gate A questions, October 12, 2026) for about 70 to 95 ms; 100 gave 0.88 mean and 0.48 worst. Both searches read `servable_passage` and nothing else. In named mode both add `teacher_id = $1`.
3. Fusion. Reciprocal rank fusion with score the sum of `1 / (rrf_k + rank)` across the lists: full text, vectors, and a vector search over `servable_student_question` in which each hit stands for the teacher passage that answers it (Spec 04, Student questions, rule 6; owner's decision of October 10, 2026). A question is never a candidate itself. Keep the top `fuse_top_k`.
4. Rerank. `[ranking] ranker` picks the scorer (owner's decision of October 15, 2026). `model`, the web service's setting: the candidates go to a Haiku-class model (`LlmStage::Rank`, `WT_LLM_MODEL_RANK`) in batches of `batch` (10) that run side by side, with the prompt in `crates/wt-retrieve/prompts/rank.md`; it returns JSON scores from 0 to 10 for candidate numbers and never text (CLAUDE.md rule 5), and a score `s` counts as `s / 10`. With 60 fused candidates the six calls took 2.1 to 2.6 s together; batches of 15 took 2.3 to 4.0 s on the 20 Gate A questions. The cross-encoder (`onnx`) took over 20 s for 40 candidates on the service's two CPUs, so every beta answer had fallen back to the fused order. `onnx`: score each query and passage pair with the cross-encoder and apply a sigmoid. Either way, drop anything below `rerank_min_score` (0.35 for the model, keeping 4 and up; 0.15 for the cross-encoder). A batch that fails, or leaves a candidate unscored, fails the whole call. If the reranker times out, fall back to the fused order with the threshold dropped and record `rerank_degraded` in the log. Every stored answer logs one `ranker_served` event with `ranker` set to `llm`, `onnx` or `none (fallback)` and the answer id, never the question (Spec 11, PB2.1), and the page of a fallback answer says the relevance check did not finish. The same fallback applies while no reranker model is installed (decision of October 9, 2026, for the owner's alpha); fused scores are then scaled to `[0, 1]` so MMR weighs them on the same scale as similarity. If rerank latency is too high on the target host, lower `fuse_top_k` first, then switch to the smaller reranker.
5. Selection in standard mode. Walk the surviving candidates by score. In a seed round, take the best candidate from each distinct teacher in score order, up to `target_teachers`. Then fill remaining slots by MMR, choosing the candidate that maximizes `mmr_lambda * score - (1 - mmr_lambda) * max_cosine_to_selected`. Skip a candidate that breaks `per_teacher_cap`, would add a ninth distinct talk, or lies within `same_talk_min_gap_s` of an already selected passage in the same talk. Stop at `max_passages`.
6. The system never pads. If fewer than three teachers have candidates above the threshold, the answer has fewer teachers and says how many it found. The count is stored in `query_log.teacher_count`.
7. Selection in named mode. Take the top `named_max_passages` by score with MMR and `named_per_talk_cap`. If fewer than `named_min_passages` survive, the answer says how many passages from that teacher were found, or that none were, and never substitutes another teacher.
8. No soft preference for a minimum number of sanghas is implemented in v1. If the PRD's optional test is run later, it needs `sangha_id` added to `SelectInput`, a flag default off, and a visible disclosure on any answer where it overrides relevance.

## Summary and verifier

The summary (owner's decision of October 12, 2026, replacing the one-line themes and moving Spec 00's post-beta item 4 before beta) lets a reader get the idea without reading every excerpt, in the manner of a Consensus research answer, and never speaks for a teacher. It runs only in standard mode and only over the excerpts the page shows (Answer assembly, rule 8). Excerpts always render, so a summary failure removes the summary and nothing else. On the answer page the summary is written after the answer is stored (owner's request of October 15, 2026, for a page in under 5 seconds): the answer is stored with `summary_pending`, the page shows the summary's place at the top, and a background task stores the summary, or that there is none, through `set_answer_summary` (migration `0044_deferred_summary.sql`). `wt eval run` still waits for the summary, so an evaluation scores the same summary the page would show. Answers stored before this decision keep their themes, which render as before under "How approaches differ".

Shape, top to bottom, in a container with the stable id `summary`:

1. Headline: one sentence that answers the question as the excerpts do.
2. Framing: one or two sentences that set out what the excerpts address.
3. Where the talks agree: up to `max_themes` sentences, each citing excerpts from at least two teachers.
4. Where they differ: up to `max_themes` sentences, each citing excerpts from at least two teachers.
5. Points to keep in mind: up to two model sentences about context a quoted line leaves out, each citing the excerpt it concerns, then notes the code writes from counts: "Only \<n> talk(s) in this collection matched this question closely." when the answer's excerpts come from two talks or fewer. The page gives no note on how many excerpts come from one sangha (owner's request of October 15, 2026, B10; Spec 11, PB2.3).
6. In sum: one sentence.
7. In place of an evidence-strength meter, a line the code writes: "The summary draws on \<n> of these excerpts.", counting the distinct excerpts the shown sentences cite (Spec 11, PB2.3).
8. Quoted lines: one numbered entry per citation marker, in order of first appearance, holding the verbatim quotation, the teacher, the talk title and date, and a link to the excerpt below.

Unused excerpts (owner's decision of October 16, 2026, B10; Spec 11, PB2.2: "both"). The page serves only excerpts that pass `rerank_min_score`, and the model must cite each excerpt at least once or return it under `unused` as `{"excerpt": "E4", "reason": "off_topic" | "duplicate"}`. The code keeps the list by passage id on the stored summary (`StoredSummary.unused`), leaving out any excerpt a kept sentence cites. When the page is loaded with a summary shown, the excerpts no shown sentence cites move, in order and grouped by teacher, to a collapsed "Also related (n)" list below the teacher groups, and every excerpt is numbered again in reading order. They are never removed from a page already shown: the deferred summary's fragment is built on the page's first numbering, and only a later load of the permalink sets excerpts aside. `wt eval report` counts each served excerpt as cited, unused by reason, or neither, and the share of answers from three or more teachers with an agree or differ sentence.

The model sees the question and each excerpt as `E1..En`, with a header naming its teacher (display name) and the talk's year, followed by its sentences numbered `[1]..[m]` (the excerpt's sentences, from the boundaries the index step stores). It returns JSON only and never quoted text (CLAUDE.md rule 5):

```rust
pub struct Cite { pub excerpt: String, pub start: i64, pub end: i64 }  // "E2", sentence numbers
pub struct DraftSentence { pub text: String, pub cites: Vec<Cite> }
pub struct SummaryDraft {
    pub headline: Option<DraftSentence>, pub framing: Vec<DraftSentence>,
    pub agree: Vec<DraftSentence>, pub differ: Vec<DraftSentence>,
    pub keep_in_mind: Vec<DraftSentence>, pub sum_up: Option<DraftSentence>,
}
```

The prompt, in `crates/wt-synth/prompts/summarize.md`, tells the model to describe what is said in a talk ("In a 2011 talk, Weitsman says ...") and never what a teacher believes, thinks or holds; to name a teacher only by the name in the excerpt's header and a year only from it; to add no advice, interpretation or quotation marks; to cite, for each sentence, the shortest run of one or two sentences of each excerpt that supports it; and to leave out any section it cannot support.

The code turns each citation into a quotation: the cited sentences of the excerpt, sliced verbatim from `servable_passage` rows and stored as a span like an excerpt's (the passage ids it covers and word offsets, never text). A permalink rebuilds it; a citation whose quotation no longer serves is dropped, and a sentence left with no citation is dropped with it.

Deterministic filters run on each sentence in this order, and each drop is counted and logged with its reason:

1. The output matches the schema; sentences beyond a section's limit are dropped.
2. Every citation names an offered excerpt and a run of its sentences. Duplicate citations are collapsed. A sentence with an invalid citation, or none, is dropped. A run longer than `quote_max_words` (60) is left out on its own, and the sentence is dropped only when no other citation remains.
3. A sentence of agreement or difference cites excerpts from at least two distinct teachers.
4. The text is one sentence of at most `summary_max_words` (40) words.
5. It contains no quotation mark and no excerpt label such as E3.
6. Attribution. A teacher's name (any word of four or more letters from a registry alias, other than title words such as roshi) appears only when that teacher gave a cited excerpt, and a year appears only when a cited talk is from that year.
7. Belief words and the reader. "believes", "believe", "thinks", "holds that" and "is convinced" are refused, since a sentence reports what a talk says, and so are "you", "your", "yours", "yourself" and "yourselves", which would make a sentence advice in the site's voice (October 13, 2026, after the first Gate A run).
8. Grounding. After stopword removal (including the words a sentence uses to report, such as talks, several and suggests), simple stemming and removal of names and years, at least `min_grounding_ratio` (0.2 since October 15, 2026; 0.4 dropped every sentence of honest paraphrases on the beta) of its content words appear in its cited excerpts. This ratio is a coarse guard against drift, not a proof; the verifier checks meaning. The prompt asks the model to keep to the teachers' own key words.

Each surviving sentence goes to the verifier, a second model call that sees only that sentence and its cited excerpts with their headers. The verifier prompt, in `crates/wt-synth/prompts/verify.md`, says a sentence is supported only if every distinct assertion in it, including who said it and when, is stated in or directly entailed by a cited excerpt; that a claim of agreement or difference needs cited excerpts that show it; and that a sentence stating what a teacher believes, rather than what a talk says, or giving advice in its own voice, is unsupported. Verifier calls run in parallel. `WT_LLM_MODEL_VERIFY` may name a different model and defaults to `WT_LLM_MODEL`.

Outcomes:

- The headline does not survive: the model gets one revision (owner's request of October 15, 2026). The second request repeats the excerpts and lists each dropped sentence with the reason in words, and its draft goes through the same filters and verifier. If the headline is lost again, the page omits the summary, and the drops of both drafts are logged. The generation message also lists each teacher with their excerpt labels, so an agree or differ sentence can cite two teachers.
- A generation or verifier call fails or exceeds `synthesis_timeout_s` (default 20): the page omits the summary and the log records `synthesis_failed`.
- The code, not the model, maps excerpt labels back to passage ids and sentence numbers to quotations.
- Every drop with its reason is written to the job log so the 50-answer integrity audit can read it, and each summary sentence is one claim row in the review file (Spec 07).
- `query_log.theme_count` counts the summary's sentences shown and `themes_dropped` its drops.

The model receives excerpt text, so `DataClass::Real` rules from Spec 04 apply. In named-teacher mode no summary call is made.

## Answer assembly

The assembler turns selected passage ids into the structure the page renders. All passage text comes from `servable_passage` rows, never from a model.

```rust
pub struct AnswerView {
    pub answer_id: AnswerId, pub query: String, pub mode: AnswerMode,   // Standard | NamedTeacher
    pub teacher_count: u8, pub notice: Option<Notice>,
    pub themes: Vec<ThemeView>,                 // answers stored before the summary
    pub summary: Option<SummaryView>,           // sentences by section, quotation markers
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

1. Passages are shown in score order, best first (owner's decision of October 15, 2026, replacing teacher groups ordered by their best passage). Selection still decides which passages appear; the stored order is then sorted by score, ties keeping selection order. Each passage sits under its teacher's name, and consecutive passages from one teacher share the heading. The page does not reorder them.
2. Source links depend on who made the transcript (owner's decision of October 12, 2026, amending this rule and Spec 03's link styles).
   - The source made it: the current transcript's origin is `aligned` or `text`, recorded as `talk.transcript_from_source`. The passage links first to the source's own transcript, `source_video.transcript_url` or else the talk page, labeled "Read the talk's transcript at \<sangha or source> (opens their page in a new tab)". The link carries a text fragment, `#:~:text=<first words>,<last words>` (the first and last four words of the passage, percent-encoded, with `-`, `,` and `&` escaped), so browsers that support it open the page at the passage and the rest open the page. When the source has a player that takes a time, a second link reads "Listen at h:mm:ss"; when it has none, the line says "starts at h:mm:ss". There is no link to a transcript on this site, since the talk is at `excerpts` at most (Spec 03, Display levels, rule 2).
   - The project made it: origin `asr`, `captions` or `reviewed`. The passage links to the recording at that moment, "Listen at h:mm:ss (opens the source in a new tab)", and, at display level `full`, to this site's transcript, "Read our transcript", with "(made by machine, may contain errors)" for `asr` and `captions`, or "(reviewed)" for `reviewed`.
   - Times: YouTube is `https://www.youtube.com/watch?v=<external_id>&t=<seconds>s`, Vimeo `<source_url>#t=<seconds>s`, any other timed player `<source_url>#t=<seconds>`. Seconds are `max(0, start_ms - link_lead_in_ms) / 1000` with `link_lead_in_ms` defaulting to 2000.
3. `give` exists only when the sangha is `listed`, `donation_verified` and has a donation URL. Otherwise the passage shows attribution with no give button. The link target is the app route `/give/<answer_id>/<sangha_id>`, which logs the click and redirects (Spec 06).
4. Footnotes come from `servable_citation`. Resolved and probable citations become numbered markers at their span offsets, numbered in order of appearance across the answer. Splitting text into runs at span offsets happens in code, and the template escapes everything.
5. A References list closes the page (owner's decision of October 12, 2026, replacing the Talks cited and Texts referenced lists). It is a `<details>` element, closed by default, whose summary reads "References (n)", and it holds one entry per distinct talk and per distinct work and locator, in MLA style (9th edition), sorted alphabetically as MLA's works cited are:
   - A talk: `<citation name>. "<Talk title>." Dharma talk. <i><Source site name></i>, <sangha, when it differs from the site name>, <day Mon. year>, <URL without the scheme>. Accessed <day Mon. year>.` The URL is the source transcript for a talk whose source made it, and otherwise the recording. The access date is the date the answer was made. A series and part follow the title as `<Series>, part <n>.`
   - A text: `<author as stored>. <i><Work title></i>. Translated by <translator>, <edition>, <publisher>, <year>, <locator>. <link-out URL without the scheme>.` with elements that have no value left out, and the work-level form without a locator. The confidence badge, resolved, probable or unresolved, follows the entry in words, since MLA has no form for an uncertain attribution.
   - Names: a teacher's entry starts with `teacher.citation_name` and sorts by `teacher.citation_sort`, both set by the owner, so dharma names and single names read as their bearers use them. MLA allows a name in the form its bearer is commonly known by, and a teacher without a citation name is entered by display name, uninverted.
   - Every footnote marker, and the metadata line of each passage, links to its entry (`#ref-<n>`). A small script in `static/` opens the list when a link targets an entry inside it, and without JavaScript the reader opens it.
   A citation of a work as a whole is listed by work alone and links to `work.link_out_url` (Spec 08, Work-level citations). Text excerpts appear only where Spec 08 allows them: the "From the texts" section, and a verified quotation collapsed under its footnote (Spec 08, Quoted passages inline). Both read through `servable_text_unit`, which requires an excerptable edition.
6. Texts never count toward the teacher count and never enter the dana split.
7. The answer is persisted as `answer.payload` holding mode, counts, the summary's sentences with the passage ids and quotation spans they cite (theme text with passage ids for answers stored before the summary), ordered passage ids and each passage's excerpt span (rule 8), never passage text.
8. Excerpts (owner's decision of October 12, 2026). Passages are fixed when a talk is indexed and are the unit retrieval finds; what the page shows is an excerpt cut for the question at answer time: the fewest whole sentences that address it, which may reach into the passages just before and after. 18 percent of passages started mid-sentence and 16 percent ended without sentence punctuation when this was decided.
   - Window. The selected passage plus the passage before and the passage after it in the same talk and the same segment, read from `servable_passage`. A window never crosses a segment boundary, so it never reaches into a student's turn, and a neighbor that does not serve is left out.
   - Sentences. The window's words split into sentences at the boundaries the index step stores for each passage (Spec 04, Chunking, rule on sentence boundaries), so text with little punctuation still has boundaries. The displayed words are never changed: boundaries only decide where an excerpt may start and end.
   - Choosing the span, in order:
     1. The model. The question and the window's sentences, numbered, go to the model with the prompt in `crates/wt-synth/prompts/excerpt.md`, which asks for the shortest run of consecutive sentences that answers the question as the teacher does, including a sentence before it only when the answer makes no sense without it. It returns JSON `{"start": <n>, "end": <n>}` and never text. Code checks that the run is inside the window, overlaps the selected passage by at least one sentence, and holds between `excerpt_min_words` (25) and the 250-word excerpt limit; otherwise the next step decides. Calls for an answer's passages run in parallel under `excerpt_timeout_s` (3 since October 15, 2026; the calls took 0.6 to 1.2 s on Gate A) and count against the spend cap. A real talk's text goes to the model only when `WT_LLM_REAL_DATA_OK` is true.
     2. The reranker. Spans of whole sentences that overlap the passage and fit the limits, at most `excerpt_max_spans` (8) of them built from the passage's best-scoring sentences outward, are scored against the question, and the shortest span scoring at least `excerpt_keep_ratio` (0.9) of the best is kept.
     3. Otherwise the passage itself, trimmed to its first and last whole sentences when that keeps at least `excerpt_min_words`.
   - What is stored. Each passage in the payload carries `span`: the ids of the passages the excerpt covers in order, the word offset where it starts in the first and ends in the last, and which step chose it. A permalink rebuilds the text from `servable_passage` rows by those offsets; if a covered neighbor no longer serves, the excerpt falls back to its own passage, and if the passage itself no longer serves, the withdrawn marker shows as before.
   - On the page. The blockquote shows the excerpt. "Listen at" and the transcript anchor follow the excerpt's first word, using the start times the index step stores for each sentence. Footnote markers whose spans fall inside the excerpt keep their places. The summary and the verifier read the excerpts, not the raw passages, so a sentence is grounded in what the reader sees.
   - Ranking is not changed: selection, the teacher count and the dana split work on passages as before. A permalink re-renders through the serving views, and a passage that no longer serves renders as a one-line withdrawn marker.

Migration for the October 12, 2026 rules (next free number, see `docs/migrations.md`): `talk.transcript_from_source boolean not null default false`, set by the `audio` job whenever it writes a transcript and backfilled from each talk's current transcript origin; `source_video.transcript_url text`, null meaning the source page itself; `teacher.citation_name text` and `teacher.citation_sort text`; and `servable_passage` recreated so its `display_level` is `excerpts` for a talk with `transcript_from_source`. `wt teacher cite --teacher <name> --citation-name <text> --sort <text> --by <name>` sets the citation fields with a `correction_log` row, and `wt teacher cite --sheet <csv>` applies a reviewed sheet.

Excerpt keys under `[excerpt]` in `config/retrieval.toml`: `excerpt_min_words = 25`, `excerpt_timeout_s = 3`, `excerpt_max_spans = 8`, `excerpt_keep_ratio = 0.9`.

Tuning keys under `[answer]` and `[synthesis]` in `config/retrieval.toml`: `link_lead_in_ms = 2000`, `max_themes = 3` (sentences in each of the agree and differ sections), `summary_max_words = 40`, `quote_max_words = 60`, `min_grounding_ratio = 0.2`, `synthesis_timeout_s = 20`.

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
- [x] Per-teacher cap, source ceiling, near-duplicate gap and MMR each have a test that fails when the rule is removed. (`wt-retrieve` `select` unit tests.)
- [x] Named mode filters to the named teacher, suspends the three-teacher target, produces the `NamedNone` and `NamedFew` notices from fixtures, and never returns another teacher's passage. (`wt-retrieve/tests/retrieve.rs`.)
- [x] The ranking invariance test shuffles sangha, donation, profile and volume fields in the fixture database and asserts identical passage order out of retrieval and selection. (`wt-retrieve/tests/retrieve.rs`; the schema has no profile or volume fields, so sangha listing and donation fields are what it shuffles.)
- [ ] Assembly tests cover source link formats for both platforms, the lead-in offset, no give button for unlisted or unverified sanghas, footnote numbering across passages, and texts excluded from the teacher count.
- [x] Source links follow rule 2 as amended on October 12, 2026: a talk with an `aligned` or `text` transcript links to its source transcript with a text fragment built from the passage's first and last four words, shows no link to this site's transcript, and is served at `excerpts` even when its teacher is at `full`; an `asr` talk links to the recording at the right second and, at `full`, to this site's transcript with the machine label. (`wt-api` `tests/references.rs` on a real Postgres, with the transcript stored through `wt_db::transcript::store` as the `audio` job stores it; `view` unit tests for the fragment's encoding, `transcript_url` and the `reviewed` label. The fragment uses the excerpt's first and last four words, since the page shows the excerpt, rule 8.)
- [x] The References list renders closed, holds one MLA entry per distinct talk and per distinct work and locator in alphabetical order by citation sort key, uses `citation_name` when set and the display name uninverted otherwise, carries the confidence word after each text entry, and every footnote marker and passage metadata line links to its entry. (`wt-api` `tests/references.rs`; `view` unit tests for MLA dates and the talk and text entry forms; `wt-cli` `tests/commands.rs` for `wt teacher cite` and `--sheet`.)
- [x] Each `link_style` of Spec 03 (Seattle Soto Zen sources item 4) renders the right link: `vimeo` gives the page address with `#t=<seconds>s`, and `none` gives the "starts at h:mm:ss" text. (Moved from M3, decision of October 5, 2026.) (`view` unit tests: the link style follows the platform, `youtube` and `vimeo` timed, `engagewisdom` and `rss`, a podcast-only match, with "starts at".)
- [x] The 50-question set runs end to end on the target host, and per-stage timings are recorded in the eval run. Latency is reported and not a pass or fail. (`wt eval run` records each answer's stage timings in `eval_result`, Spec 07. Run `65751c22` on October 12, 2026 asked the 20 approved Gate A questions against the real-data database on the owner's PC, which hosts the alpha: total p50 9.3 s, p90 24.9 s; rerank p50 3.7 s; synthesis p50 4.1 s. The other 30 questions of `eval/questions_v1.toml` are drafts awaiting the owner, and run unchanged once approved.)

- [x] Excerpts (rule 8): on fixtures, an excerpt starts and ends on sentence boundaries, can include a sentence from the passage before or after within the same segment, never crosses a segment boundary or the 250-word limit, and is rebuilt verbatim from `servable_passage` on a permalink. A model reply outside the window or the limits falls back to the reranker step, a reranker failure falls back to the trimmed passage, and with `WT_LLM_REAL_DATA_OK` unset no real text goes to the model. "Listen at" follows the excerpt's first word. (`wt-api` `tests/excerpts.rs` on a real Postgres with a `FakeLlm` and a fake reranker; `wt-core` `excerpt` and `wt-synth` `excerpt` unit tests for the boundaries, the limits and the checks; `wt-index` `tests/chunker.rs` for the stored boundaries.)
- [x] `wt eval report` prints the mean excerpt length and the share of excerpts starting or ending mid-sentence, and on `gate_a@1` that share is under 2 percent. (October 12, 2026, on a copy of the real-data database with the 127 talks behind the answers reindexed: 0 of 134 excerpts mid-sentence, mean 115 words, against 11.2 percent and 143 words for the same passages shown whole. Excerpt stage p50 3.4 s, p90 5.7 s.)

M6, synthesis

- [x] Each deterministic filter has a fixture that it drops and a fixture that it passes. (`wt-synth` `filter` unit tests and `tests/synth.rs`.)
- [x] A `FakeLlm` verifier that rejects a theme drops it, and the drop reason lands in the log. (`wt-synth/tests/synth.rs`.)
- [x] A theme containing a teacher alias or a quotation mark is dropped before the verifier is called. (`wt-synth/tests/synth.rs`.)
- [x] A generation timeout and a verifier failure each leave a fully rendered answer without a synthesis line. (`wt-api/tests/pages.rs`.)
- [x] Named mode makes no synthesis call. (`wt-api/tests/pages.rs`.)
- [ ] On the claim-audit sample, no theme that a reviewer marks invented survives the filters and verifier.

M6, the summary (owner's decision of October 12, 2026)

- [x] Each summary filter (citation, two teachers, one sentence, quotation mark or label, attribution, belief words, grounding) has a fixture that it drops and one that it passes. (`wt-synth` `filter` unit tests and `tests/synth.rs`.)
- [x] Every citation marker's quotation is the cited excerpt's sentences, verbatim, rebuilt from `servable_passage` on a permalink; the payload stores spans, never text. (`wt-synth/tests/synth.rs`; `wt-api/tests/pages.rs`, which renders through the permalink.)
- [x] The page shows the headline, framing, sections, the code's note for few talks, the drawn-from line and the quoted lines in a container with id `summary`, with the excerpts below. (`wt-api/tests/pages.rs`; the notes in `wt-api` `summary` unit tests. The summary's feedback control of Spec 06 sits inside the container.)
- [x] A verifier that rejects the headline, a generation timeout and a verifier failure each leave a fully rendered answer without a summary; named mode makes no summary call. (`wt-api/tests/pages.rs`, `wt-synth/tests/synth.rs`.)
- [x] Takedown strips summary sentences that cite a removed passage, and the whole summary when it strips the headline. (`wt-core` `answer` unit tests; takedown calls `AnswerPayload::strip_passages`.)
- [x] `wt eval run` on the 20 Gate A questions records summary latency, and the PR reports p50 and p90 and the cost per answer. (Run `a95f32ed`, October 13, 2026, on a migrated copy of the real-data database with the 120 talks behind the answers reindexed: summary p50 9.8 s, p90 11.1 s; $0.0105 per answer for the summary; a summary on 13 of 20 answers, 57 sentences shown and 91 dropped. PR #128.)

M7, dana calculator

- [x] All property tests in the dana section pass with at least 1,000 cases each. (`wt-dana/tests/properties.rs`, 2,000 cases each.)
- [x] URL template validation rejects a non-HTTPS template and a template whose host differs from the donation URL. (`wt_dana::validate_template`, run by registry import and again by `split`; a template failing the re-check falls back to the plain page.)
- [x] `cargo deny` still passes with no payment dependency in `wt-dana`. (Its dependencies are `url`, `thiserror` and `wt-core`.)

## Sensitive questions

People may bring pain to this tool. A question about despair, a wish to die, a health emergency or harm to others needs a clear pointer to help, and it is not a retrieval problem to optimize. These rules run before retrieval and never affect ranking.

Categories:

- `acute`: stated intent, a plan or means to harm oneself or someone else, or an emergency in progress.
- `distress`: despair, hopelessness, grief or thoughts of death without stated intent.
- `medical`: symptoms, medicines, doses, diagnoses or a health emergency.
- `out_of_scope`: a question about causing harm that has nothing to do with practice, such as making weapons or drugs (October 12, 2026).
- `none`.

Detection runs locally in `wt-retrieve` as `Sensitivity::classify` and works in layers:

1. Rules. A phrase list per category in `config/sensitive.toml`, matched on word boundaries without regard to case, with common misspellings.
2. Embedding check. The query embedding is compared with exemplar sentences per category in `config/sensitive_exemplars.toml`, written by the owner and, if possible, reviewed by a clinician. Thresholds are `acute_sim`, `distress_sim` and `medical_sim`.
3. Triage gate (owner's decision of October 12, 2026, replacing the optional model check). Every question goes to the model with the prompt in `crates/wt-retrieve/prompts/triage.md`, in parallel with embedding and search, and comes back as JSON: `relevance` (`practice`, `adjacent` or `unrelated`), `risk` (`none`, `distress`, `medical`, `acute`, `harm_to_others`, `abuse` or `dangerous`) and `confidence`. The question is data in the prompt, never instructions. The gate can raise the level the other layers set and never lower it: `acute` and `harm_to_others` map to `acute`; `abuse` maps to `acute` when the abuse is ongoing and to `distress` otherwise; `distress` and `medical` map to themselves; `dangerous` (weapons, synthesizing drugs, attacks or other harm unrelated to practice) maps to `out_of_scope`. A call that fails, times out after `triage_timeout_s` (5; raised from 3 by the owner on October 12, 2026, after 2 of 40 red-team calls timed out) or finds the spend cap reached leaves the other layers to decide and logs `triage_degraded`. When the gate returns after retrieval has started and the level becomes `acute` or `out_of_scope`, the retrieval result is discarded and nothing beyond counts is stored.
4. The highest level any layer returns wins. If a layer fails, the result cannot go down. If the embedder is down, the rules decide alone and `sensitivity_degraded` is logged.

| Level | Retrieval | Notice | Summary line | Giving panel | What is stored |
| --- | --- | --- | --- | --- | --- |
| acute | Not run | A full page with the support notice | None | None | Counts only. No question text and no answer row |
| distress | Runs | A banner above the answer | Omitted | Omitted | As the tester's privacy mode says |
| medical | Runs | A banner above the answer | Omitted | Shown | As the tester's privacy mode says |
| out_of_scope | Not run | A plain page saying the site shares what Zen teachers have said about practice and cannot help with this | None | None | Counts only, as for acute |
| none, relevance `unrelated` | Not run at first | A short page saying the question seems outside what these talks cover, with the question and a "Search the talks anyway" button that runs it as usual | As usual once run | As usual once run | As the tester's privacy mode says |
| none | Runs | None | As usual | As usual | As usual |

The acute page offers one link, "Show teachings on facing difficult feelings", which runs the fixed query in `sensitive.curated_query` with the notice on top, no summary line and no giving panel. The page never says a question was detected as anything. Copy is in Spec 09.

Migration `0022_sensitive_questions.sql`:

```sql
alter table query_log add column sensitive_category text not null default 'none'
  check (sensitive_category in ('none','distress','medical','acute'));
```

For the triage gate, a migration (next free number) widens the check to add `out_of_scope` and adds `query_log.relevance text` (`practice`, `adjacent`, `unrelated` or null when the gate did not answer) and `query_log.triage_degraded boolean not null default false`. The gate's reason and confidence are logged as counts by category only, never with question text.

Finding problem areas (owner's decision of October 12, 2026). Fixed rules and examples will miss phrasings nobody wrote down, so two loops look for misses:

1. Red team. `wt sensitive redteam --count <n> [--category <c>]` has the model write new edge-case questions for each category and for benign practice questions near them (grief, death in the precepts, illness in a teacher's story), runs every layer on them, and writes the disagreements and misses, with the layer verdicts, to `data/sensitive/review-<date>.csv` for the owner. Nothing changes until the owner confirms a row: `wt sensitive accept --sheet <csv>` adds a confirmed row as a rule phrase, an exemplar or an evaluation question, as the sheet's `action` column says. Generated questions are synthetic and never stored in `query_log`.
2. Signals from use. `wt sensitive report --since <duration>` runs as `wt_analyst` and prints counts only: questions by category and relevance, disagreements between the gate and the rules or exemplars, `triage_degraded`, `out_of_scope` and `unrelated` rates, and topic clusters (Spec 06, `wt report topics`) where none-found, Not relevant votes or flags are high. It never prints question text; a flagged question is read through `flagged_queries()` as before.
Acceptance criteria:

- [x] On a labeled fixture of synthetic questions, acute recall is 100 percent, distress recall is at least 90 percent, and fewer than 5 percent of ordinary practice questions are flagged.
- [x] An acute question runs no retrieval, writes a `query_log` row with counts and category only, and writes no question text and no `answer` row, even in `share` mode.
- [x] Acute and distress questions never reach topic clusters or the evaluation sets. Both read the view `analyzable_query_log`, which leaves them out. This covers testers' logged questions only: questions written for an evaluation set may include grief and other distress topics, which the PRD's 50-question set asks for (owner's decision of October 14, 2026).
- [x] A failure in any detection layer never lowers the level, and a down embedder logs `sensitivity_degraded` and falls back to rules.
- [x] Triage gate: with a `FakeLlm`, each `risk` value maps to its level, a gate verdict lower than the rules' never lowers it, a timeout or a reached cap logs `triage_degraded` and the rules decide, an `acute` or `out_of_scope` verdict that arrives after retrieval started leaves no answer row and no question text, and an `unrelated` question shows the outside-these-talks page whose "Search the talks anyway" button runs it. (`LlmTriage` parses a `FakeLlm` reply in `wt-retrieve`'s `triage` unit tests; the mapping, the timeout and the cap in `wt-retrieve/tests/sensitive.rs`; the pages, the late verdict and storage in `wt-api/tests/sensitive.rs` with a scripted gate and real Postgres. The reply also carries `ongoing`, which decides `abuse`.)
- [x] `wt sensitive redteam` with a `FakeLlm` writes a review sheet of misses and disagreements and stores nothing in `query_log`; `wt sensitive accept` adds confirmed rows as rules, exemplars or evaluation questions; `wt sensitive report` runs as `wt_analyst` and prints no question text. (`wt-cli` `sensitive_cmds` unit tests and `tests/commands.rs`. `--count` is per category. The report's per-topic clusters wait for `wt report topics`, which does not exist yet; it prints the none-found, Not relevant and flag rates overall.)
- [x] Distress and medical answers omit the summary line, and distress answers omit the giving panel.
- [x] The curated query runs with the notice on top, no summary line and no giving panel.
