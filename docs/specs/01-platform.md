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
      POSTGRES_USER: dg
      POSTGRES_PASSWORD: dg
      POSTGRES_DB: dg
    ports: ["5432:5432"]
    volumes: ["dgdata:/var/lib/postgresql/data"]
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U dg"]
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

1. `cp .env.example .env` and set `DATABASE_URL=postgres://dg:dg@localhost:5432/dg`.
2. `docker compose up -d db`.
3. `cargo run -p dg-cli -- db migrate`.
4. `cargo test --workspace`. Postgres tests start their own container through `testcontainers` and do not touch the dev database.
5. `cargo run -p dg-api` serves on `DG_BIND`, default `127.0.0.1:8080`.

The `.env.example` file lists every key in the configuration reference with a safe local value and no real secrets.

## Configuration reference

Configuration comes from environment variables, with tunable parameters in two TOML files: `config/pipeline.toml` (Specs 03 and 04) and `config/retrieval.toml` (Spec 05). The loader in `dg-core` validates everything at startup, fails fast on a missing required key, and never logs secret values. Secrets come only from the environment or AWS Secrets Manager, never from files in the repository.

| Key | Default | Meaning |
| --- | --- | --- |
| `DG_ENV` | `dev` | One of `dev`, `staging`, `prod`. In `prod` the loader refuses debug flags. |
| `DATABASE_URL` | none, required | Postgres connection string for the application role |
| `DATABASE_URL_ANALYST` | none | Connection for the analyst role, used only by `dg report` |
| `DG_BIND` | `127.0.0.1:8080` | Web listen address |
| `DG_SESSION_SECRET` | none, required in non-dev | Key that signs session cookies |
| `DG_SCRATCH_DIR` | `/var/dg/scratch` | The only place audio or video may exist |
| `DG_TRANSCRIPT_BUCKET` | none | S3 bucket for transcript JSON and the glossary. Unset means a local directory under `./data`. |
| `DG_MODEL_DIR` | `./models` | Directory holding the exported ONNX models and tokenizers |
| `DG_EMBED_MODEL` | `bge-m3-int8` | Embedding model directory name inside `DG_MODEL_DIR` |
| `DG_RERANK_MODEL` | `bge-reranker-v2-m3-int8` | Reranker model directory name |
| `DG_LLM_MODEL` | none, required for LLM stages | Model identifier sent to the Claude API |
| `ANTHROPIC_API_KEY` | none | API credential, secret |
| `DG_LLM_REAL_DATA_OK` | `false` | Must be `true` before any call carries real transcript text |
| `YOUTUBE_API_KEY` | none | Key for the YouTube Data API, secret |
| `DG_ALLOW_THIRD_PARTY_FETCH` | `false` | Enables the `yt-dlp` fetch mode. See Spec 03. |
| `DG_DIARIZE_URL` | `http://127.0.0.1:8090` | Base URL of the diarization sidecar |
| `DG_WHISPER_MODEL` | none | Path to the whisper large-v3 model file for ASR |
| `DG_SITE_DANA_URL` | none | The site's own donation page, shown separately in the dana panel |
| `DG_LOG` | `info` | Log filter. Logs are JSON in non-dev environments. |

## AWS topology

The v1 footprint is small on purpose. One web service, one database and three buckets carry everything, and a GPU host exists only while a batch is running.

&#91;embedded content: AWS topology for v1 · 8 resources, 2 audio locations\]

The tester reaches the web service through the load balancer. The web service reads one database view and writes query logs. The batch host fetches media, transcribes, diarizes and writes text, and deletes its audio when each stage ends. Secrets Manager holds the database credentials, the Claude and YouTube keys and the session secret, and both compute roles read them at start. CloudWatch Logs receives structured logs from both services.

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
| `observability` | CloudWatch log groups, alarms | 30-day log retention. Alarms on load balancer 5xx rate, database CPU and free storage, and on a nonempty scratch bucket after 24 hours. |

## Secrets, IAM and network rules

