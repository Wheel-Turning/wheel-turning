-- Spec 02: the shared updated_at trigger and the append-only guards.

create function set_updated_at() returns trigger
  language plpgsql as
$$
begin
  new.updated_at := now();
  return new;
end;
$$;

-- every mutable table with an updated_at column
create trigger sangha_set_updated_at before update on sangha
  for each row execute function set_updated_at();
create trigger teacher_set_updated_at before update on teacher
  for each row execute function set_updated_at();
create trigger talk_set_updated_at before update on talk
  for each row execute function set_updated_at();
create trigger job_set_updated_at before update on job
  for each row execute function set_updated_at();

create function reject_append_only_change() returns trigger
  language plpgsql as
$$
begin
  raise exception '% is append-only: % is not allowed', tg_table_name, lower(tg_op)
    using errcode = 'restrict_violation';
end;
$$;

create trigger speaker_consent_append_only before update or delete on speaker_consent
  for each row execute function reject_append_only_change();
create trigger correction_log_append_only before update or delete on correction_log
  for each row execute function reject_append_only_change();
create trigger takedown_log_append_only before update or delete on takedown_log
  for each row execute function reject_append_only_change();

-- row triggers do not fire on truncate, so guard that too
create trigger speaker_consent_no_truncate before truncate on speaker_consent
  for each statement execute function reject_append_only_change();
create trigger correction_log_no_truncate before truncate on correction_log
  for each statement execute function reject_append_only_change();
create trigger takedown_log_no_truncate before truncate on takedown_log
  for each statement execute function reject_append_only_change();
