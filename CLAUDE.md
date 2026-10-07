# Wheel Turning

A private, text-only retrieval demo over Zen talks used with a recorded rights basis: the teacher's permission, a license, or the owner's mandate unless the teacher opts out. The specs in `docs/specs/` are binding: read `00-overview.md` first, then the spec for the milestone in hand.

## Rules

1. Read the matching spec tab and the PRD section before writing code. If the two conflict, stop and ask. Do not resolve it silently.
2. No `unwrap` or `expect` outside tests. Library crates use `thiserror`, binaries use `anyhow`.
3. Serving code reads passages only through the `servable_passage` view. A CI test greps for direct reads of the `passage` table outside `wt-index`, the takedown routine and `wt audit`, which looks for passages that must not be served (decision of October 7, 2026; `wt-core/tests/repo_guards.rs`).
4. The ranker input type has no field for sangha, profile status, upload volume or donation link. Do not add one.
5. A language model never produces text that is displayed as a quotation. It returns passage IDs and JSON only.
6. Audio exists only under the configured scratch directory, and the pipeline deletes it explicitly on success and failure. No schema column or bucket holds audio.
7. No payment or card-handling crate may enter the dependency tree. `cargo deny` enforces this.
8. Every pipeline stage is idempotent and keyed by content hash. Rerunning a stage on unchanged input is a no-op.
9. Tests make no network calls. External services sit behind traits and tests use fakes. Postgres tests use a real container.
10. Fixtures are synthetic text. Real transcripts, audio and credentials are never committed.
11. A milestone ends with CI green and its acceptance items ticked in the spec file.

## Checks

CI and every milestone run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo sqlx prepare --workspace --check
cargo deny check
```

`cargo test` needs Docker running: database tests start their own `pgvector/pgvector:pg16` container through `testcontainers` and never touch the dev database.

## Working notes

- Migrations in `migrations/` are forward-only and never edited after merge. Add a new numbered file instead.
- Compile-time checked queries use `sqlx::query!`. After changing one, run `cargo sqlx prepare --workspace` against a migrated database and commit `.sqlx/`.
- Configuration keys are listed in `.env.example`. `wt_core::config` loads each key once its milestone wires it in, and the diarization sidecar reads `HF_TOKEN`. Secrets are wrapped in `wt_core::config::Secret`, which never prints its value.
