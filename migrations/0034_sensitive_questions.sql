-- Spec 05, Sensitive questions: the category each question was given before retrieval.
--
-- An acute question is stored as counts and its category only, with no text and no answer row.
-- 0031 (privacy modes) already made query_text nullable; this repeats it, which does nothing,
-- so the file stands on its own.

alter table query_log alter column query_text drop not null;

alter table query_log add column sensitive_category text not null default 'none'
  check (sensitive_category in ('none','distress','medical','acute'));

alter table query_log add constraint query_log_acute_keeps_no_text
  check (sensitive_category <> 'acute' or query_text is null);

-- What topic clusters, gap reports and any evaluation set built from logged questions read:
-- every logged question except acute and distress ones (Spec 05 acceptance).
create view analyzable_query_log as
select * from query_log
where sensitive_category not in ('acute','distress');

alter view analyzable_query_log owner to wt_owner;

grant select on analyzable_query_log to wt_pipeline;
