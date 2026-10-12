-- Spec 02, Donation links and verification: what kind of link a sangha has, a note on it, and
-- who checked it and when. Only a person verifies a link, and only a giving page can be
-- verified. Every link stored before this migration is unverified, so the check holds at once.

alter table sangha add column donation_kind text not null default 'none'
  check (donation_kind in ('page', 'contact', 'none'));
alter table sangha add column donation_note text;
alter table sangha add column donation_checked_at timestamptz;
alter table sangha add column donation_checked_by text;

-- The links stored so far are the giving pages Spec 02 lists.
update sangha set donation_kind = 'page' where donation_url is not null;

alter table sangha add constraint donation_verified_needs_page
  check (not donation_verified or (donation_kind = 'page' and donation_url is not null));
