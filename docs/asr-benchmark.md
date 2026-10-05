# ASR benchmark on the owner's Windows PC

Run on October 4, 2026. **The owner chose `large-v3` on Vulkan with coopmat off**, after the accuracy results below. On speed alone, Spec 04's decision rule picks **whisper.cpp with the OpenVINO encoder on the Arc GPU**, and the route **run the whole corpus on this PC**. The best measured real-time factor was 11.53, well above the rule's threshold of 3. The NPU route was close behind at 9.9 to 10.6. Vulkan with the GPU's matrix-core (coopmat) path turned off reached 7.8 to 8.8. CPU only reached 1.8. Vulkan with its default settings fails on this GPU.

Accuracy was measured afterwards against the book's own text (see Accuracy). `large-v3` makes fewer errors than `distil-large-v3`: 1.12% word error rate against 1.71% on the same OpenVINO GPU route. It also gets 1 of the 4 Buddhist terms of art in the clip wrong, where `distil-large-v3` gets 3 or all 4 wrong. It is much slower: 1.06 times real time on the OpenVINO route, because its 32-layer decoder runs on the CPU there, and 3.17 to 3.26 times real time on Vulkan, where the decoder runs on the GPU. The rules' outcome is set out under Decision rule.

## Decision rule (Spec 04)

"Owner's hardware and the decision rule" takes the best measured real-time factor on the sample:

- **3 or more:** run the whole corpus on this PC. This is the case here, at 11.53.
- **Between 1 and 3:** run the first 20 to 30 talks on this PC for Gate A, and rent a GPU machine for the rest.
- **Below 1:** rent a GPU machine for everything.

Speed only decides the route. Spec 04 also says to keep `large-v3` for the full run if `distil-large-v3` misses core vocabulary. Measured against the book text below, `distil-large-v3` misheard both Atmans and the Anatman in every setup, and Dharmakaya in three of four. `large-v3` got every term right except one Anatman.

Applied together, the two rules give:

- **Model.** `large-v3`, because `distil-large-v3` misses core vocabulary.
- **Route with `large-v3`.** Its best real-time factor is 3.26, on Vulkan with coopmat off. That is just over the threshold of 3 for running the whole corpus on this PC. On the OpenVINO route it is 1.06, which falls in the "between 1 and 3" route.

Both outcomes sit close to their lines. The rule's sample is one clip of a clean reading, and the clip has only 4 terms of art.

Each `large-v3` setup also changed one clause (see Faithfulness): on Vulkan it dropped an 8-word clause, and on OpenVINO it added a clause that is not in the book.

On October 4, 2026 the owner chose `large-v3` on Vulkan, run with `GGML_VK_DISABLE_COOPMAT=1`. Specs 01 and 04 and the architecture north star now say so, and Spec 01's configuration table lists the variable. The dropped clause is recorded in Spec 04 as a known risk for the claim audit and the timestamp checks.

## Machine and inputs

| Item | Value |
| --- | --- |
| CPU | Intel Core Ultra 7 256V, 8 cores |
| GPU | Intel Arc 140V, integrated, driver 32.0.101.6314. It shares system memory. |
| NPU | Intel AI Boost |
| RAM | 16 GB |
| OS | Windows 11 Home, native build, no WSL |
| Compiler | MSVC 14.51 (Visual Studio 18 Build Tools), CMake and Ninja from the same install |
| whisper.cpp | commit `60c0be6` (October 2, 2026) |
| Vulkan SDK | 1.4.363.0 |
| OpenVINO | 2026.3.1 runtime and CMake files from the `openvino` Python package |
| Model | `ggml-distil-large-v3.bin`, SHA-256 `2883a11b…2b69a298`, matching Hugging Face |
| OpenVINO encoder | Converted from `distil-whisper/distil-large-v3-openai` `model.bin` with whisper.cpp's own `convert_encoder`, FP32 |
| Clip | The first 10:00 of `soba_03_shaku_64kb.mp3`, chapter 3 of the LibriVox recording of *Sermons of a Buddhist Abbot*, converted to 16 kHz mono WAV. One volunteer reader. |

The clip is a public-domain reading of a Zen teacher's sermons. Spec 04 says the clip is "never a talk from a teacher". The owner chose this clip knowing that.

Every run uses `whisper-cli` with `-t 8 -l en` and the defaults: beam size 5, best-of 5, flash attention on. The setups ran one after another, in Spec 04's order, with nothing else running.

The real-time factor is seconds of audio divided by wall-clock seconds, model load included. Peak RAM is the process's peak working set. GPU memory is the process's shared plus dedicated GPU memory from Windows performance counters. On this integrated GPU that memory also comes out of the 16 GB.

## Results

