-- Spec 03, Display levels (owner's decisions of October 7, 2026): a teacher's display level,
-- a narrower one per talk, the 'reviewed' transcript origin, and the serving view's filter.

alter table teacher add column display_level text not null default 'full'
  check (display_level in ('excluded','excerpts','full','owner_only','export_then_delete','removed'));
alter table talk add column display_level text
  check (display_level in ('excluded','excerpts','owner_only'));

alter table transcript drop constraint transcript_origin_check;
alter table transcript add constraint transcript_origin_check
  check (origin in ('captions','asr','reviewed'));

-- The narrower of a teacher's and a talk's levels, from narrowest: removed, export_then_delete,
-- excluded, owner_only, excerpts, full. A talk can never be wider than its teacher.
create function display_level_rank(level text) returns integer
  language sql immutable parallel safe set search_path = public as
  $$ select case level
       when 'removed' then 0 when 'export_then_delete' then 1 when 'excluded' then 2
       when 'owner_only' then 3 when 'excerpts' then 4 when 'full' then 5 end $$;
create function narrower_display_level(teacher_level text, talk_level text) returns text
  language sql immutable parallel safe set search_path = public as
  $$ select case
       when talk_level is null then teacher_level
       when display_level_rank(talk_level) < display_level_rank(teacher_level) then talk_level
       else teacher_level end $$;
alter function display_level_rank(text) owner to wt_owner;
alter function narrower_display_level(text, text) owner to wt_owner;
revoke all on function display_level_rank(text) from public;
revoke all on function narrower_display_level(text, text) from public;
grant execute on function display_level_rank(text) to wt_web, wt_pipeline;
grant execute on function narrower_display_level(text, text) to wt_web, wt_pipeline;

-- The columns of 0017 keep their order; display_level is added at the end. Only talks whose
-- effective level is excerpts or full are servable; the transcript page also needs full.
create or replace view servable_passage as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified,
       t.rights_basis, le.display_notice,
       narrower_display_level(te.display_level, t.display_level) as display_level
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
left join license_evidence le on le.id = t.license_evidence_id
where t.state = 'ready'
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and te.opted_out_at is null
  and narrower_display_level(te.display_level, t.display_level) in ('excerpts','full')
  and (
    (t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and le.allowed)
    or (t.rights_basis = 'mandate' and le.allowed
        and (select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null
             or exists (select 1 from source_policy sp where sp.key = t.source_key and sp.enabled)))
  );
