-- Spec 02 and Spec 06: the web role flags one query through a function.
--
-- A column grant on query_log.flagged could not flag a single row, because `where id = $1`
-- needs select on id, and it could not set flagged_at. flag_query runs with its owner's
-- rights, sets both on one row, and returns whether the row exists, so wt_web still never
-- reads query_log. The direct column grant goes, leaving the function as the only route.

create function flag_query(query_id uuid) returns boolean
  language sql security definer set search_path = public as
  $$ with flagged as (
       update query_log set flagged = true, flagged_at = coalesce(flagged_at, now())
       where id = query_id
       returning 1)
     select exists (select 1 from flagged) $$;
alter function flag_query(uuid) owner to wt_owner;
revoke all on function flag_query(uuid) from public;
grant execute on function flag_query(uuid) to wt_web;

revoke update (flagged) on query_log from wt_web;
