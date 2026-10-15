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

Not diagnosed. Check the beta logs and `llm_usage` for this answer.

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
4. Excerpts the summary does not use: the beta has no reranker ([B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts), cause 4), so excerpts are ordered by search match and weak ones are served. The summary prompt says "Fewer true sentences are better than more", and the verifier drops any sentence whose words do not match the excerpts it cites, so a weak excerpt is left out rather than used. Whether the model wrote agree and differ sentences that the verifier then dropped is not checked. The stored draft and `llm_usage` for this answer would show it.
5. Thin content follows from 4: with agree and differ empty, only the headline, framing and sum-up remain, and the prompt keeps those close to the excerpts' own words.

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
