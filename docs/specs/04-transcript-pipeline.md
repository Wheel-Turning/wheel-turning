<!-- Exported from the "Dharma Gates: V1 Technical Specs" doc (https://claude.ai/code/artifact/68e63636-e0f6-43ee-ab79-ccc03b76946d), tab "04 Transcript pipeline", on 2026-10-02. The doc is the source of truth until this file is edited in the repo. -->
# Spec 04: Transcript pipeline

Implements M4. It takes a `cleared` talk to `ready`: media in, passages and embeddings out, audio deleted, student speech never stored. The gate rules are in Spec 03 and every handler here starts by re-authorizing the talk. Table definitions are in Spec 02.

| Step | Crate and trait | Reads | Writes | Talk state |
| --- | --- | --- | --- | --- |
| Fetch | `dg-ingest` `SourceAdapter` | SSZ storage object | Audio in scratch | `fetching` |
| Transcribe | `dg-asr` `Transcriber` | Scratch audio | Raw ASR words in scratch | `transcribing` |
| Diarize | `dg-segment` `Diarizer` | Scratch audio | Speaker turns in scratch | `diarizing` |
| Segment | `dg-segment` `Segmenter` | Words and turns | Teacher-only transcript JSON in S3, `transcript` and `segment` rows, scratch deleted | `segmenting` |
| Correct | `dg-registry` glossary | Segments | Corrected segment text | `correcting` |
| Extract | `dg-extract` `LlmClient` | Corrected segments | Talk facts and raw citations as staging JSON in S3 | `extracting` |
| Resolve | `dg-extract` resolver | Raw citations | Resolved citations in staging JSON | `resolving` |
| Index | `dg-index` chunker, `dg-ml` `Embedder` | Segments, staging JSON | `passage` and `citation` rows, state `ready` | `indexing` |

The four audio steps share scratch space and run as consecutive steps inside one job of kind `audio`, so audio never leaves one process or one machine and cleanup has a single `finally`. Each step still advances the talk state. Correct, extract, resolve and index are separate jobs and can run on any worker.

## Media handling and scratch space

Audio is the most sensitive artifact in the system, so its handling is strict.

1. `ScratchDir` is a type in `dg-core` that owns `DG_SCRATCH_DIR/<talk_id>/`. Its guard deletes the directory on drop, on error and on panic, and `finish()` verifies the directory is gone and returns an error if not. The `audio` handler wraps all four steps in one guard.
2. After fetch, `ffmpeg` extracts mono 16 kHz WAV (`-vn -ac 1 -ar 16000`) and the original video file is deleted at once. Only audio stays, and only until the end of the segment step.
3. The worker checks free disk before fetch and refuses the job unless free space is at least twice the source file size.
4. The raw ASR output contains every speaker's words, so it stays in scratch and is never uploaded. Only the filtered, teacher-only transcript is persisted. This is how student speech is kept out of every store.
5. A failed attempt leaves its scratch directory in place for the retry to reuse if the intermediate files match the input hash, with a time limit of `scratch_resume_ttl_h` (default 6) after which the reaper deletes it. A job that goes `dead` deletes its scratch directory.
6. Nothing in the code base writes audio or video anywhere else. A CI test greps for audio file extensions in write paths outside `dg-core::scratch`.

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

The default implementation wraps whisper.cpp through `whisper-rs` with the large-v3 model. Defaults live in `config/pipeline.toml` under `[asr]`: `model_path` from `DG_WHISPER_MODEL`, `language = "en"`, `beam_size = 5`, word-level timestamps on, `use_gpu = true`, `threads = 8`. The glossary prompt is the canonical glossary terms joined by commas, truncated to about 200 tokens, passed as the initial prompt so Dogen, shikantaza, Genjokoan and similar terms are spelled consistently from the start. Tests use a `FakeTranscriber` that returns a scripted word list.

Captions. The PRD says to use existing captions where a source has them. v1 defaults to transcribing from audio anyway, for two reasons. Student-speech removal needs diarization over audio, and passage links need word-level timestamps that cue-level captions lack. Manual captions are still valuable, so when `fetch_captions` returns a track with `is_auto_generated = false`, the handler stores it in scratch and uses it as a reference to compute word error rate against the ASR output, which gives the data quality tests free ground truth. Auto-generated captions are never used.

The setting `prefer_captions = false` flips this. When true and a manual track exists, the cue text becomes the transcript text, cue times are spread evenly across the words of each cue, and diarization and segmentation still run on the audio.

Caption parsers in `dg-asr` handle WebVTT, SRT and YouTube `json3`, normalize to the same word list, and are covered by fixtures.

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

The default `Diarizer` calls the Python sidecar in `sidecars/diarize/`, a small FastAPI service. `POST /diarize` takes a WAV upload and returns `{"turns": [{"speaker": "S1", "start_ms": 0, "end_ms": 5400}], "speaker_count": 2}`. It runs the pyannote speaker-diarization pipeline. That model needs a Hugging Face access token and acceptance of its terms on the Hugging Face site, and the sidecar reads the token from `HF_TOKEN`. The sidecar writes nothing to disk beyond a temporary file it deletes before responding. A `FakeDiarizer` serves tests.

The segmenter in `dg-segment` turns words and turns into labeled spans. It is deterministic given its inputs and the classifier's outputs.

1. Align. Each word takes the speaker of the turn it overlaps most. Words with no overlapping turn take the nearest turn within 500 ms and otherwise are dropped.
2. Teacher speaker. Over the talk, the teacher is the speaker with the largest share of speaking time outside chanting. All other speakers are treated as students. If a second speaker holds at least `multi_speaker_share` (default 0.35) of speaking time, the talk goes to `needs_review` with reason `multi_speaker` and the job stops. The segmenter never guesses between two possible teachers.
3. Sitting. A stretch with no words of at least `sitting_silence_s` (default 120) is labeled `sitting`.
4. Window classification. Remaining time is cut into windows of 60 seconds, and each window gets a label from `talk`, `chant`, `announcement` or `other`. Rules run first. Very low words per minute with high repetition or a high share of glossary chant phrases marks `chant`. For windows the rules leave ambiguous, `LlmClient` returns a label as JSON with a one-sentence reason that is stored in the job log. A majority filter smooths labels so no span is shorter than `min_span_s` (default 20) unless it is at a talk boundary.
5. Question and answer. After the first `talk` span, any run of non-teacher speech is a student turn and is labeled `qa_student` in memory. A teacher turn that follows a student turn is labeled `qa_teacher`. Student turns are removed from the word list before anything is persisted.
6. Persist. Only `talk` and `qa_teacher` spans become `segment` rows. Chant, announcement, sitting and other spans are recorded in the `spans` array of the transcript JSON for the audit and are not stored as text rows. `question_note` is always null in v1. The PRD allows omitting the question and that is the safer choice.
7. Cleanup. The `audio` handler writes the filtered transcript JSON, inserts the `transcript` and `segment` rows in one transaction, and then `finish()` deletes scratch.

`dg segment sample --count 10 --out report.html` picks ten cleared talks, runs steps 1 to 5 without persisting, and writes an HTML timeline per talk showing labeled spans with the first words of each. It is the tool for the PRD's ten-video segmentation check on talks that have passed the gate.

## Glossary correction

The glossary is a standing asset in the `glossary_term` table, loaded from `scripts/seed_glossary.toml` and maintained with `dg glossary import` and `dg glossary add`. Each term has a canonical spelling, a kind (`term`, `name` or `text_title`) and a list of known variants, which are mis-hearings and alternate spellings. The seed file starts with the terms the PRD names (Dogen, shikantaza, Genjokoan) plus the series titles and common practice vocabulary, and grows from the suggestions file below.

The correction function is pure: `apply_glossary(words, glossary) -> corrected words`.

1. An Aho-Corasick automaton over lowercased variants matches on word boundaries and replaces each match with the canonical form. A multi-word variant collapses into one or more corrected words, which inherit the start of the first replaced word and the end of the last, so timestamps stay valid.
2. Only exact variants are applied automatically.
3. A second pass looks for near misses. For tokens of at least six letters that are not in the English word list, it computes Damerau-Levenshtein distance of 2 or less to each canonical term and a phonetic match. Candidates are written to `glossary_suggestions.csv` with the talk, word index and context, and are never applied. A human adds the variant to the glossary, which triggers the next rerun.
4. The glossary version is the SHA-256 of its sorted content. It is part of the `correct` and `index` job input hashes, so adding a variant requeues exactly the affected talks.
5. `segment.text` stores corrected text. The transcript JSON in S3 stays uncorrected. The index step recomputes corrected words with timestamps by calling the same pure function, so corrected text and timing cannot disagree.

Teacher aliases are not rewritten in text. Aliases serve named-teacher detection in Spec 05.

## Extraction and citation resolution

`LlmClient` in `dg-extract` is the only code that calls the Claude API.

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete_json<T>(&self, req: LlmRequest, class: DataClass) -> Result<T, LlmError>
    where T: DeserializeOwned + JsonSchema + Send;
}
pub enum DataClass { Synthetic, Real }
```

Rules for the client:

1. It calls the Messages API at `https://api.anthropic.com/v1/messages` with `x-api-key`, an `anthropic-version` header, and temperature 0. It forces structured output by defining one tool whose `input_schema` is the JSON Schema of `T` and setting `tool_choice` to that tool. The tool input is parsed with `serde`. A parse failure gets one repair retry and then fails the job.
2. A request with `DataClass::Real` is refused unless `DG_LLM_REAL_DATA_OK=true`. Talks loaded by the fixture loader carry `raw.synthetic = true` in `source_video` and use `Synthetic`.
3. Logs record the stage, model, token counts and a hash of the request. They never contain transcript text.
4. Rate limits and 5xx responses retry with exponential backoff up to three times inside the call. The model name comes only from `DG_LLM_MODEL`.

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

