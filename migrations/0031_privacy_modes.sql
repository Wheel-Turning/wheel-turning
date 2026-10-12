-- Spec 06, Privacy modes for questions: each tester chooses whether their questions are kept
-- (share, with obvious personal details removed, for WT_QUERY_RETENTION_DAYS) or kept only for
-- WT_COUNT_ONLY_TTL_HOURS (count_only). Expired text goes, with its answer; the counts stay.

alter table tester add column privacy_mode text not null default 'share'
  check (privacy_mode in ('share','count_only'));
alter table tester add column privacy_chosen boolean not null default false;

alter table query_log alter column query_text drop not null;
alter table query_log add column privacy_mode text not null default 'share'
  check (privacy_mode in ('share','count_only'));
alter table query_log add column text_expires_at timestamptz;
create index query_log_expiry_idx on query_log (text_expires_at) where query_text is not null;
-- The answer a purge removed, so its permalink can say "This question was not kept." rather
-- than "That page does not exist."
alter table query_log add column purged_answer_id uuid;
create index query_log_purged_answer_idx on query_log (purged_answer_id)
  where purged_answer_id is not null;

-- Link clicks survive a purge, so the click counts the v1 tests depend on are not lost.
alter table dana_event drop constraint dana_event_answer_id_fkey;
alter table dana_event alter column answer_id drop not null;
alter table dana_event add constraint dana_event_answer_id_fkey
  foreign key (answer_id) references answer(id) on delete set null;

-- The nightly purge. An unflagged question goes when its text expires; a flagged one keeps its
-- text for the full retention period from when it was asked, in either mode. The answer row
-- goes with the text, and its passage list and feedback with it by cascade; the query_log
-- row and its counts stay. Returns how many questions were purged.
create function purge_expired_queries(retention interval) returns integer
  language plpgsql security definer set search_path = public as
$$
declare
  purged integer;
begin
  with expired as (
    select q.id, a.id as answer_id
    from query_log q
    left join answer a on a.query_log_id = q.id
    where q.query_text is not null
      and ((not q.flagged and q.text_expires_at <= now())
           or (q.flagged and q.created_at + retention <= now()))
  ),
  gone as (
    delete from answer a using expired e where a.id = e.answer_id
  )
  update query_log q
     set query_text = null,
         purged_answer_id = coalesce(e.answer_id, q.purged_answer_id)
    from expired e
   where q.id = e.id;
  get diagnostics purged = row_count;
  return purged;
end
$$;
alter function purge_expired_queries(interval) owner to wt_owner;
revoke all on function purge_expired_queries(interval) from public;
grant execute on function purge_expired_queries(interval) to wt_web, wt_pipeline;

-- "Delete all my questions now": every question, answer, feedback row and dana event that
-- belongs to one tester, and nothing of anyone else's. Returns how many questions went.
create function delete_tester_questions(who uuid) returns integer
  language plpgsql security definer set search_path = public as
$$
declare
  removed integer;
begin
  delete from dana_event d
   using answer a, query_log q
   where d.answer_id = a.id and a.query_log_id = q.id and q.tester_id = who;
  delete from feedback where tester_id = who;
  delete from query_log where tester_id = who;
  get diagnostics removed = row_count;
  return removed;
end
$$;
alter function delete_tester_questions(uuid) owner to wt_owner;
revoke all on function delete_tester_questions(uuid) from public;
grant execute on function delete_tester_questions(uuid) to wt_web;

-- The switch saves the tester's own choice.
grant update (privacy_mode, privacy_chosen) on tester to wt_web;
