# Generative AI Lens

The Well-Architected review of October 15, 2026 (`README.md`) applied the core framework only. This page applies the **Generative AI Lens** on top of it, because the answer path calls a language model for triage, ranking, excerpt cutting, summaries and verification, and the pipeline calls one for segment labels, citation hints and student-question edits.

**Source.** The best-practice list is the lens's live table of contents (`docs.aws.amazon.com/wellarchitected/latest/generative-ai-lens/toc-contents.json`), read on October 19, 2026: 51 best practices across six pillars. It was extracted through a summarizing fetch, not parsed directly, so check the count against the lens before relying on it. The workload facts are checked against `main` at 8549de5.

**Shape of the workload, as the lens sees it.** A retrieval-augmented application on a third-party hosted model API, with no agents, no tools, no fine-tuning, no model hosting and no training. The model never writes text shown as a quotation (CLAUDE.md rule 5): it returns ids, labels and sentence numbers, and code slices the quotes. Most of the lens's agent, training and hosting practices are therefore Not Applicable, and the rest map closely onto controls the project already has.

## Summary

| Pillar | BPs | Implemented | Partial | Not | N/A | Main gap |
| --- | --- | --- | --- | --- | --- | --- |
| Operational excellence | 10 | 7 | 2 | 0 | 1 | No end-to-end trace of one answer across stages |
| Security | 10 | 4 | 3 | 0 | 3 | Model traffic leaves AWS over the internet (no private path to this provider) |
| Reliability | 10 | 3 | 4 | 1 | 2 | One provider, one region, no fallback model |
| Performance efficiency | 8 | 6 | 1 | 1 | 0 | No load test of the model path |
| Cost optimization | 9 | 5 | 2 | 1 | 1 | No prompt caching yet (Spec 10 D2) |
| Sustainability | 4 | 2 | 1 | 0 | 1 | Fixed capacity |
| **Total** | **51** | **27** | **13** | **3** | **8** | |

No finding is High. The two Medium findings are GENREL01-BP01 and GENREL05-BP01 below. Both are accepted trade-offs for an invite-only beta and become worth revisiting at the public beta.

## Ledger

