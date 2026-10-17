-- Spec 11, PB2.4 (B4): every answer records how its summary settled, so no summary fails
-- without a trace. The web role writes the status with the answer; a deferred summary starts
-- as 'pending' and set_answer_summary_status settles it. The hourly purge marks any answer still
-- pending after ten minutes 'deferred_lost', so none stays pending. Answers stored before this
-- migration get a status only where the payload shows it: a summary is 'shown', and one left
-- pending for over ten minutes is 'deferred_lost'.

alter table answer
  add column summary_status text
    check (summary_status in ('pending', 'shown', 'not_attempted', 'timeout', 'model_error',
                              'cap_reached', 'all_sentences_rejected', 'no_key', 'deferred_lost'));

update answer
   set summary_status = case when payload ? 'summary' then 'shown' else 'deferred_lost' end,
       payload = payload || '{"summary_pending": false}'::jsonb
 where coalesce((payload ->> 'summary_pending')::boolean, false)
   and created_at < now() - interval '10 minutes';

update answer set summary_status = 'shown'
 where summary_status is null and payload ? 'summary';

create index answer_summary_pending on answer (created_at) where summary_status = 'pending';

-- As set_answer_summary (0044), with the status. It changes only an answer still pending, so a
-- summary settles once and a late one never replaces 'deferred_lost'.
create function set_answer_summary_status(answer_id uuid, summary jsonb, shown integer,
                                          dropped integer, status text)
  returns boolean language plpgsql security definer set search_path = public as
$$
begin
  if status = 'pending' then
    raise exception 'a summary cannot settle as pending';
  end if;
  update answer a
     set payload = (case when summary is null then a.payload - 'summary'
                         else jsonb_set(a.payload, '{summary}', summary) end)
                   || '{"summary_pending": false}'::jsonb,
         summary_status = set_answer_summary_status.status
   where a.id = set_answer_summary_status.answer_id
     and a.summary_status = 'pending';
  if not found then
    return false;
  end if;
  update query_log q
     set theme_count = shown, themes_dropped = dropped
    from answer a
   where a.id = set_answer_summary_status.answer_id and q.id = a.query_log_id;
  return true;
end
$$;

alter function set_answer_summary_status(uuid, jsonb, integer, integer, text) owner to wt_owner;
revoke all on function set_answer_summary_status(uuid, jsonb, integer, integer, text) from public;
grant execute on function set_answer_summary_status(uuid, jsonb, integer, integer, text) to wt_web;

-- The purge's step: answers pending for longer than `after` become 'deferred_lost'. Returns
-- their ids, one summary_settled event each. An answer with no status still flagged pending in
-- its payload was stored by a server from before this migration while it rolled out.
create function settle_lost_summaries(after interval) returns setof uuid
  language sql security definer set search_path = public as
$$
  update answer
     set summary_status = 'deferred_lost',
         payload = payload || '{"summary_pending": false}'::jsonb
   where (summary_status = 'pending'
          or (summary_status is null and coalesce((payload ->> 'summary_pending')::boolean, false)))
     and created_at < now() - after
  returning id
$$;

alter function settle_lost_summaries(interval) owner to wt_owner;
revoke all on function settle_lost_summaries(interval) from public;
grant execute on function settle_lost_summaries(interval) to wt_web;
