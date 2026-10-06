# Spec 01: Platform and infrastructure

Implements milestones M0 and M1. This spec covers the local development stack, configuration, the AWS footprint in Terraform, and CI. The data model lives in Spec 02 and every pipeline stage in Specs 03 and 04.

## Local development environment

Required tools: current stable Rust (pinned in `rust-toolchain.toml`), Docker with Compose, `sqlx-cli` (Postgres and rustls features), `cargo-deny`, Terraform 1.6 or later, the AWS CLI, and Python 3.11 with `uv` for the sidecar and scripts.

The local database is Postgres 16 with pgvector. The compose file below is the contract for `docker-compose.yml`.

```yaml
services:
  db:
    image: pgvector/pgvector:pg16
    environment:
      POSTGRES_USER: wt
      POSTGRES_PASSWORD: wt
      POSTGRES_DB: wt
    ports: ["5432:5432"]
    volumes: ["dgdata:/var/lib/postgresql/data"]
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U wt"]
      interval: 5s
      retries: 10
  diarize:
    build: ./sidecars/diarize
    profiles: ["sidecar"]
    ports: ["8090:8090"]
volumes:
  dgdata: {}
```

First run, in order:

1. `cp .env.example .env` and set `DATABASE_URL=postgres://wt:wt@localhost:5432/wt`.
2. `docker compose up -d db`.
3. `cargo run -p wt-cli -- db migrate`.
4. `cargo test --workspace`. Postgres tests start their own container through `testcontainers` and do not touch the dev database.
5. `cargo run -p wt-api` serves on `WT_BIND`, default `127.0.0.1:8080`.

The `.env.example` file lists every key in the configuration reference with a safe local value and no real secrets.

## Configuration reference

Configuration comes from environment variables, with tunable parameters in two TOML files: `config/pipeline.toml` (Specs 03 and 04) and `config/retrieval.toml` (Spec 05). The loader in `wt-core` validates everything at startup, fails fast on a missing required key, and never logs secret values. Secrets come only from the environment or AWS Secrets Manager, never from files in the repository.

