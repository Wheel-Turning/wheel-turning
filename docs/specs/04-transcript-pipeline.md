# Spec 04: Transcript pipeline

Implements M4. It takes a `cleared` talk to `ready`: media in, passages and embeddings out, audio deleted, student speech never stored. The gate rules are in Spec 03 and every handler here starts by re-authorizing the talk. Table definitions are in Spec 02.

| Step | Crate and trait | Reads | Writes | Talk state |
| --- | --- | --- | --- | --- |
| Fetch | `wt-ingest` `SourceAdapter` | SSZ storage object | Audio in scratch | `fetching` |
| Transcribe | `wt-asr` `Transcriber` | Scratch audio | Raw ASR words in scratch | `transcribing` |
| Diarize | `wt-segment` `Diarizer` | Scratch audio | Speaker turns in scratch | `diarizing` |
| Segment | `wt-segment` `Segmenter` | Words and turns | Teacher-only transcript JSON in S3, `transcript` and `segment` rows, scratch deleted | `segmenting` |
| Correct | `wt-registry` glossary | Segments | Corrected segment text | `correcting` |
| Extract | `wt-extract` `LlmClient` | Corrected segments | Talk facts and raw citations as staging JSON in S3 | `extracting` |
| Resolve | `wt-extract` resolver | Raw citations | Resolved citations in staging JSON | `resolving` |
| Index | `wt-index` chunker, `wt-ml` `Embedder` | Segments, staging JSON | `passage` and `citation` rows, state `ready` | `indexing` |

Fetching lives in `wt_ingest::media` rather than on each `SourceAdapter` (decision of October 5, 2026), because a talk's media can come from another source than the talk: an SSZ Vimeo talk's audio comes from the podcast feed's enclosure. The fetch takes a `Talk<Consented>`, checks the address against the exclusions and the host's `robots.txt`, streams into the talk's scratch directory through `ScratchDir::create_file`, and refuses a third-party source before anything starts. The `audio` job holds a talk with no media route as `no_media` without fetching. Until the transcripts bucket has an S3 client, transcript JSON goes to a local directory through `wt_core::objects`, as Spec 01 allows while `WT_TRANSCRIPT_BUCKET` is unset.

The four audio steps share scratch space and run as consecutive steps inside one job of kind `audio`, so audio never leaves one process or one machine and cleanup has a single `finally`. Each step still advances the talk state. Correct, extract, resolve and index are separate jobs and can run on any worker.

## Media handling and scratch space

Audio is the most sensitive artifact in the system, so its handling is strict.

1. `ScratchDir` is a type in `wt-core` that owns `WT_SCRATCH_DIR/<talk_id>/`. Its guard deletes the directory on drop, on error and on panic, and `finish()` verifies the directory is gone and returns an error if not. The `audio` handler wraps all four steps in one guard.
2. After fetch, `ffmpeg` extracts mono 16 kHz WAV (`-vn -ac 1 -ar 16000`) and the original video file is deleted at once. Only audio stays, and only until the end of the segment step.
3. The worker checks free disk before fetch and refuses the job unless free space is at least twice the source file size.
4. The raw ASR output contains every speaker's words, so it stays in scratch and is never uploaded. Only the filtered, teacher-only transcript is persisted. This is how student speech is kept out of every store.
5. Struck (decision of October 5, 2026): a failed attempt does not keep its scratch for the retry. Rule 1 and the acceptance items delete scratch on every outcome, as the PRD's "delete the audio" asks, so a retry fetches and transcribes again. `ScratchDir::create` also clears a directory a crashed worker left behind, and `scratch::leftovers` lists any such directory for the audit.
6. Nothing in the code base writes audio or video anywhere else. A CI test greps for audio file extensions in write paths outside `wt-core::scratch`.

## Transcription

```rust
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, audio: &Path, hints: &AsrHints) -> Result<RawTranscript, AsrError>;
}
pub struct AsrHints { pub glossary_prompt: String, pub language: String }
pub struct RawTranscript { pub model: String, pub audio_ms: i64, pub words: Vec<Word> }
pub struct Word { pub text: String, pub start_ms: i32, pub end_ms: i32, pub prob: f32 }
```

The default implementation runs whisper.cpp with the large-v3 model. Decision of October 5, 2026: it runs the `whisper-cli` build measured in `docs/asr-benchmark.md` as a subprocess (`WT_WHISPER_CLI`), rather than linking whisper.cpp through `whisper-rs`, so the worker uses exactly the measured Vulkan build and cargo builds no C++. It asks for one word per segment and full JSON (`-ml 1 -sow -ojf`), writes that output into the talk's scratch directory, reads each word's time and the mean of its tokens' probabilities, and skips special tokens. The audio length comes from the WAV header. Defaults live in `config/pipeline.toml` under `[asr]`: `model_path` from `WT_WHISPER_MODEL`, `language = "en"`, `beam_size = 5`, word-level timestamps on, `use_gpu = true`, `threads = 8`. The glossary prompt is the canonical glossary terms joined by commas, truncated to about 200 tokens, passed as the initial prompt so Dogen, shikantaza, Genjokoan and similar terms are spelled consistently from the start. Tests use a `FakeTranscriber` that returns a scripted word list.

Captions. The PRD says to use existing captions where a source has them. v1 defaults to transcribing from audio anyway, for two reasons. Student-speech removal needs diarization over audio, and passage links need word-level timestamps that cue-level captions lack. Manual captions are still valuable, so when `fetch_captions` returns a track with `is_auto_generated = false`, the handler stores it in scratch and uses it as a reference to compute word error rate against the ASR output, which gives the data quality tests free ground truth. Auto-generated captions are never used.

