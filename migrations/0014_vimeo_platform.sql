-- Spec 03, Seattle Soto Zen sources: Vimeo is SSZ's canonical list, so source_video needs the
-- platform. A new enum value cannot be used in the transaction that adds it, so this migration
-- holds nothing else.

alter type source_platform add value 'vimeo';
