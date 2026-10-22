# Roadmap: increments, features and stories

Drafted October 15, 2026. This orders every open item in four sources:

- The bug log (`docs/bugs.md`, B1 to B23, including the tech debt audit).
- The architecture review (A1 to A9, listed in `docs/backlog.md`).
- The SEO audit (S1 to S10).
- The AWS Well-Architected review of October 15 (WA: H1, M1 to M12, and the low findings by best-practice ID).

It adds the actions from `docs/strategy-research.md`: competition (C), finance (F), legal (L) and value proposition (V).

`docs/backlog.md` remains the flat index with P0 to P3. This file replaces its order.

## How this is built

- **Major increments** are the three launches, in order: private beta, public beta, public full launch.
- **Feature increments** are minimum marketable features (MMFs), in the Humanizing Work sense. Each is the smallest set of stories that gives someone outside the build value they would notice and could be released on its own. Within an increment, MMFs are listed in the order to build them.
- **Stories** are written "Do X so that Y" and listed in build order. Each names its sources and a size: S (a day or less), M (a few days), L (a week or more). "Owner" marks a story only the owner can do, such as a decision, an outreach or an account action.
- **Gates.** Between increments, a gate must pass on every dimension: feature maturity, customer feedback, market insights, usage data and finances. A sixth dimension, rights and legal, is added because the product's promises to teachers depend on it.

Everything below is Not Started unless marked. Two items have progressed: Gate A ratings were sent October 10, and PR 173 (S2 to S5) is ready to merge.

### Fit with the PRD

The PRD's roadmap is v1 private demo (Gate A), then v2a teacher track and v2b sangha track in parallel (Gate B), then v3 gifts and texts. This roadmap maps onto it as follows:

| This roadmap | PRD |
| --- | --- |
| Private beta | v1, ending at Gate A and Gate 1 below |
| Public beta | v2a and v2b, run in public |
| Full launch | Follows PRD Gate B |

The PRD does not describe a public beta, and keeps v1 private. Opening to the public before Gate B is a PRD change, which CLAUDE.md rule 1 says the owner must make. The same applies to:

- The Report link (B13).
- The one-business-day promise (B14).
- Cost recovery from sanghas (F6).
- Moving texts earlier than v3 (B11).

## Increment 1: Private beta

**For:** invited SSZ members and a few teachers. **Value:** a practitioner asks a real question and gets passages a teacher would stand behind. The teacher's rights hold. The owner learns whether the PRD's three hypotheses are true.

### PB1. Teachers' rights hold on one source of truth

Marketable to teachers: "your removal or opt-out takes effect everywhere, and we can show it."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| PB1.1 | Point the transcription worker and every downstream stage at RDS, so new talks and every rights action live in one database | B19, WA H1, A2 | L |
| PB1.2 | Move objects and records to S3 and teach takedown about them, so a removal empties every store from AWS | B19, WA H1 | M |
| PB1.3 | Only if PB1.1 slips: apply rights commands to both databases in one command that ends with the parity check, export the rows that exist only on RDS nightly, and alarm when the PC backup goes stale, so no takedown can miss a database and no tester data is lost | B15, WA REL09-BP01, REL09-BP03 | M |
| PB1.4 | Run `wt audit` and the parity check nightly against RDS as a scheduled task with an alarm, so drift and overdue removal requests surface within a day | A1, WA H1 interim | S |
| PB1.5 | Get SES out of the sandbox (owner: the open support case), so removal confirmation emails reach any rights holder and not only verified addresses | WA REL01-BP04, #165 | S, Owner |
| PB1.6 | Refuse to start without the mail settings, so removal requests are never silently left unconfirmed | B17 | S |
| PB1.7 | Remove the public one-business-day promise and keep it as an internal alert, so the project promises only what one person can keep | B14 | S |

### PB2. Answers testers can trust

