<!-- Exported from the "Dharma Gates: V1 Technical Specs" doc (https://claude.ai/code/artifact/68e63636-e0f6-43ee-ab79-ccc03b76946d), tab "02 Data model", on 2026-10-02. The doc is the source of truth until this file is edited in the repo. -->
# Spec 02: Data model and migrations

Implements the whole v1 schema for Postgres 16 with pgvector. Migrations live in `migrations/` as sqlx files named `NNNN_description.sql`, are forward-only, and are never edited after merge. The SQL below is the contract. Column names and constraints are binding, and index parameters may be tuned after measurement.

Conventions:

- Primary keys are `uuid` with `gen_random_uuid()`. Rust wraps each in a newtype in `dg-core` such as `TalkId` so ids of different tables cannot be mixed.
- Timestamps are `timestamptz` and end in `_at`. Media offsets are integer milliseconds ending in `_ms`. Money is integer cents ending in `_cents`.
- Every mutable table has `created_at` and `updated_at`, with `updated_at` kept by a shared `set_updated_at()` trigger.
- Enums are Postgres enums. New values are added with `alter type ... add value`.
- Derived data tables reference `talk` with `on delete cascade`, so removing a talk's derived data is one delete. Takedown keeps a stub `talk` row in state `withdrawn` so a re-ingest does not recreate it.

## Enums

```sql
create extension if not exists vector;
create extension if not exists pg_trgm;

create type consent_status as enum ('unknown','requested','granted','declined','withdrawn');
create type source_platform as enum ('youtube','storage','rss');
create type talk_type as enum ('dharma_talk','class','retreat_talk','service','non_talk','other');
create type talk_state as enum (
  'discovered','needs_review','awaiting_consent','cleared',
  'fetching','transcribing','diarizing','segmenting','correcting',
  'extracting','resolving','indexing','ready',
  'quarantined','excluded','withdrawn');
create type segment_kind as enum ('talk','chant','announcement','sitting','qa_teacher','qa_student','other');
create type confidence as enum ('resolved','probable','unresolved');
create type job_state as enum ('queued','running','succeeded','failed','dead');
create type rights_basis as enum ('speaker_consent','license');
```

The value `qa_student` exists so the segmenter can label student speech in memory. A check constraint on `segment` forbids storing it, so student speech cannot reach the database.

## Registry tables

```sql
create table sangha (
  id uuid primary key default gen_random_uuid(),
  name text not null unique,
  lineage text not null default 'soto',
  affiliation text,
  donation_url text,
  donation_url_template text,            -- optional, may contain {amount_cents}
  donation_verified boolean not null default false,
  donation_verified_at timestamptz,
  listed boolean not null default true,  -- false: attribution only, never a give button
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table teacher (
  id uuid primary key default gen_random_uuid(),
  display_name text not null unique,
  sangha_id uuid not null references sangha(id),
  second_donation_url text,
  second_donation_verified boolean not null default false,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table teacher_alias (
  id uuid primary key default gen_random_uuid(),
  teacher_id uuid not null references teacher(id) on delete cascade,
  alias text not null
);
create unique index teacher_alias_lower_uq on teacher_alias (lower(alias));

-- append-only consent history; the current row per teacher is the latest by created_at
create table speaker_consent (
  id uuid primary key default gen_random_uuid(),
  teacher_id uuid not null references teacher(id),
  status consent_status not null,
  scope_note text,                       -- limits the speaker set, free text
  evidence_ref text,                     -- pointer to the stored reply, never the reply text
  decided_at timestamptz,
  last_verified_at timestamptz,
  created_by text not null,
  created_at timestamptz not null default now()
);
create view current_consent as
  select distinct on (teacher_id) * from speaker_consent order by teacher_id, created_at desc;

-- a verified, human-reviewed record that a source publishes under a license we may rely on
create table license_evidence (
  id uuid primary key default gen_random_uuid(),
  sangha_id uuid not null references sangha(id),
  license_code text not null,            -- 'cc0', 'cc-by-4.0', 'cc-by-sa-4.0', 'cc-by-nc-nd-4.0', ...
  license_url text not null,
  evidence_url text not null,            -- the page or feed that states the license
  evidence_snapshot_ref text not null,   -- S3 key of the saved page text
  snapshot_sha256 text not null,
  scope_note text,                       -- what the license covers, for example 'all talks in the feed'
  captured_at timestamptz not null,
  reviewed_by text,
  reviewed_at timestamptz,
  allowed boolean not null default false -- set true only by a person after review
);

create table glossary_term (
  id uuid primary key default gen_random_uuid(),
  canonical text not null unique,
  kind text not null check (kind in ('term','name','text_title')),
  variants text[] not null default '{}', -- known mis-hearings and alternate spellings
  notes text
);
```

A trigger on `speaker_consent` raises an exception on `update` and `delete`, so history cannot be rewritten. The seed script `scripts/seed_registry.sql` loads the teacher aliases named in the PRD: Steve Given as Jundo, Shindo and Daiho Shindo, Kanshin alone and with her surname, and Chris Burkhart also as Nomon.

## Source, talk and transcript tables

```sql
create table source_video (
  id uuid primary key default gen_random_uuid(),
  platform source_platform not null,
  external_id text not null,
  url text not null,
  channel_ref text,
  title text not null,
  description text,
  published_at timestamptz,
  duration_s integer,
  raw jsonb not null default '{}'::jsonb,
  discovered_at timestamptz not null default now(),
  unique (platform, external_id)
);

create table talk (
  id uuid primary key default gen_random_uuid(),
  source_video_id uuid not null unique references source_video(id),
  duplicate_of uuid references talk(id),
  title text not null,
  talk_date date,
  duration_s integer,
  talk_type talk_type not null default 'other',
  series text,
  part integer,
  practice_period text,
  teacher_id uuid references teacher(id),
  sangha_id uuid references sangha(id),
  speaker_guess text,                    -- raw text the speaker was inferred from, for review
  consent_status consent_status not null default 'unknown',
  rights_holder text,                    -- null or 'unknown' blocks every state past cleared
  rights_basis rights_basis,             -- how the talk is cleared: speaker consent or a verified license
  license_evidence_id uuid references license_evidence(id),
  state talk_state not null default 'discovered',
  state_reason text,
  quarantine_reason text,
  content_hash text,
  last_verified_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  constraint rights_holder_not_blank check (rights_holder is null or length(trim(rights_holder)) > 0),
  constraint past_gate_requires_consent check (
    state not in ('cleared','fetching','transcribing','diarizing','segmenting',
                  'correcting','extracting','resolving','indexing','ready')
    or ((rights_basis = 'speaker_consent' and consent_status = 'granted'
         or rights_basis = 'license' and license_evidence_id is not null)
        and teacher_id is not null and sangha_id is not null
        and rights_holder is not null and lower(rights_holder) <> 'unknown')
  )
);
create index talk_state_idx on talk (state);
create index talk_teacher_idx on talk (teacher_id);

create table transcript (
  id uuid primary key default gen_random_uuid(),
  talk_id uuid not null references talk(id) on delete cascade,
  version integer not null,
  origin text not null check (origin in ('captions','asr')),
  language text not null default 'en',
  s3_key text not null,                  -- transcript JSON, schema in Spec 04
  content_hash text not null,
  word_count integer,
  is_current boolean not null default true,
  created_at timestamptz not null default now(),
  unique (talk_id, version)
);
create unique index transcript_current_uq on transcript (talk_id) where is_current;
```

The `past_gate_requires_consent` constraint is the database half of the consent gate. A talk cannot be moved into any state at or beyond `cleared` unless consent is granted, the speaker and sangha are known, and the rights holder is known. The Rust typestate in Spec 03 is the other half.

## Segment, passage, citation and work tables

```sql
create table segment (
  id uuid primary key default gen_random_uuid(),
  talk_id uuid not null references talk(id) on delete cascade,
  transcript_id uuid not null references transcript(id) on delete cascade,
  ordinal integer not null,
  kind segment_kind not null,
  start_ms integer not null,
  end_ms integer not null,
  speaker_label text,
  text text not null,
  question_note text,                    -- paraphrase or null; never the student's words
  unique (talk_id, ordinal),
  check (end_ms > start_ms),
  check (kind <> 'qa_student')           -- student speech is never stored
);

create table passage (
  id uuid primary key default gen_random_uuid(),
  talk_id uuid not null references talk(id) on delete cascade,
  segment_id uuid not null references segment(id) on delete cascade,
  segment_kind segment_kind not null check (segment_kind in ('talk','qa_teacher')),
  ordinal integer not null,
  start_ms integer not null,
  end_ms integer not null,
  text text not null check (char_length(text) between 40 and 1800),
  word_count integer not null,
  content_hash text not null,
  tsv tsvector generated always as (to_tsvector('english', text)) stored,
  embedding vector(1024) not null,
  embedding_model text not null,
  unique (talk_id, ordinal)
);
create index passage_tsv_gin on passage using gin (tsv);
create index passage_embedding_hnsw on passage
  using hnsw (embedding vector_cosine_ops) with (m = 16, ef_construction = 64);
create index passage_talk_idx on passage (talk_id);

create table work (
  id uuid primary key default gen_random_uuid(),
  title text not null unique,
  author text,
  translator text,
  license text not null default 'unknown'
    check (license in ('public_domain','open','restricted','unknown')),
  excerptable boolean not null default false,
  link_out_url text,
  notes text,
  check (not excerptable or license in ('public_domain','open'))
);

create table work_locator (
  id uuid primary key default gen_random_uuid(),
  work_id uuid not null references work(id) on delete cascade,
  locator text not null,                 -- canonical, for example 'genjokoan:5'
  label text not null,                   -- display, for example 'Genjokoan, paragraph 5'
  aliases text[] not null default '{}',
  link_out_url text,
  unique (work_id, locator)
);

create table citation (
  id uuid primary key default gen_random_uuid(),
  passage_id uuid not null references passage(id) on delete cascade,
  talk_id uuid not null references talk(id) on delete cascade,
  span_start integer,                    -- character offsets inside passage.text
  span_end integer,
  raw_text text not null,                -- the citing words exactly as they appear
  work_locator_id uuid references work_locator(id),
  confidence confidence not null,
  method text not null check (method in ('hand_list','series_label','llm')),
  created_at timestamptz not null default now(),
  check ((confidence = 'unresolved') = (work_locator_id is null))
);
create index citation_passage_idx on citation (passage_id);
```

The `work` check enforces the PRD rule that only public-domain or openly licensed texts can be stored as excerpts. Everything else is a link-out.

## Serving views and database roles

The serving views are the only objects the web role may read for passage content. A view runs with its owner's privileges, so `dg_web` needs no grant on the underlying tables.

```sql
create view servable_passage with (security_barrier = true) as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
where t.state = 'ready'
  and ((t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and exists (
          select 1 from license_evidence le where le.id = t.license_evidence_id and le.allowed)))
  and t.rights_holder is not null
  and lower(t.rights_holder) <> 'unknown';

create view servable_citation with (security_barrier = true) as
select c.id as citation_id, c.passage_id, c.span_start, c.span_end, c.raw_text, c.confidence,
       wl.locator, wl.label, wl.link_out_url as locator_url,
       w.id as work_id, w.title as work_title, w.excerptable
from citation c
join servable_passage sp on sp.passage_id = c.passage_id
left join work_locator wl on wl.id = c.work_locator_id
left join work w on w.id = wl.work_id;
```

Roles are created by migration as `NOLOGIN` group roles. An ops command, `dg db create-login --role <name>`, creates the login role that inherits from one and takes its password from Secrets Manager. Passwords never appear in migrations.

| Role | Used by | Grants |
| --- | --- | --- |
| `dg_owner` | Migrations from CI and a developer machine | Owns all objects |
| `dg_pipeline` | Batch host and CLI | Read and write on source, talk, transcript, segment, passage, citation, job, glossary, registry and log tables. Insert and read on `speaker_consent`. No access to `query_log` text. |
| `dg_web` | Web service | Select on `servable_passage`, `servable_citation`, `teacher`, `sangha`, `work`, `work_locator` and `tester`. Insert on `query_log`, `answer`, `answer_passage`, `feedback` and `dana_event`. Update on `query_log.flagged` only. |
| `dg_analyst` | Aggregate reports | Select on the aggregate views in the operational tables section and execute on `flagged_queries()`. Nothing else. |

## Operational tables

```sql
create table job (
  id uuid primary key default gen_random_uuid(),
  kind text not null,
  talk_id uuid references talk(id) on delete cascade,
  payload jsonb not null default '{}'::jsonb,
  idempotency_key text not null unique,
  state job_state not null default 'queued',
  attempts integer not null default 0,
  max_attempts integer not null default 5,
  run_at timestamptz not null default now(),
  locked_by text,
  locked_at timestamptz,
  last_error text,
  finished_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create index job_claim_idx on job (run_at) where state = 'queued';

create table tester (
  id uuid primary key default gen_random_uuid(),
  label text not null,                   -- how the founder knows them, not a legal name
  token_hash bytea not null unique,      -- SHA-256 of the invite token
  created_at timestamptz not null default now(),
  revoked_at timestamptz,
  last_seen_at timestamptz
);

create table query_log (
  id uuid primary key default gen_random_uuid(),
  tester_id uuid not null references tester(id),
  query_text text not null,
  mode text not null check (mode in ('standard','named_teacher')),
  named_teacher_id uuid references teacher(id),
  teacher_count integer not null,
  sangha_count integer not null,
  passage_count integer not null,
  theme_count integer not null default 0,
  themes_dropped integer not null default 0,
  thin_coverage boolean not null default false,
  latency_ms integer not null,
  flagged boolean not null default false,
  flagged_at timestamptz,
  created_at timestamptz not null default now()
);

create table answer (
  id uuid primary key default gen_random_uuid(),
  query_log_id uuid not null unique references query_log(id) on delete cascade,
  payload jsonb not null,                -- passage ids and theme text only, never passage text
  created_at timestamptz not null default now()
);

create table answer_passage (
  answer_id uuid not null references answer(id) on delete cascade,
  passage_id uuid not null references passage(id) on delete cascade,
  rank integer not null,
  primary key (answer_id, passage_id)
);

create table feedback (
  id uuid primary key default gen_random_uuid(),
  answer_id uuid not null references answer(id) on delete cascade,
  tester_id uuid not null references tester(id),
  kind text not null check (kind in ('flag_query','good','not_faithful','not_relevant','note')),
  note text,
  created_at timestamptz not null default now()
);

create table dana_event (
  id uuid primary key default gen_random_uuid(),
  answer_id uuid not null references answer(id) on delete cascade,
  sangha_id uuid references sangha(id),  -- null for the site's own line
  kind text not null check (kind in ('panel_open','split_computed','link_click')),
  total_cents integer,
  created_at timestamptz not null default now()
);

create table correction_log (             -- append-only
  id uuid primary key default gen_random_uuid(),
  entity text not null,
  entity_id uuid not null,
  field text not null,
  old_value jsonb,
  new_value jsonb,
  actor text not null,
  reason text not null,
  created_at timestamptz not null default now()
);

create table takedown_log (
  id uuid primary key default gen_random_uuid(),
  scope text not null check (scope in ('teacher','talk')),
  teacher_id uuid references teacher(id),
  talk_id uuid references talk(id),
  actor text not null,
  counts jsonb not null,                 -- rows deleted per store
  created_at timestamptz not null default now()
);

create table eval_set (
  id uuid primary key default gen_random_uuid(),
  name text not null,
  version integer not null,
  created_at timestamptz not null default now(),
  unique (name, version)
);
create table eval_question (
  id uuid primary key default gen_random_uuid(),
  set_id uuid not null references eval_set(id) on delete cascade,
  ordinal integer not null,
  text text not null,
  tags text[] not null default '{}',     -- multi_voice, single_teacher, grief, named_teacher
  unique (set_id, ordinal)
);
create table eval_run (
  id uuid primary key default gen_random_uuid(),
  set_id uuid not null references eval_set(id),
  build text not null,
  config jsonb not null,
  started_at timestamptz not null default now()
);
create table eval_result (
  run_id uuid not null references eval_run(id) on delete cascade,
  question_id uuid not null references eval_question(id),
  answer_id uuid not null references answer(id) on delete cascade,
  primary key (run_id, question_id)
);
create table eval_rating (
  id uuid primary key default gen_random_uuid(),
  run_id uuid not null references eval_run(id) on delete cascade,
  question_id uuid not null references eval_question(id),
  passage_id uuid not null,
  reviewer text not null,
  faithful boolean not null,
  relevant boolean not null,
  note text,
  created_at timestamptz not null default now()
);
create table claim_audit (
  id uuid primary key default gen_random_uuid(),
  run_id uuid not null references eval_run(id) on delete cascade,
  answer_id uuid not null references answer(id) on delete cascade,
  claim_text text not null,
  verdict text not null check (verdict in ('supported','drifting','invented')),
  reviewer text not null,
  note text,
  created_at timestamptz not null default now()
);
```

Aggregate views for the analyst role, and the one function that opens flagged query text:

```sql
create view v_answer_stats as
select date_trunc('day', created_at) as day, mode, teacher_count,
       count(*) as answers, avg(latency_ms)::int as avg_latency_ms,
       count(*) filter (where thin_coverage) as thin_answers
from query_log group by 1, 2, 3;

create view v_consent_coverage as
select t.state, t.consent_status, te.display_name as teacher, count(*) as talks
from talk t left join teacher te on te.id = t.teacher_id group by 1, 2, 3;

create view v_dana_clicks as
select date_trunc('day', created_at) as day, kind, count(*) as events
from dana_event group by 1, 2;

create function flagged_queries() returns setof query_log
  language sql security definer set search_path = public as
  $$ select * from query_log where flagged $$;
revoke all on function flagged_queries() from public;
grant execute on function flagged_queries() to dg_analyst;
```

The `eval_rating.passage_id` column carries no foreign key on purpose. After a takedown the rating row survives as a record of what was rated, and the passage is gone.

## Migration order and acceptance

Migration files, in order:

1. `0001_extensions_enums.sql`
2. `0002_registry.sql`
3. `0003_source_talk_transcript.sql`
4. `0004_segment_passage_citation.sql`
5. `0005_operational.sql`
6. `0006_serving_views.sql`
7. `0007_triggers.sql` with `set_updated_at()` and the append-only guards on `speaker_consent`, `correction_log` and `takedown_log`
8. `0008_roles_grants.sql`

Seed data lives outside migrations in `scripts/seed_registry.sql` and `scripts/seed_works.sql`. The works seed holds the hand-listed texts and locators for the Genjokoan, Uji, Bendowa, Blue Cliff Record and Song of the Grass Hut series.

Acceptance criteria, each as a database integration test that connects as the named role where relevant:

- [ ] All migrations apply to an empty database, and the runner is idempotent.
- [ ] Moving a talk to `cleared` with consent not granted, rights holder null or `unknown`, or teacher null fails.
- [ ] Updating or deleting a `speaker_consent` row raises.
- [ ] Inserting a segment of kind `qa_student` fails.
- [ ] `servable_passage` returns no rows for a talk that is not `ready`, whose consent is not `granted`, or whose rights holder is `unknown`.
- [ ] `dg_web` cannot select from `passage`, `talk`, `segment` or `query_log`, and can update only `query_log.flagged`.
- [ ] `dg_analyst` cannot select from `query_log`, can select the aggregate views, and `flagged_queries()` returns only flagged rows.
- [ ] A vector query on `servable_passage` uses the HNSW index in `EXPLAIN` on a table with 10,000 synthetic rows.
- [ ] `.sqlx` offline query data is committed and `cargo sqlx prepare --check` passes.
