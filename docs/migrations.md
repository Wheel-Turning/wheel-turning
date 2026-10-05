# Migrations

Migrations live in `migrations/` as sqlx files named `NNNN_description.sql`, are forward-only, and are never edited after merge (Spec 02).

## Rules

1. **Numbers follow merge order.** A migration gets the next free number when its pull request is ready to merge, not the number a spec suggests. Specs refer to migrations by description; where a spec names a number, this ledger is what counts.
2. **Grants live with the object.** The migration that creates a table, view or function also grants on it. A grants-only migration can run before an object it names exists, as Spec 08's planned texts-grants file would before the proofread table.
3. **Record every migration here** in the same pull request that adds it.
4. **If two open branches pick the same number**, the second to merge renumbers its file before merging. A migration is never renumbered after merge, because sqlx records versions in `_sqlx_migrations`.

## Ledger

| Number | File | Merged in | Creates | Grants it holds |
|---|---|---|---|---|
| 0001 | `0001_extensions_enums.sql` | M0 (#1) | extensions `vector`, `pg_trgm`; enums `consent_status`, `source_platform`, `talk_type`, `talk_state`, `segment_kind`, `confidence`, `job_state`, `rights_basis` | none |
| 0002 | `0002_registry.sql` | M0 (#1) | `sangha`, `teacher`, `teacher_alias`, `speaker_consent`, view `current_consent`, `license_evidence`, `glossary_term` | none (in 0008) |
| 0003 | `0003_source_talk_transcript.sql` | M0 (#1) | `source_video`, `talk` with `past_gate_requires_consent`, `transcript` | none (in 0008) |
| 0004 | `0004_segment_passage_citation.sql` | M0 (#1) | `segment`, `passage` with the HNSW index, `work`, `work_locator`, `citation` | none (in 0008) |
| 0005 | `0005_operational.sql` | M0 (#1) | `job`, `tester`, `query_log`, `answer`, `answer_passage`, `feedback`, `dana_event`, `correction_log`, `takedown_log`, `eval_*`, `claim_audit`, views `v_answer_stats`, `v_consent_coverage`, `v_dana_clicks`, function `flagged_queries()` | revoke on `flagged_queries()` from public (its grant to `dg_analyst` is in 0008) |
| 0006 | `0006_serving_views.sql` | M0 (#1) | views `servable_passage`, `servable_citation` | none (in 0008) |
| 0007 | `0007_triggers.sql` | M0 (#1) | `set_updated_at()` and its triggers; append-only guards on `speaker_consent`, `correction_log`, `takedown_log` | none |
| 0008 | `0008_roles_grants.sql` | M0 (#1) | roles `dg_owner`, `dg_pipeline`, `dg_web`, `dg_analyst`; ownership of every object to `dg_owner` | all grants for objects in 0002 to 0006 |
| 0009 | `0009_owner_runs_migrations.sql` | M1 (#2) | none | `usage, create` on schema `public` and table rights on `_sqlx_migrations` to `dg_owner` |

0001 to 0008 predate rule 2: Spec 02 put every grant in 0008, after the roles exist. They are applied and stay as they are. From 0009 on, rule 2 applies.

## Planned, not yet numbered

These come from the October 3 to 5 specs. Each gets its number at merge.

| Spec | Spec's number | Description | Notes |
|---|---|---|---|
| 08 | 0009 | texts and reference objects | Spec 08 also adds `query_log` columns to it |
| 08 | 0010 | texts grants | Under rule 2 these grants move into the migrations that create each object, so this file is not needed |
| 08 | 0011 | OCR proof | |
| 08 | 0012 | URL ingestion | |
| 08 | 0013 | mandate rights | |
| 08 | 0014 | rights ledger | |
| 03 | 0015 | `mandate` value on `rights_basis` | Must be its own migration: a new enum value cannot be used in the transaction that adds it |
| 03 | 0016 | mandated talks: `rights_setting`, opt-out, new `past_gate_requires_consent` and `servable_passage` | Depends on objects from Spec 08's migrations, so it merges after them |
| 06 | 0017 | `accessibility_report` table | New table |
| 08 | 0018 | retention and legal text: `text_edition.legal_text`, `legal_pages`, `source_deleted_at`, `source_policy.source_retention` | Alters Spec 08's `text_edition` and `source_policy`, so it merges after the migrations that create them |
| 06 | 0019 | privacy modes: `tester.privacy_mode`, `privacy_chosen`, nullable `query_log.query_text`, `query_log.privacy_mode`, `text_expires_at` and its index | Alters tables from 0005 |
| 04 | 0020 | `llm_usage` table and its monthly index | New table |
| 02 | 0021 | donation links: `sangha.donation_kind`, `donation_note`, `donation_checked_at`, `donation_checked_by` and the `donation_verified_needs_page` check | Alters `sangha` from 0002 |
| 05 | 0022 | `query_log.sensitive_category` | Alters `query_log` from 0005. Storing an acute question without its text also needs 0019, which makes `query_text` nullable, so it merges after 0019 |
