-- Spec 11, PB1.4: a read-only group role for the nightly audit on RDS. `wt db create-login
-- --role wt_audit` creates `wt_audit_login`, whose password Terraform keeps as
-- wt-v1/db_wt_audit_login. The role reads what the audit checks and nothing testers wrote:
-- their questions, answers, feedback and accounts stay out, and of a removal request it sees
-- only what the overdue check reads, not who sent it.
--
-- Grants are explicit rather than default privileges, so a table added later is not readable
-- until its migration grants it. The audit test runs every check as a wt_audit login and fails
-- on a missing grant.

do $$
begin
  if not exists (select 1 from pg_roles where rolname = 'wt_audit') then
    create role wt_audit nologin;
  end if;
end
$$;

grant usage on schema public to wt_audit;

do $$
declare
  obj record;
begin
  for obj in
    select c.relname
    from pg_class c
    join pg_namespace n on n.oid = c.relnamespace
    where n.nspname = 'public'
      and c.relkind in ('r', 'v', 'm', 'p')
      and c.relname not in (
        '_sqlx_migrations',
        -- what testers wrote or did
        'tester', 'query_log', 'analyzable_query_log', 'v_query_triage',
        'answer', 'answer_passage', 'feedback', 'dana_event',
        -- read by column below
        'takedown_request')
      and not exists (select 1 from pg_depend d
                      where d.classid = 'pg_class'::regclass and d.objid = c.oid and d.deptype = 'e')
  loop
    execute format('grant select on public.%I to wt_audit', obj.relname);
  end loop;
end
$$;

grant select (id, status, received_at, verified_at) on takedown_request to wt_audit;
