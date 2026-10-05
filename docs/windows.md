# Working on Windows

The owner develops on a Windows 11 PC with an Intel Arc 140V integrated GPU, an Intel NPU, 16 GB of RAM and no NVIDIA card. Spec 01's Windows notes and Spec 04's hardware section set the rules. This page lists the installs, where files live, and the commands that differ from Linux.

There are two places code runs:

- **WSL 2 (Ubuntu).** The repository, Rust builds, tests, Docker, Terraform and `aws`. Spec 01 recommends this for everything that assumes a Unix shell.
- **Native Windows.** The transcription worker only. Spec 04 builds it natively because WSL 2 reaches Intel graphics for compute only through slower layers, or not at all.

Each step below is marked **tested** if it was run on the owner's PC on October 4, 2026, or **from the spec** if it is still unchecked. Spec 01's acceptance item for this page is met only after the whole page runs on a clean machine or a clean WSL distribution.

## One-time Windows settings

Run these in PowerShell.

1. **Long paths.** From an administrator PowerShell, turn on long path support, then restart *(from the spec)*:

   ```powershell
   New-ItemProperty -Path HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem -Name LongPathsEnabled -Value 1 -PropertyType DWORD -Force
   ```

2. **Git settings.** The repository's `.gitattributes` keeps LF endings, and these settings stop Git converting them and allow deep paths *(from the spec)*:

   ```powershell
   git config --global core.autocrlf false
   git config --global core.longpaths true
   ```

3. **WSL 2 memory limit.** WSL 2 and Docker Desktop share the 16 GB. Create `C:\Users\<you>\.wslconfig` with a limit, then run `wsl --shutdown` *(from the spec; 8 GB is a starting value)*:

   ```ini
   [wsl2]
   memory=8GB
   ```

## Installs on Windows

| Tool | How | Used for | Status |
| --- | --- | --- | --- |
| WSL 2 with Ubuntu | `wsl --install -d Ubuntu` | The development environment | From the spec |
| Docker Desktop | `winget install Docker.DockerDesktop`, then turn on the WSL 2 backend | Postgres for `cargo test` and the dev database | From the spec |
| Visual Studio Build Tools with the C++ workload | Visual Studio Installer | MSVC, CMake and Ninja for the native whisper.cpp build | Tested (Build Tools 18, MSVC 14.51) |
| Vulkan SDK | `winget install KhronosGroup.VulkanSDK` | The Vulkan build of whisper.cpp | Tested (1.4.363.0) |
| Python 3.12 and uv | `winget install astral-sh.uv`, then `uv python install 3.12` | OpenVINO runtime and encoder conversion | Tested. If uv reports a missing version link, use `uv python install 3.12 --reinstall` and point `uv venv` at the full path of the downloaded `python.exe`. |
| ffmpeg | `winget install Gyan.FFmpeg` | Audio conversion for the worker | Tested (9.0.2) |
| AWS CLI v2 and the Session Manager plugin | `winget install Amazon.AWSCLI`, `winget install Amazon.SessionManagerPlugin` | Signing in, and the SSM port forward the worker uses to reach the database | Installed: AWS CLI 2.34.36, Session Manager plugin 1.2.814.0 (`session-manager-plugin --version`, October 4, 2026). The port forward against AWS is not tested yet. |
| Rust | `winget install Rustlang.Rustup` | Building `wt` natively for the worker | Installed |

Scoop works too. There is no Homebrew on Windows.

## Installs in WSL 2 (Ubuntu)

