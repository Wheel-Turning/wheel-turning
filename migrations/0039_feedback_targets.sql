-- Spec 06, Logging, rule 4, as the owner decided on October 12, 2026: feedback on an excerpt
-- or on the summary takes several reasons and a note in one submission.
--
-- Each row now says what it is about (target), which passage for an excerpt, and which
-- submission it came in. Earlier rows kept the passage id in note; it moves to passage_id.

alter table feedback add column target text;
alter table feedback add column passage_id uuid references passage(id) on delete set null;
alter table feedback add column submission_id uuid;

update feedback set target = 'query' where kind = 'flag_query';
update feedback
   set target = 'excerpt',
       passage_id = case
         when note ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
              and exists (select 1 from passage p where p.id = note::uuid)
         then note::uuid
       end,
       note = null
 where kind <> 'flag_query';
update feedback set submission_id = id;

alter table feedback alter column target set not null;
alter table feedback alter column submission_id set not null;
alter table feedback alter column submission_id set default gen_random_uuid();

alter table feedback drop constraint feedback_kind_check;
alter table feedback add constraint feedback_kind_check check (
  (target = 'query' and kind = 'flag_query')
  or (target = 'excerpt' and kind in
      ('good','not_relevant','out_of_context','not_faithful','wrong_details','unclear','note'))
  or (target = 'summary' and kind in
      ('good','unsupported','misrepresents','missing','unclear','note'))
);
alter table feedback add constraint feedback_note_length
  check (note is null or char_length(note) <= 1000);
alter table feedback add constraint feedback_note_only_on_note
  check (kind = 'note' or note is null);

create index feedback_submission_idx on feedback (submission_id);

-- Counts only: submissions and reasons per day, target, reason and the question's privacy
-- mode. No note and no question text.
create view v_feedback_counts as
select date_trunc('day', f.created_at) as day, f.target, f.kind, q.privacy_mode,
       count(*) as rows,
       count(distinct f.submission_id) as submissions
from feedback f
join answer a on a.id = f.answer_id
join query_log q on q.id = a.query_log_id
join tester t on t.id = f.tester_id
where t.label <> 'eval'
group by 1, 2, 3, 4;

alter view v_feedback_counts owner to wt_owner;
grant select on v_feedback_counts to wt_analyst;

-- Notes, with their target and the reasons sent with them, never the question: a question is
-- read only through flagged_queries().
create function feedback_notes(since timestamptz)
  returns table (created_at timestamptz, target text, reasons text[], note text)
  language sql stable security definer set search_path = public as
$$
  select n.created_at, n.target,
         coalesce(array(select r.kind from feedback r
                         where r.submission_id = n.submission_id and r.kind <> 'note'
                         order by r.kind), '{}'),
         n.note
  from feedback n
  join tester t on t.id = n.tester_id
  where n.kind = 'note' and n.created_at >= since and t.label <> 'eval'
  order by n.created_at
$$;

alter function feedback_notes(timestamptz) owner to wt_owner;
revoke all on function feedback_notes(timestamptz) from public;
grant execute on function feedback_notes(timestamptz) to wt_analyst;
