-- Spec 05, Answer assembly, rules 2 and 5 as amended on October 12, 2026: source links by who
-- made the transcript, and the MLA References list.
--
-- A talk whose current transcript is the source's own text (origin `aligned` or `text`) links
-- to the source's transcript and is served at `excerpts` at most, since this site shows no
-- transcript of its own for it. The `audio` job sets the flag whenever it writes a transcript;
-- existing talks are backfilled from their current transcript's origin.

alter table talk add column transcript_from_source boolean not null default false;

update talk t set transcript_from_source = true
where exists (select 1 from transcript tr
              where tr.talk_id = t.id and tr.is_current and tr.origin in ('aligned','text'));

-- The source's own transcript page, when it differs from the talk page; null means the talk
-- page itself holds it.
alter table source_video add column transcript_url text;

-- How a teacher is named and sorted in References, set by the owner with `wt teacher cite`.
-- A teacher without a citation name is entered by display name, uninverted.
alter table teacher add column citation_name text;
alter table teacher add column citation_sort text;

-- The columns of 0036 keep their order. `display_level` is `excerpts` for a talk whose
-- transcript came from its source, and the new fields are added at the end: the flag, the
-- source transcript page, the current transcript's origin for the "Read our transcript"
-- label, and the teacher's citation name and sort key. The filter is unchanged.
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
       te.citation_name, te.citation_sort
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
