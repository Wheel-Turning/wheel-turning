-- Spec 05, step 2, owner's request of October 15, 2026 (answers under 5 seconds without losing
-- recall). The vector search walks an HNSW index over the embeddings cast to half precision. On
-- the 143,223 servable passages it is 372 MB against 1,117 MB for the full-precision index, so
-- it stays in RDS's buffer cache (about 1 GB on db.t4g.medium), where the full one was read from
-- disk on most questions (2 to 7 s). Over 119 eval questions at ef_search 1000, recall@60 against
-- an exact search was 0.984 against 0.986, and the warm median 72 ms against 125 ms.
--
-- The stored column stays full precision; only the index and the query's cast change. The old
-- index has no other reader (named mode scans one teacher's passages exactly), so it goes.

set local maintenance_work_mem = '1GB';

create index passage_embedding_half_hnsw on passage
  using hnsw ((embedding::halfvec(1024)) halfvec_cosine_ops) with (m = 16, ef_construction = 64);

drop index passage_embedding_hnsw;
