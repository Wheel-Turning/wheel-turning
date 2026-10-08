-- The proofread gate (Spec 08, PDFs and OCR): a passing proofread holds only for the units it
-- read. text_edition.units_hash changes whenever a unit does (a new import or `wt text fix`),
-- and a proof_check counts only while its units_hash matches, so a fix needs a new proofread
-- before the edition serves again. The counts of each sample group are kept beside the totals.

alter table text_edition add column units_hash text;

alter table proof_check add column units_hash text;
alter table proof_check add column random_sampled integer;
alter table proof_check add column random_errors integer;
alter table proof_check add column worst_sampled integer;
alter table proof_check add column worst_errors integer;

create or replace view servable_text_unit as
select u.id as text_unit_id, u.text, u.locator, u.label, u.ordinal, u.embedding, u.tsv,
       e.id as edition_id, e.edition_label, e.language,
       w.id as work_id, w.title as work_title, w.link_out_url as work_url,
       ap.id as author_id, ap.display_name as author_name,
       tp.display_name as translator_name,
       le.license_code, le.license_url,
       u.source_page,
       le.display_notice, le.kind as evidence_kind, u.source_anchor, e.source_url
from text_unit u
join text_edition e on e.id = u.edition_id
join work w on w.id = e.work_id
left join person ap on ap.id = w.author_person_id
left join person tp on tp.id = e.translator_person_id
join license_evidence le on le.id = e.license_evidence_id
where e.state = 'ready' and e.excerptable and le.allowed
  and exists (select 1 from proof_check pc
              where pc.edition_id = e.id and pc.passed and pc.units_hash = e.units_hash)
  and (e.source_key is null
       or exists (select 1 from source_policy sp where sp.key = e.source_key and sp.enabled));
