-- Spec 04, Chunking, embedding and indexing, sentence boundaries (owner's decision of October 12,
-- 2026), for Spec 05's answer excerpts (Answer assembly, rule 8). Each passage stores where its
-- sentences start, as word offsets into its text, and when each is spoken. Both start empty and
-- `wt reindex --all` fills them; the stored words never change.

alter table passage
  add column sentence_starts integer[] not null default '{}',
  add column sentence_start_ms integer[] not null default '{}',
  add constraint passage_sentence_arrays_match
    check (cardinality(sentence_starts) = cardinality(sentence_start_ms));

-- The columns of 0019 keep their order; the segment, so a window of neighbors stays inside one
-- segment, and the boundaries are added at the end. The filter is unchanged.
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
            else 'full' end as display_level,
       p.segment_id, p.sentence_starts, p.sentence_start_ms
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

-- Spec 02: vector search on servable_passage must keep using the HNSW index (see 0012).
alter view servable_passage reset (security_barrier);
