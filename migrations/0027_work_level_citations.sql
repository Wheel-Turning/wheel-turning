-- Work-level citations (Spec 08, Work-level citations; decision of October 9, 2026): every
-- work has one locator '*' standing for the work as a whole, labeled with its title, which the
-- resolver uses for a citation that names the work and no place in it.
alter table citation drop constraint citation_method_check;
alter table citation add constraint citation_method_check
  check (method in ('hand_list','series_label','llm','work_name'));

-- However a work is created (text import, link-out, a locator list), it gets its work-level
-- locator, and the label follows a renamed title. Aliases are set by the commands.
create function ensure_work_level_locator() returns trigger
language plpgsql as $$
begin
  insert into work_locator (work_id, locator, label)
  values (new.id, '*', new.title)
  on conflict (work_id, locator) do update set label = excluded.label;
  return new;
end $$;

create trigger work_level_locator after insert or update of title on work
  for each row execute function ensure_work_level_locator();
alter function ensure_work_level_locator() owner to wt_owner;

insert into work_locator (work_id, locator, label)
select id, '*', title from work
on conflict (work_id, locator) do nothing;
