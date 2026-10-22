-- Spec 07, Usage report (owner's decisions of October 20 and 21, 2026): each request that carried
-- a tester, kept for 30 days as the About page says, and the counts `wt report usage` reads as
-- the analyst role. `route` is the matched template, so it never holds an answer id, an invite
-- token or a query string. No function here returns question text or a tester's id.

create table page_view (
  id bigint generated always as identity primary key,
  tester_id uuid not null references tester(id) on delete cascade,
  method text not null,
  route text not null,
  status smallint not null,
  ms integer not null,
  created_at timestamptz not null default now()
);
create index page_view_created_idx on page_view (created_at);
create index page_view_tester_idx on page_view (tester_id, created_at);
alter table page_view owner to wt_owner;

grant insert on page_view to wt_web;

-- The hourly purge deletes rows older than the retention. Returns how many went.
create function purge_page_views(retention interval) returns integer
  language plpgsql security definer set search_path = public as
$$
declare
  removed integer;
begin
  delete from page_view where created_at < now() - retention;
  get diagnostics removed = row_count;
  return removed;
end
$$;
alter function purge_page_views(interval) owner to wt_owner;
revoke all on function purge_page_views(interval) from public;
grant execute on function purge_page_views(interval) to wt_web;

-- One row per tester other than `eval`, active or not, by label. A visit is a run of page views
-- with no gap over 30 minutes. `opened` is whether the tester ever used the invite.
create function usage_testers(since timestamptz)
  returns table (label text, revoked boolean, invited timestamptz, last_seen timestamptz,
                 opened boolean, page_views integer, visits integer, days_active integer,
                 first_view timestamptz, last_view timestamptz, questions integer,
                 answered integer, flagged integer, feedback integer, dana_clicks integer)
  language sql stable security definer set search_path = public as
$$
  with pv as (
    select p.tester_id, p.created_at,
           lag(p.created_at) over (partition by p.tester_id order by p.created_at) as prev_at
    from page_view p
    where p.created_at >= since
  ),
  views as (
    select tester_id,
           count(*)::int as page_views,
           count(*) filter (where prev_at is null
                              or created_at - prev_at > interval '30 minutes')::int as visits,
           min(created_at) as first_view,
           max(created_at) as last_view
    from pv group by tester_id
  ),
  asked as (
    select q.tester_id,
           count(*)::int as questions,
           count(*) filter (where a.id is not null or q.purged_answer_id is not null)::int
             as answered,
           count(*) filter (where q.flagged)::int as flagged
    from query_log q left join answer a on a.query_log_id = q.id
    where q.created_at >= since
    group by q.tester_id
  ),
  days as (
    select tester_id, count(distinct (created_at at time zone 'UTC')::date)::int as days_active
    from (select tester_id, created_at from page_view where created_at >= since
          union all
          select tester_id, created_at from query_log where created_at >= since) d
    group by tester_id
  ),
  fb as (
    select tester_id, count(distinct submission_id)::int as feedback
    from feedback
    where created_at >= since and target <> 'query'
    group by tester_id
  ),
  clicks as (
    select q.tester_id, count(*)::int as dana_clicks
    from dana_event d
    join answer a on a.id = d.answer_id
    join query_log q on q.id = a.query_log_id
    where d.kind = 'link_click' and d.created_at >= since
    group by q.tester_id
  )
  select t.label, t.revoked_at is not null, t.created_at, t.last_seen_at,
         t.last_seen_at is not null
           or exists (select 1 from page_view p where p.tester_id = t.id),
         coalesce(v.page_views, 0), coalesce(v.visits, 0), coalesce(dy.days_active, 0),
         v.first_view, v.last_view,
         coalesce(a.questions, 0), coalesce(a.answered, 0), coalesce(a.flagged, 0),
         coalesce(f.feedback, 0), coalesce(c.dana_clicks, 0)
  from tester t
  left join views v on v.tester_id = t.id
  left join asked a on a.tester_id = t.id
  left join days dy on dy.tester_id = t.id
  left join fb f on f.tester_id = t.id
  left join clicks c on c.tester_id = t.id
  where t.label <> 'eval'
  order by coalesce(a.questions, 0) desc, coalesce(v.page_views, 0) desc, t.label
$$;

-- Page views per method and route template, the testers who opened them, and server errors.
create function usage_routes(since timestamptz)
  returns table (method text, route text, page_views integer, testers integer, errors integer)
  language sql stable security definer set search_path = public as
$$
  select p.method, p.route, count(*)::int, count(distinct p.tester_id)::int,
         count(*) filter (where p.status >= 500)::int
  from page_view p join tester t on t.id = p.tester_id
  where p.created_at >= since and t.label <> 'eval'
  group by p.method, p.route
  order by count(*) desc, p.route, p.method
$$;

-- Per UTC day: testers with a page view or a question, page views and questions.
create function usage_days(since timestamptz)
  returns table (day date, active_testers integer, page_views integer, questions integer)
  language sql stable security definer set search_path = public as
$$
  with events as (
    select p.tester_id, (p.created_at at time zone 'UTC')::date as day, 1 as view, 0 as asked
    from page_view p join tester t on t.id = p.tester_id
    where p.created_at >= since and t.label <> 'eval'
    union all
    select q.tester_id, (q.created_at at time zone 'UTC')::date, 0, 1
    from query_log q join tester t on t.id = q.tester_id
    where q.created_at >= since and t.label <> 'eval'
  )
  select day, count(distinct tester_id)::int, sum(view)::int, sum(asked)::int
  from events group by day order by day
$$;

do $$
declare
  f text;
begin
  foreach f in array array['usage_testers(timestamptz)', 'usage_routes(timestamptz)',
                           'usage_days(timestamptz)']
  loop
    execute format('alter function %s owner to wt_owner', f);
    execute format('revoke all on function %s from public', f);
    execute format('grant execute on function %s to wt_analyst', f);
  end loop;
end
$$;
