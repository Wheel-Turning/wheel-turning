-- Spec 07, Evaluation harness: what each evaluation answer took, stage by stage, and how it came
-- out, and when a run finished. Answers asked as the reserved tester label `eval` stay out of
-- the tester analytics views.

alter table eval_result
  add column timings jsonb not null default '{}'::jsonb,   -- milliseconds per stage
  add column outcome jsonb not null default '{}'::jsonb;   -- mode, counts, notice, failures
alter table eval_run add column finished_at timestamptz;

create or replace view v_answer_stats as
select date_trunc('day', q.created_at) as day, q.mode, q.teacher_count,
       count(*) as answers, avg(q.latency_ms)::int as avg_latency_ms,
       count(*) filter (where q.thin_coverage) as thin_answers
from query_log q join tester t on t.id = q.tester_id
where t.label <> 'eval'
group by 1, 2, 3;

create or replace view v_dana_clicks as
select date_trunc('day', d.created_at) as day, d.kind, count(*) as events
from dana_event d
join answer a on a.id = d.answer_id
join query_log q on q.id = a.query_log_id
join tester t on t.id = q.tester_id
where t.label <> 'eval'
group by 1, 2;