Marketable to practitioners: "every excerpt is on point, and the summary says something the question alone did not."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| PB2.0 | Run the eval set and claim audit with Claude Haiku 5.5 on every model stage, against Haiku 4.5, and switch each stage that holds its quality. Add the model to `config/llm_prices.toml` and test whether it accepts sampling settings. This way the B10 prompt work is tuned once, on the model that ships, and model cost per question falls about sevenfold | Pricing of October 15, Spec 10 | M |
| PB2.1 | Confirm the relevance ranker is live on the beta, so excerpts that only mention a word are dropped | B1 (cause 4), B10 | S |
| PB2.2 | Settle B10's open question, then change selection, the prompt or both, so every served excerpt is either drawn on by the summary or not served | B10 | M, Owner decision |
| PB2.3 | Ban em dashes, drop the sangha-count note, and show "x teachers, y teachings, z excerpts", so the page reads cleanly and its counts agree | B10 | S |
| PB2.4 | Diagnose the summary that did not load, so a failure shows up instead of passing unnoticed | B4 | S |
| PB2.5 | Label each summary as model-written, checked against the excerpts and not reviewed by the teachers, so no reader takes it for a teacher's words | L9, L13, C1 | S |
| PB2.6 | Complete Gate A with the teacher reviewer, so retrieval quality is judged by a teacher before anyone else sees it | Spec 00 | Owner |

### PB3. Testers' data is handled as promised

Marketable to practitioners: "what we say about your questions is exactly what happens."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| PB3.1 | Correct the privacy line everywhere it appears, so testers are told exactly what happens to their questions | B9, L4 | S, Owner wording |
| PB3.2 | Say on the privacy page that questions go to a language-model provider and how long anything is kept, so the notice is complete | L4 | S |
| PB3.3 | Get counsel's view on Washington's health-data law and apply it (a notice, consent, deletion, minimal retention of sensitive questions), so Seattle testers' mental-health signals are lawfully handled | L3 | M, Owner and counsel |
| PB3.4 | Review the support page's crisis resources and add a "not a substitute for care" notice, so a tester in crisis is pointed to real help | L7 | S |
| PB3.5 | Add HSTS, `frame-ancestors 'none'` and Permissions-Policy headers, so the pages cannot be framed or downgraded | WA SEC09-BP02 | S |
| PB3.6 | Put a WAF with managed rules and a rate rule in front of the site, and turn on GuardDuty, Access Analyzer and a root or IAM-change alarm, so the public forms cannot be abused and an intrusion is noticed | WA M1, M2, SEC03-BP04 | S |

### PB4. The owner can learn from the beta