| BP | Status | Risk | Evidence | Recommendation |
| --- | --- | --- | --- | --- |
| GENOPS01-BP01 Periodically evaluate functional performance | Implemented | | `wt eval run` / `compare` on fixed sets (`eval/`), claim audit, Gate A; stage moves gated on eval results (Spec 11 PB2.0) | None |
| GENOPS01-BP02 Collect and monitor user feedback | Implemented | | Answer feedback (migrations 0039, 0042); `wt report beta` (PB4.6) | None |
| GENOPS02-BP01 Monitor all application layers | Implemented | | ALB, ECS, RDS alarms; answer-path metric filters (PB4.3); `wt-v1` dashboard (PR 215) | None |
| GENOPS02-BP02 Monitor foundation model metrics | Implemented | | `llm_usage` per call with stage, tokens and cost (migration 0018); `wt-v1-model-failures`, `wt-v1-slow-answers` | None |
| GENOPS02-BP03 Mitigate the risk of system overload | Implemented | | Per-tester ask limit, Cloudflare rate rule, monthly spend cap enforced in code (`crates/wt-extract/src/llm.rs`) | UB3.4: reserve spend before each call |
| GENOPS03-BP01 Prompt template management | Implemented | | Prompts are versioned files beside the code (`crates/*/prompts/*.md`), reviewed in PRs, scored by eval before a change ships (runbook §7) | None |
| GENOPS03-BP02 Tracing for agents and RAG workflows | Partial | Low | Per-stage timings and `ask_completed` events; no single trace id across retrieval, model calls and storage | Log one request id on every stage event and `llm_usage` row |
| GENOPS04-BP01 Automate the lifecycle with IaC | Implemented | | Terraform for every AWS resource; model and stage settings are Terraform variables (`envs/v1`) | None |
| GENOPS04-BP02 GenAIOps for the lifecycle | Partial | Low | Eval gates and per-stage model keys; the comparison is run by hand (PB2.0) | Script the PB2.0 comparison as one `wt eval` command |
| GENOPS05-BP01 Learn when to customize models | Not Applicable | | Decided not to customize: hosted models with prompts and retrieval meet the need | None |
| GENSEC01-BP01 Least privilege to model endpoints | Implemented | | One API key in Secrets Manager, read only by the web, eval and batch roles; project workspace (`ANTHROPIC_WORKSPACE_ID`); spend cap | Rotate the key every 90 days (UB3.8) |
| GENSEC01-BP02 Private network path to the model | Partial | Low | TLS to the provider's public API over the internet; no private path exists to a third-party API from this VPC | Accept. A provider reachable through PrivateLink (for example through Amazon Bedrock) would close it; that is a model-sourcing decision |
| GENSEC01-BP03 Least privilege for models reaching data stores | Not Applicable | | The model reaches no data store; code sends it only the passages it may see, read through `servable_passage` | None |
| GENSEC01-BP04 Monitor access to model services | Partial | Low | Every call logged in `llm_usage`; provider-side usage in the provider console; no alert on calls from outside the expected roles | Review the provider's usage page monthly with the cost review |
| GENSEC02-BP01 Guardrails against harmful or incorrect responses | Implemented | | No model-written quotations (rule 5); sentence-level verifier; deterministic filters; rules, embedding check and model triage for sensitive questions; crisis page | None |
| GENSEC03-BP01 Control-plane and data-access monitoring | Partial | Low | CloudTrail and GuardDuty cover the AWS side (secret reads); the provider's own audit log is not collected | Same as GENSEC01-BP04 |
| GENSEC04-BP01 Secure prompt catalog | Implemented | | Prompts in git, changed only through reviewed PRs and built into the image | None |
| GENSEC04-BP02 Sanitize and validate user inputs | Implemented | | Questions delimited and declared data, not instructions (`crates/wt-retrieve/prompts/triage.md`, `crates/wt-synth/prompts/summarize.md`); outputs are structured JSON validated in code; the red-team set (`redteam.md`) exercises the screen | Add injection-style questions to the gate set |
| GENSEC05-BP01 Least privilege for agentic workflows | Not Applicable | | No agents or tools | None |
| GENSEC06-BP01 Data purification for training | Not Applicable | | No training | None |
| GENREL01-BP01 Scale model throughput with utilization | Partial | Medium | Retries with backoff on 429 and 5xx; no knowledge of the provider's rate tier against peak demand | Record the account's rate limits per model in Spec 01 and compare with UB3.2's load test |
| GENREL02-BP01 Redundant network to model endpoints | Not Applicable | | One public HTTPS endpoint run by the provider | None |
| GENREL03-BP01 Manage prompt flows and recover from failure | Implemented | | A failed or slow call drops only the summary; triage failure degrades to rules and embedding check; ranker falls back to fused order and logs `none (fallback)` | None |
| GENREL03-BP02 Timeouts on agentic workflows | Partial | Low | `synthesis_timeout_s` on the ask path; client timeout 600 s | UB3.11 |
| GENREL04-BP01 Prompt catalog | Implemented | | As GENSEC04-BP01 | None |
| GENREL04-BP02 Model catalog | Implemented | | `config/llm_prices.toml` lists each model with price, context limit and `thinking_off`; per-stage keys (PB2.0) | None |
| GENREL05-BP01 Load-balance inference across regions | Not Implemented | Medium | One provider, one model family; an outage there removes summaries, ranking and model triage | Name a fallback model per stage in `llm_prices.toml`, used only when the first fails, and test it in eval |
| GENREL05-BP02 Replicate embedding data across regions | Partial | Low | Embeddings live in RDS in one region; 7-day PITR | See `reliability.md`, cross-region snapshot copy |
| GENREL05-BP03 Agent capabilities across regions | Not Applicable | | No agents | None |
| GENREL06-BP01 Fault tolerance for distributed computation | Partial | Low | GPU transcription jobs are idempotent and requeueable (`crates/wt-db/src/job.rs`); the Oct 9 crash needed a manual requeue | UB7.1 |
| GENPERF01-BP01 Ground-truth set of prompts and responses | Implemented | | `eval/questions_v1.toml`, `gate_questions_v1.toml`, teacher prep set; Gate A ratings | None |
| GENPERF01-BP02 Collect performance metrics | Implemented | | Per-stage ms, `AskMilliseconds` p95 alarm, `llm_usage` tokens | None |
| GENPERF02-BP01 Load test model endpoints | Not Implemented | Low | No load test | UB3.2 |
| GENPERF02-BP02 Optimize inference parameters | Implemented | | Per-stage `max_tokens`; temperature 0 where supported; `thinking_off` after PB2.0's first run (`config/llm_prices.toml`) | None |
| GENPERF02-BP03 Select the right model for the use case | Implemented | | Per-stage comparison with pass rules (Spec 11 PB2.0) | None |
| GENPERF03-BP01 Managed solutions where appropriate | Implemented | | Hosted model API; managed Postgres with pgvector | None |
| GENPERF04-BP01 Test embeddings for latency and relevance | Implemented | | Embedding latency measured on 0.5, 1 and 2 vCPU (`docs/cost.md`); retrieval eval; arm64 drift test refused | None |
| GENPERF04-BP02 Optimize vector sizes | Partial | Low | `halfvec` HNSW halves index size (migration 0043); full 1,024 dimensions kept | Measure recall at 512 dimensions only if index memory becomes a limit |
| GENCOST01-BP01 Right-size model selection | Implemented | | Small model on every stage; per-stage moves gated on quality (PB2.0) | None |
| GENCOST02-BP01 Choose the inference paradigm by cost and performance | Implemented | | On-demand API; summary deferred so the page shows first; UB3.12 weighs summary-on-request | None |
| GENCOST02-BP02 Minimize hosting costs | Implemented | | Task cut to 1 vCPU / 3 GB, database to `db.t4g.small` on measurements (#207, #208) | None |
| GENCOST03-BP01 Optimize prompt token length | Partial | Low | Excerpts trimmed and capped before the summary; no per-stage token budget alarm | Track mean input tokens per stage in `wt report beta` |
| GENCOST03-BP02 Control response length | Implemented | | Per-stage `max_tokens`; summary sentence and part limits in the prompt | None |
| GENCOST03-BP03 Prompt caching | Not Implemented | Low | Spec 10 D2 not done | UB3.1 |
| GENCOST03-BP04 Annotate input for cost-aware filtering | Partial | Low | Rules stop acute questions before any model call; named-teacher and sensitive questions skip the summary | None beyond UB3.12 |
| GENCOST04-BP01 Reduce vector length | Implemented | | `halfvec` storage (migration 0043) | None |
| GENCOST05-BP01 Stopping conditions for long-running workflows | Not Applicable | | No agent loops; each stage is one call with a token limit | None |
| GENSUS01-BP01 Auto scaling and serverless | Partial | Low | Fargate with scale to zero between rounds; no autoscaling | FL4.1 |
| GENSUS01-BP02 Efficient model customization | Not Applicable | | No customization | None |
| GENSUS02-BP01 Efficient data processing and storage | Implemented | | Audio deleted after each job; one store for text and vectors; lifecycle rules | None |
| GENSUS03-BP01 Smaller models and optimized inference | Implemented | | int8 ONNX embedder; small hosted model per stage | None |

## Design principles

The lens's design principles, checked against the project's own rules:

| Principle | Where the project stands |
| --- | --- |
| Controlled autonomy | Strong. The model chooses among ids and labels code gives it, and every summary sentence is verified before it is shown. Nothing acts on the model's output beyond rendering it. |
| Comprehensive observability | Good at the stage level; one trace id per answer is the gap (GENOPS03-BP02). |
| Resource efficiency | Good: right-sized models and compute, a spend cap. |
| Distributed resilience | Weak by design: one provider and one region (GENREL05-BP01). |
| Standardized resource management | Good: prompts and models are versioned with the code. |
| Secure interaction boundaries | Good: the question is data, not instructions, and outputs are validated structured JSON. The provider's terms hold the data (no zero retention, runbook §8), which the privacy page states. |

## Proposed additions to the roadmap

For the owner's decision, not in `docs/roadmap.md` today:

| Proposal | Lens BP | Size |
| --- | --- | --- |
| One request id across every stage event and `llm_usage` row, so one answer can be followed end to end | GENOPS03-BP02 | S |
| A fallback model per stage, used only after the first fails, scored by the same eval rules | GENREL05-BP01 | M |
| Record the provider's rate limits per model in Spec 01, and include the model path in UB3.2's load test | GENREL01-BP01, GENPERF02-BP01 | S |
| Prompt-injection questions added to the gate set (`eval/gate_questions_v1.toml`) | GENSEC04-BP02 | S |
