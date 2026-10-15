# Bug log

Bugs found by using the app. Each entry records what was asked, what came back, what should have come back and the likely cause. A fix links its PR and moves the status to fixed. Entries are never deleted.

Status is one of open, diagnosed, fixed or won't fix.

| ID | Date | Summary | Status |
| --- | --- | --- | --- |
| [B1](#b1-a-question-about-shakyamuni-returns-talks-not-texts) | 2026-10-15 | A question about Shakyamuni returns talks, not texts | diagnosed |

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
- Product question for the owner: should "Shakyamuni", "the Buddha" and "the historical Buddha" act as an author alias for texts attributed to the Buddha (the Dhammapada and the Pali suttas), even though Spec 08 attributes such works to the work itself? This needs a spec change, so it is not decided here.
- Install the reranker on the beta.
