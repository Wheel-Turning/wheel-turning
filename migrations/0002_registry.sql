-- Spec 02: registry tables.

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
