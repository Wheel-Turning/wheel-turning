# Model licenses

Every model the system can load (Spec 04, Model license record). A stage refuses to start when its configured model is not listed here with a revision, and CI fails when `config/` names a model missing from this file (`wt_core::licenses`; the CI check is in `wt-core/tests/repo_guards.rs`). A model counts as listed when its name is in backticks in a table row, and as having a revision when that row holds a commit or file hash, or is under Language models, where the model id is the revision.

Revisions are Hugging Face commit hashes, or for a single file its SHA-256 as Hugging Face's LFS record gives it. Checked on October 5, 2026 by acowen with Claude Code, from the public model pages and the Hugging Face API.

## Speech recognition

Run with whisper.cpp, so the files loaded are the ggml conversions. Spec 04 runs the corpus on `large-v3-turbo` (owner's decision of October 8, 2026, replacing `large-v3`, `docs/asr-benchmark.md`); the others are allowed through `asr.model_path`.

| Model | Loaded as | License | Source and revision | File SHA-256 |
| --- | --- | --- | --- | --- |
| Whisper large-v3 | `ggml-large-v3.bin` | Apache-2.0, https://www.apache.org/licenses/LICENSE-2.0 | Weights `openai/whisper-large-v3` at `06f233fe06e710322aca913c1bc4249a0d71fce1`; ggml file from `ggerganov/whisper.cpp` at `5359861c739e955e79d9a303bcbc70fb988958b1` (MIT) | `64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2`, matching the file in `.bench/models` |
| Whisper large-v3-turbo | `ggml-large-v3-turbo.bin` | MIT, https://opensource.org/license/mit | Weights `openai/whisper-large-v3-turbo`; ggml file from `ggerganov/whisper.cpp` (MIT), downloaded October 8, 2026. The worker's model since the owner's decision of October 8, 2026 | `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69`, the file in `.bench/models` |
| distil-large-v3 | `ggml-distil-large-v3.bin` | MIT, https://opensource.org/license/mit | `distil-whisper/distil-large-v3-ggml` at `0d78dd96ed9fc152325f63b53788fec3b43de031`; weights `distil-whisper/distil-large-v3` at `8031d2e6ce6631b7fc45629dddfc00271116d981` | `2883a11b90fb10ed592d826edeaee7d2929bf1ab985109fe9e1e7b4d2b69a298`, as in `docs/asr-benchmark.md` |
| distil-large-v3, OpenVINO encoder | `ggml-distil-large-v3-encoder-openvino.bin` | MIT | Converted from `distil-whisper/distil-large-v3-openai` at `81941037893cb90dc82af45bc5dc5146ab0a818f` with whisper.cpp's `convert_encoder`. Benchmark only; the worker does not load it | Not recorded: a local conversion |

## Diarization

The sidecar in `sidecars/diarize/` runs one pyannote speaker-diarization pipeline. Both pipelines are gated: each needs its user conditions accepted on Hugging Face by the account whose token the sidecar uses (`HF_TOKEN`).

| Model | License | Revision | Notes |
| --- | --- | --- | --- |
| `pyannote/speaker-diarization-community-1` | CC-BY-4.0, https://creativecommons.org/licenses/by/4.0/ | `3533c8cf8e369892e6b79ff1bf80f7b0286a54ee` | The pipeline pyannote.audio 4 documents and the sidecar uses. CC BY needs attribution to pyannote, which the About page's credits will carry |
| `pyannote/speaker-diarization-3.1` | MIT | `84fd25912480287da0247647c3d2b4853cb3ee5d` | The earlier pipeline, which pyannote now calls legacy. Listed so the sidecar can fall back to it |
| `pyannote/segmentation-3.0` | MIT | `e66f3d3b9eb0873085418a7b813d3b369bf160bb` | Loaded by the 3.1 pipeline |
| `pyannote/wespeaker-voxceleb-resnet34-LM` | CC-BY-4.0 | `837717ddb9ff5507820346191109dc79c958d614` | Speaker embedding loaded by the 3.1 pipeline. Not gated |

## Retrieval

Loaded as ONNX with int8 quantization, exported by `scripts/export_models.py` (Spec 04) into `WT_MODEL_DIR`. The export takes the revision below.

| Model | Loaded as | License | Revision |
| --- | --- | --- | --- |
| `BAAI/bge-m3` | `bge-m3-int8` (`WT_EMBED_MODEL`) | MIT | `5617a9f61b028005a4858fdac845db406aefb181` |
| `BAAI/bge-reranker-v2-m3` | `bge-reranker-v2-m3-int8` (`WT_RERANK_MODEL`) | Apache-2.0 | `953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e` |

The export of October 6, 2026 took `onnx/model.onnx` and `onnx/tokenizer.json` from bge-m3 at that revision and quantized the weights to int8 per channel with ONNX Runtime 1.30.0. Its `model.onnx` has SHA-256 `63e806d9763c82e8ffb55e32d577a64cacf2a57be1ac0d803f118cf770cb399d`, as its `export.json` records. The reranker is not exported yet.

## Language models

Called through the Claude API (`ANTHROPIC_API_KEY`) under Anthropic's Commercial Terms of Service, https://www.anthropic.com/legal/commercial-terms. The revision of an API model is its model id; `WT_LLM_MODEL` and the per-stage settings name it, and each is added here when first configured. None is configured yet.

| Model id | Stages | Added |
| --- | --- | --- |
| `claude-haiku-4-5` | Extract and window labeling (`WT_LLM_MODEL` or `WT_LLM_MODEL_EXTRACT`), and synthesis and verification on the owner's alpha (`WT_LLM_MODEL`, `WT_LLM_MODEL_SYNTH`, `WT_LLM_MODEL_VERIFY`), priced in `config/llm_prices.toml` | October 6, 2026 |
| `claude-sonnet-5-5` | Priced in `config/llm_prices.toml`; not configured for a stage yet | October 6, 2026 |
| `claude-opus-5-5` | Priced in `config/llm_prices.toml`; not configured for a stage yet | October 6, 2026 |

Data handling, read October 5, 2026 (https://privacy.claude.com/en/articles/7996866-how-long-do-you-store-my-organization-s-data and https://platform.claude.com/docs/en/manage-claude/api-and-data-retention):

- Commercial use, which covers the API and the Team and Enterprise plans, is not used for training unless the customer opts in.
- Inputs and outputs are deleted from Anthropic's systems within 30 days by default. A request flagged under the usage policy is kept up to 2 years, and its classification scores up to 7 years. Feedback a user submits is kept 5 years.
- Zero data retention is a separate agreement, arranged through Anthropic's sales team and enabled per API organization. A Team plan subscription does not change the API's retention.

Zero data retention is not available to the project (decision of October 9, 2026). Real transcripts go to the model under the default terms above once the owner sets `WT_LLM_REAL_DATA_OK=true`. Student speech is never sent (Spec 04, paraphrases).
