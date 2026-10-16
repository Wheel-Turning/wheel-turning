# Bug log

Bugs found by using the app. Each entry records what was asked, what came back, what should have come back and the likely cause. A fix links its PR and moves the status to fixed. Entries are never deleted.

Status is one of open, diagnosed, fixed or won't fix.

| ID | Date | Summary | Status |
| --- | --- | --- | --- |
| [B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts) | 2026-10-15 | A question about Shakyamuni returns talks, not texts | diagnosed |
| [B2](#b2-the-sticky-question-does-not-always-stick) | 2026-10-15 | The sticky question does not always stick | open |
| [B3](#b3-a-question-about-the-pali-canon-returns-talks-not-suttacentral-texts) | 2026-10-15 | A question about the Pali Canon returns talks, not SuttaCentral texts | open |
| [B4](#b4-a-summary-did-not-load) | 2026-10-15 | A summary did not load | open |
| [B5](#b5-cite-opens-a-pop-up-instead-of-scrolling-to-the-reference) | 2026-10-15 | Cite opens a pop-up instead of scrolling to the reference | open |
| [B6](#b6-talks-with-audio-outside-ssz-have-no-listen-link) | 2026-10-15 | Talks with audio outside SSZ have no Listen link | open |
| [B7](#b7-navigation-does-not-mark-the-current-page) | 2026-10-15 | Navigation does not mark the current page | open |
| [B8](#b8-no-placeholder-research-search-page-for-priests) | 2026-10-15 | No placeholder research search page for priests | open |
| [B9](#b9-the-privacy-line-is-not-fully-accurate) | 2026-10-15 | The privacy line is not fully accurate | open |
| [B10](#b10-summaries-use-few-excerpts-and-say-little) | 2026-10-15 | Summaries use few excerpts and say little | open |
| [B11](#b11-source-texts-are-not-in-search-answers-or-excerpt-references) | 2026-10-15 | Source texts are not in search, answers or excerpt references | open |
| [B12](#b12-feedback-is-one-thin-form-not-a-structured-review-of-the-answer) | 2026-10-15 | Feedback is one thin form, not a structured review of the answer | open (backlog) |
| [B13](#b13-every-excerpt-has-a-report-link) | 2026-10-15 | Every excerpt has a Report link | open |
| [B14](#b14-public-copy-promises-a-time-for-acting-on-requests) | 2026-10-15 | Public copy promises a time for acting on requests | open |
| [B15](#b15-rights-actions-run-by-hand-on-two-databases) | 2026-10-15 | Rights actions run by hand on two databases | open (debt) |
| [B16](#b16-alerts-and-teardown-are-not-verified) | 2026-10-15 | Alerts and teardown are not verified | open (debt) |
| [B17](#b17-the-web-server-reads-configuration-outside-the-loader) | 2026-10-15 | The web server reads configuration outside the loader | open (debt) |
| [B18](#b18-spec-acceptance-items-do-not-match-the-code) | 2026-10-15 | Spec acceptance items do not match the code | open (debt) |
| [B19](#b19-the-pc-and-rds-are-both-sources-of-truth) | 2026-10-15 | The PC and RDS are both sources of truth | open (debt) |
| [B20](#b20-major-dependency-updates-wait-unmerged) | 2026-10-15 | Major dependency updates wait unmerged | open (debt) |
| [B21](#b21-the-gpu-worker-and-alpha-server-live-outside-the-repo) | 2026-10-15 | The GPU worker and alpha server live outside the repo | open (debt) |
| [B22](#b22-thin-tests-for-the-cli-and-ml-crates-and-timing-based-tests) | 2026-10-15 | Thin tests for the CLI and ML crates, and timing-based tests | open (debt) |
| [B23](#b23-very-large-modules) | 2026-10-15 | Very large modules | open (debt) |

## B1. A question about Shakyamuni returns talks, not texts

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).
- Answer: `/a/a4c96226-50f6-4945-a68f-612d048b8b50`.
- Question: "What does Shakyamuni buddha say about suffering?"

### Expected

The question names a source (the historical Buddha), not a Zen teacher. The answer should lead with his words in the foundational texts, such as the Dhammapada, in the "From the texts" section. Teachers who speak about the Buddha's teaching on suffering can follow.

### Actual

The page shows eight talk excerpts from four teachers and no text excerpts. Three excerpts are relevant: Weitsman, "Continuous Practice" (2020) and "Suffering: How We Create It" (2006), and Haller, "The Koan of Life" (2015). The other five match only on the words "Buddha" or "Shakyamuni":

- Weitsman, "Title Unknown" (1987): Nirmanakaya Buddha in the chant.
- Suzuki, Lotus Sutra II-12: the three bodies of the Buddha.
- Suzuki, "On Bodhidharma's Day": reincarnation.
- Suzuki, "Why We Must Have Rules": rules in the kitchen.
- Lieberman, "The Artful Place of Letter Writing" (2022): "only a Buddha and a Buddha".

The summary is grounded, but it puts the teachers' paraphrase in the Buddha's mouth ("Shakyamuni Buddha taught that his only teaching is suffering"). The page says the relevance check is not installed, so these excerpts are ordered by search match only.

### Cause

1. The texts path is not built. Spec 08 ("Retrieval and answer changes") calls for a `TextRetriever` over `servable_text_unit`, settings under `[texts]` in `config/retrieval.toml` and a "From the texts" section on the answer page. None of these is in the code on `main` (56f478a), so text units can never appear in an answer.
2. No Dhammapada edition is loaded. The Dhammapada appears only as an example in Spec 08 and the PRD. No source in `config/` loads it, and no excerptable edition is known to be on the beta. Not checked against the beta database.
3. The Buddha would not trigger named-author mode even once it is built. That mode matches `person_alias` for authors of a ready work. Spec 08 attributes a work with an unknown or collective author, which covers most sutras and the Dhammapada, to the work itself, so "Shakyamuni" has no alias to match. The question would fall to standard mode: text search would run, but it would not be restricted to texts attributed to the Buddha.
4. No reranker on the beta. Without the relevance check, excerpts that only mention "Buddha" outrank on-topic ones. The answer page itself says the relevance check is not installed.

### Fix

- Build the Spec 08 texts path: `TextRetriever`, the `[texts]` settings and the "From the texts" section.
- Load an excerptable Dhammapada edition under a recorded license or mandate.
- Attribute works given as the Buddha's words to Shakyamuni Buddha, with an alias list for his names, and keep a summary from naming him as the speaker. Decided by the owner on October 15, 2026, and specced in Spec 08, Traditional attribution (branch `spec/buddha-attribution`).
- Install the reranker on the beta.

## B2. The sticky question does not always stick

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).

### Expected

On an answer page, once the question heading scrolls out of view, the sticky question bar shows the question at the top of the page every time.

### Actual

The bar sometimes does not appear. The conditions are not yet recorded.

### Cause

Not diagnosed. The bar is shown and hidden by an `IntersectionObserver` in `crates/wt-api/static/answer.js` (`data-sticky-q`), which is the place to start.

## B3. A question about the Pali Canon returns talks, not SuttaCentral texts

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).
- Question: "What does the pali cannon say about suffering?"

### Expected

The question names a source text. The answer should lead with excerpts from the Pali Canon, which the owner reports is available from SuttaCentral, in the "From the texts" section. SuttaCentral is not named anywhere in `config/` or the specs on `main`, so whether it is loaded on the beta is not checked. Teachers who speak about it can follow.

### Actual

The page shows talk excerpts only.

### Cause

Not diagnosed. Likely the same as cause 1 of [B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts): the Spec 08 texts path is not built, so text units never appear in an answer. A question that names a work or canon, rather than a person, also needs a way to restrict text search to that work, which named-author mode does not give.

## B4. A summary did not load

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).
- Answer: `/a/131b0c65-f5af-45f3-9cca-c1b4338b6dc7`.

### Expected

The summary loads on the answer page.

### Actual

The summary did not load.

### Cause

Diagnosed October 16, 2026, from the beta's logs (`/wt-v1/web`). The answer id is not in the logs, and `wt` has no command that reads an answer row from RDS. So the answer is matched by its outcome: on October 15 only two deferred summaries settled without a summary, and both failed the same way.

1. **Every sentence was rejected.** At 23:19 UTC (a question on the Pali Canon and suffering) and at 00:21 UTC on October 16 (a question on the meaning of life), the draft and its one revision lost every sentence: 16 model calls each, 0 sentences kept, 8 and 10 dropped. No model call failed, timed out or reached the cap. At 23:19 there were 2 drafts and 14 verifier calls, all on `claude-haiku-4-5`.
   - The drop reasons were `addresses_reader` (a headline with "you", taken from the question's own wording), `one_teacher` (agree or differ sentences citing two excerpts by one teacher), `unknown_ref` (sentence ranges past an excerpt's end, such as `E10[1-10]`), `wrong_year`, `quote_too_long`, and `unsupported` from the verifier. Without a headline there is no summary.
2. **The page said nothing.** The summary route answered 204, and `answer.js` removed the "Writing a summary" placeholder without a word. To a reader it looked as if the summary never loaded.

### Fix

- PB2.4 (#180): `answer.summary_status` records `all_sentences_rejected` for this case. Each answer logs `summary_settled`, and the PB4.3 alarm fires when more than 30% of an hour's answers end without a summary.
- PB2.2 tunes the prompt against the drop reasons above.
- Open for the owner: whether the page should say that no summary could be written, instead of removing its place silently (Spec 09 copy).

## B5. Cite opens a pop-up instead of scrolling to the reference

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).

### Expected

Choosing Cite on an excerpt moves the page down to that excerpt's entry in the references, with no pop-up.

### Actual

Cite opens a disclosure with the citation in it (`<details class="cite-item">` in `crates/wt-api/templates/answer.html`).

### Cause

Design change, not a defect in the code. Spec 09 and the answer template need the new behavior.

## B6. Talks with audio outside SSZ have no Listen link

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).

### Expected

Any talk with a public audio page, such as the Engaged Wisdom talks, shows a Listen link to that page, as Seattle Soto Zen talks do.

### Actual

Only SSZ talks show the Listen link.

### Cause

Not diagnosed. The template shows the link whenever `p.links.listen` is set, so the gap is likely in where the listen link is filled in for each source.

## B7. Navigation does not mark the current page

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).

### Expected

A link to the page the reader is on is not a link, and has a distinct style that shows it is the current page (with `aria-current="page"`).

### Actual

Links to the current page stay live and look like every other link.

### Cause

Not built.

## B8. No placeholder research search page for priests

- Reported: October 15, 2026, by the owner.

### Expected

A placeholder research search page, for priests, that would return all relevant results rather than a short answer. It is a fake door test: the page explains what it would do and lets a visitor record their interest.

### Actual

No such page exists.

### Cause

Not built. Needs a spec section before code: the page copy, what "record interest" stores, and how it fits the privacy rules.

## B9. The privacy line is not fully accurate

- Reported: October 15, 2026, by the owner.

### Expected

The privacy line says exactly what happens to a reader's questions.

### Actual

The line "You choose whether your questions are kept. Only totals are read, and one question is opened only if you flag it." is not fully accurate. It appears as `LOGGING_LINE` in `crates/wt-api/src/chrome.rs`, and a longer form appears in `crates/wt-api/templates/home.html`, `copy/home.md` and Spec 09.

### Cause

Not diagnosed. The new wording needs the owner's approval, and every copy of the line should change together.

## B10. Summaries use few excerpts and say little

- Reported: October 15, 2026, by the owner, on the beta (app.wheelturning.org).
- Answer: `/a/40fc360b-7287-4172-b40c-2a1b10b8442a`.
- Question: "What is the difference between practice and trying to improve myself?"

### Expected

1. No em dashes anywhere in a summary.
2. No note on how many excerpts come from one sangha.
3. The top of the page gives the counts as teachers, teachings and excerpts, for example "6 teachers, 7 teachings, 10 excerpts".
4. Every excerpt on the page is either used by the summary or not served. An excerpt worth showing is worth drawing on.
5. The summary says something a reader could not get from the question alone: where the talks agree, where they put the weight differently, and how each one draws the line between practice and self-improvement.

### Actual

- The top line says "Passages from 6 teachers". The summary's closing line says "Drawn from 3 talks by 3 teachers". The two counts read as if they disagree.
- Points to keep in mind holds only "8 of the 10 excerpts come from teachers at Berkeley Zen Center."
- The summary cites 3 of the 10 excerpts: Rosenbaum (2007), Weitsman (2010) and Suzuki (1971). The other seven are shown with no mention, which suggests they are not relevant.
- There are no "Where the talks agree" or "Where they differ" sentences, though the excerpts come from six teachers. The headline restates the question, the framing sentence only says what the talks address, and the sum-up repeats the Suzuki line.
- The owner reports em dashes in summaries. This answer, as pasted, has none, so the rule needs an example to test against.

### Cause

Not fully diagnosed.

1. Em dashes: the prompt (`crates/wt-synth/prompts/summarize.md`) does not forbid them and no code removes them.
2. The sangha note is required by Spec 05, Summary and verifier, item 5, and written by `notes` in `crates/wt-api/src/summary.rs`. Removing it is a spec change, which the owner asked for in this report.
3. The top line is `result_line` in `crates/wt-api/src/view.rs`, and it counts teachers only. "Drawn from" counts only the excerpts the shown summary sentences cite (Spec 05, item 7), so it is always smaller.
4. Excerpts the summary does not use: the beta has no reranker ([B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts), cause 4), so excerpts are ordered by search match and weak ones are served. The summary prompt says "Fewer true sentences are better than more", and the verifier drops any sentence whose words do not match the excerpts it cites, so a weak excerpt is left out rather than used. Checked October 16, 2026, in the beta's logs: the model did write them, and the filters dropped them before the verifier ever ran (see the note below).
5. Thin content follows from 4: with agree and differ empty, only the headline, framing and sum-up remain, and the prompt keeps those close to the excerpts' own words.

**The draft for this answer (beta logs, October 15, 23:27 UTC; matched by the question's subject, since logs carry no answer id).** The summary kept 3 sentences and dropped 16, across the draft and its revision. The model wrote 6 agree and 4 differ sentences; none reached the verifier:

- `addresses_reader`: 6 sentences (the headline, a framing sentence, 2 agree and 2 differ), for "yourself" or "you" taken from the question's own words ("trying to improve yourself").
- `one_teacher`: 4 (3 agree, 1 differ), each citing two excerpts by the same teacher.
- `unknown_ref`: 4 (1 differ, 3 keep in mind), citing sentence ranges past an excerpt's end, such as `E10[1-10]` and `E9[1-9]`.
- `misattributed`: 1 agree sentence, for naming Suzuki Roshi as the source of a teaching quoted by others.
- `quote_too_long`: 1.

So the thin summary comes from the filters, not the verifier. PB2.2 addresses this: the prompt must use each excerpt or say why not, and it is tuned against these drop reasons.

### Open question for the owner

Point 4 sets two ways to fix this against each other. One is to serve fewer excerpts: install the reranker and drop excerpts below a relevance threshold. The other is to make the summary draw on every excerpt it is given. They can be combined: serve only excerpts that pass the relevance check, and require the summary to cite each one or say why it does not.

### Fix

- Ban em dashes in the prompt and replace any that come back in code before the verifier runs.
- Remove the sangha note from Spec 05 and `notes`.
- Change the top line to "\<x> teachers, \<y> teachings, \<z> excerpts" in Spec 09 and `result_line`.
- Settle the open question, then change selection, the prompt or both.
- Pull the stored draft for this answer to see whether agree and differ were written and dropped.

## B11. Source texts are not in search, answers or excerpt references

- Reported: October 15, 2026, by the owner. Related: [B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts) and [B3](#b3-a-question-about-the-pali-canon-returns-talks-not-suttacentral-texts), which are symptoms of this entry.
- Sources in scope: SuttaCentral and Terebess.

### Expected

Source texts take part in the app in three ways:

1. Search. Text units are searched alongside talks, so a question about a text or its teaching can return passages from the text itself.
2. Answers, with the work as a teacher. A work and its purported or historical author can stand as a teacher: a question that names the Buddha, the Pali Canon or a sutta returns that text's own words. Text excerpts appear in their own section, never mixed into the teacher groups.
3. References in an excerpt. When a teacher's excerpt refers to a text, the excerpt links to that text at its source. Second priority (P2): link to the place in the source where we think the quoted or referenced passage appears.

Owner's decision of October 15, 2026, on references: a quotation or a work's name in an excerpt is a link to the work, and nothing more. No excerpt of the text is shown inline. The link goes to the first of these that has the work:

1. The text on this site, when we hold rights to serve it.
2. A free official edition at the publisher, such as a BDK America PDF.
3. Bookshop.org.
4. The publisher's page.
5. Amazon.

This replaces the collapsed inline excerpt in Spec 08, Quoted passages inline, and the order of October 9 for `wt text link-out` (the publisher's page first, non-profit publishers preferred).

### Actual

None of the three works on the beta. Answers contain talk excerpts only.

### Cause

Mostly specced, not built.

1. Search and the separate section: Spec 08, Retrieval and answer changes, specs a `TextRetriever` over `servable_text_unit`, `[texts]` settings and a "From the texts" section. None of it is on `main` (B1, cause 1).
2. The work or historical author as a teacher: Spec 08 specs named-author mode, which matches `person_alias` for authors of ready works, and Traditional attribution (#167) attributes works given as the Buddha's words to Shakyamuni Buddha. A question that names a work or a canon rather than a person, such as "the Pali Canon", has no matching rule yet (B3). Not built.
3. References in an excerpt: work-level citations are built through resolution, and their footnote rendering is unticked in Spec 08 ("With the answer page in M5"). Verified inline excerpts (Quoted passages inline) are specced and not built. Linking to the place in the source is covered for hand-listed locators with a `link_out_url`. A place found by a quotation match alone has no link rule.
4. Sources: Terebess is specced as a mandated aggregator (Spec 08, Mandated sources). SuttaCentral is named nowhere in `config/` or the specs on `main`. Its texts are released under CC0 (owner, October 15, 2026), so its rights basis is a license and it needs no mandate; the license should be confirmed per translation when it is recorded, since SuttaCentral lists the license of each one. Its edition choice and import path need a spec entry, and what is loaded on the beta is not checked.
5. Inline quotations: Spec 08, Quoted passages inline, and the link-out order differ from the owner's decision above, so both need a spec change.

### Fix

To be ordered in the build plan once the register is complete. In outline: spec SuttaCentral as a CC0 source; change Spec 08 for the owner's link order and drop the inline excerpt; add Bookshop.org and Amazon links to `wt text link-out`; build the texts retrieval path and section; add a rule for questions that name a work or canon; render work-level footnotes and the link to the source; then, as P2, link to the likely place in the source, on this site or at the source.

## B12. Feedback is one thin form, not a structured review of the answer

- Reported: October 15, 2026, by the owner, as a backlog feature. Model: the per-thread feedback dialog in Consensus (consensus.app), which the owner calls best in class.

### Expected

One "Give feedback" button on the answer page opens a dialog with the question at its top and two tabs, so a reader can say in under a minute what worked and what did not.

1. Excerpts tab.
   - "How useful are these excerpts?" on a 1 to 5 scale, labelled "Not at all useful" and "Very useful".
   - "Teachers or talks you expected to see", optional free text.
   - "Choose excerpts to give specific feedback", optional: a numbered checklist of the page's excerpts, each with the excerpt reasons in `EXCERPT_REASONS` (helpful, not relevant, cut off, words look wrong, wrong teacher, talk or date, hard to follow) and a note.
2. Summary tab.
   - "What's wrong?", optional chips: too long, too short, did not answer my question, not supported by the excerpts, misrepresents a teacher, other.
   - "Any other feedback?", optional free text.
   - "Critique a specific citation", optional: pick a cited excerpt in the summary and say what is wrong with how it is used.
3. Cancel and Submit. One submit records everything chosen in both tabs as one submission.

### Actual

- Live (image `main-490f844d6fb6`): a separate feedback control under each excerpt, with reasons and a note. Nothing rates the answer as a whole and nothing asks what is missing.
- On `main` (#164, not deployed): one "Feedback on these answers" form at the foot of the page, with seven reasons and a note, following the owner's decisions of October 12 and 15. It cannot say which excerpt a reason is about.
- Neither can be reviewed on AWS yet: `wt report feedback` needs a `wt_analyst` login, which RDS does not have.

### Cause

Not built. The current form follows Spec 06, Answer page structure, rule 6, and Spec 09, Excerpt footer and feedback. Before code:

1. Spec 06 and Spec 09: the dialog, its copy and its no-script form. The CSP forbids inline scripts, so the dialog opens from `answer.js`, and without the script the button links to a plain feedback page for the answer (for example `GET /a/{answer}/feedback`) with the same two sections.
2. Spec 02 and a new migration: `feedback` has no column for a 1 to 5 rating or for "expected to see", and one submission spanning several excerpts needs its rows tied by `submission_id`, which exists, so only the rating and the expected-to-see text are new. `v_feedback_counts` and `feedback_notes()` need the new fields, and the expected-to-see text is a note under the same privacy rule: read without its question unless the query is flagged.
3. The review path from the analyst-login work, so the owner can read the results.

### Fix

To be ordered in the build plan once the register is complete.

## B13. Every excerpt has a Report link

- Reported: October 15, 2026, by the owner.

### Expected

No Report link on any excerpt. A reader reports a problem with an excerpt through the answer's feedback ([B12](#b12-feedback-is-one-thin-form-not-a-structured-review-of-the-answer)). A rights holder asks for removal through the Rights and removal page, which stays linked in the footer of every page.

### Actual

Each excerpt's footer ends with "Report", which opens `/rights?excerpt=<id>#request` (`crates/wt-api/templates/answer.html`, line 51). Text units are specced to carry one too.

### Cause

Required by the PRD and the specs, so removing it is a decision that changes them:

- PRD, answer page item 2 ("a 'Report this excerpt' link"), principle 4 (the link as one way to opt out) and the removal row of the scope table ("a report link on every passage").
- Spec 06: the excerpt footer order (Listen, Read, Cite, Give, Report), the text unit route and page (`GET /text/{unit_id}` "and report link"), and the help and feedback rule.
- Spec 08, Quoted passages inline ("Report this excerpt" under an inline excerpt), which B11 already drops.
- Spec 09, Excerpt footer, and the reply template for "a removal or report request".
- `crates/wt-api/src/rights.rs` takes the excerpt id the link passes, and `/rights/request` stores "a removal or report request".

### Fix

To be ordered in the build plan once the register is complete. Change the PRD and Specs 06, 08 and 09, remove the link from the answer and text pages, and keep the excerpt id field on the rights form only if the B12 feedback can hand a removal request to it.

## B14. Public copy promises a time for acting on requests

- Reported: October 15, 2026, by the owner. The project is run by one person, who cannot guarantee the time to act on feedback or removals by a deadline.

### Expected

No public page, email or form promises when feedback or a removal request will be acted on.

### Actual

The Rights and removal page promises "Within one business day of your request, the material you name stops appearing", and its confirmation says "What you named stops appearing within one business day" (`crates/wt-api/templates/rights.html`, lines 18 and 25; `copy/rights.md`; Spec 09).

### Cause

The promise is set in the PRD and specs:

- PRD: principle 4, the rights handling success criterion, the risk row for a teacher who objects, and the removal row of the scope table.
- Spec 03, Display levels, rule 4: a narrower level is honored "within one business day".
- Spec 08: Mandated sources, handling ("suspend ... within one business day"); its acceptance item that fails the audit for a request open more than one business day; and Alerts to the founder, which alert on a request still open after one business day.
- Spec 09: the Rights and removal copy.

Retention periods (24 hours, 180 days, the provider's 30 days) are not promises to act and are not affected.

### Decision

The internal target stays (owner, October 15, 2026). The alert and the audit check for a request open more than one business day remain as a private reminder to the owner. Only the public promise goes.

### Fix

To be ordered in the build plan once the register is complete. Change the PRD (principle 4, the success criterion, the risk row and the scope table) and Specs 03 and 09 so no public text states a time, and reword Spec 08's handling and alert sections so one business day is an internal target, not a promise. Then change the Rights page and its confirmation. The copy can still say requests are handled in the order they arrive and that material is suspended before the requester's identity is checked, if that practice stays.

## Tech debt audit, October 15, 2026

B15 to B23 come from a tech debt audit of `main` at d95de08 (#165), not from using the app. Each carries a priority score, (impact + risk) × (6 − effort), with each factor from 1 to 5. A higher score goes earlier. The scores are inputs to the build plan, not its order.

## B15. Rights actions run by hand on two databases

- Found: October 15, 2026, by the tech debt audit. Priority 40 (impact 5, risk 5, effort 2).

### Expected

A takedown, opt-out or display change is recorded once and stops the material being served everywhere it could be served.

### Actual

The PC's `wt-m3-ssz` and the beta's RDS both hold real data. `docs/runbook.md` (§3, and §12 "Rights actions on both databases") has the owner run every rights action twice, once per `DATABASE_URL`, then run `wt rights parity`. In the runbook's words, a takedown run on one side only leaves the talk served from the other. Removal requests reach RDS only, and objects live only on the PC.

### Cause

Every `wt` command acts on the one database `DATABASE_URL` names. The second database arrived with the beta on October 13, and the procedure was written around the command rather than the command changed.

### Fix

Give the rights commands (takedown, opt-out, exclusion, display level, `talks_basis`) a second database URL, apply the action to both in one command, and run the parity check at its end, failing on any `"unsafe": true` difference. Stop-gap until [B19](#b19-the-pc-and-rds-are-both-sources-of-truth) removes the second database.

## B16. Alerts and teardown are not verified

- Found: October 15, 2026, by the tech debt audit. Priority 30 (impact 2, risk 4, effort 1).

### Expected

The beta's alerts are known to reach the owner, and the stack can be built and destroyed from an empty account.

### Actual

Spec 01 leaves three acceptance items open: a `wt alerts test` message reaching the owner after the subscription is confirmed, the web role publishing to the alert topic and nothing else in SNS, and `terraform apply` then `terraform destroy` from an empty account. The beta is live, so an alert that never arrives would go unnoticed.

### Cause

Not run since the stack went up.

### Fix

Confirm the SNS subscription, send `wt alerts test`, check the web role's SNS policy, and tick the items. The empty-account apply and destroy can wait for a spare account.

## B17. The web server reads configuration outside the loader

- Found: October 15, 2026, by the tech debt audit. Priority 28 (impact 3, risk 4, effort 2).

### Expected

Every key is read and checked once at startup by `wt_core::config`, as its module comment says, and a missing key that a live feature needs stops startup.

### Actual

`wt-api` reads `WT_MAIL_FROM`, `WT_PUBLIC_URL`, `WT_OWNER_NAME`, `WT_TRUST_FORWARDED_FOR`, `WT_CONFIG_DIR` and the privacy keys straight from the environment (`crates/wt-api/src/site.rs`, `crates/wt-api/src/privacy.rs`). Without `WT_MAIL_FROM` or `WT_PUBLIC_URL`, `mailer()` logs a warning and the site starts with removal requests unconfirmed by email (#165). `WT_CONFIG_DIR` is missing from `.env.example`, and `WT_OWNER_NAME`, `WT_TRUST_FORWARDED_FOR`, `WT_POPPLER_BIN`, `WT_TESSERACT` and `WT_CONFIG_DIR` are missing from Spec 01.

### Cause

Keys added with later features were read where they were used.

### Fix

Move the keys into `Config`, list every key in `.env.example` and Spec 01, and fail startup when `WT_PUBLIC_URL` is set without `WT_MAIL_FROM` (or the reverse).

## B18. Spec acceptance items do not match the code

- Found: October 15, 2026, by the tech debt audit. Priority 28 (impact 3, risk 4, effort 2).

### Expected

CLAUDE.md rule 11: a milestone ends with its acceptance items ticked, so a ticked item means verified and an open one means not done.

### Actual

About 100 items are open across the specs (Spec 08: 28, Spec 06: 21, Spec 03: 19, Spec 01: 8). Some are done and unticked: `.gitattributes` exists, but its Spec 01 item is open. Some name things that no longer exist: 22 references to the `dg_*` roles, renamed to `wt_*` in migration 0010. Some may be genuinely unchecked, such as Spec 06's "A failed log insert produces an error page and no answer".

### Cause

Items were not ticked as work merged, and renames did not reach the acceptance lists.

### Fix

One pass over every open item: tick it with the test or check that proves it, rewrite it if it names something renamed, or mark it a known gap. The gaps then go into the build plan.

## B19. The PC and RDS are both sources of truth

- Found: October 15, 2026, by the tech debt audit. Priority 30 (impact 5, risk 5, effort 3).

### Expected

One database and one object store hold the real data.

### Actual

The transcription worker writes to the PC's database, so new talks reach the beta only by reloading RDS from the PC. Since October 13 some rows exist only on RDS (`tester`, `query_log`, `feedback`, `takedown_request`, and any rights action run there), and the runbook warns that a reload overwrites them. Objects live only on the PC, which keeps the local backups of runbook §10 in use.

### Cause

The beta was loaded from the PC on October 13 while the worker stayed on the PC. Moving it is the owner's call (runbook §12).

### Fix

Point the worker and downstream stages at RDS (already planned for SSZ), move `objects/` and `records/` to S3, then retire the PC database, the parity check, runbook §10 and the dual-database parts of §12. Removes [B15](#b15-rights-actions-run-by-hand-on-two-databases).

## B20. Major dependency updates wait unmerged

- Found: October 15, 2026, by the tech debt audit. Priority 20 (impact 2, risk 3, effort 2).

### Expected

Dependabot updates are merged, or closed with a reason, within a week or two.

### Actual

Major updates open since October 12: `zip` 2 to 9 (#107, used by `wt-ingest` on downloaded archives), `askama` 0.14 to 0.16 (#108), `fs4` 0.13 to 1.1 (#109) and `toml_edit` 0.23 to 0.25 (#110), plus the grouped minor update #172 and the actions and a11y updates #130 and #131. `Cargo.lock` holds 24 crates at two versions each. `deny.toml` ignores RUSTSEC-2024-0436 (`paste`, through `tokenizers`).

### Cause

Major versions need code changes, and the beta push came first.

### Fix

One update per PR, `zip` first. Recheck the `paste` ignore when `tokenizers` moves.

## B21. The GPU worker and alpha server live outside the repo

- Found: October 15, 2026, by the tech debt audit. Priority 18 (impact 3, risk 3, effort 3).

### Expected

Machines that run the pipeline or serve the app are set up from the repo, and idle machines stop on their own.

### Actual

The Lambda GPU worker runs from the shell scripts in `infra/gpu-worker/`. Shutting idle machines down and staying under the spend cap are done by hand. The alpha server's `serve.sh` and `data/alpha/config` are untracked, and that config is a copy of `config/` that can drift from it.

### Cause

Both were set up quickly for runs and the alpha, and the beta on AWS has since taken over serving testers.

### Fix

Have the worker terminate its machine when the queue is empty and refuse to start past the cap. Retire the alpha server, or track its script and have it read `config/` rather than a copy.

## B22. Thin tests for the CLI and ML crates, and timing-based tests

- Found: October 15, 2026, by the tech debt audit. Priority 15 (impact 2, risk 3, effort 3).

### Expected

Each crate's main paths are tested, and no test depends on wall-clock timing.

### Actual

`wt-cli` has about 9,700 lines and 21 tests, and `wt-ml` has 8. Some tests sleep and assert on elapsed time, such as the 700 ms sleep in `crates/wt-db/tests/job.rs`. #168 fixed a CI failure of this kind.

### Cause

The CLI is tested mostly through the library crates it calls.

### Fix

Add tests as each command is touched, starting with the rights and takedown commands. Replace sleeps with paused tokio time or an explicit signal.

## B23. Very large modules

- Found: October 15, 2026, by the tech debt audit. Priority 9 (impact 2, risk 1, effort 3).

### Expected

Modules small enough to read and review in one sitting.

### Actual

`crates/wt-api/src/lib.rs` is 1,541 lines and holds every route and handler. `crates/wt-db/src/rights.rs` is 1,399 lines and `crates/wt-ingest/src/texts/mod.rs` is 1,226.

### Cause

Growth by feature.

### Fix

Split each when it is next changed, for example the `wt-api` handlers by page. Not worth a change of its own.
