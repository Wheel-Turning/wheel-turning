-- Spec 10, D3 (owner's decision of November 12, 2026): pinned answers. The owner pins an
-- answer to a question they write themselves, such as a home page example. Any tester who asks
-- that question, matched after normalization, gets a new answer of their own that reuses the
-- pinned passages, excerpts and summary, with no retrieval, ranking or model call. The question
-- text here is the owner's, never a tester's. A pin whose passages do not all still serve is
-- skipped and the question runs normally; a takedown deletes the pins that cite what it removes.

create table pinned_answer (
  question_key text primary key check (question_key <> ''),
  question_text text not null,
  payload jsonb not null,
  source_answer_id uuid,
  pinned_by text not null,
  pinned_at timestamptz not null default now()
);
alter table pinned_answer owner to wt_owner;

grant select on pinned_answer to wt_web;
