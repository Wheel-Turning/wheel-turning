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
| 0010 | `0010_rename_roles_to_wt.sql` | v1 rebuild | none; renames `dg_owner`, `dg_pipeline`, `dg_web`, `dg_analyst` to `wt_owner`, `wt_pipeline`, `wt_web`, `wt_analyst`, and grants and ownership follow | none |
| 0011 | `0011_gate_requires_rights_basis.sql` | M2 (#23) | none; rewrites `past_gate_requires_consent` with `coalesce(..., false)` so a talk with no `rights_basis` cannot pass the gate | none |
| 0012 | `0012_servable_passage_without_barrier.sql` | M2 (#23) | none; resets `security_barrier` on `servable_passage` so vector search uses the HNSW index | none |
| 0013 | `0013_flag_query.sql` | M2 (#23) | function `flag_query(uuid)` | execute on `flag_query` to `wt_web`; revokes `update (flagged)` on `query_log` from `wt_web` |
| 0014 | `0014_vimeo_platform.sql` | M3 (#26) | `vimeo` value on `source_platform` | none |
| 0015 | `0015_rights_ledger.sql` | M3R | the rights parts of Spec 08's texts, URL-ingestion and mandate-rights migrations on `license_evidence` (nullable `sangha_id`, `publisher`, `kind` with `mandate`, `grantor`, `url_prefixes`, nullable `license_url` and `evidence_url`, `mandated_by`, `rationale`, `display_notice`, the `license_evidence_owner` and `permission_has_scope` checks); `source_policy`; the rights ledger `rights_event` (append-only), `permission_request`, `source_exclusion`, view `v_source_rights` | select, insert, update on `source_policy`, `permission_request`, `source_exclusion` and select, insert on `rights_event` to `wt_pipeline`; select on `v_source_rights` to both; select on `source_policy` and `v_source_rights` to `wt_web` |
| 0016 | `0016_mandate_basis.sql` | M3R | `mandate` value on `rights_basis` (its own migration: a new enum value cannot be used in the transaction that adds it) | none |
| 0017 | `0017_mandated_talks.sql` | M3R | `rights_setting` with `talks_basis = consent`; `source_policy.kind`; `talk.source_key`; `teacher.opted_out_at`, `opted_out_reason`; `teacher_id` on `rights_event` and `permission_request` with their target checks (global events only for the `talks_basis` switch); new `past_gate_requires_consent`; `servable_passage` with the mandate basis, opt-out and the switch, and `rights_basis` and `display_notice` columns | select on `rights_setting` to `wt_web` and `wt_pipeline`, update to `wt_pipeline` |
| 0018 | `0018_llm_usage.sql` | M4 | `llm_usage` with its month index; `llm_setting` (the owner's cap); `llm_alert` (one 80 percent alert a month) | select, insert on `llm_usage` and `llm_alert` to `wt_pipeline` and `wt_web`; select, insert, update on `llm_setting` to `wt_pipeline`, select to `wt_web` |
| 0019 | `0019_display_levels.sql` | M4T | `teacher.display_level` (default `full`), `talk.display_level` (a, b or d), `'reviewed'` on `transcript.origin`, `servable_passage` with `display_level` at the end and only `excerpts` and `full` talks | none: the view keeps its grants |
| 0020 | `0020_repair_dg_roles_on_shared_cluster.sql` | M4 | none; on a database migrated in a cluster that already held the `wt_*` roles, where 0008 recreated the `dg_*` roles and 0010 could not rename them, hands everything each `dg_*` role owns in the database to its `wt_*` counterpart, moves its privileges there, reapplies 0013's revoke of `update (flagged)` on `query_log` from `wt_web`, and drops the `dg_*` role once nothing in the cluster depends on it. A no-op on a database without `dg_*` roles. Only the bootstrap user can repair a database: a login inheriting `wt_owner` holds no rights there and fails without changing anything | the privileges the `dg_*` roles held, moved to `wt_*` |
| 0021 | `0021_texts_schema.sql` | M4b | Spec 08's texts and reference schema: `text_state`, `reference_kind`, `person`, `person_alias`, `teacher.person_id`, `work.author_person_id` and `tradition`, `reference_asset`, `source_asset_id` on `glossary_term` and `work_locator`, `text_edition` (with the OCR, URL-ingestion, mandate and legal-text columns), `text_unit`, `lineage_edge`, `import_run`, `proof_check`, `takedown_request`, the edition and source takedown scopes, `source_policy.source_retention`, `citation.text_unit_id`, `query_log.named_author_id` and `text_count`, the `servable_text_unit` view and `servable_citation` with `text_unit_id` | `wt_pipeline` read and write on the new tables; `wt_web` select on `servable_text_unit`, `person` and `person_alias`, and insert on `takedown_request` |
| 0022 | `0022_proof_gate.sql` | M4b | The proofread gate holds only for the units it read: `text_edition.units_hash`, `proof_check.units_hash` and the per-group counts `random_sampled`, `random_errors`, `worst_sampled`, `worst_errors`; `servable_text_unit` requires a passing check whose hash matches | None new: the view keeps its grants, and `wt_pipeline` already writes both tables |
| 0023 | `0023_text_notices.sql` | M4b | `text_edition.notice`: copyright notices naming a holder the evidence does not cover, kept for a flagged or held edition and listed by `wt source report` | None new: `wt_pipeline` already writes `text_edition` |
| 0024 | `0024_engagewisdom_platform.sql` | M4E | The `engagewisdom` source platform, and `aligned` as a transcript origin for a source's own text timed by recognition (Spec 03, EngageWisdom sources; Spec 04, Aligned source transcripts) | None new |
| 0026 | `0026_text_unit_vector_version.sql` | Embeddings (#72) | `text_unit.vector_version`, default 1: how a unit's vector was computed beyond its model, so `wt text reembed` finds units embedded in batches (Spec 04, decision of October 9, 2026) | None new: `wt_pipeline` already updates `text_unit` |

0001 to 0008 predate rule 2: Spec 02 put every grant in 0008, after the roles exist. They are applied and stay as they are. From 0009 on, rule 2 applies.

## Planned, not yet numbered

These come from the October 3 to 5 specs. Each gets its number at merge.

| Spec | Spec's number | Description | Notes |
|---|---|---|---|
| 08 | 0014 | rights ledger | Landed whole in 0015 |
| 06 | 0017 | `accessibility_report` table | New table |
| 06 | 0019 | privacy modes: `tester.privacy_mode`, `privacy_chosen`, nullable `query_log.query_text`, `query_log.privacy_mode`, `text_expires_at` and its index | Alters tables from 0005 |
| 02 | 0021 | donation links: `sangha.donation_kind`, `donation_note`, `donation_checked_at`, `donation_checked_by` and the `donation_verified_needs_page` check | Alters `sangha` from 0002 |
| 05 | 0022 | `query_log.sensitive_category` | Alters `query_log` from 0005. Storing an acute question without its text also needs 0019, which makes `query_text` nullable, so it merges after 0019 |