| Key | Default | Meaning |
| --- | --- | --- |
| `WT_ENV` | `dev` | One of `dev`, `staging`, `prod`. In `prod` the loader refuses debug flags. |
| `DATABASE_URL` | none, required | Postgres connection string for the application role |
| `DATABASE_URL_ANALYST` | none | Connection for the analyst role, used only by `wt report` |
| `WT_DB_HOST` | none | Database host. When DATABASE\_URL is unset and this is set, the loader builds the URL from the WT\_DB\_\* keys, so the password never sits in a stored connection string. |
| `WT_DB_PORT` | `5432` | Database port, used with WT\_DB\_HOST |
| `WT_DB_NAME` | `wt` | Database name, used with WT\_DB\_HOST |
| `WT_DB_USER` | none, required with WT\_DB\_HOST | Database login |
| `WT_DB_PASSWORD` | none, required with WT\_DB\_HOST | Database password, secret, injected from Secrets Manager |
| `WT_BIND` | `127.0.0.1:8080` | Web listen address |
| `WT_SESSION_SECRET` | none, required in non-dev | Key that signs session cookies |
| `WT_SCRATCH_DIR` | `./data/scratch` | The only place audio or video may exist |
| `WT_TRANSCRIPT_BUCKET` | none | S3 bucket for transcript JSON and the glossary. Unset means a local directory under `./data`. |
| `WT_MODEL_DIR` | `./models` | Directory holding the exported ONNX models and tokenizers |
| `WT_EMBED_MODEL` | `bge-m3-int8` | Embedding model directory name inside `WT_MODEL_DIR` |
| `WT_MODEL_LICENSES` | `docs/model-licenses.md` | Model license record; a stage refuses a model it does not list with a revision (Spec 04) |
| `WT_ORT_DYLIB` | unset | ONNX Runtime library, 1.22 or newer, loaded at run time (unset: `ORT_DYLIB_PATH` or the search path) |
| `WT_RERANK_MODEL` | `bge-reranker-v2-m3-int8` | Reranker model directory name |
| `WT_LLM_MODEL` | none, required for LLM stages | Model identifier sent to the Claude API |
| `WT_LLM_MODEL_EXTRACT` | `WT_LLM_MODEL` | Model for extraction. See Spec 04. |
| `WT_LLM_MODEL_SYNTH` | `WT_LLM_MODEL` | Model for synthesis. See Spec 05. |
| `WT_LLM_MODEL_VERIFY` | `WT_LLM_MODEL` | Model for the synthesis verifier. See Spec 05. |
| `WT_LLM_MODEL_VISION` | `WT_LLM_MODEL` | Model for the vision re-read of poorly read OCR pages. See Spec 08. |
| `WT_LLM_MONTHLY_CAP_USD` | `25` | Monthly spending cap for language model calls. See Spec 04. |
| `ANTHROPIC_API_KEY` | none | API credential, secret |
| `WT_LLM_REAL_DATA_OK` | `false` | Must be `true` before any call carries real transcript text |
| `YOUTUBE_API_KEY` | none | Key for the YouTube Data API, secret |
| `YOUTUBE_OAUTH_REFRESH_TOKEN` | none | OAuth refresh token for caption download in owner\_api mode, secret, kept in Secrets Manager. See Spec 03. |
| `WT_VIMEO_ACCESS_TOKEN` | none | Vimeo API token, secret. Unset means discovery reads the public talks page. See Spec 03. |
| `WT_ALLOW_THIRD_PARTY_FETCH` | `false` | Enables the `yt-dlp` fetch mode. See Spec 03. |
| `WT_DIARIZE_URL` | `http://127.0.0.1:8090` | Base URL of the diarization sidecar |
| `HF_TOKEN` | none | Hugging Face access token for the pyannote model, secret. Read by the diarization sidecar, not by the loader. See Spec 04. |
| `WT_WHISPER_MODEL` | none | Path to the whisper large-v3 model file for ASR |
| `WT_WHISPER_CLI` | none | Path to whisper.cpp's `whisper-cli`, built for the worker's hardware, which the worker runs as a subprocess (Spec 04, decision of October 5, 2026) |
| `GGML_VK_DISABLE_COOPMAT` | none | Set to 1 on the transcription worker. Read by whisper.cpp's Vulkan backend, not by the loader. Without it Vulkan produces garbage on the Arc 140V. See Spec 04. |
| `WT_SITE_DANA_URL` | none | The site's own donation page, shown separately in the dana panel |
| `WT_ALERT_TOPIC_ARN` | none | SNS topic for alerts to the owner. Empty turns alerts off. See Spec 08. |
| `WT_PRIVACY_DEFAULT` | `share` | Question-keeping mode for new testers, share or count\_only. Set to count\_only before any public launch. See Spec 06. |
| `WT_QUERY_RETENTION_DAYS` | `180` | Days a kept question's text is stored. See Spec 06. |
| `WT_COUNT_ONLY_TTL_HOURS` | `24` | Hours a question is kept in count-only mode before it is deleted. See Spec 06. |
| `WT_LOG` | `info` | Log filter. Logs are JSON in non-dev environments. |

## AWS topology

The v1 footprint is small on purpose. One web service, one database and three buckets carry everything, and a small batch host exists only while the owner's PC is transcribing, as the PC's route into the database.

A tester's browser, arriving by invite link, reaches the application load balancer over HTTPS with a certificate from ACM, which forwards to the web service on Fargate. The web service runs Axum, the templates and ONNX embedding and reranking; it reads one database view, writes query logs, loads its ONNX files from the S3 models bucket at service start, and calls the Claude API. RDS Postgres 16 with pgvector is the system of record for all text, in a private subnet with no public access. The batch host runs on demand as a small CPU instance reached through SSM and forwards the PC's database connection. The owner's Windows PC runs the wt worker, whisper.cpp and diarization with the owner's SSO profile. It fetches from the video sources (the YouTube Data API and SSZ storage), reaches RDS through an SSM port forward via the batch host, writes transcript JSON and the glossary to the S3 transcripts bucket, which holds no audio, and stages media in the S3 scratch bucket, which has a one-day expiry. The Claude API is called by the web service and the pipeline. Audio can exist in two places only: the owner's PC and the scratch bucket. The pipeline deletes it, and the bucket expires it in a day.

