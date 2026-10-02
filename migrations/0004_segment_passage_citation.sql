-- Spec 02: segment, passage, citation and work tables.

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
