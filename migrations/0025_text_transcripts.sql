-- A transcript built from a source's own text alone, with no recording to time it or tell its
-- speakers apart (Spec 04, Text-only source transcripts; owner's decision of October 8, 2026).
alter table transcript drop constraint transcript_origin_check;
alter table transcript add constraint transcript_origin_check
  check (origin in ('captions','asr','reviewed','aligned','text'));