The code verifies that `text` is a substring of the window under whitespace and case normalization and discards anything that is not. The model's offsets are never used. The code finds the word range itself. Discard counts are logged per talk. Verified citations are saved with word ranges in the staging object `citations/<talk_id>.json` in S3, which the index step consumes once passages exist.

Resolution assigns one of three confidence levels, in this order of precedence:

1. Hand list, method `hand_list`, `resolved`. The normalized `text`, hints and window context are matched to `work_locator.label` and `aliases` by exact match or `pg_trgm` similarity of at least 0.85.
2. Series label, method `series_label`, `probable`. If the talk has a `series` and `part`, and the citation is generic (for example "this passage" or "as Dogen says here") with no locator hint, the locator mapped to that part in `config/series.toml` is used.
3. Model hint, method `llm`, `probable`. A `work_hint` and `locator_hint` that fuzzy-match a hand-listed locator at 0.7 or better are accepted.
4. Anything else is `unresolved`, stored with its raw text and no locator.

The model never creates a locator. A hint that matches nothing in the hand list becomes `unresolved`. For talks in labeled series, the resolver is scored against the series label as ground truth.

## Chunking, embedding and indexing

```rust
#[async_trait]
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;                       // 1024
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError>;   // L2-normalized
}
```

`dg-ml` implements `Embedder` for bge-m3 and `Reranker` for bge-reranker-v2-m3 over `ort`, with the `tokenizers` crate for tokenization. `scripts/export_models.py` converts both models to ONNX with int8 quantization and writes them with their tokenizers to `DG_MODEL_DIR`. That script is the only place Python touches the models, and exported files go to the models bucket, never into git.

