-- Spec 04, Language-model spend cap: one row per call, the owner's cap override, and the
-- month's 80 percent alert, recorded so it is sent once.

create table llm_usage (
  id uuid primary key default gen_random_uuid(),
  created_at timestamptz not null default now(),
  stage text not null,
  model text not null,
  input_tokens integer not null,
  output_tokens integer not null,
  est_cost_usd numeric(10,4) not null
);
create index llm_usage_month_idx on llm_usage (created_at);

-- `wt llm cap set` writes `monthly_cap_usd`; without a row, WT_LLM_MONTHLY_CAP_USD applies.
create table llm_setting (
  key text primary key,
  value text not null,
  updated_by text not null,
  updated_at timestamptz not null default now()
);
create trigger llm_setting_set_updated_at before update on llm_setting
  for each row execute function set_updated_at();

-- The first day of a month in which the 80 percent alert went out.
create table llm_alert (
  month date primary key,
  sent_at timestamptz not null default now()
);

alter table llm_usage owner to wt_owner;
alter table llm_setting owner to wt_owner;
alter table llm_alert owner to wt_owner;

-- Ingestion calls run on the pipeline; synthesis and verification calls run in the web
-- service (Spec 05). Both record usage and read the month so far.
grant select, insert on llm_usage, llm_alert to wt_pipeline, wt_web;
grant select, insert, update on llm_setting to wt_pipeline;
grant select on llm_setting to wt_web;
