# Spec 11: Private beta readiness

Draft, October 15, 2026 · @Alex

This spec covers every story `docs/roadmap.md` places before the private beta (PB1 to PB5). It is a draft for the owner's review. Code starts only after the bug register is closed and this spec is approved. Where it changes the PRD or an earlier spec, the change is listed under "Changes to other documents" and the owner must approve it first (CLAUDE.md rule 1).

Facts about the code are checked against `main` at d95de08.

## Problem

The beta at app.wheelturning.org works for the owner, but four things make it unready for invited testers:

1. **Rights.** Rights actions must be run by hand on two databases. The nightly audit has only ever checked an empty database. A removal confirmation email cannot reach an unverified address while SES is in its sandbox.
2. **Answers.** Without the relevance check, weak excerpts are served. A summary sometimes fails without a trace. The page counts disagree with each other.
3. **Data.** The privacy line overstates what is enforced, and the health-data law that covers Seattle testers has not been reviewed.
4. **Learning.** The owner cannot read tester feedback on AWS. The spend-cap alert only writes a log line. Nothing alarms on the answer path. No report measures the PRD's hypotheses.

## Goals

1. A removal, opt-out or display change takes effect in one database and every object store, and an automated check proves it each night.
2. Every served excerpt is relevant, and every summary failure is recorded with a reason.
3. Every statement to testers about their data is true, and Washington health-data handling has counsel's review.
4. The owner learns of any outage, model failure or reached cap within 15 minutes, and has a weekly report that answers Gate 1's usage questions.
5. Model cost per question falls about sevenfold through Claude Haiku 5.5 on every stage that keeps its quality.

## Non-goals

- **Texts in answers (B11).** These are the public beta's (UB2).
- **The structured feedback dialog (B12 in full).** The single form from #164 is enough for the private beta (UB4).
- **Spec 10's latency work.** Only the model switch is pulled forward (UB3.1).
- **Multi-AZ, a second web task, staging, or a pen test.** The private beta is invite-only (UB3).
- **Removing the Report link (B13).** It needs a PRD change and is paired with B12 (UB4).

## Users

| User | Needs from this spec |
| --- | --- |
| Invited tester (SSZ member) | Relevant excerpts, a summary that never silently disappears, a true account of what happens to their questions |
| Teacher or rights holder | A removal that takes effect everywhere, a confirmation email that arrives, no promises the project cannot keep |
| Owner and operator | Alerts that arrive, feedback that can be read, a weekly report against the gate |

## Success metrics

Leading, checked weekly from PB4.6:

- **Model failures:** under 2% of model calls fail.
- **Summaries:** every summary failure carries a reason, and at least 90% of standard answers show a summary.
- **Alerts:** at least one test alert reaches the owner each month.
- **Audit:** zero nightly audit findings at warning severity or above.

Lagging: Gate 1's criteria in `docs/roadmap.md`, measured over the private beta.

## PB1. Teachers' rights hold on one source of truth

User stories:
- As a teacher, I want my opt-out recorded once to stop my talks everywhere, so that I never find them served from a copy.
- As a rights holder, I want the confirmation email for my removal request to reach me, so that the request is acted on.
- As the owner, I want a nightly check that nothing removed is still served, so that I do not have to remember to look.

### PB1.1 The pipeline writes to RDS (B19, WA H1)

**Today.** The worker writes to the PC's `wt-m3-ssz`, and the GPU host reaches it through an SSH reverse tunnel. RDS was loaded from the PC on October 13. Since then, `tester`, `query_log`, `feedback`, `takedown_request` and any rights action run on RDS exist only there. The runbook warns that a reload overwrites them.

**Requirements.**

1. **The worker, every pipeline stage and the text import run against RDS** as `wt_pipeline_login`.
   - From the PC: through the batch-host forward (runbook §1).
   - From the GPU host: through its reverse tunnel to the PC's forwarded port.
   - The batch host runs only while a worker runs, as today.
2. **A database knows its own name.** A migration adds a one-row `database_identity` table (name, created at), written once at setup: `rds-v1` on RDS, `pc` on the PC.
   - `wt worker`, `wt pipeline`, every rights command and `wt takedown` read it at start and print it.
   - They refuse to run when it differs from `WT_DATABASE_NAME`, if that key is set.
   - In `WT_ENV=prod` the key is required.
3. **A one-time catch-up**, `wt db catch-up --from <pc url> --to <rds url>`, copies only pipeline-owned rows: talks, transcripts, segments, citations, passages, student questions and text units.
   - It covers talks whose state changed on the PC after the October 13 load.
   - It never writes `tester`, `query_log`, `feedback`, `takedown_request`, `rights_event` or any table the web service writes.
   - It skips any talk that is withdrawn, quarantined, excluded or opted out on RDS.
   - It runs RDS's takedown replay before copying and the parity check after, and exits non-zero on any unsafe difference.
   - It is a dry run unless `--apply` is given.
