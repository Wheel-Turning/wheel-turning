# ADR: a cheaper edge for the private beta

Status: **accepted, a + d.** Written and approved October 17, 2026. The owner chose to replace Spec 11 PB3.6's AWS WAF with Cloudflare's free protection, to put Cloudflare in the request path with a line on the About page, and to make the load balancer IPv6-only. "What was built" lists the changes. The owner cuts over by hand (runbook §12, "The edge"). The Tunnel follow-on (b, UB3.13) is designed in "Follow-on" below; T1 is decided (a), and it is built after the a + d cutover.

## Context

The edge is everything between a tester's browser and the web task. Today, from `infra/terraform`, the live account and the Cloudflare zone as read on October 17:

| Piece | Where | What it does | Per month |
| --- | --- | --- | --- |
| Application load balancer `wt-v1`, two zones | `modules/service/main.tf` | TLS with the ACM certificate (`ELBSecurityPolicy-TLS13-1-2-2021-06`), HTTP to HTTPS redirect, `/healthz` target health check, the `maintenance` fixed page (PB4.4), health-gated deploys with the circuit breaker | $16.48 ($0.542 a day in Cost Explorer, October 10 to 16) |
| Public IPv4, three | two on the load balancer, one on the web task (`assign_public_ip = true`; there is no NAT gateway) | The task's address is its only way out: ECR, Secrets Manager, CloudWatch Logs, SNS, SES, `public.ecr.aws` and the model API | $11.10 ($0.36 to $0.38 a day; $3.65 each, plus one-off tasks) |
| AWS WAF web ACL `wt-v1` | `modules/service/waf.tf` | AWS Common and Known Bad Inputs managed groups, 300 requests per 5 minutes per IP, all in count until `waf_block` (PB3.6); removed by this change | about $8 + $0.60 per million requests; attached since October 16, so not yet on the bill. The load balancer served 17,000 to 20,000 requests a day, so requests add about $0.35 |
| Cloudflare | zone `wheelturning.org`, Free plan, set by hand | DNS only: `app` and `demo` are DNS-only CNAMEs to the load balancer, so testers' traffic never passes through Cloudflare. Zone SSL mode `full`, minimum TLS 1.0, Always Use HTTPS off (they matter only once a record is proxied) | $0 |
| **Edge total** | | | **about $37** |

What depends on the load balancer or AWS WAF:

- **Alarms** (`modules/observability`): `wt-v1-alb-5xx-rate` and `wt-v1-no-healthy-web-task` read `AWS/ApplicationELB` metrics. `wt-v1-healthz-unreachable` (Route 53, HTTPS, `/healthz` on `app.wheelturning.org`) checks the public path and works through any front. No alarm reads WAF metrics.
- **Spec 11 PB3.6** asks for "a WAFv2 web ACL" on the load balancer with the two AWS groups, the 300/5-minute rate rule and count-then-block; its open acceptance items are "Terraform validates and applies" and "a burst of 400 requests from one address in 5 minutes is counted, then blocked".
- **Spec 11 PB4.4** item 4 (ticked): `maintenance = true` makes the load balancer answer every path, tested in `modules/service/tests/maintenance.tftest.hcl`. Runbook 13's alarm entries and "The maintenance page" assume it.
- **Spec 11 PB3.5** (HSTS, CSP, Permissions-Policy) is set by `wt-api`, so it passes through any front unchanged.
- **Spec 11 PB4.5 and UB3.3**: two zones and two tasks. The load balancer already spans two zones, so nothing here blocks UB3.3.
- **Spec 01**: the topology and network rules ("the web security group accepts traffic only from the load balancer"), `deploy.yml`'s smoke test (HTTPS `/healthz`, HTTP redirects, unknown path 404).
- **Client address**: `WT_TRUST_FORWARDED_FOR=true` makes `wt-api` count public-form rate limits against the **last** `X-Forwarded-For` entry (`crates/wt-api/src/lib.rs`, `ClientAddress`). Behind Cloudflare and the load balancer, that last entry is a Cloudflare edge address, not the tester's.
- **Roadmap UB3.13** already proposes trialling a Cloudflare Tunnel in place of the load balancer (option b).

## Options

Prices are us-east-2 and Cloudflare list prices, October 2026, 730 hours a month.

