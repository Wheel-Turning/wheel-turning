-- Spec 02: extensions and enums.

create extension if not exists vector;
create extension if not exists pg_trgm;

create type consent_status as enum ('unknown','requested','granted','declined','withdrawn');
create type source_platform as enum ('youtube','storage','rss');
create type talk_type as enum ('dharma_talk','class','retreat_talk','service','non_talk','other');
create type talk_state as enum (
  'discovered','needs_review','awaiting_consent','cleared',
  'fetching','transcribing','diarizing','segmenting','correcting',
  'extracting','resolving','indexing','ready',
  'quarantined','excluded','withdrawn');
create type segment_kind as enum ('talk','chant','announcement','sitting','qa_teacher','qa_student','other');
create type confidence as enum ('resolved','probable','unresolved');
create type job_state as enum ('queued','running','succeeded','failed','dead');
create type rights_basis as enum ('speaker_consent','license');