| Setup | RTF, run 1 | RTF, run 2 | Wall time, run 2 | Peak RAM | GPU memory | Words | Errors |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Vulkan, default | — | not run | — | 2.0 GB | 1.7 GB | 0 | Crashed after 107 s with exit code −1 and no message, and wrote no transcript |
| Vulkan, `GGML_VK_DISABLE_COOPMAT=1` | 7.79 | 8.77 | 68.4 s | 2.0 GB | 1.7 GB | 1,749 | None |
| OpenVINO encoder on GPU | 11.03 | 11.53 | 52.0 s | 5.7 GB | 1.7 GB | 1,747 | None |
| OpenVINO encoder on NPU | 10.56 | 9.90 | 60.6 s | 4.1 GB | 1.4 GB | 1,753 | None |
| CPU only, 8 threads | 1.81 | 1.75 | 342.8 s | 1.8 GB | 0 | 1,752 | None |

Notes on the results:

- **Default Vulkan.** It ran once, to confirm the failure seen on the earlier clip, where it wrote repeated fragments instead of text.
- **OpenVINO cache.** The OpenVINO encoder was already compiled and cached from the earlier runs, so run 1 on each device includes no compilation. A first run on a fresh machine is slower; on the earlier clip the NPU's first run was 2.22.
- **Where the time goes.** With OpenVINO only the encoder moves off the CPU, and the decoder stays on the CPU threads. Encoding the clip took 6.6 s on the GPU, 14.0 s on the NPU, 39.4 s on Vulkan and 296.5 s on the CPU. Decoding took 19 to 30 s in every setup.
- **Transcripts.** The four working setups produce the same text apart from punctuation, capitals and line breaks, and all four end mid-sentence where the clip is cut. No reference transcript exists, so no setup's accuracy is measured.

## Accuracy

Measured on October 4, 2026, on the same 10-minute clip, against the book text the reader was reading.

**Reference.** Chapter 3, "Assertions and Denials", of the 1906 Open Court edition.
- The text is the OCR of the Cornell scan on archive.org (`cu31924023155223`). Page numbers and running headers were removed, and words split at line ends were joined.
- In forty places it disagreed with a second scan (`sermonsofbuddhis00shakrich`). Each was checked by hand and fixed, for example "Dharmak§,ya" to Dharmakaya, "An^tman" to Anatman, and "win overwhelm" to "will overwhelm".
- OCR errors that both scans share would remain.
- The book is not on Project Gutenberg or Wikisource.

**Scoring.**
- Both texts are lowercased, accents and punctuation are removed, and hyphens count as spaces.
- A few spellings of the same sound are treated as equal. The book's "my self", "some one", "thorough-going", "self-advertising" and "et cetera" match "myself", "someone", "thoroughgoing", "selfadvertising" and "etc".
- The LibriVox preamble is not in the book, so scoring starts at the chapter's first words, "There are two avenues".
- The clip stops mid-chapter, so the reference ends where it best matches each transcript's end. That gives 1,693 reference words.
- Word error rate is substitutions plus deletions plus insertions, divided by reference words.
- Where the reader adds or skips a word, it counts against every setup alike. The comparison between setups stays fair, but no setup can reach 0%.

| Model and setup | Word error rate | Substitutions | Deletions | Insertions | Terms of art wrong (of 4) | All Buddhist words wrong (of 15) |
| --- | --- | --- | --- | --- | --- | --- |
| `large-v3`, OpenVINO encoder on GPU | 1.12% | 4 | 1 | 14 | 1: Anatman as "Atman" | 1 |
| `large-v3`, Vulkan, coopmat off | 1.48% | 5 | 10 | 10 | 1: Anatman as "Atman" | 1 |
| `distil-large-v3`, OpenVINO encoder on GPU | 1.71% | 18 | 3 | 8 | 3: Atman twice as "ottman", Anatman as "notman" | 4 |
| `distil-large-v3`, Vulkan, coopmat off | 1.95% | 20 | 3 | 10 | 4: Atman twice and Anatman as "ottman", Dharmakaya as "kaya" | 5 |
| `distil-large-v3`, CPU only | 2.01% | 20 | 3 | 11 | 4: Atman twice and Anatman as "ottman", Dharmakaya as "kaya" | 4 |
| `distil-large-v3`, OpenVINO encoder on NPU | 2.07% | 20 | 2 | 13 | 4: Atman twice and Anatman as "ottman", Dharmakaya as "kaya" | 5 |

Both runs of each setup produced the same transcript, so each row stands for both.

**Terms.**
- The scored span holds 15 Buddhist words. Four are terms of art: Atman (twice), Anatman and Dharmakaya.
- The other eleven are Buddhist, Buddhism, Buddhists and Sanskrit. Every setup got these right except for "Buddhists" heard as "Buddhist" or "on", and "Sanskrit" spelled "Sanscrit" on the NPU.
- The chapter has none of the Japanese Zen vocabulary of the SSZ talks, such as Dogen, shikantaza or Genjokoan. So these results do not show how either model handles that vocabulary.
- No glossary prompt was used. Spec 04's glossary pass is meant to correct terms like these after transcription.

