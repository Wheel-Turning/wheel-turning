-- EngageWisdom talk collections (Spec 03, EngageWisdom sources; decision of October 7, 2026).

alter type source_platform add value 'engagewisdom';

-- A transcript whose words are the source's own text, timed by recognition and diarization
-- (Spec 04, Aligned source transcripts).
alter table transcript drop constraint transcript_origin_check;
alter table transcript add constraint transcript_origin_check
  check (origin in ('captions','asr','reviewed','aligned'));
