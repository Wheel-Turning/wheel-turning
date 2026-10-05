-- Spec 02: let vector search on servable_passage use the HNSW index.
--
-- A security_barrier view is planned as a subquery, so `order by embedding <=> $1 limit k`
-- cannot reach passage_embedding_hnsw and every served passage is sorted instead. The barrier
-- only matters when a role can attach its own functions to a query on the view. wt_web runs
-- fixed queries from application code and has no grant to create functions, so it gave
-- little protection. The view's filter is unchanged.

alter view servable_passage reset (security_barrier);
