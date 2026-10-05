# Wheel Turning: Data Quality North Star

Oct 2, 2026 · @Alex

## Purpose

This document defines what good data means for Wheel Turning, where quality is checked, and what happens when it fails. It pairs with the PRD, which sets the product's promises, and with the architectural north star, which sets the structures that enforce them. Those documents say what must be true of the system. This one says what must be true of the corpus the system serves.

Data quality here carries more weight than in most retrieval products. A mislabeled speaker puts one teacher's words under another's name. A mangled term of art changes what a passage says. A wrong donation link sends a gift to the wrong sangha. Each of these is a failure of attribution or faithfulness, which the PRD treats as product failure, so quality is defined by those harms and not by generic completeness scores.

## North star

Every passage shown is a faithful, correctly attributed, rightfully held and current rendering of what a consenting teacher said, and a record that cannot be shown to be so is withheld, not shown with a caveat.

Five properties follow, and every quality check traces back to one of them.

1. Faithful. The text matches the spoken words closely enough that the teacher would recognize them, with terms of art, names and Sanskrit and Japanese words spelled correctly.
2. Correctly attributed. Teacher, sangha, date, series and part are right, and the timestamped link lands on the moment quoted.
3. Rightfully held. A rights basis is recorded (consent, a verified license or the owner's mandate), the rights holder is known, no opt-out is outstanding, and no student speech is present anywhere in the index.
4. Current. Donation destinations, consent status and registry entries carry a last-verified date and are re-checked on a schedule, so stale facts are visible as stale.
5. Honest about what is unknown. An unresolved citation, a thin-coverage answer and an unverified link are shown as exactly that. Unknown is a valid value in this corpus. Wrong is not.

## Quality dimensions

Ten dimensions cover the five properties. Where a line is numeric it is a starting value to set before the first test, in the same spirit as the PRD's thresholds, and is written down once set.

| Dimension | What it means here | How it is measured | Line below which data does not serve |
| --- | --- | --- | --- |
| Transcript accuracy | Terms of art, teacher names, Sanskrit and Japanese words are spelled correctly | Error rate on core vocabulary in a hand-corrected sample of talks | Set from the first hand-corrected sample. A talk below it is not indexed. |
| Speaker attribution | The speaker of each passage is the teacher on the talk record | Spot check of a labeled sample, plus guest-speaker flags | Any confirmed misattribution blocks the talk until fixed |
| Segment labeling | Talk, chant, announcement, sitting and Q and A are separated correctly | Confusion matrix on the ten-video sample, then on larger samples | No chant or sitting text in the index. Zero student speech. |
| Timestamp accuracy | The source link lands on the quoted moment | Click-through check of sampled passages | Offset tolerance in seconds, set before the first test |
| Deduplication | One record per distinct talk | Duplicate clusters on title, date and duration | No known duplicate indexed |
| Metadata completeness | Type, series, part, source URL, duration, consent status, rights holder and last-verified date are all present | Share of indexed talks with every required field | All required fields present. Consent and rights holder are already hard gates. |
| Registry integrity | Every alias maps to exactly one teacher, and every sangha has a donation destination with a verified flag | Alias collision check, verified-flag coverage among cited sanghas | An unverified destination renders with attribution and no give button |
| Citation resolution | Cited texts map to canonical locators at three confidence levels | Precision of resolved citations against the series-label ground truth | A resolved citation must be correct. Anything doubtful is probable or unresolved. |
| Consent and rights currency | Consent and rights status were verified recently enough to trust | Age of each last-verified date | Past the review interval the talk is flagged for reverification |
| Coverage | The corpus can support multi-teacher answers on the topics tested | Distribution of teachers and sanghas per answer from the query log | A measurement, not a gate. It sizes the corpus needed for v2b. |

## Quality gates

Checks sit at the stage where an error is cheapest to catch, and the earliest ones keep bad data from ever becoming text.

| Stage | Check | On failure |
| --- | --- | --- |
| Ingest | Duplicates merged on title, date and duration; non-talks dropped | Merge or drop is logged and listed for review |
| Rights gate | A recorded rights basis (consent, verified license or mandate) and a known rights holder | Talk stops, no media pulled |
| Transcribe | Timestamps present; link offset within tolerance on the sample | Talk held out of the index until repaired |
| Correct | Glossary terms and teacher names match the registry | Fix glossary or alias, rerun affected talks |
| Segment | No student speech; chant and sitting text excluded | Quarantine the whole talk and recheck |
| Extract and resolve | Teacher, series and part present; citations graded at three levels | Doubtful citations shown as unresolved |
| Index | Required fields complete; typed proof of rights at indexing | Write rejected, talk not indexed |
| Serve | Read only through the serving view; donation link verified | Shown with no give button, out of the dana split |

Read each row left to right: the stage, the check it must pass, and where a failing talk goes. The consent gate comes first because nothing downstream should exist for a talk that fails it. Blocking failures follow the quarantine rules in the failure handling section.

## Ground truth and evaluation sets

Quality is only claimed against something fixed. Four reference sets carry that weight, and each is versioned so a score always names the set it was measured on.

The series-labeled talks are free ground truth for citation resolution. The Genjokoan, Uji, Bendowa, Blue Cliff Record and Song of the Grass Hut series are labeled in their titles, so the text a talk is about is known before the resolver runs. The resolver is scored against those labels.

The hand-corrected sample is a small set of talks whose transcripts a person has corrected word by word, starting with the ten videos sampled for the segmentation check. It scores transcript accuracy, segment labeling and timestamp offset. The glossary and the segmentation classifier are tuned on one half of the sample and scored on the other half, so the score does not reward memorizing the set it was tuned on.

The 50-question practitioner set is built around topics with several voices: compassion, anger, the Heart Sutra, the precepts, repentance, practicing within sangha, and discrimination and suffering. It also holds single-teacher topics, a grief question, and a named-teacher query for Norman Fischer that tests thin coverage. A teacher reviewer rates the passages returned, with an explicit brief to rate unflatteringly.

The claim audit samples synthesis output. A knowledgeable reviewer marks 20 answers claim by claim as supported, drifting or invented, and the 50-answer integrity sample checks that no invented claim survives the filter.

A failure on any set is recorded with the set's version, the build under test and the reviewer, so a regression can be traced to the change that caused it.

## Known hazards in the SSZ corpus

The PRD already names the main hazards in the v1 corpus. Each has a treatment that is part of the pipeline, not a one-time cleanup, so the same problem is caught again when a new source arrives.

| Hazard | What it breaks | Treatment |
| --- | --- | --- |
| Duplicate talks across titles, dates and durations | Repeated passages inflate one teacher and distort the dana split | Deduplicate on title, date and duration at ingest, and log each merge with the records it joined |
| Non-talk entries such as the 11-second quote clip | Fragments reach the index as if they were talks | Drop non-talks at ingest by type and duration, and list each drop for review |
| Teacher name variants: Steve Given as Jundo, Shindo and Daiho Shindo, Kanshin alone and with her surname, Chris Burkhart also as Nomon | Attribution splits one teacher into several, and named-teacher queries miss talks | Teacher registry with aliases, and a check that every alias maps to one teacher |
| Auto-captions mangle Dogen, shikantaza, Genjokoan and other terms | Passages change meaning or fail to match queries | Glossary correction pass before indexing, scored on the hand-corrected sample |
| Files may not be trimmed to the talk | Chanting, sitting and announcements enter the index | Sample ten videos first, then segment and index talk and teacher answers only |
| Genjokoan paragraphs 5 and 6 appear to be missing from the series | Series ground truth has a hole, and the interface cannot say what a talk presupposes | Check the series list against the text, and record the result in series metadata |
| Gap between April 2022 and February 2023 | Topics from that stretch look absent when they are only unrecorded | Record the gap as a known coverage hole, so a thin answer is not read as teacher silence |
| About three in four talks come from one teacher | Answers tilt toward one voice and the three-teacher rule cannot hold | Relax and display the rule in v1, diversify retrieval, and log teacher counts per answer |
| Held-out and excluded material: guest speakers who have not responded, and the Antiracist 12 Steps session | Absence looks like a retrieval failure | Mark each as held out or excluded with the reason, so coverage reports separate it from missing data |
| The Spotify feed may add talks or only duplicate existing ones | Unknown overlap with the video archive | Cross-check against the deduplicated set before deciding whether it joins the corpus |

## Provenance and ownership

Every field in the corpus has one source of truth, one party allowed to correct it, and a record of where its current value came from. In v1 the founder is the only editor. From v2b, claimed profiles let teachers and sanghas correct their own entries and nothing else.

| Field group | Source of truth | Who corrects it | Origin recorded as |
| --- | --- | --- | --- |
| Talk facts: title, date, duration, source URL | The source video and its archive listing | Founder, by rerunning ingest | Source link and content hash |
| Consent status and rights holder | The speaker's own reply | Founder, with the reply attached as evidence | Dated consent record and last-verified date |
| Transcript text | Existing captions or one ASR pass, then the glossary pass | Glossary edits apply corpus-wide. Per-talk edits come from the reviewer only. | Transcript version keyed by content hash |
| Segment labels | Classifier output, checked on samples | Reviewer corrections on samples feed classifier changes | Label version and classifier build |
| Teacher and sangha registries | The founder's registry, with aliases | Founder in v1, the claimed profile owner for their own entry from v2b | Change log with who, when and why |
| Donation destinations | The page the source video points to, plus a teacher's second destination where given | Founder verifies in v1. Teacher or sangha corrects it from v2b. | Verified flag and last-verified date |
| Series, part and presupposed texts | Talk titles and the series lists | Founder | Source title and the date checked |
| Citations and locators | Extractor, resolver and the hand-listed set | Founder in v1, teachers adding context from v2b | Confidence level and the method that produced it |

Corrections never overwrite silently. Each one writes a row holding the previous value, the new value, who made it, when, and why, and a teacher's correction outranks any automated value for their own words. A correction to a global asset such as the glossary or a registry alias queues the affected talks for reindexing.

## Failure handling

A failing record leaves serving first and is repaired second. The unit of quarantine is the talk, because attribution, consent and segmentation errors rarely stay inside one passage.

| Severity | Examples | Action |
| --- | --- | --- |
| Blocking | Wrong speaker, student speech in the index, consent or rights holder missing or withdrawn, a donation link pointing at the wrong sangha | Remove from the serving view at once, quarantine the talk, and release only after a re-check passes |
| Degrading | Transcript accuracy below the line, timestamps off by more than tolerance, a duplicate found after indexing | Quarantine the talk, repair, reindex from the stored transcript version, and re-score on the sample |
| Informational | Unresolved citation, thin coverage on a topic, an unverified donation link | Show as unknown, with no give button where the link is unverified, and log it for the coverage report |

Quarantined talks stay in the store, flagged and excluded from the serving view, so the repair has something to work on and the removal is reversible. Takedown is separate and complete: it deletes, not quarantines.

Each logged answer stores the passage IDs it returned, so the answers touched by a bad record can be counted without reading anyone's query text. When a blocking error reaches testers, the founder decides whether to tell them, and the decision and its reason go in the change log.

The founder decides when quality blocks serving. Reviewers advise. No line is lowered to ship a build, and any change to a line is recorded with the reason and the date.

## Metrics and review cadence

The metrics below are the ones that change a decision. A metric with no decision attached is not tracked.

| Metric | Source | Cadence | Decision it feeds |
| --- | --- | --- | --- |
| Share of the 50 questions rated faithful and relevant | Teacher reviewer | Any change to chunking, embeddings, reranking or glossary | v1 pass or fail: fewer than half is a fail |
| Invented claims surviving the synthesis filter | 50-answer sample and the 20-answer claim audit | Any change to synthesis | Any survivor is a v1 fail |
| Student speech found in the index | Scan of indexed segments | Every ingest | Must be zero, otherwise quarantine |
| Indexed talks without consent or a known rights holder | Consent records against the index | Nightly | Must be zero, otherwise quarantine |
| Speakers awaiting a reply, by speaker | Consent records | Weekly | Order of outreach and corpus size |
| Share of citations unresolved | Resolver output on series-labeled talks | Every ingest | Whether the hand-listed set needs to grow |
| Cited sanghas with a verified donation link | Sangha registry | Weekly | Whether the dana panel is meaningful for the answers shown |
| Answers by number of teachers: one, two, three or more | Answer log | Weekly | Corpus needed for v2b |
| Talks past their review interval | Last-verified dates | Monthly | Reverification queue |
| Quarantined talks, with age | Quarantine flags | Weekly | Repair backlog |

This document is reviewed at each build that changes a pipeline stage, at each roadmap gate, and whenever a line is changed. A reviewer's unflattering ratings are kept as given and are not softened in the record.

## Open decisions

None of these changes the north star. Each has a trigger that forces it.

| Decision | Options | Trigger |
| --- | --- | --- |
| Numeric lines for transcript accuracy and timestamp tolerance | Set from the first hand-corrected sample | After the first sample is scored |
| Who produces the hand-corrected sample | Founder, volunteers from the sangha, or paid help | Before the first scoring run |
| Review interval for consent and donation links | Fixed schedule, or on any change at the source | Before the first tester sees the page |
| Whether the Spotify feed joins the corpus | Join, cross-check only, or leave out | After comparison with the deduplicated archive |
| Treatment of guest speakers who never reply | Stay excluded, or revisit after the first demo | After the first round of speaker replies |
| Scope of a teacher's corrections from v2a | Transcript wording and context, or context only | Before the teacher track begins |
