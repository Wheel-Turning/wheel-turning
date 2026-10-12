-- How a passage's vector was computed, beyond its model (Spec 04, Chunking, embedding and
-- indexing; decision of October 12, 2026). The index step now keeps a passage whose segment and
-- content hash are unchanged, with its id, and re-embeds it only when its model or this version
-- is not the embedder's. Passages stored before this column cannot say which version made them,
-- so they start at 1 and are re-embedded once, the next time their talk is indexed. Table
-- grants on passage cover the new column.
alter table passage add column vector_version integer not null default 1;