The tester reaches the web service through the load balancer. The web service reads one database view and writes query logs. The transcription worker runs on the owner's Windows PC. It fetches media, transcribes, diarizes and writes text, and deletes its audio when each stage ends. It reaches the database through an SSM port forward via the batch host and writes to the transcripts bucket, both with the owner's SSO profile. Secrets Manager holds the database credentials, the Claude and YouTube keys and the session secret, and both compute roles read them at start. CloudWatch Logs receives structured logs from both services.

## Terraform modules

Terraform lives in `infra/terraform/`, one module per row below, composed by a single environment named `v1`. Remote state sits in an S3 bucket created by a small bootstrap stack. Sizes are defaults to tune after measurement, not commitments.

| Module | Resources | Key settings |
| --- | --- | --- |
| `network` | VPC over two availability zones, public and private subnets, S3 gateway endpoint | No NAT gateway in v1. Compute runs in public subnets with security groups that allow no inbound traffic except the load balancer to the web service. The database sits in private subnets. |
| `database` | RDS Postgres 16, parameter group, subnet group | `db.t4g.medium`, 50 GB gp3, encrypted, 7-day backups, `rds.force_ssl=1`, no public access. The `vector` extension is created by migration. Master credentials live in Secrets Manager. |
| `storage` | Three S3 buckets: transcripts, scratch, models | All block public access and deny non-TLS requests. Scratch expires objects after one day and aborts multipart uploads after one day. Transcripts has versioning off. Models has versioning on. |
| `service` | ECR repository, ECS cluster, Fargate service, load balancer, ACM certificate, HTTPS listener | One task, 2 vCPU and 8 GB to start, health check on `/healthz`, HTTP redirects to HTTPS. A `domain_name` variable wires DNS when set. |
| `batch_host` | EC2 launch template and instance, instance profile | `g5.xlarge` by default with an NVIDIA-ready image, 200 GB gp3 scratch volume, access through SSM only. The variable `batch_host_enabled` creates or destroys it. |
| `secrets` | Secrets Manager entries for database, Anthropic key, YouTube key, session secret | Values are set out of band and never appear in Terraform state as plaintext inputs |
| `iam` | Task execution role, web task role, batch host role, CI deploy role | Least privilege per the rules below. The CI role trusts GitHub OIDC and holds no long-lived keys. |
| `observability` | CloudWatch log groups, alarms, a Route 53 health check | 30-day log retention. Alarms on load balancer 5xx rate, database CPU and free storage, and on a nonempty scratch bucket after 24 hours. Two alarms fire with no traffic: no healthy web task behind the load balancer for 5 minutes, and a Route 53 HTTPS health check on `/healthz` failing for 3 minutes. Route 53 health check metrics exist only in us-east-1, so that alarm and a second alarm topic live there. |

## Secrets, IAM and network rules

1. The web task role reads the models bucket and the database, Anthropic and session secrets. It cannot write to the transcripts or scratch buckets.
2. The batch host role reads and writes the transcripts and scratch buckets, reads the models bucket, and reads the database, Anthropic and YouTube secrets.
3. Database access uses four roles. `dg_owner` runs migrations and is used only from CI and a developer machine. `dg_pipeline` writes pipeline tables and is used by the batch host and by the worker on the owner's PC through the port forward. `dg_web` reads the serving view and writes logs and is used by the web service. `dg_analyst` reads aggregate views only. Spec 02 defines the grants.
4. The database security group accepts port 5432 only from the web and batch security groups. The web security group accepts traffic only from the load balancer. The batch host accepts no inbound traffic. The owner's PC reaches the database through an SSM port forward via the batch host, so its connections arrive from the batch security group and need no new rule.
5. Connections to Postgres require TLS outside `dev`, with `sslmode=require` in `DATABASE_URL`.
6. No static AWS keys exist anywhere. Compute uses roles. CI uses OIDC. The transcription worker on the owner's PC uses the owner's SSO profile, which gives short-lived credentials.
7. The batch host is stopped when idle, since it is the one costly resource.

## CI and deployment

Three GitHub Actions workflows cover the lifecycle.

