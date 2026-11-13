# Wheel Turning

A private, text-only retrieval demo over Zen talks. A tester asks a situational question and gets grouped verbatim passages, a verified synthesis line, source links that land on the right moment, and a dana panel, drawn only from talks used with a recorded rights basis: the teacher's permission, a license, or the owner's mandate unless the teacher opts out.

- Specs: [`docs/specs/`](docs/specs/), starting with [`00-overview.md`](docs/specs/00-overview.md)
- Architecture diagrams and a reading guide: [`docs/architecture.md`](docs/architecture.md)
- Conventions for contributors and Claude Code: [`CLAUDE.md`](CLAUDE.md)

## Status: shut down (November 13, 2026)

The site is offline. Every AWS resource was deleted to stop billing, and the AWS account is being closed. The deploy and nightly audit workflows are disabled. The database, transcripts, rights-ledger records and backups were downloaded before deletion and are kept offline by the owner; nothing in this repository points to them.

To run it again, create a new AWS account, apply `infra/terraform/bootstrap`, then the other stacks as `docs/runbook.md` describes, after filling in the placeholders below.

### Placeholders

Account-specific values were removed from the files and replaced with these markers:

| Placeholder | What goes there | Where it appears |
|---|---|---|
| `<AWS_ACCOUNT_ID>` | The 12-digit ID of the AWS account that hosts the stack. Bucket names (`wt-tfstate-<AWS_ACCOUNT_ID>`, `wt-v1-transcripts-<AWS_ACCOUNT_ID>` and the rest), the ECR registry and the CI deploy role ARN are built from it | Terraform backends and variables, `.github/workflows/deploy.yml`, the runbook, cost and spec docs |
| `<OWNER_ALERT_EMAIL>` | The address that receives budget, cost anomaly and CloudWatch alarm emails (`alert_email` in `infra/terraform/account` and `infra/terraform/envs/v1`) | Terraform variables, `docs/cost.md` |
| `<ANOMALY_MONITOR_ID>` | The ID of the account's AWS-created `Default-Services-Monitor` in Cost Explorer, which the account stack subscribes to | `infra/terraform/account/terraform.tfvars`, `docs/cost.md` |
| `<WA_WORKLOAD_ID>` | The AWS Well-Architected Tool workload ID that `scripts/ops/wa_tool_import.py` loads the ledger into | `docs/well-architected/README.md` |
| `<OWNER_NAME>` | The workload owner's name recorded in the Well-Architected Tool | `scripts/ops/wa_tool_import.py` |

No credentials were ever committed: secrets lived in AWS Secrets Manager and a local, git-ignored `.env`, with every key listed in `.env.example`.

## Local development

Requirements: Rust (version pinned in `rust-toolchain.toml`), Docker with Compose, `sqlx-cli`, `cargo-deny`, Terraform 1.10 or later, the AWS CLI, and Python 3.11 with `uv`.

```bash
cp .env.example .env
docker compose up -d db
cargo run -p wt-cli -- db migrate
cargo test --workspace
cargo run -p wt-api
```

`wt-api` serves on `WT_BIND`, default `127.0.0.1:8080`. `GET /healthz` checks database connectivity.