**Speed of `large-v3`.**

| Setup | RTF, run 1 | RTF, run 2 | Wall time, run 2 | Peak RAM | GPU memory |
| --- | --- | --- | --- | --- | --- |
| `large-v3`, OpenVINO encoder on GPU | 1.06 | 1.06 | 563.7 s | 5.8 GB | 1.7 GB (3.8 GB in run 1) |
| `large-v3`, Vulkan, coopmat off | 3.17 | 3.26 | 184.1 s | 4.3 GB | 3.9 GB |

- On the OpenVINO route only the encoder runs on the GPU. Encoding took 46 s, and decoding took 425 s on the CPU, because `large-v3` has 32 decoder layers to `distil-large-v3`'s 2.
- The two models' converted OpenVINO encoders are byte for byte identical, so the speed difference is all in the decoder.
- Run 1 on OpenVINO includes compiling the encoder cache. A file comparison also read both encoder files from disk during that run.

**Faithfulness.** Two passages show the kind of error a word error rate hides, and a third may be the reader.

- **Inserted clause, `large-v3` on OpenVINO.** At about 3:54 the book has "whereby this mythical entity receives its due and last judgment". This transcript has "whereby this mythical entity returns to its original state? This mythical entity receives its due and last judgment". `distil-large-v3` and `large-v3` on Vulkan do not have the extra clause. Either the model invented it, or the reader made a false start that only this setup kept. Listening settles it.
- **Dropped clause, `large-v3` on Vulkan.** At about 8:27 the book has "I take these two meaning the same thing". This transcript drops those eight words.
- **Possible reader repetition.** At about 1:25 both `large-v3` setups have "different philosophies and different" where the book has "different philosophical". It looks like a reader stumble that `large-v3` keeps.

Short clips of the three passages are in `data-inbox/benchmark/`, a folder git ignores:
- `listen-1-philosophical-schools.wav`
- `listen-2-whereby-mythical-entity.wav`
- `listen-3-take-these-two.wav`

## What the speeds mean for the SSZ corpus

The PRD now puts the SSZ corpus at about 163 hours, after a full inventory of the Vimeo account; it said 140 when these runs were made. Compute time is 163 hours divided by the mean real-time factor of the two runs. The first four rows are `distil-large-v3`:

| Setup | Mean RTF | Compute time for 163 h |
| --- | --- | --- |
| OpenVINO encoder on GPU | 11.28 | about 14.5 h |
| OpenVINO encoder on NPU | 10.23 | about 15.9 h |
| Vulkan, coopmat off | 8.28 | about 19.7 h |
| CPU only, 8 threads | 1.78 | about 91.6 h |
| `large-v3`, Vulkan, coopmat off (chosen) | 3.22 | about 50.7 h |
| `large-v3`, OpenVINO encoder on GPU | 1.06 | about 154 h |

The clip is a single clean reading. Recorded talks with room sound, several speakers and long silences may run at a different speed.

## Earlier run on a shorter clip

The first run used the 5:13 LibriVox preface to the same book. The speeds were close:

| Setup | RTF on the 5:13 preface |
| --- | --- |
| Vulkan, coopmat off | 8.23 |
| OpenVINO encoder on GPU | 10.10 |
| OpenVINO encoder on NPU | 11.03 |
| CPU only | 1.94 |

On that clip default Vulkan finished but wrote repeated fragments. That earlier clip and its results are kept as `clip-preface-313s.wav` and `.bench/results-preface/`.

## Not covered

- **Accuracy on talk audio.** Word error rate was measured on one clean reading of a book, not on a recorded dharma talk. The clip has only 4 terms of art, none of them Japanese Zen vocabulary. Spec 04's hand-corrected sample of SSZ talks is still needed.
- **`wt asr benchmark`.** The command in Spec 04's acceptance items does not exist yet. These runs used `whisper-cli` through the scripts below. The chosen setup is recorded here by hand.
- **Configurations.** These were not run: `large-v3` on the NPU or CPU, quantized models, a glossary prompt, smaller beam sizes, and thread counts other than 8.

## Reproducing

The build scripts, runner, models and results are in `.bench/` in the primary checkout, which is kept out of git through `.git/info/exclude`:

- `build.ps1`: builds `build-cpu`, `build-vulkan` (`-DGGML_VULKAN=ON`) and `build-openvino` (`-DWHISPER_OPENVINO=1 -DOpenVINO_DIR=…`).
- `convert_distil.py`: converts the encoder for OpenVINO.
- `run_all.ps1`: runs every setup in Spec 04's order.
- `results/runs.jsonl`: one line per run, with the transcripts and stderr logs beside it.
- `accuracy/extract.py` and `accuracy/reference_ch3.txt`: the chapter text and the corrected reference.
- `accuracy/score.py`: the word error rate and term scoring. `accuracy/scores.jsonl` holds the results.