1. `ci.yml` runs on every pull request and every push to `main`. Jobs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` with Docker available for Postgres containers, `cargo sqlx prepare --check` against a migrated database, `cargo deny check`, `cargo machete` for unused dependencies, Terraform `fmt -check`, `validate` and `tflint`, `ruff` on the Python sidecar and scripts, `gitleaks` over the full history, `actionlint` and `zizmor` on the workflows, and `typos`. Cargo commands run with `--locked`, third-party actions are pinned to commit SHAs, and Cargo and target directories are cached. A final `ci ok` job passes only when every other job passes, and it is the one check the `main` ruleset requires.
2. `deploy.yml` runs on a version tag or by hand. It builds the container image, pushes it to ECR, runs `terraform plan` and holds for manual approval, applies, then runs migrations as a one-off ECS task under `dg_owner` before the new service revision starts taking traffic. After the apply it smoke-tests the deploy: the service's primary deployment completed and runs the new image tag (the circuit breaker can roll back silently), `/healthz` reports ok over HTTPS, plain HTTP redirects to HTTPS, and an unknown path returns 404.
3. `nightly.yml` runs `wt audit` as a one-off task against the live database and fails loudly if any audit check fails. Spec 07 lists the checks.

The `Dockerfile` is multi-stage. The final image holds the `wt` binary, the ONNX Runtime shared library located through `ORT_DYLIB_PATH`, and nothing else. Models are not baked into the image. The service downloads them from the models bucket at start into ephemeral storage and refuses to serve until they load.

## Acceptance criteria

M0, foundation

- [x] Every crate in the layout exists, builds, and has a short crate-level doc comment stating its single responsibility.
- [x] `ci.yml` is green with all jobs listed above.
- [x] `wt db migrate` is idempotent. Running it twice changes nothing.
- [x] Config loader tests cover a missing required key failing at startup and secret values never appearing in `Debug` or log output.
- [x] `wt-api` serves `/healthz` with database connectivity checked.
- [x] `deny.toml` bans payment SDKs, including `async-stripe`, and a test dependency on a banned crate fails CI.

M1, AWS baseline

- [ ] `terraform apply` from an empty account creates the stack and `terraform destroy` removes it cleanly.
- [x] `/healthz` answers over HTTPS through the load balancer.
- [x] A test proves the database accepts connections from the web and batch security groups and from nothing else.
- [x] A check script reads the scratch bucket lifecycle configuration and asserts a one-day expiry rule exists.
- [x] Setting `batch_host_enabled` to false destroys the batch host and true recreates it, run with `batch_host_instance_type = "t3.small"` (see GPU access).
- [x] `deploy.yml` ships a tagged image and runs migrations as a one-off task.

Dropped from M1 on October 3, 2026: GPU quota for one `g5.xlarge`. AWS declined the request, and v1 transcribes on the owner's Windows PC, so M1 does not need it (see GPU access).

## Domain, naming and alerts

Decisions of October 3, 2026.

- Naming. The GitHub organization is `Wheel-Turning` and the repository is `Wheel-Turning/wheel-turning`. Crates are `wt-*`, the command is `wt`, environment variables start with `WT_`, and AWS resources are prefixed `wt-`. Wherever the specs say `dg`, `DG_` or `dharma-gates`, the first Claude Code pass renames them and lists every change for review.
- Domain. The project owns `wheelturning.org`. The Terraform variable `domain_name` is set to `demo.wheelturning.org` for v1, which leaves the apex free. The `service` module requests an ACM certificate with DNS validation. The owner adds the validation record at the registrar, or moves DNS to Route 53 and sets `route53_zone_id` so Terraform does it. The same module adds the record that points the name at the load balancer. `/rights`, `/accessibility` and `/robots.txt` are public on that name.
- Alerts. The `observability` module adds an SNS topic, an email subscription to the variable `alert_email`, and an `sns:Publish` permission on that topic for the web service and the batch host. The owner must click the confirmation link AWS emails to that address, or nothing arrives. The topic ARN goes to the code as `WT_ALERT_TOPIC_ARN`. Spec 08 lists what is published.
- Accounts. AWS, Anthropic, Google Cloud and Hugging Face accounts are opened in the owner's own name for now. Moving them to a nonprofit later means creating new accounts and migrating, which the runbook notes.

Acceptance criteria:

- [ ] `terraform plan` shows the certificate, the DNS validation record and the alias, and `https://demo.wheelturning.org/healthz` answers after apply.
- [ ] A test message from `wt alerts test` reaches the owner's address after the subscription is confirmed.
- [ ] The web role can publish to the alert topic and to nothing else in SNS.