Marketable to the owner and to the gate: "we can say what testers did, what failed and what it cost."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| PB4.1 | Add the analyst login on RDS and deploy the single feedback form (#164), so tester feedback can be read at all | B12 (part) | S |
| PB4.2 | Confirm alerts reach the owner and add a second recipient, so an outage is never missed | B16, WA OPS10-BP04 | S |
| PB4.3 | Alarm on model errors, the spend cap, dropped summaries and slow answers, so a broken answer path shows up before testers report it | A3, WA OPS04-BP04 | S |
| PB4.4 | Write one runbook entry per alarm, a ten-line incident procedure with an incident record template, a security contacts list, and a short outage notice for testers, so each alert has a known first step and testers hear when the site is down | WA M8, OPS10-BP01, OPS10-BP05, OPS11-BP02, SEC10-BP01 | S |
| PB4.5 | Write the recovery-time, data-loss and availability targets into Spec 01, so later reliability work has a target | WA M4 | S, Owner |
| PB4.6 | Produce a weekly beta report from totals: active testers, testers asking in two or more weeks, questions per tester, feedback reasons, dana click-through, cost per question and time to the first progress event, so each gate criterion has data | V-A1, F1, WA OPS04-BP01, OPS04-BP03 | M |
| PB4.7 | Model cost at 10 and 100 times current use, so the public beta has a budget ceiling | F1, WA COST05-BP06 | S |

### PB5. Evidence for the next increment

These are learning stories, not software. They run during the beta so the gate has evidence.

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| PB5.1 | Offer three sanghas their talks' transcripts in exchange for explicit permission, so we learn whether archive value wins consent (V4) | V-A2 | Owner |
| PB5.2 | Interview 10 testers and 5 teachers on the job, the alternatives and how often questions arise, so the value proposition is tested beyond usage numbers | V-A6 | Owner |
| PB5.3 | Engage copyright and privacy counsel on L1, L2, L6, L8 and L11, so their answers arrive before the public beta gate | L1, L2, L6, L8, L11 | Owner |
| PB5.4 | Choose a fiscal sponsor or form the entity, so gifts are lawful and the owner is not personally exposed | F2, L5, L14 | Owner |

### Gate 1: private beta to public beta

Thresholds are proposals for the owner to confirm before the beta opens to testers.

| Dimension | Pass when |
| --- | --- |
| Feature maturity | All PB1 to PB4 stories done. Gate A passed. The 50-question set rated mostly faithful and relevant. A 20-answer claim audit with no invented claim. The nightly audit clean for 14 nights in a row. WA H1 closed |
| Customer feedback | At least 10 active testers and 2 teachers who reviewed answers from their own talks. At least 60% of provided feedback "helpful". No unresolved complaint from a rights holder. PB5.2 interviews done |
| Market insights | Strategy research refreshed (C5), including a hands-on chatbot comparison. V4 answered: at least 2 of 3 sanghas say yes to transcripts for permission |
| Usage data | At least half the testers ask in two or more separate weeks (V1). A median of at least 5 questions per active tester over the beta. Dana click-through measured and above zero (V3, the PRD v1 hypothesis) |
| Finances | Cost per question known and within the F1 model. Monthly spend within the AWS budget and the model cap.  (F2) |
| Rights and legal | Every public page true to practice (B9, B14), including the accessibility statement (L16) |

## Increment 2: Public beta

**For:** any practitioner, labeled beta. **Value:** ask in your own words and get teachers' own words, with texts beside them, from a growing set of sanghas. Teachers and sanghas can see where their teaching goes and support flows back.

### UB1. Open to the public on a consent-first footing

Marketable to teachers and readers: "anyone can use it, and only talks with clear rights are shown."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB1.1 | Decide the public access model (open, or a waitlist) and change Spec 06, so the public can reach the app | PRD change | M, Owner decision |
| UB1.2 | Serve the public only talks with consent or a license (the `talks_basis` switch) unless counsel clears mandated use, so the largest copyright risk stays with invited testers | L1 | S |
| UB1.3 | Hold back full-transcript display for talks under CC No-Derivatives terms until counsel answers, so no license is breached | L2 | S |
| UB1.4 | Publish terms with a minimum age of 13, the removal policy and a registered DMCA agent, so rights holders and the public know the rules | L12, L17 | S |
| UB1.5 | Decide to geo-limit or to comply for EU and UK users, so religious-belief data is lawfully handled | L6 | M, Owner decision |
| UB1.6 | Confirm how student questions are edited and stored, and give students a request path, so third parties in recordings are protected | L8 | S |
| UB1.7 | Record in each sangha's permission the route used to fetch its media, so no platform's terms are broken | L10 | S, Owner |

### UB2. Texts beside the teachers

Marketable to practitioners: "ask about the Buddha, a sutra or a koan and read the text itself, next to how teachers speak about it." This is the clearest differentiator not yet live (C3).

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB2.1 | Spec SuttaCentral as a CC0 source and change Spec 08 to the October 15 link-out order with no inline excerpt, so the build has a settled target | B11, L11 | S |
| UB2.2 | Build the text retriever over the servable text units, with its settings and a "From the texts" section, behind a `Retriever` trait, so text excerpts appear in answers | B11, B1, Spec 00 | L |
| UB2.3 | Answer questions that name a work or canon from that work, and attribute the Buddha's traditional words to him, so "what does the Pali Canon say" returns the canon | B3, B1, Spec 08 | M |
| UB2.4 | Load only licensed or CC0 texts, and keep aggregator texts to excerpt-and-link, so text rights match talk rights | L11 | S |
| UB2.5 | Render work-level footnotes in excerpts with the link-out order (this site, free official edition, Bookshop.org, publisher, Amazon), so a teacher's reference leads to the text | B11 | M |
| UB2.6 | Count clicks on text references, so V6 is tested | V6 | S |

### UB3. Holds up under public traffic

Marketable to everyone: "it is up, fast and safe." The reliability findings of the WA review belong here.

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB3.1 | Resolve Spec 10's open decisions and meet its summary-latency and cost targets, including the answer cache and prompt caching, so answers are fast and affordable at public volume | Spec 10 (workstreams A to E), F7, WA PERF03-BP05 | M |
| UB3.2 | Load-test the ask path at 10 and 20 concurrent users, so web sizing is measured, not guessed | WA M7 | S |
| UB3.3 | Run the database in two availability zones with two web tasks, and move rate limits into the database or the WAF, so one failure does not take the site down | WA M5, A4, WA REL05-BP02 | M |
| UB3.4 | Reserve model spend before each call, so concurrent calls cannot pass the cap | A2 | S |
| UB3.5 | Script restore, takedown replay and audit as one runbook step, then run it and time it, so recovery is proven against PB4.5's targets and repeatable | WA M6, REL13-BP05 | S |
| UB3.6 | Stand up a short-lived staging environment from the same modules, so changes are tried before the public sees them | WA M12 | M |
| UB3.7 | Write a one-page threat model and playbooks for a leaked key or a data exposure, so security incidents have a plan | WA M9, M10 | S |
| UB3.8 | Replace the backup user's long-lived key, write a handover page and an offline break-glass note, set a 90-day secret rotation calendar, subscribe to RustSec and AWS security bulletins, and give one trusted person scale-to-zero access, so the project survives the owner's absence | WA M3, M11, SEC03-BP03, SEC02-BP05, SEC01-BP04 | S |
| UB3.9 | Apply the batched Terraform hygiene (egress to 443 and 5432, ALB access logs, a dashboard, a task-memory alarm, Performance Insights, storage autoscaling, base images pinned by digest, cost allocation tags, recorded Fargate and SES quotas), and bring the hand-applied infrastructure into the plan-and-apply flow, so operations are visible and tight. Written ahead of order on October 19, 2026 (egress, access logs, dashboard, memory alarm, Database Insights, storage autoscaling, cost tags; digest pins were already done); left: applying it, the quota values, and the plan-and-apply flow | WA SEC05-BP02, SEC04-BP01, OPS08-BP05, REL06-BP01, PERF03-BP03, SUS04-BP04, OPS05-BP05, COST03-BP02, REL01-BP01, OPS05-BP10, A6 | S |
| UB3.10 | Commission a light external penetration test, so the public site is checked by someone other than its builder | WA SEC11-BP03 | M, Owner |
| UB3.11 | Shorten the model timeout on the request path (the client allows 600 s), so one slow model call cannot hold a web connection | WA REL05-BP05 | S |
| UB3.12 | Decide whether the summary is written only when the reader asks for it, so the summary and verifier cost is paid only by readers who want it | Cost review of October 15 | S, Owner decision |
| UB3.13 | Trial a Cloudflare Tunnel in place of the load balancer and its public addresses, so about $16 to $20 a month more is saved after the Cloudflare proxy (`docs/adr-edge-cost.md`, "Follow-on"; built October 20, 2026, cut over by the owner per runbook §12) | Cost review of October 15, WA M1 | M |
| UB3.14 | Benchmark the web task on ARM, and schedule scale-to-zero between test rounds and overnight, so fixed AWS cost falls without slowing answers | WA SUS05-BP02, COST04-BP04 | S |

### UB4. Feedback that teaches us

Marketable to practitioners: "tell us in a minute what worked and what did not."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB4.1 | Build the two-tab feedback dialog with its no-script page and migration, so feedback rates the answer and names what is missing | B12 | M |
| UB4.2 | Remove the per-excerpt Report link after changing the PRD and specs, so problems go through feedback and removal stays on the Rights page | B13 | S, Owner decision |
| UB4.3 | Spec and run the research-search fake door for priests and teachers, so V5 is tested before anything is built | B8, V-A4 | M |

### UB5. Readers reach the source, the sangha and the giving

Marketable to teachers and sanghas: "readers are sent to you."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB5.1 | Fill Listen links for every source with a public audio page, so readers can hear any talk, not only SSZ's | B6 | S |
| UB5.2 | Link each teacher group to the sangha's own page, so practitioners can find a living teacher | V-A5 | S |
| UB5.3 | Add the site's own optional dana line through the fiscal sponsor's page, kept apart from the sangha panel, so readers can support the commons | F3, PRD principle 8 | S |
| UB5.4 | Mark the current page in the navigation with `aria-current`, and update the public accessibility statement for every new page, so screen-reader users know where they are and the conformance claim stays true | B7, L16 | S |
| UB5.5 | Say on the site and answer page "teachers' own words, never an imitation of a teacher", so the difference from chatbots is plain | C1 | S |

### UB6. Findable

Marketable to new readers: "you can find us when you search."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB6.1 | Merge PR 173 (title, description, JSON-LD, favicon) | S2 to S5 | S, Ready |
| UB6.2 | Verify the domain in Google Search Console and Bing and submit the sitemap, so the site gets indexed | S1 | S, Owner |
| UB6.3 | Publish a sources page listing the participating sanghas and texts, so search engines and the public see where material comes from | S7 | S |
| UB6.4 | Decide which public pages search engines may index, then allow crawling of the app with `noindex` kept elsewhere, so no excerpt leaks into search while approved pages can rank | S8, S10 | M, Owner decision |
| UB6.5 | Ask each sangha that has said yes to link to the site, so established sites lend authority | S6 | S, Owner |

### UB7. A corpus that grows safely

Marketable to sanghas: "join, and your archive is handled with care."

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| UB7.1 | Make the GPU worker stop its machine when the queue is empty and refuse to start past the spend cap, and pin the GPU host's SSH key before connecting, so ingestion runs cannot overspend or connect to the wrong machine | B21, WA SEC09-BP03 | S |
| UB7.2 | Merge `zip` 2 to 9, so the dependency that opens downloaded archives is current | B20 | S |
| UB7.3 | Test the rights and takedown commands in `wt-cli`, so the commands that keep the rights promise are covered | B22 | M |
| UB7.4 | Tick, rewrite or log as a gap every open spec acceptance item, so "done" means verified | B18 | M |
| UB7.5 | Onboard sanghas under consent or license toward Gate B's breadth, so most answers have three or more teachers | C2, PRD Gate B | L, Owner |
| UB7.6 | Offer partnership to EngageWisdom and one university digital-dharma project, so likely competitors become channels | C4 | Owner |
| UB7.7 | Pilot "sponsor a sangha's archive" with one sangha and one donor, and apply to two grant programs, so funding starts beyond reader dana | F4, F5 | Owner |

### Gate 2: public beta to full launch

| Dimension | Pass when |
| --- | --- |
| Feature maturity | All UB stories done. PRD Gate B met: 15 to 25 centers, a few thousand segmented talks, three or more teachers in most answers. The availability target met for 60 days. The restore drill within target. Pen test findings closed. Spec acceptance items all true (B18) |
| Customer feedback | Median answer rating of at least 4 out of 5 on the new dialog (B12). No unresolved rights-holder complaint. At least 5 teachers willing to be named as supportive |
| Market insights | Strategy research refreshed, with the chatbot comparison repeated. V5 (research search) and V6 (text links) answered. A decision on the sangha-archive pillar (V-A3) |
| Usage data | Weekly active users growing for 8 weeks. At least 25% of new users return within 4 weeks. Organic search impressions recorded in Search Console. Dana click-through holding or rising |
| Finances | Cost per question at the Spec 10 target. 1,000 questions a day fits the funded budget (F7). At least 6 months of projected cost committed. F6 decided |
| Rights and legal | Counsel's answers on L1, L2, L8 and L11 applied. Trademark search done (L15). Any EU decision (L6) implemented. The accessibility statement current (L16) |

## Increment 3: Public full launch

**For:** the public, without the beta label. **Value:** the trusted place to read what Zen teachers have actually said, and the shared archive sanghas rely on.

### FL1. A sangha's own archive

Built only if V4 and V-A3 hold.

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| FL1.1 | Spec and build search scoped to one sangha, with its transcripts and a link it can embed on its own site, so sanghas get a searchable, accessible archive in exchange for permission | V-A3, strategy section 4 | L |
| FL1.2 | Offer owner exports and transcripts to every participating sangha, so the archive value is delivered, not promised | Spec 03 display level d | M |

### FL2. Public pages search engines can see

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| FL2.1 | Publish teacher, sangha and teacher-approved talk pages with structured data, so people searching a teacher's name find their real words and sangha | S8 | M |
| FL2.2 | Publish talk-to-text pages ("every talk on this koan"), so the differentiator is visible from search | Strategy gap 3, C3 | M |

### FL3. Research search for teachers

Built only if V5 holds.

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| FL3.1 | Build the research mode that returns all relevant passages, the teacher's own talks first, so priests can prepare talks and trace references | B8, V5 | L |

### FL4. Launch-grade operations

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| FL4.1 | Add target-tracking autoscaling, so demand spikes are absorbed | WA REL07-BP03 | S |
| FL4.2 | Give a second person break-glass access, and hold a monthly ops and cost review and a quarterly restore drill, so operations do not rest on one person | WA OPS02-BP04, OPS11-BP07, COST10-BP01, M6 | S, Owner |
| FL4.3 | Buy a Savings Plan after two steady months, and split staging into its own account, so cost and blast radius both shrink | WA COST07-BP01, SEC01-BP01 | S |

### FL5. Funded for the year ahead

| # | Story | Sources | Size |
| --- | --- | --- | --- |
| FL5.1 | Secure 12 months of projected cost in gifts and grants, so the launch does not outrun its funding | F8 | Owner |
| FL5.2 | Form the planned nonprofit, or confirm the fiscal sponsor long term, and clear the name, so the project has a durable legal home | L5, L15, PRD vision | Owner |

### Launch criteria

Launch when FL4 and FL5 are done and every Gate 2 dimension still holds. FL1 to FL3 are launch features only if their tests passed. Otherwise they move to fast follows or the parking lot.

## Fast follows (after the full launch)

| Item | Sources |
| --- | --- |
| Link a reference to the likely place in the source | B11 (P2 part) |
| Cite scrolls to the reference instead of a pop-up | B5 |
| The sticky question bar | B2 |
| Topic pages (titles and links only, unless teachers opt in), after a rights decision | S9 |
| Remaining dependency majors and the `paste` advisory | B20 |
| Paused-time tests and `wt-ml` tests | B22 |
| Retire the alpha server, or track it | B21 |
| Terraform apply and destroy from an empty account | B16 (rest) |
| Watch database memory as the corpus grows past 137,000 passages | A6 |

## Parking lot (consider later)

| Item | Sources |
| --- | --- |
| Move `LlmClient` into its own crate | A5 |
| Split the large modules, and the texts subsystem out of `wt-ingest` | B23, A7 |
| Retrieval tuning without an image deploy | A8 |
| Prune branches and worktrees | A9 |
| Tracing, chaos testing, image signing, AWS Config rules, customer-managed keys | WA deferred (OPS04-BP05, REL12-BP04, SEC06-BP04, SEC04-BP04, SEC08-BP01) |
| Liturgy and chant guidance | PRD backlog |
| Practice companion, journaling or daily-reading features | Set aside in strategy section 4 |

## Owner decisions, in the order they block work

1. **B10:** how summaries use the excerpts (PB2.2).
2. **B9:** the privacy wording (PB3.1).
3. **Gate 1 thresholds:** confirm or change them before testers are invited.
4. **The public beta itself:** the PRD change, the access model, and texts before v3 (UB1.1, UB2).
5. **B13:** removing the Report link (UB4.2).
6. **L6:** EU and UK users (UB1.5).
7. **S8:** which pages may be indexed (UB6.4).
8. **Spec 10:** the five decisions (UB3.1).
9. **F6:** cost recovery from sanghas.
10. **S9:** topic pages.
