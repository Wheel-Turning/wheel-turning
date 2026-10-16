-- Spec 11 PB4.6, with Spec 10 E1 pulled forward: the cost of each answer's model calls, the
-- time to its first progress event and to its page, and the counts `wt report beta` reads as
-- the analyst role. No function here returns question text.

-- Each model call made for an answer carries that answer's id. Not a foreign key: the calls are
-- recorded before the answer is stored, the spend ledger outlives a purged answer, and calls for
-- a question the triage gate stopped name an answer that was never stored.
alter table llm_usage add column answer_id uuid;
create index llm_usage_answer_idx on llm_usage (answer_id) where answer_id is not null;

-- A Haiku 5.5 call costs a few hundredths of a cent, which four places rounded away.
alter table llm_usage alter column est_cost_usd type numeric(14,8);

-- The sum of the answer's calls, written when the answer is stored and again when its deferred
-- summary is. Null on rows logged before this migration, or for a question with no answer.
alter table query_log add column llm_cost_usd numeric(14,8);
-- Milliseconds from the request to the first progress event (a streamed ask only), and to the
-- answer stored behind the page.
alter table query_log add column first_progress_ms integer;
alter table query_log add column page_ms integer;

-- Rewrites an answer's cost from its calls, after the deferred summary's calls are recorded.
create function settle_answer_cost(answer_id uuid) returns void
  language sql security definer set search_path = public as
$$
  update query_log q
     set llm_cost_usd = (select coalesce(sum(u.est_cost_usd), 0) from llm_usage u
                          where u.answer_id = settle_answer_cost.answer_id)
    from answer a
   where a.id = settle_answer_cost.answer_id and q.id = a.query_log_id
$$;

alter function settle_answer_cost(uuid) owner to wt_owner;
revoke all on function settle_answer_cost(uuid) from public;
grant execute on function settle_answer_cost(uuid) to wt_web;

-- One row per question since `since`, without its text or its tester's id: `tester` numbers the
-- testers within this call only. Evaluation runs are left out. A count-only question whose text
-- and answer the purge removed still counts, as answered, with its cost and times.
create function beta_questions(since timestamptz)
  returns table (tester integer, answered boolean, llm_cost_usd float8,
                 first_progress_ms integer, page_ms integer, link_clicks integer)
  language sql stable security definer set search_path = public as
$$
  select dense_rank() over (order by q.tester_id)::int,
         (a.id is not null or q.purged_answer_id is not null),
         q.llm_cost_usd::float8,
         q.first_progress_ms,
         q.page_ms,
         (select count(*) from dana_event d
           where d.answer_id = a.id and d.kind = 'link_click')::int
  from query_log q
  join tester t on t.id = q.tester_id
  left join answer a on a.query_log_id = q.id
  where q.created_at >= since and t.label <> 'eval'
$$;

-- Testers who asked in two or more distinct weeks since the beta began.
create function beta_returning_testers() returns bigint
  language sql stable security definer set search_path = public as
$$
  select count(*) from (
    select q.tester_id
    from query_log q join tester t on t.id = q.tester_id
    where t.label <> 'eval'
    group by q.tester_id
    having count(distinct date_trunc('week', q.created_at)) >= 2
  ) r
$$;

-- Dana link clicks since `since`, including those on an answer the purge has since removed,
-- whose tester can no longer be known.
create function beta_link_clicks(since timestamptz) returns bigint
  language sql stable security definer set search_path = public as
$$
  select count(*) from dana_event d
  left join answer a on a.id = d.answer_id
  left join query_log q on q.id = a.query_log_id
  left join tester t on t.id = q.tester_id
  where d.kind = 'link_click' and d.created_at >= since and coalesce(t.label, '') <> 'eval'
$$;

-- Feedback submissions since `since`, and those that marked the answer helpful.
create function beta_feedback(since timestamptz)
  returns table (submissions bigint, helpful bigint)
  language sql stable security definer set search_path = public as
$$
  select count(distinct f.submission_id),
         count(distinct f.submission_id) filter (where f.kind = 'good')
  from feedback f join tester t on t.id = f.tester_id
  where f.created_at >= since and t.label <> 'eval' and f.target <> 'query'
$$;

-- Feedback submissions since `since` that gave each reason.
create function beta_feedback_reasons(since timestamptz)
  returns table (kind text, submissions bigint)
  language sql stable security definer set search_path = public as
$$
  select f.kind, count(distinct f.submission_id)
  from feedback f join tester t on t.id = f.tester_id
  where f.created_at >= since and t.label <> 'eval' and f.target <> 'query'
  group by f.kind
$$;

do $$
declare
  f text;
begin
  foreach f in array array['beta_questions(timestamptz)', 'beta_returning_testers()',
                           'beta_link_clicks(timestamptz)', 'beta_feedback(timestamptz)',
                           'beta_feedback_reasons(timestamptz)']
  loop
    execute format('alter function %s owner to wt_owner', f);
    execute format('revoke all on function %s from public', f);
    execute format('grant execute on function %s to wt_analyst', f);
  end loop;
end
$$;