| | Option | Edge per month | Saves | What it costs the specs and operations |
| --- | --- | --- | --- | --- |
| 0 | Today | $37 | | |
| a | **Cloudflare proxy in front**: `app` and `demo` proxied, load balancer reachable only from Cloudflare (security group on Cloudflare's ranges plus a secret header the listener checks), AWS WAF removed, Cloudflare's Free Managed Ruleset and one rate rule instead | $29 | $8 | Replaces PB3.6 item 1. The AWS Common group (OWASP-style rules, size limits) has no free equivalent: the Free Managed Ruleset covers only high-impact, widely exploited vulnerabilities. The free rate rule counts over 10 seconds, so 300 per 5 minutes cannot be expressed; 1-minute windows need Pro at $20 to $25 a month, more than AWS WAF. Cloudflare terminates TLS, so it sees questions in plain text: a new party in the request path. AWS WAF's sampled requests and per-rule metrics go; Cloudflare's free Security Events log replaces them with less history. Needs the client-address change below |
| b | **Cloudflare Tunnel from the task** (UB3.13): a `cloudflared` sidecar dials out to Cloudflare; no load balancer, no load balancer addresses, no inbound rule at all | $4 (the task's IPv4 and a $0.40 tunnel-token secret) | $33 | Everything in (a), plus: PB4.4's maintenance page and its test go (it is a listener action; a Cloudflare Worker or rule would replace it, outside Terraform today); `alb-5xx-rate` and `no-healthy-web-task` go (replaced by a log-metric 5xx alarm and an ECS running-task alarm); deploys gate on a container health check instead of the target group; the sidecar adds memory to the web task that the Fargate session is resizing now; ACM and the HTTP redirect move to Cloudflare. Spec 01's topology and network rules are rewritten. Existing client-address code works unchanged (Cloudflare's entry is last) |
| c | **Keep the load balancer, drop the task's public IPv4** | $33 at best | $3.65 at best | Not viable. A NAT gateway costs $33 a month plus data. IPv6-only egress needs every dependency on IPv6: on October 17 Secrets Manager (`secretsmanager.us-east-2.api.aws`) and `public.ecr.aws` (the models image) had no AAAA record, and an interface endpoint costs $7.30 a month per zone, more than the address it saves. The code's own SNS, SES and S3 clients would also need dual-stack endpoints |
| d | **IPv6-only load balancer** (`ip_address_type = "dualstack-without-public-ipv4"`, no extra charge) | $30 | $7.30 | Alone, testers on IPv4-only networks cannot reach the site, so it is only usable behind a front that speaks IPv4 to browsers. Needs an Amazon IPv6 block on the VPC and dual-stack public subnets (free) |
| **a + d** | **Cloudflare proxy in front of an IPv6-only load balancer** | **$21** | **$15.30** | (a)'s costs, nothing more: the load balancer, its alarms, the maintenance page, health-gated deploys and the Route 53 check all stay. Cloudflare reaches the origin over IPv6; the cutover runbook checks this before testers do |

## Recommendation

**a + d now; b (UB3.13) after a beta week on the proxy, if the owner wants the further $17.**

a + d cuts the edge from about $37 to about $21 a month and keeps every ticked Spec 11 acceptance item: the load balancer, the maintenance page and its test, both load balancer alarms, the Route 53 check and the health-gated deploy. Only PB3.6's WAF changes vendor. Rollback is one DNS toggle while the load balancer still has IPv4, and one apply after.

b saves the most, but it undoes a ticked PB4.4 item and two Spec 01 alarms, changes how deploys decide a task is healthy, and adds a container to the web task while another session is resizing it. Everything a + d sets up at Cloudflare (proxied records, TLS settings, rules, the privacy line, the client address) carries over to b, so doing a + d first leaves b smaller and lower risk. Only the IPv6 load balancer work would be thrown away.

Not recommended: (c) costs more than it saves; (d) alone locks out IPv4-only testers; Cloudflare Pro costs more than AWS WAF.

## What was built

Built on PR 204 after the owner's approval. Cutover stages, checks and rollback are in runbook §12, "The edge".

1. **Network** (`modules/network`): an Amazon-provided IPv6 /56 on the VPC, a /64 per public subnet, `::/0` to the internet gateway. Load balancer ingress is a variable (`alb_ingress`). Until the cutover it stays open to all on 443 and 80, over IPv4 and IPv6. With `cloudflare_only` it admits only `cloudflare_ranges` on 443, as read from `https://www.cloudflare.com/ips-v4` and `ips-v6` on October 17. The two existing rules move into the new map without being replaced.
2. **Service** (`modules/service`): `ip_address_type` is `dualstack`, or `dualstack-without-public-ipv4` once `alb_public_ipv4 = false`; the env's validation refuses that value without `cloudflare_only`. A listener rule forwards requests carrying `X-WT-Origin`, whose value is a `random_password` held in state and output as sensitive. The rule exists from the first stage, so turning `cloudflare_only` on only switches the default action to a 403. `maintenance` answers through both paths. `waf.tf` and `waf_block` are removed. `tests/edge.tftest.hcl` checks all of this offline.
3. **Client address**: `WT_FORWARDED_HOPS` (default 1) in `wt_core::config`. `wt-api` counts the entry that many from the end of `X-Forwarded-For`. Terraform sets it to 2 with `cloudflare_only`. `behind_cloudflare_the_public_form_limit_counts_the_client_cloudflare_saw` covers a forged first entry and a changing Cloudflare edge.
4. **Cloudflare** (owner, by hand): Full (strict), minimum TLS 1.2, Always Use HTTPS, the Free Managed Ruleset, the transform rule, and the Free plan's one rate rule. That rule can match only the path, counts over 10 seconds and has no log mode, so it is 20 requests per 10 seconds per IP on the asks, the public forms and `/join/`, blocking from the start.
5. **Docs**: Spec 11 PB3.6 rewritten, Spec 01 (topology, keys, modules, edge section, network rules), Spec 09 and the About page's Cloudflare line, `docs/cost.md`, and runbook §12 and the `healthz-unreachable` entry.

## Decisions for the owner

Decided October 17, 2026: 1 yes, 2 yes, 3 yes: the Tunnel (UB3.13) stays on the roadmap as the follow-on.

1. **Replace PB3.6's AWS WAF with Cloudflare's free protection?** This loses the AWS Common rule group and the exact 300-per-5-minutes rule, and changes PB3.6's 400-request acceptance test to a 10-second window. Yes, a + d is built as above. No, the edge stays at $37, or only (d) behind a proxy that keeps AWS WAF, which saves $7 and needs the WAF rate rule to read the client address from a header.
2. **Put Cloudflare in the request path?** It decrypts every request, questions included, the same way the static site already runs on Cloudflare. If yes, the About page says so in one sentence, as `site/privacy.html` does for the static site.
3. **b later?** Whether UB3.13 stays on the roadmap as the follow-on, with PB4.4's maintenance page moved to Cloudflare.

## Follow-on: a Cloudflare Tunnel in place of the load balancer (b, UB3.13)

Status: **designed, T1 decided, not built.** Written October 17, 2026. The owner chose T1 (a) the same day: an owner-only `infra/terraform/edge` root with Cloudflare's provider and the maintenance Worker. The build starts once PR 204's three edge stages are applied; read live again after the decision, the load balancer was still `ipv4`, the VPC had no IPv6 block and `app` resolved straight to the load balancer. T2 is open; the build follows the recommendation (T-D last) unless the owner says otherwise.

### What the load balancer does today, and what replaces it

A `cloudflared` sidecar in the web task dials out to Cloudflare and carries `app` and `demo` to `wt-api` on `localhost`. Cloudflare's proxy, the Free Managed Ruleset and the rate rule from a + d stay as they are.

| What the load balancer gives today | Where it is specified | Replacement with the Tunnel | What is lost |
| --- | --- | --- | --- |
| **The maintenance page**: `maintenance = true` swaps the listener's forward for a fixed 503 page on every path, `/healthz` included, while the task keeps running | Spec 11 PB4.4 item 4, acceptance ticked (`maintenance.tftest.hcl`); runbook 13 | A Cloudflare Worker serving the same page with a 503, bound to `app.wheelturning.org/*` and `demo.wheelturning.org/*` only while maintenance is on. Workers on the Free plan allow 100,000 requests a day; the site served 17,000 to 20,000. Cloudflare's own custom error pages are not on the Free plan, so a Worker is the only free way to serve our own page. Nothing inside the task can do it: the page matters most when the task is down | See decision T1: either the switch stays a Terraform variable through Cloudflare's Terraform provider and a Cloudflare API token, or it becomes a step done by hand at Cloudflare and PB4.4's item and test are rewritten |
| **`wt-v1-alb-5xx-rate`**: more than 5% of requests answered 5xx over two 5-minute periods (`AWS/ApplicationELB`) | Spec 01 `observability`; runbook 13 | `wt-v1-web-5xx-rate`: `wt-api` writes one `http_response` log event per request, carrying only the route template (never the path, which can hold an invite token), the status and the milliseconds; two metric filters (all responses, 5xx) and the same metric-math alarm. About 3 MB of logs a day at today's traffic, a few cents a month | 5xx answers that Cloudflare or `cloudflared` produce when the task is unreachable (502, 530) never reach the log. The two alarms below cover that case |
| **`wt-v1-no-healthy-web-task`**: no target passed `/healthz` for 5 minutes | Spec 01 `observability`; runbook 13 | `wt-v1-no-running-web-task`: the service's `AWS/ECS` `CPUUtilization` sample count below 1 for 5 minutes, missing data breaching. ECS publishes that metric every minute while a task runs, with Container Insights still off. The container health check below makes ECS replace a task that stops answering `/healthz` | "Running" replaces "healthy": a task that answers `/healthz` badly but stays up past its health check is not seen. The Route 53 check still sees it from outside |
| **Health-gated deploys**: a new task takes traffic only once the target group sees `/healthz`; the circuit breaker rolls back a revision that never gets there | Spec 01, `deploy.yml` | An ECS container health check on `web`. The image is distroless (no shell, no `curl`), so `wt-api --healthcheck` gets `http://127.0.0.1:<port>/healthz` and exits 0 or 1. `cloudflared` starts only when `web` is `HEALTHY` (`dependsOn`), so a new task joins the tunnel only once it can answer; the circuit breaker works the same on container health. At shutdown ECS stops `cloudflared` first, which drains for its grace period (set to 90 seconds, task `stopTimeout` 120) | An answer stream still open when the grace period ends is cut, as the load balancer's deregistration delay cuts one today |
| **The Route 53 health check** (`wt-v1-healthz-unreachable`) on `https://app.wheelturning.org/healthz` | Spec 01, PB4.5 availability | Unchanged: it already goes through Cloudflare | Nothing |
| **TLS**: the ACM certificate on the HTTPS listener; Cloudflare to origin in Full (strict) | Spec 01 | Browsers get Cloudflare's free edge certificate, as they already do after a + d. `cloudflared` holds an outbound TLS (QUIC or HTTP/2) connection to Cloudflare; `cloudflared` to `wt-api` is plain HTTP on the task's `localhost`. The ACM certificate and its two validation records go | No AWS-side certificate. Cloudflare decrypts every request, as it already does after a + d |
| **HTTP to HTTPS** on port 80 | `deploy.yml` smoke test | Cloudflare's Always Use HTTPS, already on after a + d. The smoke test goes through the public name and passes unchanged | Nothing |
| **The origin lock**: Cloudflare's ranges in the security group plus the `X-WT-Origin` header | PB3.6 item 1, acceptance item 3 (open) | Nothing to lock: the web security group has no inbound rule at all, and there is no origin address to find. The header, its transform rule and `cloudflare_ranges` go | Nothing; this is stronger |
| **The client address**: `WT_FORWARDED_HOPS=2` (Cloudflare's entry, then the load balancer's) | PB3.6 item 1, acceptance ticked | Cloudflare writes the client into `X-Forwarded-For` and `cloudflared` passes it on with no hop of its own, so `WT_FORWARDED_HOPS` goes back to 1. The existing `behind_cloudflare_…` test gains a one-hop case | Nothing |
| **Two zones, two tasks** (PB4.5, UB3.3) | Spec 11, roadmap | Each task runs its own connector on the same tunnel; Cloudflare spreads requests over the connectors as the load balancer spreads them over targets | Nothing |

The tunnel token is a credential: whoever holds it can serve `app.wheelturning.org`. It is a new Secrets Manager secret, `wt-v1/cloudflare_tunnel_token` ($0.40 a month), injected by ECS into the `cloudflared` container as `TUNNEL_TOKEN`, and rotated with the others in runbook 5. No Rust binary reads it, so it does not pass through `wt_core::config::Secret`; it never appears in a task definition, a log or a Terraform output. The sidecar is the official `cloudflare/cloudflared` image pinned by digest, mirrored into a private ECR repository (below), with about 64 MB reserved out of the task's 3 GB.

### Getting out without the task's public IPv4

The load balancer's removal saves $16.48. The task's own address ($3.65) is its only way out to ECR, Secrets Manager, CloudWatch Logs, S3, SNS, SES, the model API and now Cloudflare. AAAA lookups through 1.1.1.1 on October 17, 2026:

| Endpoint | IPv6 |
| --- | --- |
| `ecr.us-east-2.api.aws` (dual-stack ECR), `s3.dualstack.us-east-2.amazonaws.com` | yes |
| `secretsmanager.us-east-2.amazonaws.com` (the default name), `logs.us-east-2.api.aws`, `sns.us-east-2.api.aws`, `email.us-east-2.api.aws` | yes |
| `api.anthropic.com`, `region1.v2.argotunnel.com`, `region2.v2.argotunnel.com` | yes |
| `public.ecr.aws` (the `models` container's `aws-cli` image) | no |

AWS's Fargate networking guide says a task in a **dual-stack** subnet still needs IPv4 (a public address or a NAT gateway) for its start-up dependencies (ECR, Secrets Manager, SSM), so keeping the current subnets and dropping the address does not work. An **IPv6-only** subnet does: Fargate supports it in us-east-2 on platform 1.4.0, with ECR's dual-stack image URIs.

| Option | Per month | Saves on top of the load balancer | What it needs |
| --- | --- | --- | --- |
| **Keep the task's public IPv4** | $3.65 | $0 | Nothing. The tunnel works over IPv4 today |
| **Web task in IPv6-only subnets** | $0 (plus about $0.01 of ECR storage) | $3.65 | Two IPv6-only subnets (free); RDS in dual-stack mode (`network_type = "DUAL"`, IPv6 on its subnets, free) so an IPv6-only task can reach it; `aws-cli` and `cloudflared` mirrored into private ECR and pulled through the dual-stack URI; `AWS_USE_DUALSTACK_ENDPOINT=true` for `wt-api`'s own SNS, SES and S3 clients; `cloudflared --edge-ip-version 6`. ECS Exec does not work in IPv6-only subnets (we do not use it). The eval, admin, audit and migrate tasks stay in the public subnets with a public address while they run (cents), since the audit's link check reaches IPv4-only sites |
| VPC interface endpoints | $43.80 or more ($7.30 per endpoint per zone: ECR API, ECR Docker, Logs, Secrets Manager, SNS, SES) | none: costs $40 more | The model API and Cloudflare are not AWS services, so the task would still need a way out |
| NAT gateway | $32.85 plus $0.045 per GB | none: costs $29 more | |

Recommended: the IPv6-only subnets, as the last stage, once the tunnel has run a week on the current subnets. Everything else in the design works with either.

### Cost

| | Edge per month |
| --- | --- |
| After a + d | $21 (load balancer $16.48, task address $3.65, rest rounding) |
| Tunnel, task address kept | $4.05 (address and the token secret) |
| Tunnel, IPv6-only web subnets | $0.40 (the token secret) |

So the Tunnel saves about $16.40 a month with the address kept and about $20 with IPv6-only subnets. The new log events add a few cents.

### Against Spec 11

- **PB4.4 item 4 and its ticked acceptance ("Setting `maintenance = true` in a plan shows the fixed response")** fall short as written: they name the load balancer. Decision T1 picks the replacement; with T1 (a) the item and its test carry over in Terraform form, with T1 (b) they are rewritten and the switch leaves Terraform.
- **PB3.6** item 1 is rewritten from "Cloudflare in front of the load balancer" to "Cloudflare Tunnel from the web task". Its open acceptance item 3 (the 403 without the origin header) becomes "the web security group has no inbound rule and nothing answers on the old load balancer name". The ticked client-address item stays ticked with the one-hop test. Items 2 and 4 are unaffected.
- **PB4.2, PB4.3, PB3.5**: unaffected. **PB4.4 item 1** (a runbook entry per alarm) holds with the two renamed alarms; `runbook_alarms.rs` checks it.
- **PB4.5, UB3.3**: unaffected (above).
- Outside Spec 11, Spec 01's topology, network rules, `observability` row and its ticked item "`/healthz` answers over HTTPS through the load balancer" are rewritten to "through Cloudflare".

### Cutover, once built

Each stage is one apply, or one change at Cloudflare, and each is undone by the reverse.

| Stage | Change | Rollback |
| --- | --- | --- |
| T-A | Tunnel created at Cloudflare (`cf` CLI, by hand, or Terraform under T1 (a)); its token in Secrets Manager; the `cloudflared` sidecar, the container health check and `http_response` events deployed. The load balancer still serves every tester; the tunnel's public hostname is a test name such as `tunnel-test.wheelturning.org` | Remove the sidecar with the next apply |
| T-B | `app` and `demo` CNAMEs repointed at Cloudflare from the load balancer to `<tunnel id>.cfargotunnel.com`, still proxied; `WT_FORWARDED_HOPS=1` | Point the CNAMEs back at the load balancer (it keeps serving): seconds, and `WT_FORWARDED_HOPS=2` again |
| T-C, a week later | `edge = "tunnel"`: load balancer, target group, listeners, ACM certificate, its alarms and the origin header removed; the two new alarms and the maintenance Worker in place | `edge = "alb"` recreates the load balancer under a new DNS name in about 10 minutes; the CNAMEs go back to it |
| T-D, optional | Web task into IPv6-only subnets, RDS dual-stack | The service back to the public subnets with `assign_public_ip = true` |

### Decisions for the owner

T1. **The maintenance page without a load balancer.** Decided October 17, 2026: (a).
   - (a) **Terraform keeps the switch** (recommended). A separate Terraform root, `infra/terraform/edge`, applied only by the owner, holds the Cloudflare side under Cloudflare's provider: the tunnel and its route, the `app` and `demo` records, and the maintenance Worker and its routes, which `maintenance = true` creates. PB4.4 item 4 then reads "a Terraform variable `maintenance` that makes Cloudflare answer every path with a fixed page", and `maintenance.tftest.hcl` moves to that root with a mocked provider. It needs a Cloudflare API token limited to the `wheelturning.org` zone (DNS edit, Workers routes edit) and the account's Tunnel and Workers scripts, kept on the owner's machine only. `deploy.yml` never sees it. The records set by hand today move into Terraform's state with `terraform import`.
   - (b) **By hand at Cloudflare.** The Worker is deployed once with the `cf` CLI and its route added or removed in the dashboard. No new token, but PB4.4's ticked item and its test are rewritten to a runbook step that nothing tests, and the switch leaves the plan-and-apply record.
   - (c) **No Tunnel.** Stay on a + d at $21.
T2. **The task's address.** End with the IPv6-only stage (T-D, about $3.65 more saved, RDS changes network type), or keep the address and stop at T-C.

## Sources

- Repository: `infra/terraform/modules/{network,service,observability}`, `docs/cost.md` (measured October 14 and 15), Spec 01, Spec 11 (PB3.5, PB3.6, PB4.4, PB4.5), `docs/roadmap.md` UB3.13.
- Cloudflare zone read with the `cf` CLI on October 17, 2026 (records, `ssl`, `min_tls_version`, `always_use_https`, rulesets). AWS read live with the `wheel-turning` profile, read-only, on October 17: the load balancer (`ipv4`, internet-facing, two public addresses), the one web-task address, the WAF association, no IPv6 block on the VPC, `RequestCount` by day, and Cost Explorer by service for October 10 to 16.
- [Cloudflare rate limiting parameters](https://developers.cloudflare.com/waf/rate-limiting-rules/parameters/) and [features by plan](https://developers.cloudflare.com/learning-paths/application-security/rate-limiting/features/).
- [ALB IPv6-only for internet clients](https://aws.amazon.com/about-aws/whats-new/2024/05/application-load-balancer-ipv6-internet-clients) and [the AWS blog post](https://aws.amazon.com/blogs/networking-and-content-delivery/introducing-dual-stack-without-public-ipv4-application-load-balancer).
- AAAA lookups through 1.1.1.1 on October 17: `api.anthropic.com`, `ecr.us-east-2.api.aws`, `logs.us-east-2.api.aws`, `sns.us-east-2.api.aws` and `email.us-east-2.api.aws` have IPv6; `secretsmanager.us-east-2.api.aws` and `public.ecr.aws` did not answer.
- For the Tunnel follow-on: the load balancer's address type and the VPC's IPv6 blocks read live on October 17, 2026 (read-only); AAAA lookups through 1.1.1.1 the same day, as tabled there; [Fargate task networking](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/fargate-task-networking.html) (dual-stack needs IPv4 for start-up dependencies; IPv6-only subnets and dual-stack ECR URIs); [Cloudflare Custom Errors](https://developers.cloudflare.com/rules/custom-errors/) (not on the Free plan).
