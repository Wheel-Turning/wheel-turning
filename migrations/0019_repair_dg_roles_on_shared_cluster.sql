-- Repair a database migrated on a cluster that already held the wt_* roles.
--
-- Roles are cluster-wide but migrations run per database. On a second database in a cluster
-- where 0010 already renamed the roles, 0008 finds no dg_* role and creates them again, gives
-- every object from 0001 to 0006 to dg_owner and grants to dg_pipeline, dg_web and dg_analyst,
-- and 0010 cannot rename them because the wt_* names are taken. Objects from 0013 on belong to
-- wt_owner, which then has no rights on the dg_owner tables they read (servable_passage failed
-- with "permission denied for table rights_setting").
--
-- For each dg_* role this hands what it owns in this database to its wt_* counterpart, copies
-- every privilege it holds here to the counterpart and revokes it, and drops the dg_* role once
-- nothing in the cluster depends on it. It leaves the database as 0001 to 0018 leave a database
-- on a fresh cluster. On a database without dg_* roles it changes nothing.

do $$
declare
  pair text[];
  old_role text;
  new_role text;
  old_oid oid;
  obj record;
begin
  foreach pair slice 1 in array array[
    ['dg_owner', 'wt_owner'],
    ['dg_pipeline', 'wt_pipeline'],
    ['dg_web', 'wt_web'],
    ['dg_analyst', 'wt_analyst']
  ] loop
    old_role := pair[1];
    new_role := pair[2];
    select oid into old_oid from pg_roles where rolname = old_role;
    continue when old_oid is null;
    continue when not exists (select 1 from pg_roles where rolname = new_role);

    -- Ownership, only when the role owns something here: REASSIGN OWNED needs membership in
    -- both roles even when there is nothing to move.
    if exists (select 1 from pg_shdepend
               where refclassid = 'pg_authid'::regclass and refobjid = old_oid and deptype = 'o'
                 and dbid in (0, (select oid from pg_database where datname = current_database()))) then
      if not pg_has_role(current_user, new_role, 'MEMBER') then
        execute format('grant %I to current_user', new_role);
      end if;
      execute format('reassign owned by %I to %I', old_role, new_role);
    end if;

    -- Privileges. Each statement names the object and, for column privileges, the column.
    for obj in
      select format('on %s %s', case c.relkind when 'S' then 'sequence' else 'table' end,
                    c.oid::regclass) as target,
             null::text as col, a.privilege_type, a.is_grantable
      from pg_class c, aclexplode(c.relacl) a
      where a.grantee = old_oid
      union all
      select format('on table %s', c.oid::regclass), quote_ident(att.attname),
             a.privilege_type, a.is_grantable
      from pg_attribute att join pg_class c on c.oid = att.attrelid, aclexplode(att.attacl) a
      where att.attnum > 0 and not att.attisdropped and a.grantee = old_oid
      union all
      select format('on routine %s', p.oid::regprocedure), null, a.privilege_type, a.is_grantable
      from pg_proc p, aclexplode(p.proacl) a
      where a.grantee = old_oid
      union all
      select format('on schema %I', n.nspname), null, a.privilege_type, a.is_grantable
      from pg_namespace n, aclexplode(n.nspacl) a
      where a.grantee = old_oid
      union all
      select format('on type %s', t.oid::regtype), null, a.privilege_type, a.is_grantable
      from pg_type t, aclexplode(t.typacl) a
      where a.grantee = old_oid
      union all
      select format('on database %I', d.datname), null, a.privilege_type, a.is_grantable
      from pg_database d, aclexplode(d.datacl) a
      where d.datname = current_database() and a.grantee = old_oid
    loop
      execute format('grant %s%s %s to %I%s',
                     obj.privilege_type,
                     coalesce(' (' || obj.col || ')', ''),
                     obj.target,
                     new_role,
                     case when obj.is_grantable then ' with grant option' else '' end);
      execute format('revoke %s%s %s from %I cascade',
                     obj.privilege_type,
                     coalesce(' (' || obj.col || ')', ''),
                     obj.target,
                     old_role);
    end loop;

    -- The role goes once no database in the cluster depends on it. Another database still to be
    -- repaired keeps it, and that database's own run of this migration drops it.
    begin
      execute format('drop role %I', old_role);
    exception when dependent_objects_still_exist or insufficient_privilege then
      raise notice 'kept role %: %', old_role, sqlerrm;
    end;
  end loop;
end
$$;

-- 0013 revoked this column grant from wt_web, which on a repaired database did not hold it yet:
-- dg_web did, and the copy above has just given it to wt_web. On every other database this is
-- a no-op.
revoke update (flagged) on query_log from wt_web;
