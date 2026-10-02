-- Spec 02: operational tables, aggregate views for the analyst role, and flagged_queries().
-- The execute grant on flagged_queries() to dg_analyst is in 0008_roles_grants.sql,
-- because the role does not exist until that migration.

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