4. **After the cutover**, the PC database is set read-only (`default_transaction_read_only = on`) for 14 days, then archived and removed from the runbook. Its `.env` entries are removed or renamed so no command reaches it by accident.
5. **Runbook updates.** §1 and §12 point every pipeline and rights command at RDS. The "Rights actions on both databases" and "Reloading RDS from the PC" sections are retired when PB1.2 is also done.

**Acceptance.**
- [ ] A synthetic talk advances from cleared to ready through a worker pointed at a test database named `rds-v1`.
- [ ] A worker with `WT_DATABASE_NAME=rds-v1` refuses to start against a database named `pc`, and the reverse.
- [ ] Catch-up on two test databases copies a talk changed on the source, leaves the target's `tester`, `query_log`, `feedback` and `takedown_request` row counts unchanged, and skips a talk withdrawn on the target.
- [ ] After the real catch-up, `wt rights parity` reports no unsafe difference, and the PC database refuses writes.

### PB1.2 Objects and records move to S3 (B19, WA H1)

**Today.** `ObjectStore` (`wt-core/src/objects.rs`) has only a local-directory implementation. Setting `WT_TRANSCRIPT_BUCKET` makes the CLI exit with an error (`wt-cli/src/stages.rs`). Records live under `data/records` (`LocalRecords`). Takedown iterates the object prefixes in `StoreRegistry`.

**Requirements.**

1. **An S3 implementation of `ObjectStore`**, selected when `WT_TRANSCRIPT_BUCKET` is set. Credentials come from the task or batch role, or the AWS profile on the PC.
   - `delete_prefix` deletes every version under the prefix, so a takedown leaves nothing behind in a versioned bucket.
   - `list` pages through results.
2. **Records get the same treatment.** A `Records` S3 implementation under `WT_RECORDS_BUCKET`, or a `records/` prefix in the transcripts bucket (open question 3).
   - Versioning on.
   - The pipeline role may not delete versions.
3. **One-time copy.** `wt objects sync --from data/objects` copies every object, verifies count and SHA-256 per key, and lists any mismatch. The same goes for records.
4. **Audit and takedown.** `wt audit` checks the S3 prefixes for every withdrawn talk. Takedown counts S3 deletions per store, as today.
5. **The web service** reads transcripts through the same trait. Its task role gets read access to the transcripts prefix only.
6. **Tests make no network calls (rule 9).** A shared contract test suite runs against the local store and an in-process fake. Whether the S3 client is also tested against a MinIO container is open question 4.

**Acceptance.**
- [ ] The contract suite passes for the local store and the fake.
- [ ] A takedown of a synthetic talk leaves zero keys and zero versions under its prefixes in the fake.
- [ ] `wt objects sync` reports matching counts and hashes for the PC's objects and records.
- [ ] `wt audit` reports a planted object under a withdrawn talk's prefix.
- [ ] Runbook §10 (local backups) is retired once both PB1.1 and PB1.2 are done.

### PB1.3 Fallback while two databases remain (B15, WA REL09-BP01, REL09-BP03)

Build this only if PB1.1 cannot land before the beta.

1. **Rights commands act on both databases.** Every rights command takes `--also <url>` or `WT_OTHER_DATABASE_URL`: takedown, `teacher opt-out`, exclusion, display level and `talks_basis`.
   - The action is applied to each database in its own transaction.
   - The command then runs `wt rights parity` and exits non-zero on any `"unsafe": true` difference.
2. **A nightly export.** `tester`, `query_log`, `feedback`, `takedown_request` and `rights_event` are exported from RDS to the backups bucket.
3. **A stale-backup check.** The nightly audit (PB1.4) reports a warning when the newest PC backup object is more than 36 hours old.

**Acceptance.**
- [ ] A takedown with `--also` removes a synthetic talk from both test databases, and parity passes.
- [ ] A forced failure on the second database exits non-zero and names the database.

### PB1.4 The nightly audit runs on RDS (A1, WA H1)

**Today.** `.github/workflows/nightly.yml` audits a freshly migrated database whenever `WT_AUDIT_DATABASE_URL` is unset, which it has been on every run. GitHub cannot reach private RDS.

**Requirements.**

1. **A scheduled ECS task.** A `wt-v1-audit` task definition, on the deployed image with the eval task's network settings, runs `wt audit --config-dir config --fail-on warning` against RDS. It runs as a read-only audit login, or as `wt_owner_login` if no audit role exists (open question 5). EventBridge Scheduler starts it daily at 10:17 UTC.
2. **An alarm on failure.** An EventBridge rule on ECS task state changes sends SNS a message when the audit task stops with a non-zero exit code. The report goes to the `/wt-v1/admin` log group.
3. **What the audit checks.** It includes `takedown_request_overdue`, the internal one-business-day reminder kept by B14, and the stale-backup check from PB1.3 while it applies.
4. **The GitHub workflow** keeps its fresh-database run and is renamed "audit smoke test", so it is not mistaken for the real audit.