1. The web task role reads the models bucket and the database, Anthropic and session secrets. It cannot write to the transcripts or scratch buckets.
2. The batch host role reads and writes the transcripts and scratch buckets, reads the models bucket, and reads the database, Anthropic and YouTube secrets.
3. Database access uses four roles. `dg_owner` runs migrations and is used only from CI and a developer machine. `dg_pipeline` writes pipeline tables and is used by the batch host. `dg_web` reads the serving view and writes logs and is used by the web service. `dg_analyst` reads aggregate views only. Spec 02 defines the grants.
4. The database security group accepts port 5432 only from the web and batch security groups. The web security group accepts traffic only from the load balancer. The batch host accepts no inbound traffic.
5. Connections to Postgres require TLS outside `dev`, with `sslmode=require` in `DATABASE_URL`.
6. No static AWS keys exist anywhere. Compute uses roles. CI uses OIDC.
7. The batch host is stopped when idle, since it is the one costly resource.

## CI and deployment

Three GitHub Actions workflows cover the lifecycle.

1. `ci.yml` runs on every pull request and push. Jobs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` with Docker available for Postgres containers, `cargo sqlx prepare --check` against a migrated database, `cargo deny check`, Terraform `fmt -check` and `validate`, and `ruff` on the Python sidecar and scripts. Cargo and target directories are cached.
2. `deploy.yml` runs on a version tag or by hand. It builds the container image, pushes it to ECR, runs `terraform plan` and holds for manual approval, applies, then runs migrations as a one-off ECS task under `dg_owner` before the new service revision starts taking traffic.
3. `nightly.yml` runs `dg audit` as a one-off task against the live database and fails loudly if any audit check fails. Spec 07 lists the checks.

The `Dockerfile` is multi-stage. The final image holds the `dg` binary, the ONNX Runtime shared library located through `ORT_DYLIB_PATH`, and nothing else. Models are not baked into the image. The service downloads them from the models bucket at start into ephemeral storage and refuses to serve until they load.

## Acceptance criteria

M0, foundation

- [ ] Every crate in the layout exists, builds, and has a short crate-level doc comment stating its single responsibility.
- [ ] `ci.yml` is green with all jobs listed above.
- [ ] `dg db migrate` is idempotent. Running it twice changes nothing.
- [ ] Config loader tests cover a missing required key failing at startup and secret values never appearing in `Debug` or log output.
- [ ] `dg-api` serves `/healthz` with database connectivity checked.
- [ ] `deny.toml` bans payment SDKs, including `async-stripe`, and a test dependency on a banned crate fails CI.

M1, AWS baseline

- [ ] `terraform apply` from an empty account creates the stack and `terraform destroy` removes it cleanly.
- [ ] `/healthz` answers over HTTPS through the load balancer.
- [ ] A test proves the database accepts connections from the web and batch security groups and from nothing else.
- [ ] A check script reads the scratch bucket lifecycle configuration and asserts a one-day expiry rule exists.
- [ ] Setting `batch_host_enabled` to false destroys the batch host and true recreates it, run with `batch_host_instance_type = "t3.small"` (see GPU access).
- ~~GPU quota granted for one `g5.xlarge`.~~ Dropped on October 3, 2026: AWS declined the GPU quota request, and v1 transcribes on the owner's Mac, so M1 does not need it (see GPU access).
- [ ] `deploy.yml` ships a tagged image and runs migrations as a one-off task.

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
- Transcription for v1 runs on the owner's Apple Silicon Mac through whisper.cpp with Metal. Spec 04 sets the model.
- Appealing is optional. AWS often declines GPU quota on accounts with little billing history. Reapply after a few weeks of normal use, ask for 4 vCPUs, which is one `g5.xlarge` or `g4dn.xlarge`, name the use, and say the instance runs on demand and stops when idle.
- If the Mac proves too slow, rent a GPU virtual machine by the hour from a provider, run `wt worker --kinds audio` on it, and delete it afterward, so audio sits only on a machine the owner controls. Hosted transcription services are not used, because they would send audio of talks that may not have consented to a third party.
