<!-- Exported from the "Dharma Gates: V1 Technical Specs" doc (https://claude.ai/code/artifact/68e63636-e0f6-43ee-ab79-ccc03b76946d), tab "01 Platform", on 2026-10-02. The doc is the source of truth until this file is edited in the repo. -->
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
| `WT_BIND` | `127.0.0.1:8080` | Web listen address |
| `WT_SESSION_SECRET` | none, required in non-dev | Key that signs session cookies |
| `WT_SCRATCH_DIR` | `/var/wt/scratch` | The only place audio or video may exist |
| `WT_TRANSCRIPT_BUCKET` | none | S3 bucket for transcript JSON and the glossary. Unset means a local directory under `./data`. |
| `WT_MODEL_DIR` | `./models` | Directory holding the exported ONNX models and tokenizers |
| `WT_EMBED_MODEL` | `bge-m3-int8` | Embedding model directory name inside `WT_MODEL_DIR` |
| `WT_RERANK_MODEL` | `bge-reranker-v2-m3-int8` | Reranker model directory name |
| `WT_LLM_MODEL` | none, required for LLM stages | Model identifier sent to the Claude API |
| `ANTHROPIC_API_KEY` | none | API credential, secret |
| `WT_LLM_REAL_DATA_OK` | `false` | Must be `true` before any call carries real transcript text |
| `YOUTUBE_API_KEY` | none | Key for the YouTube Data API, secret |
| `WT_ALLOW_THIRD_PARTY_FETCH` | `false` | Enables the `yt-dlp` fetch mode. See Spec 03. |
| `WT_DIARIZE_URL` | `http://127.0.0.1:8090` | Base URL of the diarization sidecar |
| `WT_WHISPER_MODEL` | none | Path to the whisper large-v3 model file for ASR |
| `WT_SITE_DANA_URL` | none | The site's own donation page, shown separately in the dana panel |
| `WT_LOG` | `info` | Log filter. Logs are JSON in non-dev environments. |

## AWS topology

The v1 footprint is small on purpose. One web service, one database and three buckets carry everything, and a GPU host exists only while a batch is running.

> Diagram: AWS topology for v1 · 8 resources, 2 audio locations (see the "01 Platform" tab in the source doc)

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
- [ ] Setting `batch_host_enabled` to false destroys the batch host and true recreates it.
- [x] `deploy.yml` ships a tagged image and runs migrations as a one-off task.
