-- Spec 02, roles, as the owner decided on October 13, 2026: the web role reads a tester's own
-- answer through functions scoped to that tester, and still cannot select from query_log or
-- answer. Found when the web service first ran as wt_web_login on AWS: the answer page, the
-- feedback, flag and dana routes, and the session check had each read tables wt_web has no
-- grant on, which the owner's alpha never showed because it connected as a superuser.
--
-- Each function runs as wt_owner and returns only what the page needs for one answer and one
-- tester, as flag_query (0013) does for the flag.

-- The query log row behind an answer, when that tester asked it; null otherwise.
create function tester_answer_query(answer_id uuid, tester_id uuid) returns uuid
  language sql stable security definer set search_path = public as
  $$ select q.id from answer a join query_log q on q.id = a.query_log_id
     where a.id = answer_id and q.tester_id = tester_answer_query.tester_id $$;

-- The answer page: the question as kept, its notice category, the payload and when it was
-- asked. No row when the answer is not the tester's.
create function tester_answer(answer_id uuid, tester_id uuid)
  returns table (query_text text, sensitive_category text, payload jsonb, created_at timestamptz)
  language sql stable security definer set search_path = public as
  $$ select q.query_text, q.sensitive_category, a.payload, a.created_at
     from answer a join query_log q on q.id = a.query_log_id
     where a.id = answer_id and q.tester_id = tester_answer.tester_id $$;

-- Whether an answer showed a passage, for feedback on one excerpt.
create function answer_shows_passage(answer_id uuid, passage_id uuid) returns boolean
  language sql stable security definer set search_path = public as
  $$ select exists (select 1 from answer_passage ap
                    where ap.answer_id = answer_shows_passage.answer_id
                      and ap.passage_id = answer_shows_passage.passage_id) $$;

-- Whether a purge removed this tester's answer, so its permalink says the question was not kept.
create function tester_answer_purged(answer_id uuid, tester_id uuid) returns boolean
  language sql stable security definer set search_path = public as
  $$ select exists (select 1 from query_log q
                    where q.purged_answer_id = answer_id
                      and q.tester_id = tester_answer_purged.tester_id) $$;

do $$
declare
  f text;
begin
  foreach f in array array['tester_answer_query(uuid, uuid)', 'tester_answer(uuid, uuid)',
                           'answer_shows_passage(uuid, uuid)', 'tester_answer_purged(uuid, uuid)']
  loop
    execute format('alter function %s owner to wt_owner', f);
    execute format('revoke all on function %s from public', f);
    execute format('grant execute on function %s to wt_web', f);
  end loop;
end
$$;

-- The session check notes when a tester was last seen (Spec 06, Invite token access).
grant update (last_seen_at) on tester to wt_web;

-- Named-teacher detection reads the aliases, which are public names like teacher's.
grant select on teacher_alias to wt_web;