The setting `prefer_captions = false` flips this. When true and a manual track exists, the cue text becomes the transcript text, cue times are spread evenly across the words of each cue, and diarization and segmentation still run on the audio.

Caption parsers in `wt-asr` handle WebVTT, SRT and YouTube `json3`, normalize to the same word list, and are covered by fixtures.

The persisted transcript is one JSON object in S3 under `transcripts/<talk_id>/v<version>.json`, written only after student words are removed.

```json
{
  "schema": 1,
  "talk_id": "uuid",
  "version": 1,
  "origin": "asr",
  "model": "whisper-large-v3",
  "language": "en",
  "audio_ms": 3612400,
  "glossary_version": null,
  "words": [
    { "i": 0, "w": "Dogen", "s": 12340, "e": 12710, "p": 0.93 }
  ],
  "spans": [
    { "kind": "talk", "s": 0, "e": 2400000, "w0": 0, "w1": 5230 }
  ]
}
```

`words` holds only teacher-attributed words, indexed by `i`. `spans` records the labeled stretches with word index ranges. `glossary_version` is null until the correct step writes corrected text into `segment` rows, and the stored JSON stays uncorrected so a glossary change never needs a new transcript.

## Diarization and segmentation

```rust
#[async_trait]
pub trait Diarizer: Send + Sync {
    async fn diarize(&self, audio: &Path) -> Result<Vec<Turn>, DiarizeError>;
}
pub struct Turn { pub speaker: String, pub start_ms: i32, pub end_ms: i32 }   // "S1", "S2", ...
```

The default `Diarizer` calls the Python sidecar in `sidecars/diarize/`, a small FastAPI service. It runs `pyannote/speaker-diarization-community-1` by default (`WT_DIARIZE_PIPELINE`, see `docs/model-licenses.md`), decodes the upload in memory with `soundfile` and passes pyannote a waveform, so it writes no file at all, and it relabels pyannote's speakers `S1`, `S2`, ... in order of first appearance. `POST /diarize` takes a WAV upload and returns `{"turns": [{"speaker": "S1", "start_ms": 0, "end_ms": 5400}], "speaker_count": 2}`. It runs the pyannote speaker-diarization pipeline. That model needs a Hugging Face access token and acceptance of its terms on the Hugging Face site, and the sidecar reads the token from `HF_TOKEN`. The sidecar writes nothing to disk beyond a temporary file it deletes before responding. A `FakeDiarizer` serves tests.

The segmenter in `wt-segment` turns words and turns into labeled spans. It is deterministic given its inputs and the classifier's outputs.

1. Align. Each word takes the speaker of the turn it overlaps most. Words with no overlapping turn take the nearest turn within 500 ms and otherwise are dropped.
2. Teacher speaker. Over the talk, the teacher is the speaker with the largest share of speaking time outside chanting. All other speakers are treated as students. If a second speaker holds at least `multi_speaker_share` (default 0.35) of speaking time, the talk goes to `needs_review` with reason `multi_speaker` and the job stops. The segmenter never guesses between two possible teachers.
3. Sitting. A stretch with no words of at least `sitting_silence_s` (default 120) is labeled `sitting`.
4. Window classification. Remaining time is cut into windows of 60 seconds, and each window gets a label from `talk`, `chant`, `announcement` or `other`. Rules run first. Very low words per minute with high repetition or a high share of glossary chant phrases marks `chant`. For windows the rules leave ambiguous, `LlmClient` returns a label as JSON with a one-sentence reason that is stored in the job log. A majority filter smooths labels so no span is shorter than `min_span_s` (default 20) unless it is at a talk boundary.
5. Question and answer. After the first `talk` span, any run of non-teacher speech is a student turn and is labeled `qa_student` in memory. A teacher turn that follows a student turn is labeled `qa_teacher`. Student turns are removed from the word list before anything is persisted.
6. Persist. Only `talk` and `qa_teacher` spans become `segment` rows. Chant, announcement, sitting and other spans are recorded in the `spans` array of the transcript JSON for the audit and are not stored as text rows. `question_note` is always null in v1. The PRD allows omitting the question and that is the safer choice.
7. Cleanup. The `audio` handler writes the filtered transcript JSON, inserts the `transcript` and `segment` rows in one transaction, and then `finish()` deletes scratch.

Decisions of October 5, 2026, made while building the segmenter:

- The glossary chant phrases are `chant_phrases` in `[segment]`, seeded with common lines of the Soto service in Japanese and English, to tune on the sample. The glossary's kinds do not include chant lines.
- It runs in two phases so it stays a pure function. `plan` aligns, finds sitting, labels windows by rule, picks the teacher and returns the ambiguous windows; the caller has those labeled and `finish` builds spans and segments. The text of an ambiguous window holds only the teacher's words, so student speech is not sent to the language model either.
- The window rules, in `[segment]` of `config/pipeline.toml`: a window with no words is `other`. It is `chant` below `chant_max_wpm` (60) words a minute with a type-token ratio below `chant_max_ttr` (0.35), or with at least `chant_phrase_min_share` (0.30) of its words in chant phrases. It is `talk` at `talk_min_wpm` (90) or more with a type-token ratio of at least `talk_min_ttr` (0.35) and under `talk_max_chant_share` (0.10) chant phrases. Anything else is ambiguous. These are starting points to tune on the ten-video sample.
- "Outside chanting" in step 2 means outside the windows the rules call chant, since the teacher is chosen before ambiguous windows are labeled. A recording with no aligned speech goes to `needs_review` with reason `no_speech`.
- Step 5 counts non-teacher speech as a student turn only inside a `talk` or `other` stretch after the first talk begins. Non-teacher speech before the first talk, or during chant or announcements, is simply dropped. A teacher answer, `qa_teacher`, lasts until the next student turn or the end of its stretch, and the teacher's words in an `other` stretch that answer a student are `qa_teacher` too. Student turns are kept in memory as time ranges for the sample report and never appear in the persisted `spans`.

