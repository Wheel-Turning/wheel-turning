-- How a text unit's vector was computed, beyond its model (Spec 04, Chunking, embedding and
-- indexing; decision of October 9, 2026). Version 1 embedded units in batches of 16, where the
-- int8 model's dynamic quantization made a vector depend on its neighbours; version 2 embeds one
-- text per run. `wt text reembed` re-embeds every unit below `wt_ml::VECTOR_VERSION` from its
-- stored text. Table grants on text_unit cover the new column.
alter table text_unit add column vector_version integer not null default 1;
