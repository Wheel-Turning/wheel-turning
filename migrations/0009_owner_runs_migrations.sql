-- Spec 01: dg_owner runs migrations from CI. The first migrations run as the database's
-- bootstrap user (the RDS master user, or the local superuser); from here on a login that
-- inherits dg_owner runs them, so dg_owner needs to create objects in the schema and to keep
-- sqlx's bookkeeping table.

grant usage, create on schema public to dg_owner;
grant select, insert, update, delete on table public._sqlx_migrations to dg_owner;
