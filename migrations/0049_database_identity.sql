-- Spec 11, PB1.1: a database knows its own name. One row, written once at setup (`rds-v1` on
-- RDS, `pc` on the owner's PC) by `wt db set-identity`. `wt worker`, `wt pipeline`, every
-- rights command and `wt takedown` read it at start, print it, and refuse to run when it differs
-- from WT_DATABASE_NAME, so a command meant for one database never writes to the other.

create table database_identity (
  only_row boolean primary key default true check (only_row),
  name text not null check (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
  created_at timestamptz not null default now()
);

-- Written once: a renamed database would defeat the check.
create trigger database_identity_append_only before update or delete on database_identity
  for each row execute function reject_append_only_change();
create trigger database_identity_no_truncate before truncate on database_identity
  for each statement execute function reject_append_only_change();

alter table database_identity owner to wt_owner;
grant select on database_identity to wt_pipeline, wt_web, wt_analyst;
