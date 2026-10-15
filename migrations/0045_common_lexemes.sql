-- Spec 05, step 2, owner's request of October 15, 2026: the full-text search leaves out the
-- words most passages contain. ORing "know" (52% of passages), "practice" (29%) or "feel" (17%)
-- made every question rank most of the collection: 1.1 to 1.6 s on RDS, 5 s cold. Judged by the
-- ranking model over 40 eval questions, dropping lexemes in more than 10% of passages put 13%
-- more strongly relevant passages (score 7+) among the fused candidates, at about a quarter of
-- the time.
--
-- The web role cannot read pg_stats for passage, so this function, owned by wt_owner, returns
-- each lexeme ANALYZE found in more than max_share of passages, with its share. It returns
-- nothing while the table has fewer than min_rows rows, where the statistics say little.

create function common_lexemes(max_share real, min_rows bigint)
  returns table (lexeme text, share real)
  language sql stable security definer set search_path = public, pg_catalog as
$$
  select u.lexeme, u.share
  from pg_stats s
  cross join lateral unnest(s.most_common_elems::text::text[], s.most_common_elem_freqs)
       as u(lexeme, share)
  where s.schemaname = 'public' and s.tablename = 'passage' and s.attname = 'tsv'
    and u.lexeme is not null and u.share > max_share
    and (select c.reltuples from pg_class c where c.oid = 'public.passage'::regclass) >= min_rows
$$;

alter function common_lexemes(real, bigint) owner to wt_owner;
revoke all on function common_lexemes(real, bigint) from public;
grant execute on function common_lexemes(real, bigint) to wt_web, wt_pipeline, wt_analyst;