**Acceptance.**
- [ ] The scheduled task runs nightly and writes its report to the log group.
- [ ] A one-off run with a command that exits 1 produces an SNS email.
- [ ] `wt audit --fail-on warning` exits non-zero on a planted warning in a test database.

### PB1.5 Removal emails reach anyone (WA REL01-BP04, #165)

**Today.** SES is in its sandbox (`infra/terraform/account/email.tf`), so confirmation emails reach only verified addresses. A production-access case is open.

**Requirements.**

1. **Production access.** The owner completes the open case. AWS asks for the sending purpose, volume and bounce handling.
2. **Sender authentication.** wheelturning.org has SPF, DKIM (SES Easy DKIM) and a DMARC record in Cloudflare.
3. **Bounces and complaints** go to an SNS topic, and the runbook says how to read them.

**Acceptance.**
- [ ] A removal request from an unverified address receives its confirmation email.
- [ ] DMARC reports `pass` for a test message.

### PB1.6 The web server refuses to start without its settings (B17)

**Requirements.**

1. **All keys go through the loader.** These move into `wt_core::config::Config`, read and checked once at startup: `WT_MAIL_FROM`, `WT_PUBLIC_URL`, `WT_OWNER_NAME`, `WT_TRUST_FORWARDED_FOR`, `WT_CONFIG_DIR`, `WT_PRIVACY_DEFAULT`, `WT_QUERY_RETENTION_DAYS` and `WT_COUNT_ONLY_TTL_HOURS`.
2. **Mail settings are checked.**
   - In `WT_ENV=prod`, a missing `WT_MAIL_FROM` or `WT_PUBLIC_URL` stops startup with an error naming the key.
   - In any environment, setting one without the other stops startup.
3. **Every key is documented.** `.env.example` and Spec 01 list every key, including `WT_POPPLER_BIN`, `WT_TESSERACT` and `WT_DATABASE_NAME` (PB1.1).
4. **A guard test.** A new check in `wt-core/tests/repo_guards.rs` fails on `std::env::var` outside `wt_core::config` and test code. Any exceptions are named in the test.

**Acceptance.**
- [ ] Startup in prod with `WT_PUBLIC_URL` and no `WT_MAIL_FROM` exits with an error naming `WT_MAIL_FROM`.
- [ ] The guard test passes on the changed code and fails on a planted `std::env::var` in `wt-api`.

### PB1.7 No public promise of a time to act (B14)

**Requirements.**

1. **Remove every public time promise.** The Rights page, its confirmation, `copy/rights.md` and Spec 09 no longer state a time to act. Replacement wording is the owner's (open question 1). It may say that requests are handled in the order they arrive.
2. **Keep the internal target.** The one-business-day target stays in Spec 08 as an internal reminder, with its alert and audit check.

**Acceptance.**
- [ ] A test fails if any public template or `copy/` file contains "business day", or "within" followed by a number and a unit of time.
- [ ] `takedown_request_overdue` still fires in the audit test.

## PB2. Answers testers can trust

User stories:
- As a tester, I want every excerpt on the page to be about my question, so that I do not wade through passages that only share a word with it.
- As a tester, I want to know when a summary could not be written and why, so that I trust the page when one is shown.
- As a tester, I want to know who wrote the summary, so that I never take a model's sentence for a teacher's.

### PB2.0 Claude Haiku 5.5 on every stage that keeps its quality (pricing of October 15, Spec 10)

**Today.** Every stage runs on `claude-haiku-4-5` ($1 and $5 per million tokens). Stage keys exist for synthesis, verification, extraction and vision; other stages fall back to `WT_LLM_MODEL`.

**Requirements.**

1. **Add the model to the price table.** `config/llm_prices.toml` gains `claude-haiku-5-5` at $0.10 input and $0.50 output, for prompts up to 100,000 tokens.
   - The client refuses a request to it whose prompt exceeds 100,000 tokens, since the table has no long-context price. No current stage comes near that.
   - Cache prices are added when Spec 10 D2 lands.
