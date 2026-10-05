-- Spec 08 (Mandated sources, Rights lifecycle): the source policy and the rights ledger.
--
-- M3R needs these before M4b's texts schema exists, so this file pulls forward the rights parts
-- of four Spec 08 migrations (decision of October 5, 2026): the license_evidence changes from
-- the texts, URL-ingestion and mandate-rights migrations, source_policy, and the whole rights
-- ledger. Their text-only parts (text_edition.source_key, servable_text_unit, takedown_request
-- and the edition and source takedown scopes) stay with M4b's texts migrations.

-- License evidence also covers publishers of texts, not only sanghas (Spec 08, texts).
alter table license_evidence alter column sangha_id drop not null;
alter table license_evidence add column publisher text;
alter table license_evidence add constraint license_evidence_owner
  check (sangha_id is not null or publisher is not null);

-- Evidence kinds: a stated license, a recorded permission, or the owner's mandate
-- (Spec 08, URL ingestion and mandate rights, combined into one check).
alter table license_evidence add column kind text not null default 'license'
  check (kind in ('license','permission','mandate'));
alter table license_evidence add column grantor text;
alter table license_evidence add column url_prefixes text[] not null default '{}';
alter table license_evidence alter column license_url drop not null;
alter table license_evidence alter column evidence_url drop not null;
alter table license_evidence add constraint permission_has_scope
  check (kind <> 'permission' or (grantor is not null and cardinality(url_prefixes) > 0));
alter table license_evidence add column mandated_by text;
alter table license_evidence add column rationale text;
alter table license_evidence add column display_notice text;

create table source_policy (
  key text primary key,
  publisher text not null,
  url_prefixes text[] not null,
  basis text not null check (basis in ('license','permission','mandate')),
  license_evidence_id uuid references license_evidence(id),
  enabled boolean not null default true,
  notice_action text not null default 'flag' check (notice_action in ('flag','hold','block')),
  disabled_reason text,
  disabled_at timestamptz,
  updated_at timestamptz not null default now()
);
create trigger source_policy_set_updated_at before update on source_policy
  for each row execute function set_updated_at();

create table rights_event (                 -- append-only
  id uuid primary key default gen_random_uuid(),
  source_key text not null references source_policy(key),
  event text not null check (event in (
    'mandated','request_drafted','request_sent','follow_up_sent','permission_granted',
    'permission_declined','no_response_noted','takedown_requested','suspended',
    'takedown_completed','exclusion_added','exclusion_lifted','enabled','disabled')),
  actor text not null,
  detail text,
  evidence_ref text,                        -- key of a stored request, reply or statement
  created_at timestamptz not null default now()
);
create trigger rights_event_append_only before update or delete on rights_event
  for each row execute function reject_append_only_change();
create trigger rights_event_no_truncate before truncate on rights_event
  for each statement execute function reject_append_only_change();

create table permission_request (
  id uuid primary key default gen_random_uuid(),
  source_key text not null references source_policy(key),
  recipient text not null,
  channel text not null check (channel in ('email','form','other')),
  draft_ref text,
  sent_on date,
  follow_up_on date,
  status text not null default 'draft'
    check (status in ('draft','sent','replied','granted','declined','no_response')),
  reply_ref text,
  created_at timestamptz not null default now()
);

create table source_exclusion (
  id uuid primary key default gen_random_uuid(),
  scope text not null check (scope in ('host','prefix','work','edition','person','content')),
  pattern text not null,                    -- URL prefix, work title, edition id, person name or a SHA-256
  reason text not null,
  source_key text references source_policy(key),
  created_by text not null,
  created_at timestamptz not null default now(),
  lifted_at timestamptz,
  lifted_by text,
  lift_evidence_ref text
);
create index source_exclusion_active_idx on source_exclusion (scope, pattern) where lifted_at is null;

create view v_source_rights as
select sp.key, sp.publisher, sp.basis, sp.enabled,
       (select count(*) from source_exclusion x
         where x.source_key = sp.key and x.lifted_at is null) as active_exclusions,
       (select pr.status from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_status,
       (select pr.sent_on from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_sent_on
from source_policy sp;

-- wt_owner owns every object, whoever runs the migration (as 0013 does for its function).
alter table source_policy owner to wt_owner;
alter table rights_event owner to wt_owner;
alter table permission_request owner to wt_owner;
alter table source_exclusion owner to wt_owner;
alter view v_source_rights owner to wt_owner;

-- Grants live with the objects (docs/migrations.md, rule 2). The web role reads the policy and
-- the public status view only. The ledger is append-only and exclusions are lifted, never
-- deleted, so the pipeline gets no delete on them.
grant select, insert, update on source_policy, permission_request, source_exclusion to wt_pipeline;
grant select, insert on rights_event to wt_pipeline;
grant select on v_source_rights to wt_pipeline;
grant select on source_policy, v_source_rights to wt_web;
