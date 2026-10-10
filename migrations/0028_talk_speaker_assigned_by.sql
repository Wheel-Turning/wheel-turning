-- Who assigned a talk's speaker by hand (Spec 03, Assigning a speaker by hand; decision of
-- October 9, 2026). Null means the alias registry resolved it, or nothing has yet. Classification
-- leaves a hand-assigned speaker alone, so `wt ingest reclassify` never undoes a person's
-- decision. Table grants on talk cover the new column.
alter table talk add column speaker_assigned_by text;
