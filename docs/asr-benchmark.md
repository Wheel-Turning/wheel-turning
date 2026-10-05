# ASR benchmark on the owner's Windows PC

Run on October 4, 2026. Spec 04's decision rule picks **whisper.cpp with the OpenVINO encoder on the Arc GPU**, and the route **run the whole corpus on this PC**. The best measured real-time factor was 11.53, well above the rule's threshold of 3. The NPU route was close behind at 9.9 to 10.6. Vulkan with the GPU's matrix-core (coopmat) path turned off reached 7.8 to 8.8. CPU only reached 1.8. Vulkan with its default settings fails on this GPU.

## Decision rule (Spec 04)

"Owner's hardware and the decision rule" takes the best measured real-time factor on the sample:

- **3 or more:** run the whole corpus on this PC. This is the case here, at 11.53.
- **Between 1 and 3:** run the first 20 to 30 talks on this PC for Gate A, and rent a GPU machine for the rest.
- **Below 1:** rent a GPU machine for everything.

Speed only decides the route. Spec 04 also says to keep `large-v3` for the full run if `distil-large-v3` misses core vocabulary. That needs word error rates on a hand-corrected sample, and this benchmark does not measure them.

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

## What the speeds mean for the SSZ corpus

The PRD puts the SSZ corpus at about 140 hours. Compute time is 140 hours divided by the mean real-time factor of the two runs:

| Setup | Mean RTF | Compute time for 140 h |
| --- | --- | --- |
| OpenVINO encoder on GPU | 11.28 | about 12.4 h |
| OpenVINO encoder on NPU | 10.23 | about 13.7 h |
| Vulkan, coopmat off | 8.28 | about 16.9 h |
| CPU only, 8 threads | 1.78 | about 78.7 h |

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

- **Accuracy.** No word error rate was measured, and Spec 04's `large-v3` versus `distil-large-v3` check was not run.
- **`wt asr benchmark`.** The command in Spec 04's acceptance items does not exist yet. These runs used `whisper-cli` through the scripts below. The chosen setup is recorded here by hand.
- **Configurations.** `large-v3`, quantized models, smaller beam sizes and thread counts other than 8 were not run.

## Reproducing

The build scripts, runner, models and results are in `.bench/` in the primary checkout, which is kept out of git through `.git/info/exclude`:

- `build.ps1`: builds `build-cpu`, `build-vulkan` (`-DGGML_VULKAN=ON`) and `build-openvino` (`-DWHISPER_OPENVINO=1 -DOpenVINO_DIR=…`).
- `convert_distil.py`: converts the encoder for OpenVINO.
- `run_all.ps1`: runs every setup in Spec 04's order.
- `results/runs.jsonl`: one line per run, with the transcripts and stderr logs beside it.
