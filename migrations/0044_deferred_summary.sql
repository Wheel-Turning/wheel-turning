-- Spec 06, owner's request of October 15, 2026: the answer page comes back before its summary
-- is written, and the summary fills in at the top when it is. The answer is stored with
-- payload.summary_pending = true; when synthesis ends, the web role sets the summary (or none)
-- through this function, which also sets the query log's summary counts. It changes only an
-- answer still pending, so a summary is written once and never replaces another.

create function set_answer_summary(answer_id uuid, summary jsonb, shown integer, dropped integer)
  returns boolean language plpgsql security definer set search_path = public as
$$
begin
  update answer a
     set payload = (case when summary is null then a.payload - 'summary'
                         else jsonb_set(a.payload, '{summary}', summary) end)
                   || '{"summary_pending": false}'::jsonb
   where a.id = set_answer_summary.answer_id
     and coalesce((a.payload ->> 'summary_pending')::boolean, false);
  if not found then
    return false;
  end if;
  update query_log q
     set theme_count = shown, themes_dropped = dropped
    from answer a
   where a.id = set_answer_summary.answer_id and q.id = a.query_log_id;
  return true;
end
$$;

alter function set_answer_summary(uuid, jsonb, integer, integer) owner to wt_owner;
revoke all on function set_answer_summary(uuid, jsonb, integer, integer) from public;
grant execute on function set_answer_summary(uuid, jsonb, integer, integer) to wt_web;
