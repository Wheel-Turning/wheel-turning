# Spec 12: One server and general availability

Draft, November 2, 2026 · @Alex

This spec moves the site from AWS to one rented server and opens it to everyone. It is a draft for the owner's review. Code starts only after the owner approves it, including the changes listed under "Changes to other documents" (CLAUDE.md rule 1).

Facts about the code are checked against `main` at 9e47434.

## Owner's decisions of November 2, 2026

1. **Host on one server.** One x86 virtual server runs Postgres 16 with pgvector, the web service and `cloudflared`. It replaces ECS, RDS and the batch host. The owner compared it with a trimmed AWS stack: about $60 to $70 a month on AWS (the web task and `db.t4g.small` in `docs/cost.md`) against $10 to $20 on one server.
2. **General availability.** Anyone can use the site without an invite or an account. This skips Gate A's remaining items, Gate 1 and Gate 2 in `docs/roadmap.md`, and changes the PRD, which keeps v1 private.
3. **Mandated talks are excerpts only.** The public sees a talk in full only when it has a teacher's consent, a license, or a sangha's recorded permission. Every other talk is served at display level b, `excerpts`, at most.
4. **Model spend cap of $40 a month** (from $60). At the cap, summaries pause and excerpts still serve (Spec 11, PB4.7).
5. **EU, EEA and UK visitors are geo-blocked for now.** A Cloudflare rule shows them a short notice page instead of the app (roadmap UB1.5).

## Problem

1. **Cost.** The fixed AWS bill is about $100 a month for a site with a few questions a day. A portfolio piece must run for years on a hobby budget.
2. **Access.** Every route assumes an invited tester: answers, rate limits, privacy modes and data deletion all key on `tester.id` (Spec 06, "Invite token access"). A public visitor gets a `404`.
3. **Rights exposure.** Most talks rest on the owner's mandate, not on recorded permission. The PRD's risk table asks for review before that exposure grows (PRD, Risks, "Exposure grows once the tool is public").
4. **Public cost risk.** A public ask form with a monthly cap can be drained in an afternoon by one script, after which no visitor gets a summary for the rest of the month.

## Goals

1. Fixed hosting cost at or below **$25 a month**, with every rights guarantee unchanged.
2. Anyone outside the blocked region can ask a question and read excerpts, without an invite.
3. No talk without consent, license or permission shows more than excerpts. A test fails the build if one does.
4. One day's traffic, or one visitor, cannot spend more than one day's share of the cap.
5. The database can be rebuilt from off-site backups within the Tier 1 targets of Spec 01.

## Non-goals