From the spec; not yet run on a clean distribution:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev ffmpeg tesseract-ocr poppler-utils python3 git unzip
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install sqlx-cli --no-default-features --features postgres,rustls
cargo install cargo-deny
```

Terraform, the AWS CLI v2, the Session Manager plugin and the GitHub CLI come from their own apt repositories or installers. The toolchain version comes from `rust-toolchain.toml`.

## Where files live

| What | WSL 2 | Native Windows |
| --- | --- | --- |
| Repository | `~/code/wheel-turning`, inside the Linux file system, not under `/mnt/c` | `C:\Users\<you>\Code\wheel-turning`, for the transcription worker |
| Local configuration | `.env` in the clone, copied from `.env.example` | `.env` in the native clone |
| Scratch audio | `./data/scratch` (the default) | `.\data\scratch` (the default) |
| whisper.cpp builds and models for the worker | Not used | Outside git. The benchmark used `.bench\`, excluded through `.git\info\exclude`. |
| Inbox for drafts and benchmark audio | `data-inbox/`, ignored by git | `data-inbox\`, ignored by git |

`./data/`, `data-inbox/`, `.env` and audio files are all ignored by git.

## Commands that differ

| Task | WSL 2 or Linux | Native Windows |
| --- | --- | --- |
| Load the MSVC compiler environment | Not needed | Run `vcvars64.bat` from Build Tools. To use it from PowerShell, run it through `cmd /c "… && set"` and import the variables, as `.bench\build.ps1` does. |
| Build whisper.cpp with Vulkan | Not used | `cmake -S . -B build-vulkan -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_VULKAN=ON` and then `cmake --build build-vulkan` |
| Build whisper.cpp with OpenVINO | Not used | `-DWHISPER_OPENVINO=1 -DOpenVINO_DIR=<venv>\Lib\site-packages\openvino\cmake`. The runtime DLLs in `openvino\libs` must be on `PATH` at run time. |
| Vulkan on the Arc 140V | Not used | Set `GGML_VK_DISABLE_COOPMAT=1`. With default settings Vulkan produces garbage or crashes on this GPU. See `docs/asr-benchmark.md`. |
| Python output with emoji (the ONNX exporter) | Works | Set `PYTHONUTF8=1`, or the console's cp1252 encoding fails |
| `scripts/check-deny-bans.sh` | `bash scripts/check-deny-bans.sh` | Run it in WSL 2 or Git Bash |
| Paths in docs and manifests | Forward slashes | Forward slashes work in PowerShell and the tools used here |

## Worker setup and the port forward

The worker on the PC reaches the database through an SSM port forward via the batch host (Spec 01). `scripts/worker-setup.ps1` sets it up from PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\worker-setup.ps1
```

1. **Tools.** It stops with an install command if the Session Manager plugin or the AWS CLI is missing. `-CheckOnly` runs only this step.
2. **Sign-in.** It stops if the `wheel-turning` profile is not signed in. Run `aws sso login --profile wheel-turning` first.
3. **Batch host and database.** It finds the running instance tagged `dg-v1-batch` and the `dg-v1` database. It stops if the batch host is not running.
4. **Forward.** It opens the forward in its own PowerShell window, from `localhost:5433` to the database's port 5432. Port 5432 stays free for the dev database in Docker; `-LocalPort` picks another.
5. **Connection string.** It prints the `DATABASE_URL` for `.env`, with `sslmode=require`, because the database requires TLS. The password is the secret `dg-v1/db_dg_pipeline_login`, which the script never reads.

The command in the forward window:

```powershell
aws ssm start-session --profile wheel-turning --region us-east-2 --target <batch host instance id> --document-name AWS-StartPortForwardingSessionToRemoteHost --parameters host=<database address>,portNumber=5432,localPortNumber=5433
```

Close that window to end the forward, then stop the batch host (Spec 07 runbook).

Status: the tool checks and the window launch were tested on October 4, 2026. The forward has not been run against AWS.

## Open points

- **Clone layout.** Spec 01 puts the clone inside WSL 2, and Spec 04 builds the worker natively, so the owner keeps two clones. Spec 01 does not yet say how the two share `.env` and models.
- **Port forward.** `scripts/worker-setup.ps1` is not yet run against AWS. Mark it tested once a forward has opened and the worker has connected.
