# Spec 10: Cost per question and summary latency

Milestone P1, after the MVP test pass (owner's request of October 15, 2026). Two goals:

1. Lower the language-model cost of a question about fivefold, so the site does not lose money as use grows.
2. Make the summary always appear, never hang, and finish within 5 seconds.

Neither goal may cost recall or faithfulness. Every change below is measured against the baseline on the same question sets before it ships, and CLAUDE.md rules 4 and 5 hold throughout: the ranker sees no sangha, profile, volume or donation field, and a model never produces text shown as a quotation.

## Baseline (live stack, October 15, 2026)

Measured with `wt eval run --set gate_a@1 --limit 10` on the deployed image `main-76435137b8ca` against RDS, after PRs #164 and #169. Costs are the client's estimates from `config/llm_prices.toml` (Haiku 4.5, $1 per million input tokens, $5 per million output).

| Stage | Calls per question | Cost per question | Time |
| --- | --- | --- | --- |
| Triage gate (`sensitive`) | 1 | $0.002 | in parallel with search |
| Ranking (`rank`), 60 candidates in batches of 10 | 6 | $0.019 | 1.9 to 2.2 s |
| Excerpt cutting (`excerpt`), one per passage | 8 to 10 | $0.013 | about 1 s, in parallel |
| Summary (`synth`), one draft, sometimes a revision | 1 to 2 | $0.012 | 7.5 to 10 s, 19 to 21 s with a revision |
| Verifier (`verify`), one per surviving sentence | 4 to 12 | $0.008 | about 1 s, in parallel |
| **Total** | **20 to 30** | **$0.054** (range $0.042 to $0.061) | page 3.4 to 4.4 s; summary 9 to 21 s after the page |

The summary was kept on 8 to 9 of 10 questions; when it is lost the page shows none. At 1,000 questions a month the model bill is about $54 against about $170 of fixed AWS cost; at 100,000 it is about $5,400.

## Targets

| Measure | Target | Baseline |
| --- | --- | --- |
| Mean model cost per question, 50-question set | **at most $0.010** | $0.054 |
| 95th-percentile cost per question | at most $0.015 | $0.061 |
| Page with excerpts, 95th percentile | at most 5 s (unchanged) | 4.4 s |
| Summary fully shown, measured from the page opening, 95th percentile | **at most 5 s** | 9 to 21 s |
| First summary sentence shown, 95th percentile | at most 2.5 s after the page opens | 9 s |
| Standard answers with at least two excerpts that show a summary block | **100%** | 80 to 90% |
| Strongly relevant candidates (judged 7+, see Evaluation) | at least 95% of the baseline count | 1,132 on 40 questions |
| Claim audit: unsupported summary sentences shown | none on the 20-answer audit (unchanged) | none |

"Always shown" means the summary block always appears with either a verified summary or the extractive fallback in workstream C, never an empty placeholder, never an error.

## Workstream A: ranking (35% of the cost, the slowest stage)

The model ranker fixed relevance on the beta, but it is the largest line. Each option below is run through the judged harness (Evaluation) against the model ranker before one is chosen.

1. **Small cross-encoder on the web service's CPU.** A compact reranker exported to ONNX (for example `bge-reranker-base` int8 or an English MiniLM cross-encoder), scoring the 60 fused candidates truncated to 256 tokens. Marginal cost zero. The current `bge-reranker-v2-m3` took over 20 s for 40 pairs on 2 vCPU, so a candidate qualifies only if it scores 60 pairs in under 1 s at the 95th percentile on the deployed task size. A larger task (4 vCPU, about $35 a month more) is allowed if it is what makes a model qualify.
2. **A hosted rerank API** from a provider that sells reranking per search (Voyage, Cohere or similar), expected under $0.002 per question and under 0.6 s. This sends question and passage text to a second provider, so it needs the owner's decision on that provider's data terms and a change to the public copy's "a language model provider" line (Spec 09).
3. **The model ranker, trimmed.** 40 candidates instead of 60, each cut to its first 700 characters, the system prompt cached where the model's minimum cacheable length allows. Expected about $0.007; keeps the current quality most surely.

Choice rule: the cheapest option whose judged 7+ count is at least 95% of the model ranker's and whose 95th-percentile time is under 1.5 s. `[ranking] ranker` gains the chosen value; the others stay selectable.

## Workstream B: excerpt cutting (24% of the cost)

One model call per passage chooses the sentences to show (Spec 05, Answer assembly, rule 8).

1. **Sentence similarity, no model.** Embed each sentence of the passage's window with the service's embedder and pick the shortest run of consecutive sentences, inside the existing word limits, that holds the highest-scoring sentence and reaches 90% of the best run's mean score. Sentence vectors are computed at index time and stored with the passage (a new column of per-sentence half-precision vectors), so a question costs only dot products. Needs a reindex pass, which runs on the PC worker.
2. **One batched model call** for all of an answer's passages instead of one each, returning `{"passage": n, "start": i, "end": j}` per passage. Saves the repeated instructions, about a quarter of the cost; kept as the fallback if option 1 fails its review.
3. If workstream A chooses a model ranker, the ranker's reply also carries the best sentence run for each candidate it scores 7 or more, and the excerpt call goes away.

Review: the teacher reviewer rates 30 excerpts from each method side by side, blind, for "cut off or missing its context". Option 1 ships if its rate is no worse than the model step's by more than 5 points.

## Workstream C: the summary (37% of the cost, the latency goal)

1. **Start earlier.** The summary starts as soon as selection is done, over the passages' windows, in parallel with excerpt cutting instead of after it. Its citations are already window sentence numbers (Spec 05, Summary and verifier), so excerpt cutting is not an input. This moves the start about 1 s before the page opens.
2. **A shorter summary.** A headline and up to three points, each a sentence of at most 30 words, with the agree and differ points kept and labelled as such where they apply. The framing paragraph, "Points to keep in mind" from the model and the sum-up are dropped; the code's own count notes stay. Output falls from 600 to 750 tokens to about 250, which a Haiku-class model writes in about 2 to 3 s. Needs the owner's sign-off as a PRD Answer experience change.
3. **Stream and verify as it goes.** The draft streams; each sentence goes through the filters and to the verifier as soon as it is complete, and a verified sentence is pushed to the page at once over the answer's progress stream, or the existing `GET /a/{id}/summary` poll, which returns what has been verified so far. The headline comes first in the format, so it appears first.
4. **One verifier call per answer**, not per sentence. It returns a verdict per sentence number. Cuts the verify cost by about two thirds; latency stays about 1 s because it runs once the draft is done, or once per batch of sentences when streaming.
5. **A deadline, then an extractive fallback.** The summary has 4.5 s from the page opening. When the deadline passes, or the headline did not survive, the block shows "Key lines": for up to three teachers in relevance order, the excerpt's single best sentence, verbatim, sliced by code from serving rows, with the teacher, talk and date, and a link to the excerpt. No model text, so rule 5 holds, and nothing to verify. If the model summary finishes later it does not replace the key lines, so the block never changes under the reader. The revision pass of October 15, 2026 is removed; the fallback replaces it.
6. The "Writing a summary" placeholder stays for at most the deadline; there is no path in which it remains.

Expected: first sentence at about 1.5 s after the page opens, a finished summary at about 3 s, the fallback on the rest, and the summary cost from $0.020 to about $0.007.

## Workstream D: not paying twice

1. **Answer cache.** A question whose vector is within cosine 0.97 of one answered in the last 7 days, in the same mode and with no change to the serving view since (a counter bumped by indexing, takedown and rights changes), reuses that answer's ranked passages, excerpts and summary. The page still rebuilds everything through `servable_passage`, as permalinks do, so a takedown is honoured. The cache stores vectors and answer payloads, never question text. Reuse across testers needs the owner's decision.
2. **Prompt caching** on the ranking, summary and verifier system prompts wherever their length qualifies, with the cache-read price added to `llm_prices.toml` so the spend ledger stays right.

## Workstream E: knowing what a question costs

1. `query_log` gains `llm_cost_usd`, the sum of the answer's calls, written when the answer and its deferred summary are stored, and `llm_usage` rows carry the answer id. `wt report costs --since 7d` prints the mean, the 95th percentile and the cost by stage.
2. A daily spend alarm through the existing AWS notification at twice the expected daily cost.
3. At the monthly cap (`WT_LLM_MONTHLY_CAP_USD`), the page still works: fused order with the "relevance check did not finish" line, excerpts trimmed by code, and the key-lines fallback. Today it loses the summary and ranking without saying so on the page beyond the order line.

## Evaluation

1. **Judged ranking harness.** The October 15 benchmark (fused top 60 for each question, every candidate scored by the ranking prompt and model, counts of 7+ and 4+) becomes `wt eval rank-judge --set <set> --variant <ranker>`, recorded per run, so options in A are compared on the same questions and judge. The judge stays the Haiku-class ranking prompt even when the shipped ranker is not.
2. **Cost and time per question.** `wt eval run` records per question the model cost by stage, the page time (total less the deferred summary), the time to the first summary sentence and to the finished summary, and whether the fallback showed. `wt eval report` prints the targets above as pass or fail.
3. **Sets.** The 50-question set and Gate A's 20, on the deployed stack through the `wt-v1-eval` task, before and after each workstream.
4. **Faithfulness.** The 20-answer claim audit (Spec 07) runs on the shorter summary before it ships.

## Order

C1, C4 and C5 first: they deliver the "always shown, under 5 s" goal with no quality risk and cut the summary cost. Then E1, so the rest is measured. Then A and B, chosen by the harness. D last, since its value depends on how often questions repeat once real use begins.

## Owner decisions needed

1. Whether a second provider (A2) is acceptable, and the copy change that goes with it.
2. The shorter summary format (C2), as a PRD change.
3. The extractive fallback's heading and wording (C5), for Spec 09.
4. Whether one tester's question may reuse an answer computed for another's (D1).
5. Whether the 5-second summary target is measured from the page opening, as written here, or from pressing Ask.

## Acceptance

- [ ] Mean model cost per question at most $0.010, 95th percentile at most $0.015, on the 50-question set on the live stack.
- [ ] Page 95th percentile at most 5 s; summary finished within 5 s of the page opening at the 95th percentile; first sentence within 2.5 s.
- [ ] Every standard answer with at least two excerpts shows a summary block, verified or key lines, on both sets.
- [ ] Judged 7+ candidates at least 95% of the October 15 baseline on the same questions.
- [ ] Claim audit: no unsupported sentence shown.
- [ ] `wt report costs` and the per-question cost column in place, with the daily alarm.
