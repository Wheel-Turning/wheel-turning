-- Spec 06, Logging, rule 4, as the owner decided on October 15, 2026: the answer page has one
-- feedback control at its foot, "Feedback on these answers", in place of one per excerpt and one
-- on the summary. Its rows have target 'answer'. Rows already written keep their targets.

alter table feedback drop constraint feedback_kind_check;
alter table feedback add constraint feedback_kind_check check (
  (target = 'query' and kind = 'flag_query')
  or (target = 'excerpt' and kind in
      ('good','not_relevant','out_of_context','not_faithful','wrong_details','unclear','note'))
  or (target = 'summary' and kind in
      ('good','unsupported','misrepresents','missing','unclear','note'))
  or (target = 'answer' and kind in
      ('good','not_relevant','unsupported','out_of_context','not_faithful','wrong_details',
       'unclear','note'))
);