- **Texts in answers (UB2), the feedback dialog (UB4), sangha archives (FL1).** Unchanged by this spec.
- **High availability.** One server is a single point of failure by choice. The availability target changes to match (open question 3).
- **EU and UK compliance (UB1.5's "comply" path).** Revisit once usage justifies it.
- **Accounts.** The PRD keeps accounts out of scope. Visitors are anonymous.

## Users

| User | Needs from this spec |
| --- | --- |
| Public visitor | Ask without signing up, read excerpts with attribution, know what happens to their question |
| Teacher or rights holder | No full transcript without consent; removal that still takes effect everywhere; a terms page and a DMCA agent to write to |
| Invited tester and teacher reviewer | The invite link keeps working and keeps their history |
| Owner and operator | One server to run, nightly backups off the box, alerts by email, a bill under $25 |

## Part A: one server

### A1. The server

- x86-64 only. On arm64 the int8 embedder's vectors drift from the stored x86 ones (`docs/cost.md`, web task size), so an ARM plan is refused.
- At least 4 vCPU, 8 GB of memory and 80 GB of disk, in a US region. Hetzner's CPX31 in Hillsboro, Oregon fits at about $15 a month with backups. The owner buys and owns the account. Claude never creates it.
- Ubuntu 24.04 LTS with unattended security upgrades, SSH by key only, password login off, `ufw` denying all inbound except SSH. The web service has no inbound port: Cloudflare reaches it only through the tunnel.

### A2. What runs on it

One `docker compose` project under `/srv/wheel-turning`, defined in `infra/vps/compose.yaml`:

| Service | Image | Notes |
| --- | --- | --- |
| `db` | `pgvector/pgvector:pg16`, the image the tests already use | Data on a named volume. `shared_buffers` 1 GB, so the 1.7 GB database and its indexes stay in memory (`docs/cost.md`, Database size). Listens on the compose network and on `127.0.0.1:5432` only |
| `web` | `ghcr.io/<owner>/wt-v1:<tag>` | Same image as today. Models mounted read-only from `/srv/wheel-turning/models` |
| `cloudflared` | `cloudflare/cloudflared`, pinned by digest | Runs the existing tunnel `wt-v1-web` with its token. The tunnel's ingress points at `http://web:8080` |

The database roles of Spec 02 are created exactly as on RDS. The admin role is a superuser here, so role-creating migrations (Spec 11, 0050) no longer need a separate admin task.

### A3. Secrets

Secrets live in `/srv/wheel-turning/.env`, mode 0600, owned by root and read only by compose. They are the same keys `.env.example` lists, and the code still wraps each one in `wt_core::config::Secret`. Secrets Manager is retired for the server (open question 6).

AWS stays for four things that cost cents: the records bucket (Object Lock), SES removal emails, SNS alerts and the backup bucket. One IAM user, `wt-vps`, holds a policy for exactly those actions, inside the existing permissions boundary. Its access key goes in `.env` and is rotated every 90 days (runbook).

### A4. Backups and restore

- **Nightly dump.** A systemd timer runs `pg_dump -Fc` at 03:00 Pacific and uploads it to `s3://wt-backups-<account>/vps/` with server-side encryption. A lifecycle rule keeps 30 days. This meets the 24-hour data-loss target.
- **Server snapshots.** The provider's daily backups are on as a second copy.
- **Restore check.** The weekly timer restores the newest dump into a throwaway container, runs `wt audit` against it and emails the result through SNS. A failed restore is an alert.
- **Rebuild.** `infra/vps/bootstrap.sh` turns a fresh Ubuntu server into a running site from the newest dump. The twice-yearly drill (Spec 01) runs this script and times it against the 4-hour recovery target.

### A5. Alerts and the nightly jobs

Replacing CloudWatch:

| Today | On the server |
| --- | --- |
| `wt-v1-healthz-unreachable` (Route 53 check on `/healthz`) | Kept unchanged. It checks the public path through Cloudflare, so it does not care where the app runs |
| Log metric filters and the 5xx alarm | The web service counts 5xx answers itself and alerts through the existing `SnsAlerter` above 5% over 10 minutes |
| ECS task alarms, memory alarm | A timer every 5 minutes alerts at 85% memory, 80% disk, or any compose service not running |
| Nightly audit ECS task | The same `wt audit` on a systemd timer, with the same SNS email |
| `purge_expired_queries` | Unchanged, on a timer |
| CloudWatch Logs, 30 days | journald with a 30-day, 2 GB cap. Request logs keep the route template only, as today |

### A6. Deploys

`deploy.yml` builds the image, pushes it to GHCR and deploys over SSH with a deploy key restricted to one command (`/srv/wheel-turning/deploy.sh <tag>`). The script runs migrations as `wt_owner_login`, starts the new web container, waits for `/healthz`, and returns to the previous tag if the check fails within 2 minutes. During the swap the maintenance Worker serves the 503 page (Spec 11, PB4.4).

### A7. The transcription worker

The owner's PC reaches the database through an SSH port forward to `127.0.0.1:5432` on the server, on local port 5433 as today, so `.env`'s `DATABASE_URL` changes only its password. `scripts/worker-setup.ps1` opens the forward instead of the SSM session. Transcripts stay in the S3 transcripts bucket.

### A8. Cutover

1. Owner buys the server and runs `bootstrap.sh` without data. Claude checks it from the owner's terminal.
2. Pause the worker. Turn the maintenance Worker on.
3. `pg_dump` RDS through the batch-host forward, restore on the server, and run `wt rights parity` between the two. Exit 0 is required.
4. Move the tunnel token to the server's `cloudflared` and stop the ECS service. `/healthz` and a test question from the owner's browser must pass.
5. Turn the maintenance Worker off.
6. **Rollback window, 7 days.** RDS and the ECS service stay in place, scaled to zero, so going back is a token move and `desired_count = 1`. About $8 for the week.
7. After 7 days, with the owner's approval: a final RDS snapshot kept 30 days, then `terraform destroy` of `envs/v1` compute, RDS, the load balancer and the batch host. `account/`, `records/` and the trimmed `github-oidc` stay.

### A9. Expected cost

| Item | Per month |
| --- | --- |
| Server with provider backups | about $15 to $18 |
| S3 (records, transcripts, models, backups), SES, SNS, Route 53 check, CloudTrail | about $2 to $3 |
| Cloudflare (tunnel, rules, Worker), Free plan | $0 |
| **Fixed total** | **about $17 to $21** |
| Model spend | up to the $40 cap |

GuardDuty and the unused-access analyzer (about $5 to $9) are switched off with the compute they watched. The external-access analyzer stays, as it is free.

## Part B: general availability

### B1. Anonymous visitors

- A first visit sets `wt_visitor`, a signed cookie holding a random visitor id (the same HMAC scheme as `wt_session`, 180-day lifetime). Nothing identifies the person.
- A new table `visitor` (id, created_at, last_seen_at, privacy_mode, privacy_chosen) mirrors the tester columns. `query_log`, `answer`, `feedback` and `dana_event` gain a nullable `visitor_id`, with a check that exactly one of `tester_id` and `visitor_id` is set.
- Invite links keep working. A request with a valid `wt_session` acts as that tester, as today.
- `/` serves the ask page to everyone. Answer permalinks open only for the visitor or tester who asked.
- `POST /about/delete-my-data` and `POST /about/privacy` work for visitors, keyed by the cookie.

### B2. Privacy default for the public

`WT_PRIVACY_DEFAULT` becomes `count_only`, as Spec 06 already requires "before any public launch". A visitor's question text is deleted after 24 hours unless they choose to keep it or flag it. Testers keep the mode they chose.

### B3. Rate limits and the daily ceiling

- **Per visitor:** 10 questions an hour, 30 a day.
- **Per address** (`CF-Connecting-IP`): 30 questions an hour, so clearing cookies does not reset the limit.
- **Daily ceiling:** model spend in one Pacific day may not exceed the monthly cap divided by the days in the month ($1.33 at $40). Past it, the day behaves like the monthly cap: excerpts serve, summaries pause until midnight Pacific. It is recorded once a day in `llm_alert` and emailed.
- The Cloudflare rate rule stays as the outer limit.

### B4. Mandated talks served as excerpts

- A talk is **full-eligible** when any of these holds: its teacher is recorded as `granted`, its source carries a license on the allow-list, or its source carries a sangha grant (`servable_passage.permission_of`, migration 0041). SSZ talks are full-eligible through the October 13 grant.
- Every other talk is served at `excerpts` at most, whatever its teacher's display level, the same way a talk whose transcript came from its source is capped today (Spec 03, Display levels, item 2).
- A new migration replaces `servable_passage` and keeps its columns in order, applying the cap in plain column conditions so the HNSW index stays in use.
- The transcript page already refuses any talk not at `full`, so mandated talks lose their transcript links automatically.
- No-derivatives and noncommercial licenses are already refused by the allow-list (Spec 03), so roadmap UB1.3 needs no further work.
- **Test:** a fixture with a mandated talk whose teacher is at `full` returns `display_level = 'excerpts'` from the view, and `/talk/{id}/transcript` returns 404 for it.

### B5. Spend cap

`wt llm cap set 40 --by acowen` on the server, and `WT_LLM_MONTHLY_CAP_USD=40` in its `.env`. The 80% alert arrives at $32.

### B6. EU, EEA and UK

A Cloudflare custom rule (Free plan) matches `ip.src.country` in the 27 EU states, Iceland, Liechtenstein, Norway and the United Kingdom, and serves a static notice from the maintenance Worker's route: "Wheel Turning is not yet available in your region." The owner applies it in Cloudflare after seeing the rule. `/about`, `/rights`, `/terms` and the removal form (`POST /rights/request`) stay reachable from every country, so rights holders anywhere can ask for removal.

### B7. Legal pages (roadmap UB1.4, UB1.6)

- `/terms`: minimum age 13, no warranty, the removal policy, and the DMCA agent's name and address. Copy in Spec 09.
- **DMCA agent.** The owner registers a designated agent with the US Copyright Office ($6, owner action) before launch.
- **Student questions.** The About page states that student questions in talks are lightly edited and labeled (decision of October 10, 2026), and that anyone who hears themselves in a talk can ask for that passage to be removed through the removal form.

### B8. Search engines (Spec 00, item 9)

- `/robots.txt` allows `/`, `/about`, `/rights`, `/accessibility` and `/terms`, and disallows `/a/`, `/talk/`, `/give/`, `/join/` and every `POST` route.
- `X-Robots-Tag: noindex` is dropped on those five pages only and stays on answers, excerpts and transcripts. Teacher and sangha pages do not exist yet; they and a teacher's opt-in to indexing are later stories.

### B9. Copy

Spec 09 changes: the privacy line for visitors, the home page without invite wording, the terms page, the region notice, and the teacher notice, which now says mandated talks show excerpts only and the transcript level needs the teacher's yes. The `beta` label is removed.

## Owner actions

| # | Action | Before |
| --- | --- | --- |
| O1 | Buy the server (x86, 4 vCPU, 8 GB) and add Claude's deploy key | A8 |
| O2 | Approve the AWS teardown after the 7-day window | A8 step 7 |
| O3 | Register the DMCA agent | Launch |
| O4 | Apply the EU, EEA and UK rule in Cloudflare | Launch |
| O5 | Review every mandated source before launch (open question 1) | Launch |

## Build order

Part A and Part B are independent until launch and can run in parallel, in separate sessions and branches:

- **Part A:** A2 and A3 (compose, `.env` layout) → A4 and A5 (backups, timers) → A6 (deploy) → A7 (worker forward) → A8 cutover.
- **Part B:** B4 (excerpts cap, migration) → B1 and B2 (visitors, privacy default) → B3 (limits, ceiling) → B7, B8, B9 (pages and copy).
- **Launch** when both parts are merged, the cutover is done and O3 to O5 are complete: B5's cap, then remove the invite gate in production.

## Open questions

1. **Review of mandated sources.** The PRD asks for "a fresh review of every mandated source before a public launch", and for counsel review before any wider launch. Does excerpts-only replace counsel's review for launch? Recommendation: yes for launch, with the owner reviewing each mandated source's notices and footer terms (O5) and counsel remaining a later step.
2. **Daily ceiling.** Is one thirtieth of the cap a day right? Recommendation: yes. It lets a busy day borrow nothing from the rest of the month.
3. **Availability target.** 99.9% is not reachable on one server. Recommendation: Tier 1 becomes 99.5% (about 3.6 hours a month), with the recovery and data-loss targets unchanged.
4. **Switzerland.** Its data law is close to GDPR. Recommendation: block it with the EEA.
5. **Bot check.** Cloudflare Turnstile on the ask form would add a third-party script. Recommendation: not at launch; add it only if the daily ceiling trips on abuse.
6. **Secrets.** A root-only file on the server, or keep Secrets Manager through the IAM user ($0.40 a secret)? Recommendation: the file. Specs 00, 01 and 02 name Secrets Manager and change with this spec.

## Changes to other documents

| Document | Change |
| --- | --- |
| PRD | v1 scope: public, not private. Roadmap: general availability on November 2, 2026 with the gates waived. Risks: exposure row updated with decision 3. Open items: counsel review moved after launch (if open question 1 holds) |
| `docs/roadmap.md` | Gates 1 and 2 waived for launch, with the date; UB1.1, UB1.2, UB1.3, UB1.4, UB1.5, UB1.6 marked by this spec |
| Spec 00 | Locked decisions: hosting and access. Spec index gains Spec 12 |
| Spec 01 | "AWS topology" replaced by the server topology; Terraform modules trimmed; secrets; recovery targets (open question 3) |
| Spec 02 | `visitor` table and the `visitor_id` columns; the `servable_passage` excerpts cap |
| Spec 03 | Display levels: the full-eligibility rule from B4. Notices: wording for excerpts-only |
| Spec 06 | Access: visitors beside invites. Privacy default `count_only`. Robots and noindex rules |
| Spec 09 | Copy listed in B9 |
| Spec 11 | Non-goals: the private-only items that this spec overrides |
| `docs/cost.md`, `docs/runbook.md` | New fixed-cost table; server procedures (deploy, restore, key rotation, cutover) |

## Acceptance criteria

### Part A

- [ ] `infra/vps/bootstrap.sh` builds a running site on a fresh Ubuntu 24.04 x86 server from the newest dump, and refuses to run on arm64.
- [ ] The web service has no inbound port. A scan from outside finds only SSH.
- [ ] The nightly dump lands in S3, encrypted, and the weekly restore check emails its `wt audit` result.
- [ ] The memory, disk and service-down timer sends an SNS email when its test hook fires.
- [ ] `deploy.yml` deploys a tag, and a tag that fails `/healthz` is rolled back automatically.
- [ ] The worker's SSH forward reaches the database on 5433 and `wt` commands run against it.
- [ ] `wt rights parity` between RDS and the server exits 0 at cutover.
- [ ] One full month's AWS bill after teardown is under $5, and the server's under $20.

### Part B

- [ ] A visitor with no cookie can ask a question and read the answer at `/`.
- [ ] A visitor's question text is null after 24 hours unless kept or flagged.
- [ ] The visitor and address limits return 429 with a wait time, and the daily ceiling pauses summaries until midnight Pacific.
- [ ] A mandated talk without consent, license or sangha grant is served at `excerpts`, with no transcript link, and its transcript page returns 404. A test fails otherwise.
- [ ] An SSZ talk is still served in full.
- [ ] A request from a blocked country gets the region notice, and `/rights` and the removal form still answer it.
- [ ] `/terms` shows the minimum age, removal policy and DMCA agent.
- [ ] `robots.txt` and `X-Robots-Tag` match B8.
- [ ] Invite links and tester sessions still work.
