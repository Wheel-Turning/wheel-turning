# Backlog

Prioritized October 15, 2026, from three sources:

- The bug log, `docs/bugs.md`: B1 to B14 from using the app, B15 to B23 from the tech debt audit of `main` at d95de08.
- The architecture review of `main` at d95de08: items A1 to A9 below, which are not in the bug log.
- The SEO audit of wheelturning.org and app.wheelturning.org: items S1 to S10 below. Quick wins S2 to S5 are in PR 173, which is open and not merged.

Each item appears once. Where sources overlap, the item keeps its bug log ID and the overlap is noted.

## Priorities

| Priority | Meaning |
| --- | --- |
| P0 | Must be done before the limited beta tests |
| P1 | Must be done if the beta test clears, before the public launch |
| P2 | Fast follows after the public launch |
| P3 | Nice to have; consider later |

Effort is S (a day or less), M (a few days) or L (a week or more). "Decision" marks an item that needs the owner's call before code.

The test for P0: would an invited tester see something false or broken, would a rights promise fail, or would the owner be unable to learn from the test. The test for P1: anything the public, a search engine or a teacher who never opted in would meet.

## P0: before the limited beta tests

Gate A (Spec 00) is a prerequisite and is not listed as work: no tester other than the owner sees the beta until the teacher reviewer's ratings pass.

| # | ID | Item | Source | Why P0 | Effort |
| --- | --- | --- | --- | --- | --- |
| 1 | B19 | Make RDS the single source of truth: worker and downstream stages write to RDS, objects and records move to S3, PC database retired | Debt, architecture | Some rows exist only on RDS and a reload from the PC would bring back a talk taken down there. While two databases hold real data, every takedown depends on the owner remembering to run it twice. Also fixes A2's doubled spend cap. Decision: the owner's call per runbook §12 | L |
| 1a | B15 | Stop-gap if B19 cannot land first: rights commands apply to both databases in one command and finish with the parity check; no PC-to-RDS reloads until then | Debt | Same risk as B19, smaller fix. Drop it if B19 lands first | M |
| 2 | A1 | Run the nightly `wt audit` against RDS as a scheduled ECS task, alarmed on failure or warnings | Architecture | Every nightly run so far has audited a fresh, empty database (`WT_AUDIT_DATABASE_URL` is unset, and GitHub Actions cannot reach private RDS). The `takedown_request_overdue` reminder that B14 keeps depends on it | S |
| 3 | B16 | Verify alerts reach the owner: confirm the SNS subscription, send `wt alerts test`, check the web role's SNS policy | Debt | With testers on the beta, an alert that never arrives goes unnoticed. The empty-account apply and destroy test can wait (P2) | S |
| 4 | A3 | Alarms for the answer path: model errors and timeouts, spend cap reached, summary dropped, ask latency | Architecture | Today only infrastructure alarms exist. A reached cap or a failing model drops every summary with no signal (see B4) | S |
| 5 | B4 | Diagnose the summary that did not load, from the beta logs and `llm_usage` | App | Could be systematic. Testers judge the summary, so a silent failure spoils the test | S |
| 6 | B1 (cause 4), B10 | Confirm the ranker is live on the beta, then fix summary quality: ban em dashes, remove the sangha note, show "x teachers, y teachings, z excerpts" at the top, and settle how the summary draws on the excerpts | App | The beta exists to test whether retrieval and summaries hold up. Without the relevance check, weak excerpts are served and summaries say little. Decision: B10's open question (serve fewer excerpts, require the summary to use each one, or both) | M |
| 7 | B12 (part) | Let the owner read beta feedback: add a `wt_analyst` login on RDS and deploy the single feedback form from #164 | App | Today `wt report feedback` cannot run against RDS, so tester feedback cannot be read. The full structured dialog is P1 | S |
| 8 | B9 | Correct the privacy line everywhere it appears (`chrome.rs`, `home.html`, `copy/home.md`, Spec 09) | App | Testers are told something not fully accurate about their own questions. Decision: the owner approves the new wording | S |
| 9 | B14 | Remove the public promise to act within one business day; keep it as an internal alert | App | A public commitment that one person cannot guarantee, made to the rights holders the project depends on. The decision is already recorded | S |
| 10 | B17 | Move the web server's environment keys into `wt_core::config` and fail at startup when `WT_PUBLIC_URL` or `WT_MAIL_FROM` is missing | Debt | Without them the site starts with removal requests left unconfirmed, with only a log warning | S |

## P1: after the beta clears, before the public launch

