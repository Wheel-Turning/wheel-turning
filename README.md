# Wheel Turning

A private, text-only retrieval demo over Zen talks. A tester asks a situational question and gets grouped verbatim passages, a verified synthesis line, source links that land on the right moment, and a dana panel, drawn only from talks used with a recorded rights basis: the teacher's permission, a license, or the owner's mandate unless the teacher opts out.

- Specs: [`docs/specs/`](docs/specs/), starting with [`00-overview.md`](docs/specs/00-overview.md)
- Conventions for contributors and Claude Code: [`CLAUDE.md`](CLAUDE.md)

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
