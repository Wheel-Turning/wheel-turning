-- Spec 06 (Accessibility standard, Reporting) and Spec 08 (Rights lifecycle, Transparency):
-- the accessibility report table, and the dates and license names the rights page states.

create table accessibility_report (
  id uuid primary key default gen_random_uuid(),
  received_at timestamptz not null default now(),
  page_path text,
  description text not null check (char_length(description) between 5 and 4000),
  assistive_tech text,
  contact text,
  status text not null default 'open' check (status in ('open','fixing','fixed','declined')),
  handled_by text,
  handled_at timestamptz,
  note text
);
alter table accessibility_report owner to wt_owner;

-- The web role only files reports; `wt accessibility reports` reads and handles them.
grant insert on accessibility_report to wt_web;
grant select, update on accessibility_report to wt_pipeline;

-- The rights page states each source's status in plain words with its date (Spec 09, Rights
-- and removal page). The view keeps its seven columns in order and gains these at the end:
-- the source's kind, its license name and link, when permission was recorded, when the
-- license was recorded, and whether the rights holder asked for removal.
create or replace view v_source_rights as
select sp.key, sp.publisher, sp.basis, sp.enabled,
       (select count(*) from source_exclusion x
         where x.source_key = sp.key and x.lifted_at is null) as active_exclusions,
       (select pr.status from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_status,
       (select pr.sent_on from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_sent_on,
       sp.kind,
       le.license_code as license_name,
       le.license_url,
       (select max(e.created_at)::date from rights_event e
         where e.source_key = sp.key and e.event = 'permission_granted') as permission_recorded_on,
       coalesce(le.reviewed_at, le.captured_at)::date as license_recorded_on,
       exists (select 1 from rights_event e
                where e.source_key = sp.key
                  and e.event in ('permission_declined','takedown_requested','takedown_completed'))
         as removal_requested
from source_policy sp
left join license_evidence le on le.id = sp.license_evidence_id;
