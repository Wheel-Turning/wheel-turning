# ADR: a cheaper edge for the private beta

Status: **accepted, a + d.** Written and approved October 17, 2026. The owner chose to replace Spec 11 PB3.6's AWS WAF with Cloudflare's free protection, to put Cloudflare in the request path with a line on the About page, and to make the load balancer IPv6-only. "What was built" lists the changes. The owner cuts over by hand (runbook §12, "The edge").

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

Decided October 17, 2026: 1 yes, 2 yes. 3 is still open.

1. **Replace PB3.6's AWS WAF with Cloudflare's free protection?** This loses the AWS Common rule group and the exact 300-per-5-minutes rule, and changes PB3.6's 400-request acceptance test to a 10-second window. Yes, a + d is built as above. No, the edge stays at $37, or only (d) behind a proxy that keeps AWS WAF, which saves $7 and needs the WAF rate rule to read the client address from a header.
2. **Put Cloudflare in the request path?** It decrypts every request, questions included, the same way the static site already runs on Cloudflare. If yes, the About page says so in one sentence, as `site/privacy.html` does for the static site.
3. **b later?** Whether UB3.13 stays on the roadmap as the follow-on, with PB4.4's maintenance page moved to Cloudflare.

## Sources

- Repository: `infra/terraform/modules/{network,service,observability}`, `docs/cost.md` (measured October 14 and 15), Spec 01, Spec 11 (PB3.5, PB3.6, PB4.4, PB4.5), `docs/roadmap.md` UB3.13.
- Cloudflare zone read with the `cf` CLI on October 17, 2026 (records, `ssl`, `min_tls_version`, `always_use_https`, rulesets). AWS read live with the `wheel-turning` profile, read-only, on October 17: the load balancer (`ipv4`, internet-facing, two public addresses), the one web-task address, the WAF association, no IPv6 block on the VPC, `RequestCount` by day, and Cost Explorer by service for October 10 to 16.
- [Cloudflare rate limiting parameters](https://developers.cloudflare.com/waf/rate-limiting-rules/parameters/) and [features by plan](https://developers.cloudflare.com/learning-paths/application-security/rate-limiting/features/).
- [ALB IPv6-only for internet clients](https://aws.amazon.com/about-aws/whats-new/2024/05/application-load-balancer-ipv6-internet-clients) and [the AWS blog post](https://aws.amazon.com/blogs/networking-and-content-delivery/introducing-dual-stack-without-public-ipv4-application-load-balancer).
- AAAA lookups through 1.1.1.1 on October 17: `api.anthropic.com`, `ecr.us-east-2.api.aws`, `logs.us-east-2.api.aws`, `sns.us-east-2.api.aws` and `email.us-east-2.api.aws` have IPv6; `secretsmanager.us-east-2.api.aws` and `public.ecr.aws` did not answer.