2. **Find out whether it accepts sampling settings.** Make one request with temperature 0 and record the answer as its `sampling` flag. If it does not accept them, record what changes for the stages that rely on temperature 0.
3. **One model key per stage.** Every stage in `llm_usage.stage` gets its own key, falling back to `WT_LLM_MODEL`. That adds `WT_LLM_MODEL_RANK`, `WT_LLM_MODEL_EXCERPT` and `WT_LLM_MODEL_TRIAGE` where they do not exist yet.
4. **Compare each stage on the deployed stack.** Run every stage on each model with `wt eval run` and `wt eval compare`. A stage moves to Haiku 5.5 only if it meets its rule:

   | Stage | Moves to Haiku 5.5 when |
   | --- | --- |
   | Triage | No acute question in the sensitive set is missed, and the rate of wrongly stopped questions is no higher |
   | Verifier | The 20-answer claim audit shows no unsupported sentence, and the share of drafted sentences kept is within 5 points |
   | Summary | The share of answers showing a summary is no lower, and the owner prefers it or calls it equal on 20 blind pairs |
   | Ranking | Judged 7+ candidates are at least 95% of Haiku 4.5's |
   | Excerpts | The owner's "cut off or missing context" rate on 30 excerpts is within 5 points |

5. **Record the result.** The chosen model per stage goes into the Terraform variables and Spec 01, with the eval run ids and the measured cost per question.

**Acceptance.**
- [ ] The eval runs and comparisons for each stage are recorded in the PR.
- [ ] Each stage's model is set by its own key, and a test shows the fallback to `WT_LLM_MODEL`.
- [ ] Mean model cost per question on the 50-question set is reported before and after.

### PB2.1 The relevance check is live (B1 cause 4, B10)

**Requirements.**

1. **Turn it on.** The deployed `config/retrieval.toml` names the model ranker, and the beta's answer page no longer shows "the relevance check is not installed".
2. **Log which ranker served.** Every answer logs which ranker served it, `llm`, `onnx` or `none (fallback)`, for PB4.3's alarm.

**Acceptance.**
- [ ] Ten eval questions on the live stack show ranked order and no "not installed" notice.
- [ ] A forced ranker timeout logs `none (fallback)` and the page says so.

### PB2.2 The summary uses what the page shows (B10)

This is blocked on open question 2: serve fewer excerpts, require the summary to use each one, or both.

**Recommended (both).**
- Serve only excerpts that pass the relevance threshold (`rerank_min_score`), as PB2.1 makes true.
- The summary prompt must cite each served excerpt at least once, or return its id under `unused` with a reason code: `off_topic` or `duplicate`.
- Unused excerpts move to a collapsed "Also related" list under the teacher groups. They are never removed from a page already shown.

**Diagnosis first.** Pull the stored draft and `llm_usage` rows for answer `40fc360b`. Record whether "Where the talks agree" and "Where they differ" sentences were written and then dropped by the verifier.

**Acceptance (recommended option).**
- [ ] On the 50-question set, every served excerpt is cited by the summary or listed as unused with a reason.
- [ ] The claim audit still shows no unsupported sentence.
- [ ] Answers with three or more teachers show at least one agree or differ sentence in at least 60% of cases, or the shortfall is reported with its cause.

### PB2.3 The page reads cleanly (B10)

**Requirements.**

1. **No em dashes in summaries.** The summary prompt forbids em dashes. Code replaces any that come back in model text with a comma or a colon before the verifier runs. Quotations are never altered.
2. **No sangha-count note.** The "n of the m excerpts come from one sangha" note is removed from `notes` (`wt-api/src/summary.rs`) and from Spec 05.
3. **Counts that agree.**
   - The top line (`view.rs`, "Passages from n teachers") becomes "x teachers, y teachings, z excerpts". Teachings are distinct talks; texts count once texts ship. Each count uses its singular when it is 1.
   - The summary's "Drawn from" line becomes "The summary draws on n of these excerpts".
   - Final wording is the owner's, through Spec 09.

**Acceptance.**
- [ ] A test feeds a draft containing an em dash and finds none in the stored summary, while a quotation containing one is unchanged.
- [ ] View tests cover the singular and plural forms of the three counts.

### PB2.4 No summary fails silently (B4)

**Requirements.**

1. **Diagnose B4.** Use the beta logs and `llm_usage` for answer `131b0c65` and record the cause in `docs/bugs.md`.
2. **Record a reason on every answer.** A migration adds `summary_status` to the answer row:
   - `shown`, `not_attempted` (a named-teacher or sensitive question), `timeout`, `model_error`, `cap_reached`, `all_sentences_rejected`, `no_key` or `deferred_lost`.
   - It is written whenever the deferred summary settles, and set to `deferred_lost` by the hourly purge for any answer still pending after 10 minutes.
3. **Report it.** `wt eval report` and PB4.6's report count each status.

**Acceptance.**
- [ ] A test for each failure path stores its status.
- [ ] No answer row stays pending past the purge.

### PB2.5 Summaries are labeled (L9, L13, C1)

**Requirements.**

1. **A note under every summary.** Every summary block carries one line, for example: "Written by a language model from the excerpts on this page and checked against them. The teachers have not reviewed it."
   - It never names the provider (public copy rule).
   - Final wording goes through Spec 09.
