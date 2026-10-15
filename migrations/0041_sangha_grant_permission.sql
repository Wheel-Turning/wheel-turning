-- Owner's decision of October 14, 2026: talks are given to the sangha, and sharing them is the
-- sangha's decision, not the individual teacher's. So a sangha's written yes for its talks source
-- (recorded with `wt rights grant` on its talks source, basis `permission`) is permission
-- for its talks, and the page says "Used with permission of <publisher>".
--
-- The columns of 0038 keep their order; `permission_of`, the publisher of the talk's source when
-- that source's basis is `permission`, is added at the end. The filter is unchanged: who may be
-- served still follows the talk's own basis, and a teacher's decline or opt-out still wins.

create or replace view servable_passage as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified,
       t.rights_basis, le.display_notice,
       case when te.display_level = 'excerpts' or t.display_level = 'excerpts'
                 or t.transcript_from_source then 'excerpts'
            else 'full' end as display_level,
       p.segment_id, p.sentence_starts, p.sentence_start_ms,
       t.transcript_from_source, sv.transcript_url,
       (select tr.origin from transcript tr
        where tr.talk_id = t.id and tr.is_current) as transcript_origin,
       te.citation_name, te.citation_sort,
       (select sp.publisher from source_policy sp
        where sp.key = t.source_key and sp.kind = 'talks' and sp.basis = 'permission')
         as permission_of
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
