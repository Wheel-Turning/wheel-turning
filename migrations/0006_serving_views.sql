-- Spec 02: serving views. These are the only objects the web role may read for passage content.

create view servable_passage with (security_barrier = true) as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
where t.state = 'ready'
  and ((t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and exists (
          select 1 from license_evidence le where le.id = t.license_evidence_id and le.allowed)))
  and t.rights_holder is not null
  and lower(t.rights_holder) <> 'unknown';

create view servable_citation with (security_barrier = true) as
select c.id as citation_id, c.passage_id, c.span_start, c.span_end, c.raw_text, c.confidence,
       wl.locator, wl.label, wl.link_out_url as locator_url,
       w.id as work_id, w.title as work_title, w.excerptable
from citation c
join servable_passage sp on sp.passage_id = c.passage_id
left join work_locator wl on wl.id = c.work_locator_id
left join work w on w.id = wl.work_id;
