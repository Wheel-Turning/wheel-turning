-- Spec 03, Mandated talks: the owner's mandate as a rights basis for talks. A new enum value
-- cannot be used in the transaction that adds it, so it has a migration of its own.

alter type rights_basis add value 'mandate';
