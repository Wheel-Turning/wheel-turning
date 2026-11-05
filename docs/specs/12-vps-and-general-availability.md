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
- **Provider: OVHcloud US, VPS-1, in Hillsboro, Oregon** (owner's decision of November 4, 2026: "working but cheap"). It has 2 vCores, 4 GB, 40 GB NVMe, unlimited traffic and daily backups included, from $4.54 a month (read November 2, 2026). The memory budget in A10 is what makes 4 GB enough.
- **Upgrade path.** OVHcloud resizes a VPS in place with one reboot. VPS-2 (4 vCores, 8 GB, 75 GB, $8.50) is the step up, taken when A10's alarm fires twice in a week. OVHcloud does not resize down, so the site starts on the smaller plan.
- Hetzner was the first choice, but its US prices rose in 2026: a CPX31 in Ashburn or Hillsboro now costs €62.99 a month, and a CPX32 in Germany €35.99.
- OVHcloud has a REST API and a Terraform provider. The owner creates API credentials limited to this server's `/vps/<name>` routes and stores them in `.env`, so Claude can snapshot, reboot and reinstall it. Day-to-day changes go over SSH.
- The owner buys and owns the account. Claude never creates it.
- Ubuntu 24.04 LTS with unattended security upgrades, SSH by key only, password login off, `ufw` denying all inbound except SSH. The web service has no inbound port: Cloudflare reaches it only through the tunnel.

### A2. What runs on it

Three systemd services, installed natively rather than in Docker. That saves the 100 to 150 MB that `dockerd` and `containerd` hold on a 4 GB server. Unit files live in `infra/vps/systemd/`. Tests still use the `pgvector/pgvector:pg16` container (CLAUDE.md rule 9), so the server runs the same Postgres and pgvector versions from the PGDG apt repository.

| Service | Installed from | Notes |
| --- | --- | --- |
| `postgresql@16-main` | PGDG apt: `postgresql-16`, `postgresql-16-pgvector` | Listens on `127.0.0.1:5432` only. Settings in A10 |
| `wt-web` | The release binary from CI, in `/srv/wheel-turning/releases/<tag>/`, with `current` a symlink | Runs as an unprivileged `wt` user. Models read-only in `/srv/wheel-turning/models` |
| `cloudflared` | Cloudflare's apt repository | Runs the existing tunnel `wt-v1-web` with its token. The tunnel's ingress points at `http://127.0.0.1:8080` |

The database roles of Spec 02 are created exactly as on RDS. The admin role is a superuser here, so role-creating migrations (Spec 11, 0050) no longer need a separate admin task.

### A3. Secrets

Secrets live in `/srv/wheel-turning/.env`, mode 0600, owned by root and loaded by the `wt-web` unit as its `EnvironmentFile`. They are the same keys `.env.example` lists, and the code still wraps each one in `wt_core::config::Secret`. Secrets Manager is retired for the server (open question 6).

AWS stays for four things that cost cents: the records bucket (Object Lock), SES removal emails, SNS alerts and the backup bucket. One IAM user, `wt-vps`, holds a policy for exactly those actions, inside the existing permissions boundary. Its access key goes in `.env` and is rotated every 90 days (runbook).

### A4. Backups and restore

- **Nightly dump.** A systemd timer runs `pg_dump -Fc` at 03:00 Pacific and uploads it to `s3://wt-backups-<account>/vps/` with server-side encryption. A lifecycle rule keeps 30 days. This meets the 24-hour data-loss target.
- **Server snapshots.** The provider's daily backups are on as a second copy.
- **Restore check, off the server.** A second Postgres does not fit beside the live one in 4 GB. A weekly GitHub Actions job (`restore-check.yml`) reads the newest dump through the existing OIDC role, restores it into a `pgvector/pgvector:pg16` service container on the runner, runs `wt audit` against it and emails the result through SNS. A failed restore is an alert.
- **Rebuild.** `infra/vps/bootstrap.sh` turns a fresh Ubuntu server into a running site from the newest dump. The twice-yearly drill (Spec 01) runs this script and times it against the 4-hour recovery target.

### A5. Alerts and the nightly jobs

Replacing CloudWatch:

| Today | On the server |
| --- | --- |
| `wt-v1-healthz-unreachable` (Route 53 check on `/healthz`) | Kept unchanged. It checks the public path through Cloudflare, so it does not care where the app runs |
| Log metric filters and the 5xx alarm | The web service counts 5xx answers itself and alerts through the existing `SnsAlerter` above 5% over 10 minutes |
| ECS task alarms, memory alarm | A timer every 5 minutes alerts on A10's memory thresholds, 80% disk, or any of the three services not running |
| Nightly audit ECS task | The same `wt audit` on a systemd timer, with the same SNS email |
| `purge_expired_queries` | Unchanged, on a timer |
| CloudWatch Logs, 30 days | journald with a 30-day, 2 GB cap. Request logs keep the route template only, as today |

### A6. Deploys

`deploy.yml` builds the release binary on an x86-64 runner, copies it over SSH with a deploy key restricted to one command (`/srv/wheel-turning/deploy.sh <tag>`), and keeps the last three releases. The script runs migrations as `wt_owner_login`, points `current` at the new release, restarts `wt-web`, waits for `/healthz`, and points back to the previous release if the check fails within 2 minutes. A migration that builds or rebuilds a vector index runs only in a maintenance window, with `maintenance_work_mem` raised for that session (A10). During the swap the maintenance Worker serves the 503 page (Spec 11, PB4.4).

### A7. The transcription worker

The owner's PC reaches the database through an SSH port forward to `127.0.0.1:5432` on the server, on local port 5433 as today, so `.env`'s `DATABASE_URL` changes only its password. `scripts/worker-setup.ps1` opens the forward instead of the SSM session. Transcripts stay in the S3 transcripts bucket.

### A8. Cutover

1. Owner buys the server and runs `bootstrap.sh` without data. Claude checks it from the owner's terminal.
2. Pause the worker. Turn the maintenance Worker on.
3. `pg_dump` RDS through the batch-host forward, restore on the server, and run `wt rights parity` between the two. Exit 0 is required.
4. Move the tunnel token to the server's `cloudflared` and stop the ECS service. `/healthz` and a test question from the owner's browser must pass.
5. Turn the maintenance Worker off.
6. **Rollback window, 7 days.** RDS and the ECS service stay in place, scaled to zero, so going back is a token move and `desired_count = 1`. About $8 for the week.
7. **Teardown.** The owner approved it on November 2, 2026, on one condition: Claude first confirms the site is fully working on the server. Before teardown, every Part A acceptance item that can be checked before teardown must pass, plus these checks on the live site: `/healthz`, a full question with summary and excerpts, a transcript page, a feedback post, a removal request with its confirmation email, a nightly audit, a nightly dump with a successful restore check, and a worker run through the SSH forward. The results go in `docs/incidents/` as the cutover record. Then, after the 7-day window: a final RDS snapshot kept 30 days, then `terraform destroy` of `envs/v1` compute, RDS, the load balancer and the batch host. `account/`, `records/` and the trimmed `github-oidc` stay.

### A9. Expected cost

| Item | Per month |
| --- | --- |
| OVHcloud VPS-1 with its daily backup | from $4.54 (VPS-2: $8.50) |
| S3 (records, transcripts, models, backups), SES, SNS, Route 53 check, CloudTrail | about $2 to $3 |
| GitHub Actions for the weekly restore check | $0 (free minutes) |
| Cloudflare (tunnel, rules, Worker), Free plan | $0 |
| **Fixed total** | **about $7 to $8** |

### A10. Memory budget on 4 GB

The 1.5 GB that `docs/cost.md` gives Postgres was measured to avoid reads from RDS's gp3 volume, at about 1 ms each. What searches actually read is about 630 MB: the half-precision HNSW index (375 MB), the passage heap (231 MB) and the full-text index (25 MB). The full-precision embeddings are stored out of line and are read only by the student-question search. On local NVMe a cache miss costs about 0.1 ms, so the 130 misses a question that the small RDS showed would add about 15 ms, not 150. The plan keeps that 630 MB in the operating system's page cache, not in Postgres's own buffers.

| Part | Budget | How |
| --- | --- | --- |
| Ubuntu, sshd, journald, `cloudflared` | 300 MB | No Docker. journald capped at 200 MB in memory |
| `wt-web` | 800 MB, target 650 MB | M1 to M3 below |
| Postgres processes and `shared_buffers` | 700 MB | `shared_buffers = 256MB`, `max_connections = 20`, `work_mem = 8MB`, `maintenance_work_mem = 128MB`, one autovacuum worker, `huge_pages = off`, `jit = off` |
| Page cache for the database | about 2 GB | What is left. It holds the 630 MB search set with room for the rest of the 1.7 GB database |
| Safety | zram swap of 1 GB (compressed, lz4) plus a 2 GB swap file at low `vm.swappiness = 10` | An answer slows down under pressure instead of a process being killed |

Changes to the code, each measured before and after against the 50 practitioner questions, four at a time, reading peak resident memory (`VmHWM`) of `wt-web`:

- **M1. ONNX Runtime arena off.** `OnnxSettings` gains `cpu_arena` (default `true`, `false` on the server). The session builder calls `with_memory_pattern(false)` and turns off the CPU memory arena, so activation buffers are freed after each run instead of held at their largest size. The 570 MB int8 model stays resident either way. Vectors must match the current build bit for bit on the 50 questions, or the change is dropped.
- **M2. One inference thread.** `WT_ORT_THREADS=1` and two Tokio worker threads on a 2-vCore server. One embedding takes 54 ms on one vCPU against 49 ms on two (`docs/cost.md`), and it frees the second core for Postgres's full-text ranking.
- **M3. Allocator.** The web binary uses `mimalloc` as its global allocator, or sets `MALLOC_ARENA_MAX=2` if `mimalloc` is refused by `cargo deny`. glibc's per-thread arenas hold freed memory from tokenizer and template work.
- **M4. A narrow full-text table (Spec 05 change, owner's approval needed).** Full-text ranking reads the whole passage heap on most questions (`docs/cost.md`) because `tsv` sits in the wide `passage` row. A `passage_fts (passage_id, tsv)` table, kept by `wt-index` and read through the serving view, would cut that read from 231 MB to the size of the `tsv` values. It is not needed for launch. Build it only if the page p95 passes 4.8 s on the server.

**Alarm.** The 5-minute timer of A5 alerts when available memory stays under 300 MB for 10 minutes, or when swap use passes 500 MB. Two alerts in one week mean resizing to VPS-2.

**Measure before cutover.** Restore the dump on the new server, apply A10's settings, and run the 50 questions four at a time. Pass when `wt-web`'s peak stays under 800 MB, no swap is used at rest, and the page p95 stays under Spec 10's 5 s. If it fails after M1 to M3, resize to VPS-2 before cutover.
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

- **No hourly question limit** (owner's decision of November 2, 2026). The per-tester `ask_rate_per_hour` limit (Spec 06) is removed, and no per-visitor or per-address hourly limit replaces it. The public form limits per address (accessibility and rights reports) are unchanged.
- **Daily ceiling:** model spend in one Pacific day may not exceed the monthly cap divided by the days in the month ($1.33 at $40). Past it, the day behaves like the monthly cap: excerpts serve, summaries pause until midnight Pacific. It is recorded once a day in `llm_alert` and emailed.
- The Cloudflare rate rule stays. It counts requests over 10 seconds and only stops floods; it is not a question quota.
- With no hourly limit, the daily ceiling is the only guard on spend. One script can still use a whole day's share, after which summaries pause for everyone until midnight Pacific.

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

A Cloudflare custom rule matches `ip.src.country` in the 27 EU states, Iceland, Liechtenstein, Norway and the United Kingdom on `app` and `demo`, and blocks the request. **Applied November 2, 2026** (zone custom ruleset `ac4c0f2e…`, rule `f143a87a…`). The Free plan refuses the `ip.src.is_in_european_union` field and custom block responses, so the rule lists the countries by code and shows Cloudflare's standard block page. A notice page ("Wheel Turning is not yet available in your region") needs a Worker on the blocked paths and is part of Part B. `/about`, `/rights`, `/terms` and the removal form (`POST /rights/request`) stay reachable from every country, so rights holders anywhere can ask for removal.

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
| O1 | Buy the OVHcloud VPS-1 with Claude's SSH public key, and create scoped API credentials | A8 |
| O2 | Approve the AWS teardown. Done November 2, 2026, on the condition in A8 step 7 | A8 step 7 |
| O3 | Register the DMCA agent | Launch |
| O4 | Apply the EU, EEA and UK rule in Cloudflare. Delegated to Claude and applied November 2, 2026 | Done |
| O5 | Review every mandated source before launch (open question 1) | Launch |

## Build order

Part A and Part B are independent until launch and can run in parallel, in separate sessions and branches:

- **Part A:** A2 and A3 (systemd units, `.env` layout), A10 measurement → A4 and A5 (backups, timers) → A6 (deploy) → A7 (worker forward) → A8 cutover.
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
- [ ] A10's measurement passes on VPS-1: `wt-web` peak under 800 MB with four questions at a time, no swap at rest, page p95 under 5 s.
- [ ] With M1, embeddings of the 50 practitioner questions match the current build exactly.
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
- [ ] No question is refused for an hourly limit, and the daily ceiling pauses summaries until midnight Pacific.
- [ ] A mandated talk without consent, license or sangha grant is served at `excerpts`, with no transcript link, and its transcript page returns 404. A test fails otherwise.
- [ ] An SSZ talk is still served in full.
- [ ] A request from a blocked country gets the region notice, and `/rights` and the removal form still answer it.
- [ ] `/terms` shows the minimum age, removal policy and DMCA agent.
- [ ] `robots.txt` and `X-Robots-Tag` match B8.
- [ ] Invite links and tester sessions still work.
