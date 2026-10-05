-- Spec 02: close a NULL hole in the database half of the consent gate.
--
-- In 0003 the rights clause of past_gate_requires_consent evaluates to NULL, not false, when
-- rights_basis is NULL, and a CHECK accepts NULL. So a talk with consent granted, a known
-- holder and a teacher but no rights basis could reach cleared. coalesce makes an unknown
-- clause a refusal. The rule is otherwise unchanged.

alter table talk drop constraint past_gate_requires_consent;
alter table talk add constraint past_gate_requires_consent check (
  state not in ('cleared','fetching','transcribing','diarizing','segmenting',
                'correcting','extracting','resolving','indexing','ready')
  or coalesce(
    (rights_basis = 'speaker_consent' and consent_status = 'granted'
     or rights_basis = 'license' and license_evidence_id is not null)
    and teacher_id is not null and sangha_id is not null
    and rights_holder is not null and lower(rights_holder) <> 'unknown',
    false)
);