`wt segment sample --count 10 --out report.html` picks ten cleared talks, runs steps 1 to 5 without persisting, and writes an HTML timeline per talk showing labeled spans with the first words of each. It is the tool for the PRD's ten-video segmentation check on talks that have passed the gate.

## Glossary correction

The glossary is a standing asset in the `glossary_term` table, loaded from `scripts/seed_glossary.toml` and maintained with `wt glossary import` and `wt glossary add`. Each term has a canonical spelling, a kind (`term`, `name` or `text_title`) and a list of known variants, which are mis-hearings and alternate spellings. The seed file starts with the terms the PRD names (Dogen, shikantaza, Genjokoan) plus the series titles and common practice vocabulary, and grows from the suggestions file below.

The correction function is pure: `apply_glossary(words, glossary) -> corrected words`.

1. An Aho-Corasick automaton over lowercased variants matches on word boundaries and replaces each match with the canonical form. A multi-word variant collapses into one or more corrected words, which inherit the start of the first replaced word and the end of the last, so timestamps stay valid.
2. Only exact variants are applied automatically.
3. A second pass looks for near misses. For tokens of at least six letters that are not in the English word list, it computes Damerau-Levenshtein distance of 2 or less to each canonical term and a phonetic match. Candidates are written to `glossary_suggestions.csv` with the talk, word index and context, and are never applied. A human adds the variant to the glossary, which triggers the next rerun.
4. The glossary version is the SHA-256 of its sorted content. It is part of the `correct` and `index` job input hashes, so adding a variant requeues exactly the affected talks.
5. `segment.text` stores corrected text. The transcript JSON in S3 stays uncorrected. The index step recomputes corrected words with timestamps by calling the same pure function, so corrected text and timing cannot disagree.

Teacher aliases are not rewritten in text. Aliases serve named-teacher detection in Spec 05.

Decisions of October 5, 2026, made while building the correct step:

- `apply_mapped` returns, with each corrected word, the range of stored (uncorrected) teacher words it came from. Staged citations record positions in the stored words, which no glossary change moves, so after a glossary change `correct` re-runs and queues `index` directly, reusing the staged citations, and `extract` and `resolve` do not run again. A talk not yet extracted continues to `extract` as usual.
- A glossary change re-queues `correct` for exactly the talks whose corrected text it changes: `wt glossary import` and `wt glossary add` recompute each corrected talk's segments with the new glossary and compare them with the stored text. The `correct` job's input hash is the transcript's hash and the glossary version.
- Near misses go to one file per talk, `transcripts/<talk_id>/glossary_suggestions.csv`, under the talk's transcript prefix so a takedown removes them with the transcript. No English word list is loaded yet, so every near miss that sounds like and nearly spells a glossary term is suggested; suggestions are never applied, so this only makes the file longer. `scripts/seed_glossary.toml` holds the starting terms and some guessed variants, to grow from the suggestions.

## Extraction and citation resolution

`LlmClient` in `wt-extract` is the only code that calls the Claude API.

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete_json<T>(&self, req: LlmRequest, class: DataClass) -> Result<T, LlmError>
    where T: DeserializeOwned + JsonSchema + Send;
}
pub enum DataClass { Synthetic, Real }
```

Rules for the client:

1. It calls the Messages API at `https://api.anthropic.com/v1/messages` with `x-api-key` and an `anthropic-version` header. It constrains the reply with structured outputs, `output_config: {format: {type: "json_schema", schema}}`, where the schema is that of `T` closed with `additionalProperties: false` and without the numeric and length bounds the API does not accept. The reply is parsed with `serde`. A parse failure gets one repair retry and then fails the job. Decision of October 5, 2026: the earlier rule forced one tool with `tool_choice`, which Claude Sonnet 5.5, Opus 5.5 and Fable 5.1 reject with a 400, and they also reject sampling settings, so temperature 0 is sent only to models that `config/llm_prices.toml` marks `sampling = true` (Claude Haiku 4.5 today).
2. A request with `DataClass::Real` is refused unless `WT_LLM_REAL_DATA_OK=true`. Talks loaded by the fixture loader carry `raw.synthetic = true` in `source_video` and use `Synthetic`.
3. Logs record the stage, model, token counts and a hash of the request. They never contain transcript text.
4. Rate limits and 5xx responses retry with exponential backoff up to three times inside the call. The model comes from the stage's setting, falling back to `WT_LLM_MODEL` (see the spend cap below). Labeling ambiguous segment windows is stage `classify` and uses the extraction model (decision of October 5, 2026).

Extraction runs over the corrected segments in windows of up to 6,000 words with 200 words of overlap and returns this per window:

```rust
pub struct WindowFacts { pub practice_period: Option<String>, pub citations: Vec<RawCitation> }
pub struct RawCitation {
    pub text: String,                 // exact words from the window that cite or quote a text
    pub work_hint: Option<String>,    // for example "Genjokoan"
    pub locator_hint: Option<String>, // for example "paragraph 5"
    pub kind: CitationKind,           // Reference | Quotation | Case
}
```

The code verifies that `text` is a substring of the window under whitespace and case normalization and discards anything that is not. Punctuation is ignored too, since recognition punctuates freely (decision of October 5, 2026). The model's offsets are never used. The code finds the word range itself. Discard counts are logged per talk. Verified citations are saved with word ranges in the staging object `citations/<talk_id>.json` in S3, which the index step consumes once passages exist.