| # | ID | Item | Source | Why P1 | Effort |
| --- | --- | --- | --- | --- | --- |
| 11 | B11, B1, B3 | Source texts in search, answers and references: the Spec 08 texts path (`TextRetriever`, `[texts]` settings, "From the texts" section, plus the `Retriever` trait Spec 00 promises), a rule for questions that name a work or canon, Buddha attribution, SuttaCentral specced as a CC0 source, work-level footnotes, and the link-out order of October 15 | App, architecture | A main part of the public offer, and specced but unbuilt. Move it to P0 if the beta should test texts, since it is the largest item here. Decision: whether the beta tests texts | L |
| 12 | B12 | The structured feedback dialog: excerpts and summary tabs, a 1 to 5 rating, "expected to see", a no-script fallback page and a migration | App | The beta can run on the simpler form (#7). The public needs the better one, and its spec changes come first | M |
| 13 | B13 | Remove the per-excerpt Report link; removal stays on the Rights page in every footer | App | Changes the PRD and Specs 06, 08 and 09. Pairs with B12, which takes over excerpt problems. Decision: needs the PRD change | S |
| 14 | B6 | Listen links for talks with public audio outside SSZ, such as EngageWisdom | App | Sending readers to teachers' own pages is part of the public promise to teachers | S |
| 15 | B7 | Mark the current page in the navigation, with `aria-current="page"` | App | Accessibility, before the public uses the site | S |
| 16 | B8 | Placeholder research search page for priests (fake door). Spec first: copy, what "record interest" stores, privacy | App | A fake door is worth running from the public launch onward. Decision: approve the spec | M |
| 17 | B18 | One pass over the ~100 open spec acceptance items: tick each with its proof, rewrite stale `dg_*` names, list real gaps | Debt | The public launch needs a true account of what is verified. Spec 06's "A failed log insert produces an error page" is one to prove | M |
| 18 | A2 | Make the language-model cap a reservation taken before each call, not a check after the fact | Architecture | Calls running side by side can pass the cap. Public traffic makes that likelier. B19 already removes the doubling | S |
| 19 | Spec 10 | Cost per question and summary latency (summary always within 5 seconds), after the owner's five decisions | Spec 10 | Specced as P1, after the MVP test pass. Live cost is about $0.054 a question. Decision: the five open decisions | M |
| 20 | B20 (part) | Merge `zip` 2 to 9 (#107) | Debt | `zip` opens downloaded archives, so it is the dependency most exposed to outside input | S |
| 21 | B21 (part) | GPU worker stops its machine when the queue is empty and refuses to start past the spend cap (see branch `gpu-run-guards`) | Debt | Larger ingestion runs before launch put real money at risk | S |
| 22 | B22 (part) | Tests for the rights and takedown commands in `wt-cli` | Debt | These commands keep the rights promises and have few tests | M |
| 23 | S1 | Verify wheelturning.org in Google Search Console and Bing Webmaster Tools and submit `sitemap.xml` (owner, DNS TXT record in Cloudflare) | SEO | The site is not indexed and does not show even for its own name, which is confused with Turning Wheel (Leicester) and the BPF magazine. Indexing takes weeks, so start before launch | S |
| 24 | S2–S5 | Merge PR 173: title, description, JSON-LD, favicon | SEO | Ready and mergeable. Waits only for the register to close | S |
| 25 | S7 | A sources page on wheelturning.org listing the participating sanghas and texts | SEO | That list now exists only on the app's `/rights` page, which search engines cannot see. It is also the public's view of where the material comes from | S |
| 26 | S8, S10 | Decide which public pages may be indexed (about, teacher, sangha, approved talk pages, with `Person`, `Organization` and `CreativeWork` markup), then for the app allow crawling and keep `noindex` everywhere else | SEO | Spec 00 item 9 puts this at the public launch. While robots.txt blocks the app, a URL linked from elsewhere can be listed without its content. Decision: which pages, and a Spec 06 change | M |

## P2: fast follows after the public launch

| # | ID | Item | Source | Effort |
| --- | --- | --- | --- | --- |
| 27 | A4 | Move rate limits from memory to Postgres before running a second web task. Today's single task is fine for the launch, but it is the limit on scaling out | Architecture | M |
| 28 | A6 | Watch database sizing: about 137,000 passages (the spec planned 8,000 to 15,000) on a `db.t4g.medium` with `hnsw_ef_search = 1000`. Turn on Performance Insights and check that the vector index stays in memory | Architecture | S |
| 29 | B11 (part) | Link an excerpt's reference to the likely place in the source, beyond hand-listed locators | App | M |
| 30 | B5 | Cite scrolls to the reference instead of opening a pop-up (Spec 09 change) | App | S |
| 31 | B2 | The sticky question bar sometimes does not appear. Record the conditions, then fix `answer.js` | App | S |
| 32 | S6 | Ask each sangha that has said yes to link to wheelturning.org | SEO | S |
| 33 | B20 (rest) | `askama` 0.16 (#108), `fs4` 1.1 (#109), `toml_edit` 0.25 (#110), the grouped minors (#172, #130, #131), and recheck the `paste` advisory ignore | Debt | M |
| 34 | B22 (rest) | Replace sleep-and-time tests with paused tokio time; add `wt-ml` tests | Debt | M |
| 35 | B21 (rest), B16 (rest) | Retire the alpha server, or track `serve.sh` and read `config/`; run Terraform apply and destroy from an empty account | Debt | S |

## P3: nice to have

| # | ID | Item | Source | Effort |
| --- | --- | --- | --- | --- |
| 36 | S9 | Topic pages ("what Zen teachers say about anger") listing talk titles and links, with excerpts only where a teacher opts in. The largest search opportunity, but it needs a rights decision | SEO | L |
| 37 | A5 | Move `LlmClient` and its ledger out of `wt-extract` into a `wt-llm` crate, so the answer path no longer pulls in pipeline crates | Architecture | M |
| 38 | B23 | Split the large modules when next touched: `wt-api/src/lib.rs`, `wt-db/src/rights.rs`, `wt-ingest/src/texts/mod.rs`; also the texts subsystem (about 9,000 lines) out of `wt-ingest` | Debt, architecture | M |
| 39 | A8 | Load retrieval tuning without a full image deploy (config is baked into the image) | Architecture | S |
| 40 | A9 | Prune stale branches and worktrees: about 80 remote branches and about 50 local worktrees | Architecture | S |

## Decisions the owner needs to make

These block the items named:

1. B19: move the worker to RDS now, or use the B15 stop-gap for the beta (#1).
2. B10: serve fewer excerpts, require the summary to use each one, or both (#6).
3. B9: the new privacy wording (#8).
4. B11: whether the beta tests source texts, which would move #11 to P0.
5. B13: the PRD change that removes the Report link (#13).
6. B8: the research page spec (#16).
7. Spec 10: the five open decisions (#19).
8. S8: which public pages search engines may index (#26).
9. S9: whether topic pages are worth the rights work (#36).
