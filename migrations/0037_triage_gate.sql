-- Spec 05, Sensitive questions, as amended on October 12, 2026: the triage gate.
--
-- out_of_scope joins the categories and, like acute, keeps no text. Each question records the
-- gate's relevance (null when the gate did not answer) and whether the gate was degraded. The
-- gate's verdicts are also counted, without any link to a question, in triage_tally, which
-- `wt sensitive report` reads as wt_analyst together with v_query_triage.

alter table query_log drop constraint query_log_sensitive_category_check;
alter table query_log add constraint query_log_sensitive_category_check
  check (sensitive_category in ('none','distress','medical','acute','out_of_scope'));

alter table query_log drop constraint query_log_acute_keeps_no_text;
alter table query_log add constraint query_log_acute_keeps_no_text
  check (sensitive_category not in ('acute','out_of_scope') or query_text is null);

alter table query_log add column relevance text
  check (relevance in ('practice','adjacent','unrelated'));
alter table query_log add column triage_degraded boolean not null default false;

-- 0034's view froze query_log's columns; recreate it so it carries the new ones, and leave
-- out_of_scope rows out as well (they hold no text anyway).
create or replace view analyzable_query_log as
select * from query_log
where sensitive_category not in ('acute','distress','out_of_scope');

-- Counts only: one row per day and combination of what each layer said.
create table triage_tally (
  day date not null,
  rules text not null,       -- the rules' level
  embedding text not null,   -- the exemplars' level, 'down' or 'skipped'
  risk text not null,        -- the gate's risk, or 'unanswered'
  gate_level text not null,  -- the level that risk maps to, or 'unanswered'
  relevance text not null,   -- the gate's relevance, or 'unanswered'
  confidence text not null,  -- the gate's confidence, or 'unanswered'
  level text not null,       -- the level the question got
  n bigint not null default 0,
  primary key (day, rules, embedding, risk, gate_level, relevance, confidence, level)
);

alter table triage_tally owner to wt_owner;

grant select, insert, update on triage_tally to wt_web;
grant select on triage_tally to wt_analyst;

-- Questions per day by category, relevance and whether the gate was degraded, with how many
-- were answered, found nothing, drew a Not relevant vote or were flagged. No text.
create view v_query_triage as
select date_trunc('day', q.created_at) as day, q.sensitive_category, q.relevance,
       q.triage_degraded,
       count(*) as questions,
       count(a.id) as answered,
       count(a.id) filter (where q.passage_count = 0) as none_found,
       count(a.id) filter (where exists (
         select 1 from feedback f where f.answer_id = a.id and f.kind = 'not_relevant'
       )) as not_relevant,
       count(*) filter (where q.flagged) as flagged
from query_log q
join tester t on t.id = q.tester_id
left join answer a on a.query_log_id = q.id
where t.label <> 'eval'
group by 1, 2, 3, 4;

alter view v_query_triage owner to wt_owner;

grant select on v_query_triage to wt_analyst;