Resolution assigns one of three confidence levels, in this order of precedence:

1. Hand list, method `hand_list`, `resolved`. The normalized `text`, hints and window context are matched to `work_locator.label` and `aliases` by exact match or `pg_trgm` similarity of at least 0.85.
2. Series label, method `series_label`, `probable`. If the talk has a `series` and `part`, and the citation is generic (for example "this passage" or "as Dogen says here") with no locator hint, the locator mapped to that part in `config/series.toml` is used.
3. Model hint, method `llm`, `probable`. A `work_hint` and `locator_hint` that fuzzy-match a hand-listed locator at 0.7 or better are accepted.
4. Anything else is `unresolved`, stored with its raw text and no locator.

The model never creates a locator. A hint that matches nothing in the hand list becomes `unresolved`. For talks in labeled series, the resolver is scored against the series label as ground truth.

Decisions of October 5, 2026, made while building the resolver: text, hints and context are compared after the normalization of Spec 03's rule 1 (lowercase, punctuation as spaces, spaces collapsed), with labels and aliases normalized the same way; the context is the 20 stored words either side of the citation. `config/series.toml` gives a series a `locator` template such as `genjokoan:{part}` where its parts name sections of a text (Genjokoan paragraphs, Blue Cliff Record cases, Song of the Grass Hut verses; not Uji or Bendowa, whose parts are talks). A series-label match needs that locator in the hand list, which Spec 08's imports load, so until then generic citations stay `unresolved`. Resolution is written into the staging object beside each citation.

## Chunking, embedding and indexing

```rust
#[async_trait]
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;                       // 1024
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError>;   // L2-normalized
}
```

`wt-ml` implements `Embedder` for bge-m3 and `Reranker` for bge-reranker-v2-m3 over `ort`, with the `tokenizers` crate for tokenization. `scripts/export_models.py` converts both models to ONNX with int8 quantization and writes them with their tokenizers to `WT_MODEL_DIR`. That script is the only place Python touches the models, and exported files go to the models bucket, never into git.

The chunker in `wt-index` is a pure function from corrected words to passages. Defaults live under `[chunk]` in `config/pipeline.toml`.

1. Input is the corrected word list of one `talk` or `qa_teacher` segment. Passages never cross a segment boundary.
2. Sentences come from Unicode sentence segmentation over the corrected text, and a pause of `sentence_pause_ms` (default 1200) between words also ends a sentence.
3. Sentences are packed greedily into passages. A passage closes when it reaches `target_words` (default 140) and is never allowed past `max_words` (default 220). A pause of `hard_break_ms` (default 8000) also closes a passage.
4. A final passage shorter than `min_words` (default 60) merges into the previous one if the result stays within `merge_cap_words` (default 260), and otherwise is kept as is. A hard break starts a new stretch, and each stretch's final passage is a final passage here: it merges only within its stretch, never across the pause (decision of October 5, 2026). The database check on character length is the last guard.
5. There is no overlap between passages in v1.
6. `start_ms` is the start of the first word and `end_ms` is the end of the last word.
7. The output is deterministic, and each passage has a `content_hash` over its text and offsets.

Embedding input is the passage text and nothing else. No teacher, sangha, title or series text is added, so retrieval is relevance-only by construction. Queries are embedded the same way with no instruction prefix. Maximum token length is 512, batch size is 16 on CPU and 64 on GPU, and vectors are L2-normalized for cosine distance.

The index step, per talk, in one transaction:

1. Delete the talk's existing passages, which cascades to citations.
2. Insert the new passages with embeddings and the `embedding_model` name.
3. Map each staged citation to the passage that contains its first word, compute span offsets relative to the passage text, and insert `citation` rows with the resolved confidence.
4. Set `talk.content_hash` to a hash of the passage hashes, `last_verified_at` to now, and state to `ready`.

After a batch, run `ANALYZE passage`. HNSW search settings are applied per query in Spec 05. `wt reindex --talk <id> | --all [--model <name>]` reruns the index step from stored segments and the transcript JSON, which is how a new glossary version or embedding model is applied.

Decisions of October 6, 2026, made while building the index step and the embedder:

- `ort` loads ONNX Runtime at run time from `WT_ORT_DYLIB` (or `ORT_DYLIB_PATH`), version 1.22 or newer, so nothing is downloaded at build time and CI builds without it. `tokenizers` is built without its C and C++ parts. `scripts/export_models.py` runs with `uv run` and needs no PyTorch: bge-m3's repository ships an fp32 ONNX graph at the recorded revision, which the script quantizes to int8 weights with ONNX Runtime, checks against fp32 on a few invented sentences (cosine of at least 0.95), and records in `export.json` with file hashes. It prints the path of the ONNX Runtime library it installed. The reranker export comes with the `Reranker` in Spec 05.
- The dense vector is the first token's final hidden state, normalized, as bge-m3 defines it.
- A passage under 40 characters, such as a short answer of "Yes, exactly.", is left out rather than failing the talk on the database check; one over 1800 characters fails the job, since the chunk settings then need changing.
- A citation's `raw_text` is its corrected words; its span is cut at the end of the passage holding its first word. A citation the resolver left `unresolved` is stored with method `llm`, since the model found it, and a citation whose first word is in no kept passage is dropped and counted in the job log.
- `talk.content_hash` covers the passages, the citations and the embedding model. An `index` job on a `ready` talk with the same hash changes nothing; a `ready` talk being reindexed stays `ready`, serving its old passages until the new ones commit.
- The worker runs `ANALYZE passage` when its queue empties after indexing at least one talk, and `wt reindex` runs it at the end.
- Rerunning a stage on unchanged input changes no rows (CLAUDE.md rule 8). `audio` does nothing for a talk with a current transcript unless its job's payload has `"retranscribe": true`, which is how a new recognition model is applied. Every stage moves a talk only forward. `extract` records the hash of the text it read in the staging object and does not call the model again for the same text. `correct` and `resolve` queue `index` with the same input hash (the segment text and the staged citations), and `extract` queues `resolve` by the text it read.
- `wt pipeline advance` (Spec 03) queues `audio` for each `cleared` talk keyed by its media route, so a talk whose route changes gets a new job; `wt pipeline retry --talk <id>` requeues the talk's latest dead job with fresh attempts and clears its `state_reason`.
- `wt segment sample` picks `cleared` talks at random, or takes `--talk`, and writes to `data/segment-sample.html` by default, since the report holds teachers' words. The language model labels ambiguous windows as in the job, so the sample is charged to the cap. Student turns appear as time ranges only.

