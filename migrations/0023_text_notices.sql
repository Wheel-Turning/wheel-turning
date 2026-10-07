-- The notice scan (Spec 08, Source policy for URL ingestion; Mandated sources): copyright notices
-- found in an edition that name a holder its evidence does not cover. Under a mandated source
-- whose notice_action is `flag` the edition loads and `wt source report` lists the notice;
-- under `hold` it loads quarantined. Without a source, or under `block`, it is refused.

alter table text_edition add column notice text;
