# Reliability deep-dive and the two-zone migration plan

Follow-up to the Well-Architected review of October 15, 2026 (`README.md`). It goes further into the Reliability pillar, the pillar with the review's one High finding and its lowest score, and plans the change roadmap story UB3.3 asks for: two availability zones and two web tasks. Facts are checked against `main` at 8549de5 and the live account on October 19, 2026.

## The targets and what they allow

Spec 01, "Recovery and availability targets" (owner's decision of October 16): the database and the site are Tier 1.

| Target | Value | What it allows per month |
| --- | --- | --- |
| Availability | 99.9%, measured by the Route 53 check on `/healthz` | 43.8 minutes down |
| Recovery time (RTO) | 4 to 24 hours | One full restore, replay and redeploy |
| Data loss (RPO) | 24 hours | RDS point-in-time recovery gives about 5 minutes |

**Measured so far.** The `wt-v1-healthz-unreachable` alarm and the health check's daily averages show:

| Date (Pacific) | Event | Down |
| --- | --- | --- |
| October 13 | Daily average 0.99838 (the RDS load day) | about 2.3 minutes |
| October 19, 11:06 to 11:08 | Alarm, cleared by itself | about 2 minutes |
| Other days October 12 to 18 | Daily average 1.0 | 0 |

That is about 4 to 5 minutes in one week of a 44-minute month. The week held no database restart, no failed deploy and no zone event. One of each would spend the month's budget, which is why Spec 01 already records the target as not reachable on this design.

RPO is met with a wide margin on RDS: point-in-time restore was current to within minutes at the time of checking. **PB1.1's cutover is the one exception.** Until the pipeline writes only to RDS, new talks live on the PC database, whose nightly dump gives a 24-hour RPO and whose rights actions must be replayed by hand. That is H1, still High.

## Failure modes

Each row is one way the site stops serving or serves the wrong thing. "Time" is the expected outage from detection to recovery on today's design.

| # | Component | Failure | Detected by | Today's effect | Recovery | Time | After UB3.3 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| F1 | RDS instance | Restart: minor upgrade (`auto_minor_version_upgrade`), class change (`apply_immediately`), host failure | `wt-v1-healthz-unreachable`, `wt-v1-alb-5xx-rate` | Site down; `/healthz` fails its database check | RDS restarts itself; pool reconnects | 2 to 10 min | Multi-AZ: a planned change runs on the standby first, so failover is about 1 to 2 min |
| F2 | RDS zone | The zone of the single instance fails | Same | Site down | Restore to another zone from PITR (runbook §6), then replay takedowns and audit | 1 to 4 h | Automatic failover, about 1 to 2 min |
| F3 | Web task | Crash, OOM, or Fargate retires the host | `wt-v1-no-healthy-web-task`, `wt-v1-web-memory` (PR 215) | Site down while a new task copies models and starts | ECS starts a replacement | 2 to 5 min | The second task keeps serving; no outage |
| F4 | Web task zone | Its zone fails | Same | Site down | ECS places the replacement in the other zone | 3 to 6 min | No outage |
| F5 | Deploy | The new image fails `/healthz` | Circuit breaker; deploy smoke checks | None: rolling 100/200 keeps the old task until the new one is healthy | Automatic rollback | 0 | Same |
| F6 | Config | A setting the server refuses to start without (PB1.6) | `wt-v1-no-healthy-web-task` | None on deploy (as F5); site down if the running task restarts with the bad setting | Fix the setting, redeploy | 10 to 30 min | The second task survives until it restarts too |
| F7 | Models bucket | S3 unavailable at task start | `models` container exits nonzero | Only a starting task is affected; a running task needs no S3 | ECS retries | Minutes, only during S3 events | Same |
| F8 | Claude API | Errors, 429s, slowness | `wt-v1-model-failures`, `wt-v1-slow-answers` | Summary, triage verdict or excerpts degrade; retrieval still answers | Retries (3, backoff); summary drops | Degraded, not down | Same |
| F9 | Spend cap | Monthly cap reached | `wt-v1-cap-reached` | Model stages stop; answers without summary | `wt llm cap set`, or wait for the month | Degraded | Same; UB3.4 makes concurrent calls unable to pass the cap |
| F10 | Edge | Cloudflare outage, a changed transform rule, DNS | `wt-v1-healthz-unreachable` without `no-healthy-web-task` | Site unreachable | Fix the rule, or roll the edge back (runbook §12) | Minutes to hours | Same: Cloudflare is a single edge by decision (`docs/adr-edge-cost.md`) |
| F11 | Certificate | ACM renewal fails because the validation CNAME is proxied | Health check after expiry | Site down | Restore the DNS-only CNAME, wait for renewal | Hours | Same |
| F12 | Region | us-east-2 fails | All alarms in us-east-2 go quiet; us-east-1 health check fires | Site down | Rebuild in another region from snapshot copies (none are copied today) | Days | Same: out of scope until a cross-region snapshot copy exists |
| F13 | Pipeline | PC or GPU host fails | Worker logs; `wt-v1-scratch-not-empty` | New talks stop; serving unaffected | Requeue (runbook §1) | Hours | Same |
| F14 | Rights data | A takedown recorded on one database only | `wt rights parity`, nightly `wt-v1-audit` | A removed talk could be served | Replay on the other side (runbook §12) | Until noticed | Removed by PB1.1, not by UB3.3 |

F1 to F4 are the outages two zones remove. F10 to F12 stay. F14 is the High finding, and PB1.1 is its fix.

## What else limits reliability today

1. **Rate limits live in each task's memory** (`crates/wt-api/src/limits.rs`). With two tasks, a tester gets twice the asks per hour, and a restart forgets every bucket. Cloudflare's rule (20 requests per 10 seconds per IP) bounds floods but not the hourly per-tester limit. UB3.3 already names this.
2. **The model client timeout is 600 seconds** (`crates/wt-extract/src/llm.rs`). The ask path has its own `synthesis_timeout_s`, but any other stage that waits on the client can hold a request for up to 10 minutes (UB3.11).
3. **No load test.** The task is sized for 1 vCPU from 11 questions and a capped-container benchmark (`docs/cost.md`). The embedder is the only CPU-heavy stage, but nothing shows how it behaves at 10 or 20 concurrent asks (UB3.2).
4. **No restore has been timed.** Spec 01 says a restore, replay and redeploy fit in 4 hours "when rehearsed", and the rehearsal is UB3.5. Until it runs, the RTO is a guess.
5. **No snapshot leaves the region.** A regional outage means rebuilding from nothing usable. A daily cross-region copy of the automated snapshot costs about $0.10 a month at 20 GB (snapshot storage) plus the copy's transfer. It is not in the roadmap and is listed under "Proposed additions".
6. **Cloudflare's edge is a single point by decision.** That trade-off is recorded in `docs/adr-edge-cost.md`, and runbook §12 holds the rollback.
7. **The edge cutover is half done in the live account** (posture check, October 19). Stage A is applied (AWS WAF removed, IPv6 on), but stages B and C are not. The load balancer still answers 80 and 443 from `0.0.0.0/0` and `::/0`, with public IPv4 and no origin-header check, so anyone can reach it directly and bypass Cloudflare's rules. This is a security gap more than a reliability one, and it closes when the owner runs stages B and C (runbook §12, "The edge").

## Migration plan: two zones and two tasks (UB3.3)

**Goal.** No single instance, task or zone failure takes the site down for more than 2 minutes, so 99.9% becomes reachable. **Scope.** The database, the web service and rate limits. The edge, the region and the pipeline are out of scope.

**Cost.** At us-east-2 list prices: Multi-AZ doubles the instance and storage (`db.t4g.small` from about $26 to about $52 a month), and a second 1 vCPU / 3 GB task adds about $39. That is about $65 a month on top of today's roughly $100. `docs/cost.md` gets the row when the plan is approved.

**Order.** Each phase stands alone and can be rolled back alone.

### Phase 0: measure and prepare (no change to the site)

1. Run UB3.2's load test (10 and 20 concurrent askers, as the eval tester) on today's single task. Record p95 answer time, CPU and memory. This sets whether two tasks of 1 vCPU are enough, or whether autoscaling (FL4.1) should come with them.
2. Move the per-tester and per-address limits into Postgres. One table, `rate_bucket (key text primary key, tokens real, at timestamptz)`, updated in one statement per check with `insert ... on conflict do update`, so every task shares the bucket. `Limiter` keeps its API and gains a database implementation behind a trait, with the in-memory one kept for tests (rule 9). That needs a migration (the next number) and a Spec 06 change, both approved first (CLAUDE.md rule 1).
3. Check the database pool. With two tasks the database sees twice the web connections: today 10 per task (`crates/wt-db/src/lib.rs`), plus audit, eval and pipeline logins. `db.t4g.small`'s default `max_connections` (memory divided by 9,531,392 bytes) is about 200, so there is room.
4. Check that the ask path holds no state between requests. Checked on October 19: sessions are signed cookies, the progress stream rides the same `POST /ask` request, and deferred summaries are written to the answer row, with an orphan marked `deferred_lost` by the hourly purge. Only the limits (step 2) and a 5-minute scope-size cache (`progress.rs`, harmless if per task) live in memory.

### Phase 1: the database in two zones

1. Set `multi_az = true` in `modules/database` (behind a variable defaulting to `false`, so the change is one tfvars line) and apply in a quiet hour. Converting to Multi-AZ takes no downtime: RDS snapshots the instance and builds the standby from it. Expect higher I/O latency for the hour it takes.
2. **Test the failover.** `aws rds reboot-db-instance --db-instance-identifier wt-v1 --force-failover`. Measure the gap on the health check and in `/wt-v1/web`, and confirm that the pool reconnects without a task restart. Record the result in `docs/incidents/` as a drill.
3. **Rollback.** `multi_az = false` and apply. It is online too.

### Phase 2: two web tasks

1. Set `desired_count = 2` in `modules/service` behind a `web_tasks` variable. ECS spreads Fargate tasks across the two public subnets' zones by default. Keep `deployment_minimum_healthy_percent = 100` and the circuit breaker.
2. Change `wt-v1-no-healthy-web-task` to fire below 1 healthy host (unchanged), and add a warning alarm for fewer than 2 healthy hosts for 15 minutes, with its runbook entry (the runbook alarms test requires one).
3. **Test.** Stop one task (`aws ecs stop-task`) during a run of the eval set and confirm that no request fails and the health check stays green.
4. **Rollback.** `web_tasks = 1` and apply.

### Phase 3: prove the target (UB3.5 joins here)

1. Run the restore drill: restore the latest snapshot to a scratch instance, run migrations, `wt takedown --replay`, `wt audit`, and time each step against the 4-hour RTO. Record it in `docs/incidents/` as a drill, then delete the instance.
2. Report the month's availability from the health check against 99.9% (Spec 01 already asks for this), and drop the "not reachable" note from Spec 01 once two months meet it.

### Done when

- [ ] A forced RDS failover costs the health check under 2 minutes.
- [ ] Stopping one web task fails no request.
- [ ] A tester's hourly ask limit holds across both tasks (test: two tasks against one database, the limit plus one ask refused).
- [ ] The restore drill is recorded within the RTO.
- [ ] Two consecutive months at or above 99.9%.

## Proposed additions to the roadmap

These came out of this deep-dive and are not in `docs/roadmap.md`. They are proposals for the owner, not commitments:

| Proposal | Why | Size |
| --- | --- | --- |
| A daily cross-region copy of the latest automated RDS snapshot (for example to us-west-2), kept 7 days | F12 has no data to recover from today; costs cents a month | S |
| `auto_minor_version_upgrade` with a declared maintenance window at a low-traffic hour | F1's planned restarts land at a time the owner picks | S |
| Finish the edge cutover's stages B and C | Item 7 above: the load balancer is reachable around Cloudflare today | S, owner |