The chunker in `dg-index` is a pure function from corrected words to passages. Defaults live under `[chunk]` in `config/pipeline.toml`.

1. Input is the corrected word list of one `talk` or `qa_teacher` segment. Passages never cross a segment boundary.
2. Sentences come from Unicode sentence segmentation over the corrected text, and a pause of `sentence_pause_ms` (default 1200) between words also ends a sentence.
3. Sentences are packed greedily into passages. A passage closes when it reaches `target_words` (default 140) and is never allowed past `max_words` (default 220). A pause of `hard_break_ms` (default 8000) also closes a passage.
4. A final passage shorter than `min_words` (default 60) merges into the previous one if the result stays within `merge_cap_words` (default 260), and otherwise is kept as is. The database check on character length is the last guard.
5. There is no overlap between passages in v1.
6. `start_ms` is the start of the first word and `end_ms` is the end of the last word.
7. The output is deterministic, and each passage has a `content_hash` over its text and offsets.

Embedding input is the passage text and nothing else. No teacher, sangha, title or series text is added, so retrieval is relevance-only by construction. Queries are embedded the same way with no instruction prefix. Maximum token length is 512, batch size is 16 on CPU and 64 on GPU, and vectors are L2-normalized for cosine distance.

The index step, per talk, in one transaction:

