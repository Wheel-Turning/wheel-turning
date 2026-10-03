# Wheel Turning

A private, text-only retrieval demo over consented Zen talks. A tester asks a situational question and gets grouped verbatim passages, a verified synthesis line, source links that land on the right moment, and a dana panel, drawn only from talks whose speakers have consented.

- Specs: [`docs/specs/`](docs/specs/), starting with [`00-overview.md`](docs/specs/00-overview.md)
- Conventions for contributors and Claude Code: [`CLAUDE.md`](CLAUDE.md)

## Local development

Requirements: Rust (version pinned in `rust-toolchain.toml`), Docker with Compose, `sqlx-cli`, `cargo-deny`, Terraform 1.10 or later, the AWS CLI, and Python 3.11 with `uv`.

```bash
cp .env.example .env
docker compose up -d db
cargo run -p dg-cli -- db migrate
cargo test --workspace
cargo run -p dg-api
```

`dg-api` serves on `DG_BIND`, default `127.0.0.1:8080`. `GET /healthz` checks database connectivity.
