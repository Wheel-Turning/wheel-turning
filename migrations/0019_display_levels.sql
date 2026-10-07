-- Spec 03, Display levels (owner's decisions of October 7, 2026): a teacher's display level,
-- a narrower one per talk, the 'reviewed' transcript origin, and the serving view's filter.

alter table teacher add column display_level text not null default 'full'
  check (display_level in ('excluded','excerpts','full','owner_only','export_then_delete','removed'));
alter table talk add column display_level text
  check (display_level in ('excluded','excerpts','owner_only'));

alter table transcript drop constraint transcript_origin_check;
alter table transcript add constraint transcript_origin_check
  check (origin in ('captions','asr','reviewed'));

-- A talk's level can only be a, b or d, so a talk is public when its teacher is at excerpts or
-- full and the talk is unset or at excerpts, and its effective level is the narrower one. Plain
-- column conditions, not a function, so the planner can estimate them and keep the HNSW index
-- (Spec 02; `a_vector_query_on_servable_passage_uses_the_hnsw_index`).

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
       case when te.display_level = 'excerpts' or t.display_level = 'excerpts' then 'excerpts'
            else 'full' end as display_level
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
left join license_evidence le on le.id = t.license_evidence_id
where t.state = 'ready'
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and te.opted_out_at is null
  and te.display_level in ('excerpts','full')
  and (t.display_level is null or t.display_level = 'excerpts')
  and (
    (t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and le.allowed)
    or (t.rights_basis = 'mandate' and le.allowed
        and (select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null
             or exists (select 1 from source_policy sp where sp.key = t.source_key and sp.enabled)))
  );
