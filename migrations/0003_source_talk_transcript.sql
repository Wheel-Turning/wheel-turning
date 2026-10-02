-- Spec 02: source, talk and transcript tables.

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