2. **Key lines carry no note.** Spec 10's extractive "key lines", when built, are verbatim and carry no model note.

**Acceptance.**
- [ ] A page test finds the note on every answer that shows a summary, and not on one without.

### PB2.6 Gate A (Spec 00)

This is the owner's work. It is done when `docs/gate-a.md` records the teacher reviewer's ratings and the pass or fail decision.

## PB3. Testers' data is handled as promised

User stories:
- As a tester, I want the line about my questions to say exactly what happens to them, so that I can choose how to use the site.
- As a tester in Washington, I want my health-related questions handled under the state's law, so that sensitive moments are not kept or shared without my consent.
- As a tester in distress, I want to be pointed to real help, so that I am not left with search results.

### PB3.1 A true privacy line (B9, L4)

**Today.**
- `LOGGING_LINE` (`wt-api/src/chrome.rs`) says "Only totals are read, and one question is opened only if you flag it".
- Testers who never chose are in `share` mode, which keeps the question text for 180 days with obvious personal details removed (Spec 06).
- Every question's text is also sent to a language-model provider.

**Requirements.**

1. **The line states only what the system enforces:**
   - The default mode.
   - How long text is kept, and that obvious personal details are removed.
   - The count-only choice and how to delete all questions.
   - That questions go to a language-model provider.
   - Who may read kept text, as decided in open question 6.
2. **One source.** The line lives in one `copy/` file that `chrome.rs`, `home.html` and the invitation read. A test fails if they differ.
3. **Suggested draft for the owner:** "Your questions are kept for 180 days with emails and phone numbers removed, unless you choose counts only. They are sent to a language model provider to find and summarize passages. You can delete them at any time."

**Acceptance.**
- [ ] A copy-consistency test passes.
- [ ] The owner approves the wording in the PR.

### PB3.2 A complete privacy page (L4)

**Requirements.** The privacy page states:

- What is stored, per mode, and for how long.
- That questions, and the passages sent with them, go to a language-model provider to rank, cut and summarize. How long the provider keeps them, from its current terms, which the owner confirms.
- That feedback notes are read without the question unless the question was flagged.
- How to delete everything.
- A contact for privacy requests.
- Whether the provider must be named is open question 7.

**Acceptance.**
- [ ] Each statement on the page cites the spec item or setting that enforces it, in the PR description.

### PB3.3 Washington health-data handling (L3)

**Requirements.**

1. **Counsel first.** The owner asks counsel whether question text and triage results are consumer health data under the My Health My Data Act. Steps 2 and 3 follow only if counsel says they are, or cannot say.
2. **If covered:**
   - A consumer health data privacy policy, linked from the home page.
   - Consent to collect, asked when an invite is accepted, separate from the terms.
   - Deletion through the existing delete-all.
   - No sharing beyond the language-model provider as processor without separate consent.
3. **Minimal retention, whatever counsel says.** A question the triage marks `medical` or `distress` is stored count-only, whatever the tester's mode. Acute questions already store counts only (`ask.rs`).

**Acceptance.**
- [ ] Counsel's answer is recorded in `docs/strategy-research.md` (L3).
- [ ] A test shows a `distress` question stored count-only under `share` mode.

### PB3.4 Crisis help is correct (L7)

**Today.** `support_text.html` gives 988 by call, text and chat, 911, and findahelpline.com. The sensitive banner says the site "is not counseling".

**Requirements.**

1. **State it on the support page too.** The support page adds the "not counseling" line.
2. **Check the help links.** The nightly audit task (PB1.4) checks that each help link answers. Checking from AWS is allowed; CI tests make no network calls.
3. **Keep acute questions out of storage.** The eval's sensitive-route scoring keeps its acute set, and a test asserts that no acute question's text reaches `query_log` or the logs.

**Acceptance.**
- [ ] The not-counseling line is shown on the support page.
- [ ] The link check reports a planted dead link.
- [ ] The acute storage test passes.

### PB3.5 Security headers (WA SEC09-BP02)

**Requirements.** `security_headers` (`wt-api/src/lib.rs`) adds three headers:

- `Strict-Transport-Security: max-age=31536000`, in `WT_ENV=prod` only.
- `frame-ancestors 'none'`, added to the CSP.
- `Permissions-Policy: camera=(), microphone=(), geolocation=(), browsing-topics=()`.

**Acceptance.**
- [ ] Page tests assert all three headers in prod mode, and no HSTS header in dev.

### PB3.6 Edge protection and detection (WA M1, M2, SEC03-BP04)

**Requirements.**

1. **A WAF on the load balancer.** A WAFv2 web ACL in `infra/terraform/modules/service` with:
   - AWS Common and Known Bad Inputs managed rule groups.
   - A rate rule of 300 requests per 5 minutes per IP.
   - Count mode for 7 days, then block, with the Common group's body-size rule kept in count if the rights form trips it.
   - The ask stream (server-sent events) is verified to work behind it.
