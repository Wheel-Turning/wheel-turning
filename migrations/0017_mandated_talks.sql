-- Spec 03, Mandated talks: the talks_basis switch, talk sources, teacher opt-out, the ledger
-- keyed by teacher, and the gate and serving view that accept the owner's mandate.

create table rights_setting (
  key text primary key,
  value text not null,
  updated_by text,
  updated_at timestamptz not null default now()
);
-- A fresh database never serves a mandated talk until the owner decides.
insert into rights_setting (key, value, updated_by) values ('talks_basis', 'consent', 'migration');
alter table rights_setting add constraint talks_basis_value
  check (key <> 'talks_basis' or value in ('consent','mandate'));
create trigger rights_setting_set_updated_at before update on rights_setting
  for each row execute function set_updated_at();

alter table source_policy add column kind text not null default 'texts'
  check (kind in ('texts','talks'));
alter table talk add column source_key text references source_policy(key);
alter table teacher add column opted_out_at timestamptz;
alter table teacher add column opted_out_reason text;

-- The ledger also follows teachers. The talks_basis switch is global, so its two events name
-- neither a source nor a teacher (decision of October 5, 2026).
alter table rights_event alter column source_key drop not null;
alter table rights_event add column teacher_id uuid references teacher(id);
alter table rights_event drop constraint rights_event_event_check;
alter table rights_event add constraint rights_event_event_check check (event in (
    'mandated','request_drafted','request_sent','follow_up_sent','permission_granted',
    'permission_declined','no_response_noted','takedown_requested','suspended',
    'takedown_completed','exclusion_added','exclusion_lifted','enabled','disabled',
    'talks_basis_mandate','talks_basis_consent'));
alter table rights_event add constraint rights_event_target
  check (source_key is not null or teacher_id is not null
         or event in ('talks_basis_mandate','talks_basis_consent'));
alter table permission_request alter column source_key drop not null;
alter table permission_request add column teacher_id uuid references teacher(id);
alter table permission_request add constraint permission_request_target
  check (source_key is not null or teacher_id is not null);

alter table talk drop constraint past_gate_requires_consent;
alter table talk add constraint past_gate_requires_consent check (
  state not in ('cleared','fetching','transcribing','diarizing','segmenting',
                'correcting','extracting','resolving','indexing','ready')
  or coalesce(teacher_id is not null and sangha_id is not null
      and rights_holder is not null and lower(rights_holder) <> 'unknown'
      and ((rights_basis = 'speaker_consent' and consent_status = 'granted')
        or (rights_basis in ('license','mandate') and license_evidence_id is not null)),
      false));

-- The columns of 0006 keep their order; rights_basis and display_notice are added at the end.
create or replace view servable_passage as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified,
       t.rights_basis, le.display_notice
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
left join license_evidence le on le.id = t.license_evidence_id
where t.state = 'ready'
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and te.opted_out_at is null
  and (
    (t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and le.allowed)
    or (t.rights_basis = 'mandate' and le.allowed
        and (select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null
             or exists (select 1 from source_policy sp where sp.key = t.source_key and sp.enabled)))
  );

alter table rights_setting owner to wt_owner;

-- The web role reads the setting; only the pipeline changes it (Spec 03).
grant select on rights_setting to wt_web, wt_pipeline;
grant update on rights_setting to wt_pipeline;