## GPU access

Decision of October 3, 2026. AWS declined the GPU quota request, and v1 does not need it.

- The M1 acceptance item for the batch host is met with a CPU instance. The variable `batch_host_instance_type` defaults to `g5.xlarge`, and the toggle test runs with a small CPU type such as `t3.small`, which proves that the host is created and destroyed without any GPU quota. The GPU quota item is dropped from the M1 checklist with this reason recorded.
- Transcription for v1 runs on the owner's Windows PC through whisper.cpp with `large-v3` on the Arc GPU through Vulkan, run with `GGML_VK_DISABLE_COOPMAT=1`, which the owner chose on October 4, 2026 after measuring speed and accuracy (docs/asr-benchmark.md). Spec 04 sets the model. The worker connects to the database through an SSM port forward via the batch host, using the owner's SSO profile, and the batch host runs as a small CPU instance (t3.small) for the length of each run.
- Appealing is optional. AWS often declines GPU quota on accounts with little billing history. Reapply after a few weeks of normal use, ask for 4 vCPUs, which is one `g5.xlarge` or `g4dn.xlarge`, name the use, and say the instance runs on demand and stops when idle.
- If the PC proves too slow, rent a GPU virtual machine by the hour from a provider, run `wt worker --kinds audio` on it, and delete it afterward, so audio sits only on a machine the owner controls. Hosted transcription services are not used, because they would send audio of talks that may not have consented to a third party.

## Windows notes

The owner develops on Windows. The repository, its shell scripts and the Terraform and `aws` commands assume a Unix shell, so the recommended setup is WSL 2 with Ubuntu, with Docker Desktop's WSL 2 backend turned on, the repository cloned inside the Linux file system (under `~`, not under `/mnt/c`), and Claude Code started from that Ubuntu terminal. The transcription worker is the exception: it is built natively on Windows, because the PC has no NVIDIA card and Spec 04 runs whisper.cpp through Vulkan on the Intel GPU. Running everything natively in PowerShell also works but needs the Visual Studio C++ Build Tools for Rust, Git for Windows and CMake, and some scripts may need small changes. Wherever the repository lives, `.env` lives in its root, in the same file system that Claude Code runs in.

- Line endings. Windows converts line endings by default, which breaks shell scripts and changes the checksums sqlx records for migration files. The repository carries a `.gitattributes` with `* text=auto eol=lf` and `*.sql text eol=lf`, and `git config core.autocrlf false` is set on the owner's machine.
- Paths. Documentation and manifests use forward slashes, which work in PowerShell, WSL and the tools used here. The scratch directory defaults to `./data/scratch` for local work, which is valid on Windows and in WSL, and the deployed service sets it to a path on its own volume.
- Long paths. Turn on long path support in Windows and run `git config core.longpaths true`, because Rust build directories and node-style trees get deep.
- Installs. Use `winget` or Scoop on Windows, or `apt` in WSL 2, for `ffmpeg`, `tesseract` and `poppler`. There is no Homebrew on this machine.
- Memory. WSL 2 and Docker Desktop each take a share of RAM, and the owner's PC has 16 GB. A `.wslconfig` file limits WSL 2 memory, and heavy builds and transcription runs are done at separate times.

Acceptance criteria:

- [ ] A fresh clone inside WSL 2 passes the M0 commands in `CLAUDE.md` with no manual changes.
- [ ] `docs/windows.md` lists the installs, the file locations and the commands that differ, and is checked by running it on a clean machine or a clean WSL distribution.

More acceptance criteria for Windows:

- [ ] `.gitattributes` exists with the line-ending rules, and a clone on Windows produces the same migration checksums as a clone on Linux.
- [ ] `wt db migrate` and `cargo test --workspace` pass on a Windows clone with the settings above.
- [ ] A Windows clone with long paths on builds without path errors.
