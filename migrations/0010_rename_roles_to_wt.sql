-- Rename the Spec 02 group roles from the old project prefix to wt_ (docs/naming.md).
-- Grants, ownership and memberships follow a role through a rename, so every object
-- 0001 to 0009 created stays owned by and granted to the same role under its new name.
--
-- Roles are cluster-wide, so each rename is guarded: it runs only when the old role exists and
-- the new one does not, which keeps the migration safe on a cluster where it already ran.
--
-- Login roles are not renamed here. They are created after migrations by `wt db create-login`
-- and start each session with SET ROLE <group>, a setting stored by name; a login created
-- before this migration must be recreated afterwards. The v1 rebuild runs this migration on a
-- fresh database before any login exists.

do $$
declare
  pair text[];
begin
  foreach pair slice 1 in array array[
    ['dg_owner', 'wt_owner'],
    ['dg_pipeline', 'wt_pipeline'],
    ['dg_web', 'wt_web'],
    ['dg_analyst', 'wt_analyst']
  ] loop
    if exists (select 1 from pg_roles where rolname = pair[1])
       and not exists (select 1 from pg_roles where rolname = pair[2]) then
      execute format('alter role %I rename to %I', pair[1], pair[2]);
    end if;
  end loop;
end
$$;
