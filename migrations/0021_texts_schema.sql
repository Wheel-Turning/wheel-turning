-- Spec 08 (M4b): the texts and reference schema. It gathers the text parts of Spec 08's planned
-- texts, OCR-proof, URL-ingestion, mandate-rights and retention migrations; their rights parts
-- landed in 0015 (decision of October 5, 2026). Grants live with the objects (docs/migrations.md,
-- rule 2), with Spec 08's dg_ roles now wt_.

create type text_state as enum ('imported','ready','quarantined','withdrawn');
create type reference_kind as enum
  ('glossary','alias_list','lineage','locator_list','chant_metadata','other');

-- People who are not necessarily teachers: authors, translators, compilers, historical figures.
create table person (
  id uuid primary key default gen_random_uuid(),
  display_name text not null unique,
  kind text not null check (kind in ('author','translator','compiler','figure','teacher')),
  tradition text,
  era_note text,                         -- free text, for example '13th century'
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create trigger person_set_updated_at before update on person
  for each row execute function set_updated_at();
create table person_alias (
  id uuid primary key default gen_random_uuid(),
  person_id uuid not null references person(id) on delete cascade,
  alias text not null
);
create unique index person_alias_lower_uq on person_alias (lower(alias));

alter table teacher add column person_id uuid references person(id);

-- work.excerptable from Spec 02 stays unused; the edition-level flag below governs.
alter table work add column author_person_id uuid references person(id);
alter table work add column tradition text;

create table reference_asset (
  id uuid primary key default gen_random_uuid(),
  kind reference_kind not null,
  title text not null,
  source_url text,
  license_evidence_id uuid references license_evidence(id),
  s3_key text not null,
  sha256 text not null unique,
  row_counts jsonb not null default '{}'::jsonb,
  imported_by text not null,
  imported_at timestamptz not null default now()
);
alter table glossary_term add column source_asset_id uuid references reference_asset(id);
alter table work_locator add column source_asset_id uuid references reference_asset(id);

create table text_edition (
  id uuid primary key default gen_random_uuid(),
  work_id uuid not null references work(id),
  edition_label text not null,           -- for example 'Muller 1881'
  translator_person_id uuid references person(id),
  language text not null default 'en',
  source_url text,
  rights_holder text,
  license_evidence_id uuid references license_evidence(id),
  excerptable boolean not null default false,
  state text_state not null default 'imported',
  content_hash text,
  imported_at timestamptz not null default now(),
  -- PDFs and OCR
  extraction text not null default 'native'
    check (extraction in ('native','text_layer','ocr','ocr_vision')),
  ocr_engine text,
  mean_confidence real,
  -- URL ingestion
  source_sha256 text,
  fetched_at timestamptz,
  source_etag text,
  -- Mandated sources
  source_key text references source_policy(key),
  -- Source copies and legal pages
  legal_text text,
  legal_pages jsonb not null default '[]'::jsonb,
  source_deleted_at timestamptz,
  unique (work_id, edition_label),
  constraint ready_requires_license check (
    state <> 'ready'
    or (excerptable and license_evidence_id is not null and rights_holder is not null))
);

create table text_unit (
  id uuid primary key default gen_random_uuid(),
  edition_id uuid not null references text_edition(id) on delete cascade,
  ordinal integer not null,
  locator text not null,                 -- canonical, for example 'dhammapada:1'
  label text not null,                   -- display, for example 'Dhammapada, verse 1'
  heading text,
  text text not null check (char_length(text) between 10 and 2500),
  word_count integer not null,
  content_hash text not null,
  tsv tsvector generated always as (to_tsvector('english', text)) stored,
  embedding vector(1024) not null,
  embedding_model text not null,
  source_page integer,                   -- PDFs: the page the unit starts on
  confidence real,                       -- OCR: the unit's mean confidence
  source_anchor text,                    -- HTML: the fragment the unit starts at
  unique (edition_id, ordinal),
  unique (edition_id, locator)
);
create index text_unit_tsv_gin on text_unit using gin (tsv);
create index text_unit_embedding_hnsw on text_unit
  using hnsw (embedding vector_cosine_ops) with (m = 16, ef_construction = 64);

create table lineage_edge (
  id uuid primary key default gen_random_uuid(),
  parent_person_id uuid not null references person(id),
  child_person_id uuid not null references person(id),
  kind text not null check (kind in ('teacher_student','transmission','founder')),
  source_asset_id uuid references reference_asset(id),
  note text,
  check (parent_person_id <> child_person_id),
  unique (parent_person_id, child_person_id, kind)
);

create table import_run (                 -- audit of every importer run
  id uuid primary key default gen_random_uuid(),
  manifest_path text not null,
  role text not null check (role in ('corpus','reference')),
  dry_run boolean not null,
  counts jsonb not null,
  actor text not null,
  created_at timestamptz not null default now()
);

create table proof_check (
  id uuid primary key default gen_random_uuid(),
  edition_id uuid not null references text_edition(id) on delete cascade,
  sample_size integer not null,
  errors integer not null,
  reviewer text not null,
  passed boolean not null,
  notes text,
  created_at timestamptz not null default now()
);

create table takedown_request (
  id uuid primary key default gen_random_uuid(),
  received_at timestamptz not null default now(),
  requester_name text,
  requester_email text,
  organization text,
  target text not null,                  -- a URL, a title or an excerpt id
  message text,
  text_unit_id uuid,
  status text not null default 'open' check (status in ('open','suspended','removed','declined')),
  handled_by text,
  handled_at timestamptz,
  action_taken text
);

alter table takedown_log drop constraint takedown_log_scope_check;
alter table takedown_log add constraint takedown_log_scope_check
  check (scope in ('teacher','talk','edition','source'));
alter table takedown_log add column edition_id uuid;
alter table takedown_log add column source_key text;

alter table source_policy add column source_retention text not null default 'keep'
  check (source_retention in ('keep','delete_after_proof'));
update source_policy set source_retention = 'delete_after_proof' where basis = 'mandate';

alter table citation add column text_unit_id uuid references text_unit(id) on delete set null;

alter table query_log add column named_author_id uuid references person(id);
alter table query_log add column text_count integer not null default 0;

-- Text units that may be shown: a ready, excerptable edition with allowed evidence, a passing
-- proofread, and an enabled source when it has one (Spec 08, Mandated sources).
create view servable_text_unit as
select u.id as text_unit_id, u.text, u.locator, u.label, u.ordinal, u.embedding, u.tsv,
       e.id as edition_id, e.edition_label, e.language,
       w.id as work_id, w.title as work_title, w.link_out_url as work_url,
       ap.id as author_id, ap.display_name as author_name,
       tp.display_name as translator_name,
       le.license_code, le.license_url,
       u.source_page,
       le.display_notice, le.kind as evidence_kind, u.source_anchor, e.source_url
from text_unit u
join text_edition e on e.id = u.edition_id
join work w on w.id = e.work_id
left join person ap on ap.id = w.author_person_id
left join person tp on tp.id = e.translator_person_id
join license_evidence le on le.id = e.license_evidence_id
where e.state = 'ready' and e.excerptable and le.allowed
  and exists (select 1 from proof_check pc where pc.edition_id = e.id and pc.passed)
  and (e.source_key is null
       or exists (select 1 from source_policy sp where sp.key = e.source_key and sp.enabled));

-- The columns of 0006 keep their order; text_unit_id is added at the end.
create or replace view servable_citation with (security_barrier = true) as
select c.id as citation_id, c.passage_id, c.span_start, c.span_end, c.raw_text, c.confidence,
       wl.locator, wl.label, wl.link_out_url as locator_url,
       w.id as work_id, w.title as work_title, w.excerptable,
       stu.text_unit_id
from citation c
join servable_passage sp on sp.passage_id = c.passage_id
left join work_locator wl on wl.id = c.work_locator_id
left join work w on w.id = wl.work_id
left join servable_text_unit stu on stu.text_unit_id = c.text_unit_id;

alter type text_state owner to wt_owner;
alter type reference_kind owner to wt_owner;
alter table person owner to wt_owner;
alter table person_alias owner to wt_owner;
alter table reference_asset owner to wt_owner;
alter table text_edition owner to wt_owner;
alter table text_unit owner to wt_owner;
alter table lineage_edge owner to wt_owner;
alter table import_run owner to wt_owner;
alter table proof_check owner to wt_owner;
alter table takedown_request owner to wt_owner;
alter view servable_text_unit owner to wt_owner;

grant select, insert, update, delete on person, person_alias, reference_asset, text_edition,
  text_unit, lineage_edge, import_run, proof_check, takedown_request to wt_pipeline;
grant select on servable_text_unit to wt_pipeline;
grant select on servable_text_unit, person, person_alias to wt_web;
grant insert on takedown_request to wt_web;
