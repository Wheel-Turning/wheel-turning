#!/usr/bin/env bash
# Turns a fresh Ubuntu 22.04 or 24.04 machine with an NVIDIA GPU into an audio worker (Spec 01,
# GPU access): whisper.cpp built for CUDA with large-v3-turbo, the diarization sidecar on the
# GPU, and `wt` built from the repository that run.sh unpacked in ~/wt. Rerunning it skips what
# is already in place. It runs the same on a rented GPU machine and on an AWS GPU instance.
set -euo pipefail

# The whisper.cpp commit the benchmark and the PC worker use (docs/asr-benchmark.md).
WHISPER_REV=60c0be6ac8fa71b1a2ae2dd938a31a34a508e774
TURBO_URL=https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
# docs/model-licenses.md
TURBO_SHA256=1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69
CUDA_TOOLKIT=cuda-toolkit-12-6

REPO=$HOME/wt
TOOLS=$HOME/wt-tools
step() { echo "=== $(date +%H:%M:%S) $*"; }

step "system packages"
sudo DEBIAN_FRONTEND=noninteractive apt-get update -q
sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq --no-install-recommends \
  build-essential cmake git curl ca-certificates ffmpeg pkg-config libssl-dev libsndfile1 pciutils

lspci | grep -qi nvidia || {
  echo "no NVIDIA GPU on this machine"
  exit 1
}
[ -f "$REPO/Cargo.toml" ] || {
  echo "$REPO has no repository; run infra/gpu-worker/run.sh from the PC"
  exit 1
}
mkdir -p "$TOOLS"

# NVIDIA's repository, for the driver when the image has none and for the CUDA toolkit.
if ! command -v nvidia-smi > /dev/null || { ! command -v nvcc > /dev/null && [ ! -x /usr/local/cuda/bin/nvcc ]; }; then
  if ! dpkg -s cuda-keyring > /dev/null 2>&1; then
    step "NVIDIA repository"
    # shellcheck disable=SC1091
    . /etc/os-release
    keyring=$(mktemp -d)
    curl -fsSL -o "$keyring/cuda-keyring.deb" \
      "https://developer.download.nvidia.com/compute/cuda/repos/ubuntu${VERSION_ID//./}/x86_64/cuda-keyring_1.1-1_all.deb"
    sudo dpkg -i "$keyring/cuda-keyring.deb"
    rm -rf "$keyring"
    sudo apt-get update -q
  fi
fi
if ! command -v nvidia-smi > /dev/null; then
  step "NVIDIA driver"
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq cuda-drivers
  echo "the driver is installed; reboot and run setup.sh again"
  exit 3
fi
nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader
if ! command -v nvcc > /dev/null && [ ! -x /usr/local/cuda/bin/nvcc ]; then
  step "CUDA toolkit"
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq --no-install-recommends "$CUDA_TOOLKIT"
fi
# Lambda Stack images have nvcc on PATH and a /usr/local/cuda that CMake cannot find headers
# under, so /usr/local/cuda/bin goes first only when no nvcc is on PATH.
command -v nvcc > /dev/null || export PATH=/usr/local/cuda/bin:$PATH
NVCC=$(command -v nvcc)

if [ ! -x "$TOOLS/whisper.cpp/build/bin/whisper-cli" ]; then
  step "whisper.cpp for CUDA"
  [ -d "$TOOLS/whisper.cpp" ] || git clone -q https://github.com/ggml-org/whisper.cpp "$TOOLS/whisper.cpp"
  git -C "$TOOLS/whisper.cpp" fetch -q origin "$WHISPER_REV"
  git -C "$TOOLS/whisper.cpp" checkout -q "$WHISPER_REV"
  # The GPU's own architecture, since older CMake has no "native".
  rm -rf "$TOOLS/whisper.cpp/build"
  arch=$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader | head -1 | tr -d '.')
  cmake -S "$TOOLS/whisper.cpp" -B "$TOOLS/whisper.cpp/build" -DCMAKE_BUILD_TYPE=Release \
    -DGGML_CUDA=1 -DCMAKE_CUDA_COMPILER="$NVCC" -DCMAKE_CUDA_ARCHITECTURES="$arch" > /dev/null
  cmake --build "$TOOLS/whisper.cpp/build" -j "$(nproc)" --target whisper-cli > /dev/null
fi

model=$TOOLS/models/ggml-large-v3-turbo.bin
if [ ! -f "$model" ] || ! echo "$TURBO_SHA256  $model" | sha256sum -c --status; then
  step "large-v3-turbo"
  mkdir -p "$TOOLS/models"
  curl -fsSL -o "$model.part" "$TURBO_URL"
  echo "$TURBO_SHA256  $model.part" | sha256sum -c --status || {
    rm -f "$model.part"
    echo "large-v3-turbo does not match docs/model-licenses.md"
    exit 1
  }
  mv "$model.part" "$model"
fi

if [ ! -x "$TOOLS/sidecar/bin/python" ]; then
  step "diarization sidecar"
  command -v uv > /dev/null || [ -x "$HOME/.local/bin/uv" ] || curl -LsSf https://astral.sh/uv/install.sh | sh
  export PATH=$HOME/.local/bin:$PATH
  uv venv -q -p 3.11 "$TOOLS/sidecar"
  # Linux torch wheels from PyPI carry CUDA.
  uv pip install -q -p "$TOOLS/sidecar/bin/python" -r "$REPO/sidecars/diarize/pyproject.toml"
fi

step "wt"
command -v cargo > /dev/null || [ -x "$HOME/.cargo/bin/cargo" ] ||
  curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain none
export PATH=$HOME/.cargo/bin:$PATH
(cd "$REPO" && SQLX_OFFLINE=true cargo build -q --release -p wt-cli)

step ready
"$TOOLS/whisper.cpp/build/bin/whisper-cli" --help > /dev/null 2>&1 && echo "whisper-cli ok"
"$TOOLS/sidecar/bin/python" -c "import torch; print('torch', torch.__version__, 'cuda', torch.cuda.is_available())"