## Acceptance criteria

M4 is done when every item below passes in CI with fakes, and one synthetic talk has run through a real worker, a real Postgres and real ONNX models.

- [ ] `SourceAdapter` gains `fetch_media` and `fetch_captions`, taking a `&Talk<Consented>` (Spec 03). With `WT_ALLOW_THIRD_PARTY_FETCH` unset, any `third_party` fetch returns `ThirdPartyFetchDisabled` and no subprocess starts. (Moved from M3, decision of October 5, 2026. The media fetch and the third-party refusal are done in `wt_ingest::media` and tested in `wt-ingest/tests/audio.rs`; `fetch_captions` comes with the first source that offers captions.)
- [x] With no usable media route (Spec 03, Seattle Soto Zen sources item 2), a talk stays at `needs_review` with reason `no_media` and nothing is fetched. (Moved from M3, decision of October 5, 2026. `wt-ingest/tests/audio.rs`.)
- [x] `ScratchGuard` deletes scratch on success, on a returned error and on panic. A test injects each failure. (`ScratchDir` in `wt-core::scratch`; a repository guard also fails on an audio file written outside it.)
- [x] After an `audio` job finishes in any outcome, nothing remains under `WT_SCRATCH_DIR` for that talk. (`wt-ingest/tests/audio.rs`: success, a failed step, a talk held for review, no media.)
- [x] The stored transcript JSON contains no words from a non-teacher speaker. A test with a `FakeDiarizer` that returns a student turn proves it, and also checks no `qa_student` row can exist. (`wt-ingest/tests/audio.rs`.)
- [x] Segmenter tests on synthetic timelines cover: silence of 120 seconds or more becomes `sitting`; a chant window is labeled `chant`; a student turn is dropped and the following teacher turn is `qa_teacher` with a null `question_note`; a second speaker at 35 percent or more sends the talk to `needs_review`. (`wt-segment/tests/segmenter.rs` covers all four in the segmenter; `wt-ingest/tests/audio.rs` checks the stored null `question_note` and the move to `needs_review`.)
- [x] The glossary replaces known variants with canonical forms, preserves timestamps across multi-word replacements, writes near misses to the suggestions file without applying them, and a glossary change requeues `correct` and `index` for the affected talks only. (`wt-registry/tests/glossary.rs`, `wt-ingest/tests/textjobs.rs`.)
- [x] Extraction discards a fabricated citation that is not a substring of the window. `DataClass::Real` is refused when `WT_LLM_REAL_DATA_OK` is not `true`. (`wt-extract` unit tests and `tests/client.rs`; real talks are marked `Real` in `wt-ingest/tests/textjobs.rs`.)
- [x] The resolver gives `resolved` only for hand-list matches, gives `probable` for series-label and fuzzy model hints, and gives `unresolved` when a model hint matches no hand-listed locator. Precision of `resolved` is 100 percent on the fixture set. (`wt-extract/tests/resolve.rs`; the Postgres hand list in `wt-ingest/tests/textjobs.rs`.)
- [x] Chunker golden tests: every passage is within `min_words` and `max_words` except an unmergeable tail, none crosses a segment boundary, timestamps are monotone, and output is identical across runs. (`wt-index/tests/chunker.rs`. The chunker takes one segment's words, so no passage can cross a segment; a tail merged within `merge_cap_words` may pass `max_words`, as rule 4 allows.)
- [x] The embedder returns 1024-dimension unit vectors that are identical across repeated runs on CPU within 1e-5. (`wt-ml/tests/onnx.rs`, ignored in CI, passed on October 6, 2026 on the owner's PC with the int8 export and ONNX Runtime 1.30.0: 23 texts in about 0.3 seconds. The export agrees with fp32 at a cosine of 0.986 or more with per-channel quantization, against 0.980 per tensor, so per channel is the default.)
- [x] A synthetic talk goes from `cleared` to `ready` through `wt worker`, `servable_passage` returns its passages, and rerunning every stage changes no rows. (`wt-ingest/tests/pipeline.rs` drives all five stages through the queue with fakes and reruns each one. On October 6, 2026 the real `wt worker --kinds correct,index` took a synthetic talk to `ready` on a fresh Postgres with the real bge-m3 export, 7 passages, and `wt reindex --all` then found it current. The real `audio` tools ran in `wt-ingest/tests/real_tools.rs`; `extract` waits for an API key.)
- [x] `wt export transcripts --format jsonl --out <dir>` writes the corrected passages of every consented `ready` talk for the Gate A notebook, in the format of Spec 07: one line per passage, read through `servable_passage` (decision of October 6, 2026; Spec 04 said segments, Spec 07 passages). (`wt-cli/tests/commands.rs`.)
- [x] `wt segment sample --count 10` renders the segmentation report. (The report's rendering is tested in `wt-cli`'s unit tests, and the command's refusal without `WT_WHISPER_CLI` in `wt-cli/tests/commands.rs`; the steps it runs are the `audio` job's, without persisting.)

## Transcript review and owner exports

Owner's decisions of October 7, 2026: transcripts are reviewed offline in v1, by reviewers such as a teacher's priests, without accounts, and a teacher can have their reviewed transcripts for their own use, such as books. Kept simple until Kanshin approves and sanghas show interest.

1. `wt transcript export-review (--talk <id> | --teacher <name>) --out <dir>` writes one self-contained HTML file per talk under `data/`: the title, teacher, sangha and date; the teacher's words paragraph by paragraph, with marked gaps where student speech, chant, announcements or sitting were removed; a "Listen at h:mm:ss" link to the source for each paragraph; and each word editable in place. Saving downloads a JSON file of edits, each naming the stored-word range it replaces and the new words. The file loads no remote script and stores nothing.
2. `wt transcript import-corrections --file <json> --reviewer <name>` checks each edit against the current transcript's hash, writes a new transcript version with `origin = 'reviewed'`, and logs each edit in `correction_log` with the reviewer. Replaced words take the time span of the words they replace, spread evenly, so passage links still land on their moment. It then queues `correct` and `index`.
3. A reviewed version is never overwritten by an automated pass. `wt asr requeue` skips talks whose current transcript is reviewed, and the glossary pass leaves reviewed words alone, writing its near misses to the suggestions file as usual.
4. `wt export teacher --teacher <name> --format markdown --out <dir>` writes the teacher's talks for their own use: one file per talk with the title, date, sangha and source link, the reviewed text where it exists and the corrected text otherwise, in paragraphs, with the gaps marked. It writes no student speech and no audio. Display levels d and e use it.

Acceptance criteria (M4T):

- [ ] A synthetic talk round-trips: export a review file, apply edits to its JSON, import them, and the new reviewed version holds the edits with word times inside the replaced spans, the passages are rebuilt, and `correction_log` names the reviewer.
- [ ] An import against a transcript that changed since the file was written is refused.
- [ ] `wt asr requeue` and a glossary change leave a reviewed version untouched.
- [ ] `wt export teacher` writes one file per talk with no student words and the reviewed text where it exists.

## Language-model spend cap

The language model is the one metered cost in the system, so v1 caps it. Settings: `WT_LLM_MONTHLY_CAP_USD` (default 25), the per-stage models `WT_LLM_MODEL_EXTRACT`, `WT_LLM_MODEL_SYNTH`, `WT_LLM_MODEL_VERIFY` and `WT_LLM_MODEL_VISION` (each falls back to `WT_LLM_MODEL`), and a hand-edited price table in `config/llm_prices.toml` with input and output dollars per million tokens for each model and a `checked_on` date.

Mechanism:

1. Every call writes a `llm_usage` row with the stage, model, input and output token counts from the API response, and an estimated cost from the price table.
2. Before each call the client adds up the month so far. At 80 percent of the cap it sends an alert. At the cap it refuses.
3. A refused ingestion call (extract, classify, vision) puts its job back in the queue until the first of the next month, unless the owner raises the cap with `wt llm cap set <usd> --by "<name>"`.
4. A refused synthesis or verifier call makes the answer render without a synthesis line, which the spec already allows, and the log records `cap_reached`.
5. The first month of ingestion needs more room, so the owner sets the cap to about 60 for that month and back to 25 afterward. Also set a monthly spend limit in the Anthropic Console as a second guard.

Estimates at list prices of October 3, 2026. Sources disagree about the current Sonnet-class price, with figures of $2 and $10 and of $3 and $15 per million input and output tokens, so these use the higher pair. Haiku 4.5 is $1 and $5. Check the price table against the provider's pricing page before relying on them. The Batch API halves both rates for jobs that can wait.

| Work | Estimate |
| --- | --- |
| One pass of extraction and classification over the 140 hours of SSZ talks, about 1.4 million words of input | About $3 on Haiku, about $8 on a Sonnet-class model |
| The OCR rescue pass for one edition at the 200-page cap | About $1 on Haiku, about $3.50 on a Sonnet-class model |
| One answer: a synthesis call and up to 4 verifier calls, about 8,000 input and 500 output tokens | About $0.03 on a Sonnet-class model |
| 500 answers in a month from a small tester group | About $16 on a Sonnet-class model |

Default models: a Haiku-class model for extraction, classification and vision, and a Sonnet-class model for synthesis and verification, because those two stages decide whether an invented claim reaches a page. Expect about $20 a month in steady use and under $60 in the first month.

The price table's models and prices were checked against the provider's pricing page on October 5, 2026: Claude Haiku 4.5 at $1 and $5, Claude Sonnet 5.5 at $2 and $10, and Claude Opus 5.5 at $4 and $20 per million input and output tokens. The ledger also keeps the owner's cap from `wt llm cap set` (`llm_setting`) and the month's 80 percent alert (`llm_alert`), so the alert goes out once a month however many workers see it. Alerts are logged as warnings until the project has an SNS client for `WT_ALERT_TOPIC_ARN`.

Migration `0020_llm_usage.sql` (merged as `0018_llm_usage.sql`, with `llm_setting` and `llm_alert`; see `docs/migrations.md`):

```sql
create table llm_usage (
  id uuid primary key default gen_random_uuid(),
  created_at timestamptz not null default now(),
  stage text not null,
  model text not null,
  input_tokens integer not null,
  output_tokens integer not null,
  est_cost_usd numeric(10,4) not null
);
create index llm_usage_month_idx on llm_usage (created_at);
```

Acceptance criteria for the spend cap:

- [x] Each call records tokens and an estimated cost, and the month-to-date sum matches the sum of the rows. (`wt-extract/tests/client.rs`.)
- [x] A fake client at 80 percent publishes one alert, and at 100 percent refuses further calls. (`wt-extract/tests/client.rs`; the alert is logged until there is an SNS client.)
- [x] A refused ingestion job returns to the queue with a run time on the first of the next month, and `wt llm cap set` releases it. (`wt-db/tests/job.rs`; the worker defers `audio` and `extract` jobs the cap refuses, without using an attempt.)
- [ ] A refused synthesis call produces a rendered answer with no synthesis line and a `cap_reached` log entry. (With synthesis in M6: the client already returns `CapReached` for any stage.)
- [x] A stage whose model setting is empty uses `WT_LLM_MODEL`, and the loader fails at startup if neither is set when a language-model stage is enabled. (`wt-core` config tests; `wt worker --kinds extract` refuses to start in `wt-cli/tests/commands.rs`.)

## Transcription model and where it runs

Decision of October 3, 2026, following the GPU quota refusal in Spec 01.

1. The default model stays `large-v3`. The SSZ talks are in English, so `distil-large-v3` is allowed through `asr.model_path`. Its model card describes it as MIT licensed, over 5 times faster than large-v3 on a Mac M1, and within 0.8 points of word error rate on long-form audio.
2. The first full run uses `large-v3` on the owner's Windows PC, built with whisper.cpp's Vulkan support on the Arc GPU and run with `GGML_VK_DISABLE_COOPMAT=1`, the setup the owner chose below, with `wt worker --kinds audio --concurrency 1`. The speed figures on the model card are for a Mac M1, so measure the real speed on this PC with the sample before planning. A CPU-only run could take weeks, and then renting a GPU machine by the hour is the better route.
3. `wt asr benchmark --talks <ids>` runs both models over the ten-talk sample and compares each with a manual caption or a hand-corrected passage where one exists, and prints word error rate on the glossary terms, on teacher names and overall. Keep `large-v3` for the full run if `distil-large-v3` misses core vocabulary.
4. Whichever model runs, the transcript JSON records its name and version, so a later switch to a better model can reprocess only the affected talks.

Decisions of October 6, 2026, made while building the benchmark and the model switch:

- `asr.model_path` is `WT_WHISPER_MODEL`. The recorded name is `whisper-` and the file's stem without `ggml-`, so `ggml-large-v3.bin` is `whisper-large-v3`. `wt asr requeue [--dry-run]` reads each current transcript's recorded model and queues `audio` with `"retranscribe": true` for the talks recorded with another, keyed by the new model so asking twice queues nothing new. A retranscribed talk's new segments replace the old ones, which takes its passages out of serving until it is indexed again.
- `wt asr benchmark --clip <file> --reference <text>` runs on a clip; `--talk <id>` (repeatable) with `--references <dir>` fetches cleared talks instead and scores each against `<dir>/<talk id>.txt` where one exists. The setups and models are in `config/asr_bench.toml`, with paths under `.bench/` from `--bench-root`, and a setup whose binary is missing is skipped. Each run uses the worker's own `whisper-cli` arguments and glossary prompt, with the setup's environment, extra arguments and `PATH` additions.
- Scoring follows `docs/asr-benchmark.md`: lowercase, accents and punctuation removed, hyphens as spaces. A preamble before the reference's first three words, found in the hypothesis's first quarter, is not scored, and reference words after the hypothesis ends cost nothing. The glossary rate counts occurrences in the reference of the glossary's canonical terms, and the names rate those of teachers' display names and aliases, as wrong when any of their words is not matched exactly. Terms come from the database's glossary.
- The worker no longer passes the glossary to `whisper-cli` as its initial prompt (owner's decision of October 6, 2026): on the benchmark clip it raised `large-v3`'s word error rate from 1.9% to 8.8% on Vulkan and from 1.6% to 59.6% on OpenVINO. `glossary_prompt = false` under `[asr]` is the default; the glossary pass corrects terms after transcription. The October 6 runs measured `large-v3` on Vulkan at 2.72 times real time after an hour of load, and the owner kept the October 4 route; the full run measures it again on its first talks. Both runs are in `docs/asr-benchmark.md`.
- `whisper-cli` runs with `-mc 0` (`max_context = 0` under `[asr]`; decision of October 6, 2026). On a synthetic talk through the real worker, `large-v3` with its default context inserted an invented sentence ten times; without context it did not, and on the benchmark clip its word error rate fell from 1.93% to 1.18% at 3.18 times real time. A looped sentence would be served as the teacher's words. Hallucination loops remain a known risk for the claim audit.
- Peak memory is the process's working set sampled every 100 milliseconds; GPU memory is not measured. The vocabulary rule (item 3) is applied by preference order in `config/asr_bench.toml`: a later model is chosen only if, on every setup where both ran, it gets no more glossary terms wrong than each earlier one, and then the fastest acceptable model's fastest setup is chosen and its real-time factor sets the route. `--record` appends the table, the decision, the date and the hardware (CPU, memory and operating system as reported, plus `--note`) to `docs/asr-benchmark.md`.
- A run is unusable, and left out of the decision though still shown in the table, when its word error rate on the known text is over `max_wer_percent` in `config/asr_bench.toml`, or when it produced under half the median word count of the completed runs. The word count applies with a reference too, because reference words after the hypothesis ends are free, so a run that stops early is scored on fewer words. The limit is 5 percent for now (owner's decision). It must come down as transcription improves, or the answers lose much of their value. Without these checks, the first recorded run chose default Vulkan, whose 38 words of garbage scored no glossary term wrong.

Acceptance criteria:

- [x] A worker on the owner's Windows PC transcribes a synthetic audio fixture end to end with the build that matches its hardware and records the model name in the transcript. (October 6, 2026: invented text spoken by Windows' speech synthesizer, served as a feed enclosure; `wt worker --kinds audio,correct,index` with `large-v3` on Vulkan and the pyannote sidecar took it to `ready`, recorded `whisper-large-v3`, and left no scratch. That run found the loop that `-mc 0` fixes.)
- [x] `wt asr benchmark` prints the three error rates for both models on a fixture with known text. (`wt-asr/tests/bench.rs`, with a fake engine; scoring unit tests in `wt_asr::bench`.)
- [x] Changing `asr.model_path` changes the recorded model name and requeues only talks transcribed with the old one when asked. (`wt asr requeue`; `wt-ingest/tests/audio.rs` and `wt-cli/tests/commands.rs`.)

## Owner's hardware and the decision rule

The owner's PC has an Intel Arc 140V integrated graphics chip with 8 GB of shared memory, an Intel NPU, 16 GB of RAM and no NVIDIA card, so CUDA is not available. The worker that uses the graphics chip is built natively on Windows and not inside WSL 2, because WSL 2 may reach Intel graphics for compute only through slower translation layers or not at all. The rest of the stack can live in WSL 2.

Measure these setups with `wt asr benchmark`, in this order: whisper.cpp with Vulkan, whisper.cpp with the OpenVINO encoder, and CPU only with 8 threads. Measure `large-v3` and `distil-large-v3`. The benchmark reports the real-time factor, which is seconds of audio transcribed per second of compute, and peak memory.

Decision rule from the best measured real-time factor on the sample:

- 3 or more: run the whole corpus on this PC.
- Between 1 and 3: run the first 20 to 30 talks on this PC for Gate A and rent a GPU machine by the hour for the rest.
- Below 1: rent a GPU machine for everything.

Result of October 4, 2026, recorded in `docs/asr-benchmark.md`: `distil-large-v3` was fastest, at 11.53 times real time with whisper.cpp's OpenVINO encoder on the Arc GPU, but it misheard core vocabulary (Atman, Anatman and Dharmakaya), so item 3's rule keeps `large-v3`. Against the book text of the sample, `large-v3` scored 1.12 to 1.48 percent word error rate. It ran at 3.17 to 3.26 times real time on Vulkan with `GGML_VK_DISABLE_COOPMAT=1`, which clears the threshold of 3. The owner chose `large-v3` on Vulkan on October 4, 2026, so the whole corpus runs on this PC, at about 51 hours of compute for the roughly 163 hours of SSZ talks in the PRD's inventory. Vulkan with its default settings produces garbage on this GPU, so the variable is required. On the sample this setup dropped one 8-word clause, so a dropped passage is a known risk for the claim audit and the timestamp checks. The worker reaches the database through an SSM port forward via the batch host, which runs as a small CPU instance for the length of each run, and writes to the transcripts bucket with the owner's SSO profile.

With 16 GB of RAM in use by other programs, close them during a run and expect `large-v3` on Vulkan to use about 4.3 GB of RAM and 3.9 GB of shared GPU memory, as it did on the sample. The benchmark clip is a public-domain or CC0 recording, never a talk from a teacher.

Acceptance criteria:

- [x] `wt asr benchmark` runs each available setup on a 10-minute clip and prints the real-time factor and peak memory. (Run on October 6, 2026 on the owner's PC: five setups and two models, recorded in `docs/asr-benchmark.md`.)
- [x] The benchmark output names the setup chosen under the decision rule and records it in `docs/asr-benchmark.md` with the date and the hardware. (`--record`; the October 6 runs.)

## Model license record

`docs/model-licenses.md` lists every model the system can load: Whisper or distil-large-v3, bge-m3, bge-reranker-v2-m3, the pyannote segmentation and diarization models, and each language model by name. For each it gives the license name, the license URL, the revision or commit hash downloaded, the date checked and who checked. A stage refuses to start when its configured model is missing from the file or has no recorded revision, and CI fails when a configuration file names a model the record lacks. The record is rechecked whenever a model or its revision changes.

Decisions of October 6, 2026: the record's path is `WT_MODEL_LICENSES` (default `docs/model-licenses.md`), read by `wt_core::licenses`. A model is listed when its name is in backticks in a table row, and has a revision when that row holds a 40- or 64-character hash, or is under Language models, where an API model's id is its revision. The worker checks the `WT_WHISPER_MODEL` file name for `audio`, each language-model stage's model for `audio` and `extract`, and `WT_EMBED_MODEL` for `index` and `wt reindex`. The diarization pipeline is the sidecar's setting and is listed in the record but not checked from Rust. The CI check covers the models in `config/llm_prices.toml`, the model files in `config/asr_bench.toml` and the retrieval defaults in `.env.example`.

Acceptance criteria:

- [x] A stage with an unlisted model refuses to start with a message naming the model. (`wt-core` unit tests; `wt-cli/tests/commands.rs` for an unlisted embedding model and Whisper file.)
- [x] CI fails when `config/` names a model that `docs/model-licenses.md` does not list with a revision. (`wt-core/tests/repo_guards.rs`.)