2. **Detection in `infra/terraform/account`.**
   - GuardDuty on.
   - IAM Access Analyzer for external and unused access.
   - CloudTrail metric filters with alarms to SNS for root sign-in and IAM policy changes.
3. **Cost.** Expected about $10 to $15 a month, recorded in `docs/cost.md`.

**Acceptance.**
- [ ] Terraform validates and applies.
- [ ] A burst of 400 requests from one address in 5 minutes is counted, then blocked once blocking is on.
- [ ] A test IAM policy change triggers the alarm.

## PB4. The owner can learn from the beta

User stories:
- As the owner, I want to read testers' feedback on the live beta, so that I can act on it.
- As the owner, I want to hear about failures before testers tell me, so that the test is not spoiled.
- As the owner, I want a weekly report against Gate 1, so that the go or no-go decision rests on data.

### PB4.1 Feedback can be read on AWS (B12, part)

**Requirements.**

1. **An analyst login.** Terraform's secrets module adds `db_wt_analyst_login`, and `wt db create-login` creates `wt_analyst_login` on RDS.
2. **Runbook §12** shows `wt report feedback` run as the analyst through the forward.
3. **Deploy the form.** The next image deploys the single feedback form from #164.

**Acceptance.**
- [ ] `wt report feedback` runs against RDS as the analyst and prints notes without their questions unless flagged.

### PB4.2 Alerts reach the owner (B16, WA OPS10-BP04)

**Today.** The spend-cap alert uses `LogAlerter` in both the web service and the CLI, so it only writes a log line. `wt alerts test`, which Spec 01 names, does not exist.

**Requirements.**

1. **Alerts go to SNS.** An `SnsAlerter` implements `Alerter`, publishing to `WT_ALERT_TOPIC_ARN`. The web service and CLI use it when the key is set, and `LogAlerter` otherwise. A fake is used in tests.
2. **A test command.** `wt alerts test` publishes one message.
3. **Narrow permissions.** The web and batch roles may publish to that topic and nothing else in SNS.
4. **Two recipients.** The topic has a second subscriber.
5. **Tick the Spec 01 items** this covers.

