-- A removal request from /rights waits for its sender to confirm their email address before
-- anyone acts on it (owner's decision of October 15, 2026: anyone could otherwise take the
-- whole collection down with forged requests). The web role stores the request as `unverified`
-- with the SHA-256 of a one-time token, emails the token, and confirms through
-- confirm_takedown_request(), the only way it can change a row. The token itself is never
-- stored.

alter table takedown_request drop constraint takedown_request_status_check;
alter table takedown_request add constraint takedown_request_status_check
  check (status in ('unverified','open','suspended','removed','declined'));
alter table takedown_request alter column status set default 'unverified';

alter table takedown_request add column verification_token_hash bytea unique;
alter table takedown_request add column verification_expires_at timestamptz
  default now() + interval '24 hours';
alter table takedown_request add column verified_at timestamptz;

-- Requests from before this migration were acted on without confirmation; they keep their
-- status and have no token.
update takedown_request set verification_expires_at = null where status <> 'unverified';

-- The web role names only what the sender typed and the token's hash. Status, expiry and
-- confirmation come from the defaults and the function, so it cannot store a confirmed request.
revoke insert on takedown_request from wt_web;
grant insert (requester_name, requester_email, target, message, text_unit_id,
              verification_token_hash) on takedown_request to wt_web;

-- Confirms the request whose token hashes to `token_hash`, once and before it expires. Returns
-- whether one was confirmed.
create function confirm_takedown_request(token_hash bytea) returns boolean
  language sql security definer set search_path = public as
  $$ with confirmed as (
       update takedown_request
       set status = 'open', verified_at = now(), verification_token_hash = null
       where verification_token_hash = token_hash
         and status = 'unverified'
         and verification_expires_at > now()
       returning 1)
     select exists (select 1 from confirmed) $$;
alter function confirm_takedown_request(bytea) owner to wt_owner;
revoke all on function confirm_takedown_request(bytea) from public;
grant execute on function confirm_takedown_request(bytea) to wt_web;

-- Deletes requests nobody confirmed, a week after their link expired, so an address typed by
-- someone else does not stay on file. Returns how many went.
create function purge_unverified_takedown_requests() returns integer
  language plpgsql security definer set search_path = public as
$$
declare
  purged integer;
begin
  delete from takedown_request
  where status = 'unverified' and verification_expires_at + interval '7 days' <= now();
  get diagnostics purged = row_count;
  return purged;
end
$$;
alter function purge_unverified_takedown_requests() owner to wt_owner;
revoke all on function purge_unverified_takedown_requests() from public;
grant execute on function purge_unverified_takedown_requests() to wt_web;
