-- Spec 04, Student questions (owner's decision of October 10, 2026): each student question that
-- a teacher answers is kept only in a lightly edited form, attributed to "Student", embedded for
-- search, and served through a view with servable_passage's conditions.

create table student_question (
  id uuid primary key default gen_random_uuid(),
  talk_id uuid not null references talk(id) on delete cascade,
  transcript_id uuid not null references transcript(id) on delete cascade,
  start_ms integer not null,
  end_ms integer not null,
  answer_start_ms integer not null,
  text text not null check (char_length(text) between 3 and 1200),
  word_count integer not null,
  content_hash text not null,
  embedding vector(1024) not null,
  embedding_model text not null,
  edit_model text not null,
  created_at timestamptz not null default now(),
  unique (talk_id, start_ms)
);
create index student_question_embedding_hnsw on student_question
  using hnsw (embedding vector_cosine_ops) with (m = 16, ef_construction = 64);

-- The same talk, teacher, sangha and rights conditions as servable_passage (0019), as plain
-- column conditions so vector search keeps the HNSW index.
create view servable_student_question as
select q.id as question_id, q.text, q.word_count, q.start_ms, q.end_ms, q.answer_start_ms,
       q.embedding, q.embedding_model,
       t.id as talk_id, te.id as teacher_id, s.id as sangha_id,
       case when te.display_level = 'excerpts' or t.display_level = 'excerpts' then 'excerpts'
            else 'full' end as display_level
from student_question q
join talk t on t.id = q.talk_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
left join license_evidence le on le.id = t.license_evidence_id
where t.state = 'ready'
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and te.opted_out_at is null
  and te.display_level in ('excerpts','full')
  and (t.display_level is null or t.display_level = 'excerpts')
  and (
    (t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and le.allowed)
    or (t.rights_basis = 'mandate' and le.allowed
        and (select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null
             or exists (select 1 from source_policy sp where sp.key = t.source_key and sp.enabled)))
  );

alter table student_question owner to wt_owner;
alter view servable_student_question owner to wt_owner;

grant select, insert, delete on student_question to wt_pipeline;
grant select on servable_student_question to wt_pipeline, wt_web;
