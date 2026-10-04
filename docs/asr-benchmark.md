# ASR benchmark on the owner's Windows PC

Run on October 4, 2026. distil-large-v3 transcribes the benchmark clip about 10 to 11 times faster than real time when OpenVINO runs the encoder on the Arc GPU or the NPU, and about 2 times faster on the CPU alone. Vulkan gives garbage with its default settings on this GPU. With the GPU's matrix-core (coopmat) path turned off it is correct and runs about 8 times faster than real time.

Spec 04 has no rule that picks a setup by speed, so this report picks none. Its only rule is to keep `large-v3` if `distil-large-v3` misses core vocabulary, and that needs word error rates on a hand-corrected sample, which this benchmark does not measure.

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
| Clip | `data-inbox/benchmark/clip.mp3`, the LibriVox preface to *Sermons of a Buddhist Abbot*, converted to 16 kHz mono WAV. It is 313.16 s long, shorter than the 10 minutes asked for, so nothing was trimmed. |

Every run uses `whisper-cli` with `-t 8 -l en` and the defaults: beam size 5, best-of 5, flash attention on. Each setup ran twice in a row with nothing else running.

The real-time factor is seconds of audio divided by wall-clock seconds, model load included. Peak RAM is the process's peak working set. GPU memory is the process's shared plus dedicated GPU memory from Windows performance counters. On this integrated GPU that memory also comes out of the 16 GB.

## Results

| Setup | RTF, run 1 | RTF, run 2 | Wall time, run 2 | Peak RAM | GPU memory | Transcript | Errors |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CPU only, 8 threads | 1.89 | 1.94 | 161.6 s | 1.8 GB | 0 | Correct, 807 words | None |
| Vulkan, default | 1.30 | 1.00 | 313.3 s | 1.9 GB | 1.7 GB | Garbage: repeated fragments such as "e e e e" and "have have" | None reported. The exit code is 0, so only the text shows the failure. |
| Vulkan, `GGML_VK_DISABLE_COOPMAT=1` | 7.31 | 8.23 | 38.0 s | 2.0 GB | 1.7 GB | Correct, 818 words | Invented last line: "I'm going to be able to be able to be." |
| OpenVINO encoder on GPU | 7.93 | 10.10 | 31.0 s | 5.8 GB | 3.8 GB in run 1, 1.7 GB in run 2 | Correct, 825 words | None |
| OpenVINO encoder on NPU | 2.22 | 11.03 | 28.4 s | 7.1 GB in run 1, 4.1 GB in run 2 | 1.4 GB | Correct, 820 words | "Thank you." repeated at the end |

Notes on the results:

- **First runs.** The first OpenVINO run on each device compiles the encoder and caches it next to the model (`…-encoder-openvino-cache`). The NPU run 1 figure of 2.22 is mostly that compilation. Run 2 shows steady-state speed.
- **Where the time goes.** With OpenVINO only the encoder moves off the CPU, and the decoder stays on the CPU threads. Encoding took 5.5 s on the GPU and 7.5 s on the NPU, against 142 s on the CPU. Decoding took 9 to 12 s in every correct setup.
- **Vulkan defaults.** With default settings Vulkan encodes every 30-second window several times and decodes far more tokens. That is the garbage output, not a slow but valid run. Turning off flash attention (`-nfa`) did not fix it on a 60-second test. Turning off coopmat did.
- **Endings.** The invented lines at the end of the Vulkan and NPU transcripts sit in the silence after the recording ends, a known whisper failure on trailing silence. The text before them matches the CPU transcript.
- **Agreement.** Against the CPU transcript, the OpenVINO GPU and NPU transcripts differ in 116 and 85 word-level diff lines, mostly spelling of names such as "Tostoi"/"Tostoy" and line breaks. No reference transcript exists, so no setup's accuracy is measured.

## What the speeds mean for the SSZ corpus

The PRD puts the SSZ corpus at about 140 hours. Compute time is 140 hours divided by the real-time factor:

| Setup | RTF, run 2 | Compute time for 140 h |
| --- | --- | --- |
| CPU only, 8 threads | 1.94 | about 72 h |
| Vulkan, coopmat off | 8.23 | about 17 h |
| OpenVINO encoder on GPU | 10.10 | about 14 h |
| OpenVINO encoder on NPU | 11.03 | about 13 h |

The clip is a single clean LibriVox reading, not a recorded dharma talk with room sound and several speakers. Talk audio may run at a different speed.

## Not covered

- **Accuracy.** No word error rate was measured for any setup, and Spec 04's `large-v3` versus `distil-large-v3` check was not run.
- **Configurations.** `large-v3`, quantized models, smaller beam sizes and thread counts other than 8 were not run.
- **Length.** The clip is 5 minutes 13 seconds, not 10 minutes.

## Reproducing

The build scripts, runner, models and results are in `.bench/` in the primary checkout, which is kept out of git through `.git/info/exclude`:

- `build.ps1`: builds `build-cpu`, `build-vulkan` (`-DGGML_VULKAN=ON`) and `build-openvino` (`-DWHISPER_OPENVINO=1 -DOpenVINO_DIR=…`).
- `convert_distil.py`: converts the encoder for OpenVINO.
- `run_all.ps1`: runs every setup twice.
- `results/runs.jsonl`: one line per run, with the transcripts and stderr logs beside it.
