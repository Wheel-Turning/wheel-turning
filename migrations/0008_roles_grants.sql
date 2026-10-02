-- Spec 02: database roles and grants.
-- Roles are NOLOGIN group roles. `dg db create-login --role <name>` creates a login role that
-- inherits from one and takes its password from Secrets Manager. Passwords never appear here.
-- Roles are cluster-wide, so creation is guarded to stay safe on a cluster that already has them.

do $$
declare
  r text;
begin
  foreach r in array array['dg_owner', 'dg_pipeline', 'dg_web', 'dg_analyst'] loop
    if not exists (select 1 from pg_roles where rolname = r) then
      execute format('create role %I nologin', r);
    end if;
  end loop;
end
$$;

-- The migrating user must be a member of dg_owner to hand objects over to it.
grant dg_owner to current_user;

-- dg_owner owns every object this schema defines. Extension members and the sqlx
-- bookkeeping table keep their owners.
do $$
declare
  obj record;
begin
  for obj in
    select c.oid, c.relname, c.relkind
    from pg_class c
    join pg_namespace n on n.oid = c.relnamespace
    where n.nspname = 'public'
      and c.relkind in ('r', 'v', 'm', 'S', 'p')
      and c.relname <> '_sqlx_migrations'
      and not exists (select 1 from pg_depend d
                      where d.classid = 'pg_class'::regclass and d.objid = c.oid and d.deptype = 'e')
  loop
    -- sequences owned by a table column move with their table
    if obj.relkind = 'S' and exists (select 1 from pg_depend d
                                     where d.classid = 'pg_class'::regclass and d.objid = obj.oid
                                       and d.deptype in ('a', 'i')) then
      continue;
    end if;
    execute format('alter %s public.%I owner to dg_owner',
                   case obj.relkind when 'v' then 'view'
                                    when 'm' then 'materialized view'
                                    when 'S' then 'sequence'
                                    else 'table' end,
                   obj.relname);
  end loop;

  for obj in
    select p.oid
    from pg_proc p
    join pg_namespace n on n.oid = p.pronamespace
    where n.nspname = 'public'
      and not exists (select 1 from pg_depend d
                      where d.classid = 'pg_proc'::regclass and d.objid = p.oid and d.deptype = 'e')
  loop
    execute format('alter function %s owner to dg_owner', obj.oid::regprocedure);
  end loop;

  for obj in
    select t.typname
    from pg_type t
    join pg_namespace n on n.oid = t.typnamespace
    where n.nspname = 'public'
      and t.typtype = 'e'
      and not exists (select 1 from pg_depend d
                      where d.classid = 'pg_type'::regclass and d.objid = t.oid and d.deptype = 'e')
  loop
    execute format('alter type public.%I owner to dg_owner', obj.typname);
  end loop;
end
$$;

grant usage on schema public to dg_pipeline, dg_web, dg_analyst;

-- dg_pipeline: batch host and CLI.
grant select, insert, update, delete on
  source_video, talk, transcript, segment, passage, citation, job,
  glossary_term, sangha, teacher, teacher_alias, license_evidence, work, work_locator
  to dg_pipeline;
grant select on current_consent to dg_pipeline;
grant select, insert on speaker_consent, correction_log, takedown_log to dg_pipeline;

-- dg_web: web service. Passage content only through the serving views.
grant select on servable_passage, servable_citation, teacher, sangha, work, work_locator, tester
  to dg_web;
grant insert on query_log, answer, answer_passage, feedback, dana_event to dg_web;
grant update (flagged) on query_log to dg_web;

-- dg_analyst: aggregate reports only.
grant select on v_answer_stats, v_consent_coverage, v_dana_clicks to dg_analyst;
grant execute on function flagged_queries() to dg_analyst;