**Acceptance.**
- [ ] `wt alerts test` reaches both subscribers. (October 16, 2026: published to the one confirmed subscriber, the owner's address; the second address is not named yet, `second_alert_email`.)
- [x] The cap alert's 80% message reaches SNS in a test with the fake. (`the_eighty_percent_alert_reaches_sns`, PR 182.)
- [x] IAM simulation denies publishing to another topic. (October 16, 2026: the web, eval and batch roles are allowed `sns:Publish` on `wt-v1-alarms` and implicitly denied on any other topic and every other SNS action. The workload boundary needed the same grant.)

### PB4.3 Alarms on the answer path (A3, WA OPS04-BP04)

**Requirements.**

1. **Structured log events** for each, never carrying question text (the existing log-scrubber test covers them):
   - `llm_call_failed` with stage.
   - `llm_cap_reached`.
   - `summary_settled` with `summary_status` (PB2.4).
   - `ranker_served` with the ranker used.
   - `ask_completed` with milliseconds.
2. **CloudWatch metric filters on these events, with alarms to SNS** (missing data counts as not breaching):

   | Alarm | Fires when |
   | --- | --- |
   | Model failures | More than 20% of model calls fail in 15 minutes, with at least 5 calls |
   | Cap reached | Any `llm_cap_reached` event |
   | Summaries | More than 30% of answers in an hour end in any status other than `shown` or `not_attempted`, with at least 5 answers |
   | Ranker fallback | Any answer served with `none (fallback)` in an hour |
   | Slow answers | p95 ask time above 8 seconds over an hour |

**Acceptance.**
- [x] Each event is emitted in a test with a fake. (`llm_call_failed`, `llm_cap_reached` and `ask_completed` in PR 182; `summary_settled` and `ranker_served` in `crates/wt-api/tests/pages.rs` with PB2.4 and PB2.1.)
- [x] Each alarm exists in Terraform with a runbook entry (PB4.4). (Applied October 16, 2026.)

### PB4.4 Runbook, incident procedure and tester notice (WA M8, OPS10-BP01, OPS10-BP05, OPS11-BP02, SEC10-BP01)

**Requirements.**

1. **A runbook entry per alarm**, covering the six existing alarms and PB1.4, PB3.6 and PB4.3's: what it means, the first check, the fix, and when to scale to zero.
2. **A ten-line incident procedure** and `docs/incidents/TEMPLATE.md`.
3. **A security contacts list**, kept privately, with its location named in the runbook.
4. **A maintenance page.** A Terraform variable `maintenance` that, when true, makes the load balancer answer every path with a fixed page.
5. **An email template** for telling testers about an outage.

**Acceptance.**
- [x] The runbook names every alarm in Terraform. A test compares the two lists. (`crates/wt-core/tests/runbook_alarms.rs`.)
- [x] Setting `maintenance = true` in a plan shows the fixed response. (`infra/terraform/modules/service/tests/maintenance.tftest.hcl`, run in CI.)

### PB4.5 Recovery and availability targets (WA M4)

Spec 01 gains targets for the private beta. Proposed:

- Recovery time: 8 hours.
- Data loss: 15 minutes. RDS point-in-time recovery allows this; it was 24 hours while the PC held pipeline data.
- Availability: 99% a month, measured by the external health check.

The owner set these in the tiered targets table below (open question 8): the database and site are Tier 1. Note: 99.9% availability is not reachable on one database instance in one availability zone and one web task. The private beta records availability against 99.9% and reports the gap. Two zones and two tasks (UB3.3) are the fix.

### PB4.6 The weekly beta report (V-A1, F1, WA OPS04-BP01, OPS04-BP03)

**Requirements.** `wt report beta --since 7d` runs as the analyst and prints totals only. It never prints question text and works for count-only questions. It prints:

- **Testers:** active testers (at least one question in the period), and returning testers (questions in two or more distinct weeks since the beta began).
- **Questions:** median questions per active tester.
- **Feedback:** counts by feedback reason, and the share marked helpful.
- **Dana:** click-through, as dana link clicks per answer and the share of testers with at least one click.
- **Summaries:** the share of answers showing a summary, by `summary_status`.
- **Cost:** model cost per question, as mean and p95. This needs Spec 10 E1's `llm_cost_usd` per answer, which is pulled forward into this story.
- **Speed:** p50 and p95 for time to the first progress event and to the page.

**Acceptance.**
- [x] A test on synthetic data checks each figure. (`wt-db/tests/beta_report.rs`, as a `wt_analyst` login, with count-only, purged and acute questions and an evaluation run left out; the per-answer cost and times are written as `wt_web` in `wt-db/tests/web_role.rs` and on a streamed ask in `wt-api/tests/progress.rs`; migration `0048_beta_report.sql`.)
- [x] A test asserts that no question text appears in the output. (Same test: no question text, feedback note or tester id in the printed report.)

### PB4.7 The cost model (F1, WA COST05-BP06)

`docs/cost.md` gains a model:

- Model cost per question by stage, from PB4.6, before and after PB2.0.
- Fixed AWS cost by service.
- Totals at 100, 1,000 and 10,000 questions a day.
- A monthly ceiling with the action taken at 80% and 100% of it.

Done in `docs/cost.md`, "Cost model": the after-switch figures are estimates until PB2.0's eval runs replace them, and the model rows are to be replaced by `wt report beta` figures after the first beta week.

## PB5. Evidence for the next increment (owner work)

No code. Each is done when its record exists.

| Story | Done when |
| --- | --- |
| PB5.1 Transcripts for permission, three sanghas | Offers sent, answers recorded in the rights ledger or `docs/strategy-research.md` (V4) |
| PB5.2 Interviews, 10 testers and 5 teachers | Notes summarized in `docs/` without names; findings against V1 to V4 |
| PB5.3 Counsel on L1, L2, L6, L8, L11 | Engagement started, questions sent, expected dates recorded |
| PB5.4 Fiscal sponsor or entity | Sponsor chosen and agreement signed, or entity formed |

## Changes to other documents

To approve before code:

| Document | Change | Story |
| --- | --- | --- |
| PRD | Principle 4, the success criterion, the risk row and the scope table drop the one-business-day promise | PB1.7 |
| Spec 01 | New keys; per-stage model keys; the audit task; WAF and detection; alarms; analyst login; recovery and availability targets; chosen models | PB1.4, PB1.6, PB2.0, PB3.6, PB4.1 to PB4.5 |
| Spec 02 | `database_identity`; `answer.summary_status`; `query_log.llm_cost_usd` | PB1.1, PB2.4, PB4.6 |
| Spec 03 | Display levels rule 4 without a public time | PB1.7 |
| Spec 05 | No sangha-count note; the B10 decision on unused excerpts | PB2.2, PB2.3 |
| Spec 06 | Security headers; the privacy line's single source; distress questions stored count-only | PB3.1, PB3.3, PB3.5 |
| Spec 07 | The audit runs on RDS; `wt report beta` | PB1.4, PB4.6 |
| Spec 08 | One business day becomes an internal target | PB1.7 |
| Spec 09 | Rights copy; the privacy line; counts line; summary label; support page line | PB1.7, PB2.3, PB2.5, PB3.1, PB3.4 |
| Spec 10 | E1 pulled forward; Haiku 5.5 recorded as the baseline once chosen | PB2.0, PB4.6 |

New migrations take the next numbers after 0046. They are forward-only, as always.

## Open questions

| # | Question | Who | Blocking | Answer |
| --- | --- | --- | --- | --- |
| 1 | Replacement wording for the removed time promise | Owner | PB1.7 | Removal request are addressed in the order. Please note this site is maintained as a side project by one person, so timelines may vary, but are approached with the intention to be as timely as possible |
| 2 | B10: serve fewer excerpts, require the summary to use each, or both (recommended) | Owner | PB2.2 | Both |
| 3 | Records in their own bucket, or a prefix in the transcripts bucket | Engineering | PB1.2 | Their own versioned bucket: records survive takedown while transcripts are deleted, so their rules differ |
| 4 | May S3 code be tested against a local MinIO container (like Postgres), or only through the fake | Owner, engineering | PB1.2 | Code can be tested in a local container, as long as it's very cheap |
| 5 | A dedicated read-only audit role, or the owner login for the nightly audit | Engineering | PB1.4 | A dedicated read-only `wt_audit_login` |
| 6 | Who may read kept question text in `share` mode, and when | Owner | PB3.1 | Who: Site code, owner, any approved team members. When: whenever requesting access. How: SQL queries, or a simple dashboard  |
| 7 | Must the privacy page name the language-model provider, given the rule against naming it in public copy | Legal | PB3.2 | No, as the model provider can change over time |
| 8 | Recovery time, data loss and availability targets | Owner | PB4.5 | See section below |
| 9 | Gate 1 thresholds in `docs/roadmap.md` | Owner | Gate 1 | Revised and approved |


## Tiered Targets Table
Based off of recommended standards for small non-profits
| Tier / Criticality | Systems Included | Recovery Time (RTO)<br>_“How long can we be down?”_ | Data Loss (RPO)<br>_“How much data can we lose?”_ | Availability Target<br>_“Uptime Standard”_ |
| :--- | :--- | :--- | :--- | :--- |
| **Tier 1: Mission-Critical** | • Core systems (databases, chat, site availability) • Core email & chat (Google Workspace/M365)<br>• | **4 to 24 hours** | **24 hours** <br>*(Daily backup minimum)* | **99.9%**<br>*(~9 hours of downtime / year)* |
| **Tier 2: Operational** | • Shared files and cloud drives<br>• Internal project tracking tools<br>•| **24 to 48 hours** | **24 to 48 hours** | **99.0%**<br>*(~3.6 days of downtime / year)* |
| **Tier 3: Low-Priority** | • Archived historical program records<br>• Past marketing materials<br>• Inactive volunteer files etc. | **3 to 7 days** | **1 week** | **95.0%**<br>*(Best effort)* |

### Key Implementation Practices

#### 1. Avoid the "Cloud Myth"
Many small non-profits mistakenly assume that because they use Google Workspace or Microsoft 365, their data is fully backed up. These platforms guarantee **system availability** (uptime), but they do not protect against data loss from human error, malicious deletion, or ransomware. An independent cloud-to-cloud backup tool is highly recommended to secure a separate copy of donor and financial records.

### 2. Follow the 3-2-1 Backup Rule
To reliably meet your RPO targets, your backup strategy should follow this industry standard:
* Maintain **3** total copies of your data.
* Store them on **2** different types of media (e.g., local server/hard drive and cloud).
* Keep **1** copy completely offsite or in an immutable cloud repository.

### 3. Test Twice a Year
A recovery target is just a guess until it is tested. Non-profits should conduct a simple data restoration test at least **twice a year**. Have a staff member or IT partner attempt to restore a randomly selected donor folder or financial spreadsheet from a backup to ensure the process works and fits within your targeted RTO.

## Order

Phases follow dependencies. Within a phase, stories can run in parallel on separate files.

1. **Small, independent fixes:** PB1.6, PB1.7 (after question 1), PB2.1, PB2.3, PB2.5, PB3.5, PB4.1, PB4.2.
2. **Measurement before change:** PB2.4 and PB4.6 (with `llm_cost_usd`), then PB2.0 (model per stage), then PB2.2 (after question 2, tuned on the chosen model).
3. **Rights consolidation:** PB1.2, then PB1.1 with its catch-up. PB1.3 only if PB1.1 slips. PB1.4 once RDS is the target. PB1.5 runs throughout, since it waits on AWS.
4. **Operations:** PB3.6, PB4.3 (needs PB2.4's events), PB4.4, PB4.5, PB4.7.
5. **Privacy:** PB3.1 and PB3.2 (after questions 6 and 7), PB3.3 (after counsel), PB3.4.
6. **Owner work** runs throughout: PB2.6, PB5.1 to PB5.4.

The private beta opens when every acceptance item above is ticked or the owner records it as waived, and Gate A has passed.