1. Delete the talk's existing passages, which cascades to citations.
2. Insert the new passages with embeddings and the `embedding_model` name.
3. Map each staged citation to the passage that contains its first word, compute span offsets relative to the passage text, and insert `citation` rows with the resolved confidence.
4. Set `talk.content_hash` to a hash of the passage hashes, `last_verified_at` to now, and state to `ready`.

After a batch, run `ANALYZE passage`. HNSW search settings are applied per query in Spec 05. `dg reindex --talk <id> | --all [--model <name>]` reruns the index step from stored segments and the transcript JSON, which is how a new glossary version or embedding model is applied.

## Acceptance criteria

M4 is done when every item below passes in CI with fakes, and one synthetic talk has run through a real worker, a real Postgres and real ONNX models.

- [ ] `ScratchGuard` deletes scratch on success, on a returned error and on panic. A test injects each failure.
- [ ] After an `audio` job finishes in any outcome, nothing remains under `DG_SCRATCH_DIR` for that talk.
- [ ] The stored transcript JSON contains no words from a non-teacher speaker. A test with a `FakeDiarizer` that returns a student turn proves it, and also checks no `qa_student` row can exist.
- [ ] Segmenter tests on synthetic timelines cover: silence of 120 seconds or more becomes `sitting`; a chant window is labeled `chant`; a student turn is dropped and the following teacher turn is `qa_teacher` with a null `question_note`; a second speaker at 35 percent or more sends the talk to `needs_review`.
- [ ] The glossary replaces known variants with canonical forms, preserves timestamps across multi-word replacements, writes near misses to the suggestions file without applying them, and a glossary change requeues `correct` and `index` for the affected talks only.
- [ ] Extraction discards a fabricated citation that is not a substring of the window. `DataClass::Real` is refused when `DG_LLM_REAL_DATA_OK` is not `true`.
- [ ] The resolver gives `resolved` only for hand-list matches, gives `probable` for series-label and fuzzy model hints, and gives `unresolved` when a model hint matches no hand-listed locator. Precision of `resolved` is 100 percent on the fixture set.
- [ ] Chunker golden tests: every passage is within `min_words` and `max_words` except an unmergeable tail, none crosses a segment boundary, timestamps are monotone, and output is identical across runs.
- [ ] The embedder returns 1024-dimension unit vectors that are identical across repeated runs on CPU within 1e-5.
- [ ] A synthetic talk goes from `cleared` to `ready` through `dg worker`, `servable_passage` returns its passages, and rerunning every stage changes no rows.
- [ ] `dg export transcripts --format jsonl --out <dir>` writes the corrected segments of every consented `ready` talk for the Gate A notebook.
- [ ] `dg segment sample --count 10` renders the segmentation report.
