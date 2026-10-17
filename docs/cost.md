# Cost guardrails

What a question and a month cost (the cost model), what watches the AWS bill, where to see it, and the steps only the root user can take. Values below were read from the account on 2026-10-05. The budget and anomaly subscription are managed in `infra/terraform/account/`; the AWS-created monitor and its default subscription are not.

## Cost model (Spec 11 PB4.7)

What a question costs, what the site costs before anyone asks one, and the monthly ceiling. Written on 2026-10-16. Model figures are estimates from `config/llm_prices.toml`; AWS figures are us-east-2 list prices for what `infra/terraform/envs/v1` deploys, over a 730-hour month. Replace the model rows with measured figures from `wt report beta` (its `cost` section) once the private beta has a week of answers, and the AWS rows with Cost Explorer's monthly totals by service.

### Model cost per question, by stage

"Before" is Spec 10's baseline on the live stack (October 15, 2026, Claude Haiku 4.5 at $1 and $5 per million input and output tokens). "After" assumes every stage moves to Claude Haiku 5.5 ($0.10 and $0.50, a tenth of the price) and uses about 30% more tokens for the same work, so each stage costs about 0.13 of what it did. Only stages that pass their rule in PB2.0 move; a stage that stays on Haiku 4.5 keeps its "before" figure. The session evaluating PB2.0 replaces "after" with its measured eval runs.

| Stage (`llm_usage.stage`) | Calls per question | Before (Haiku 4.5) | After (Haiku 5.5, estimate) |
| --- | --- | --- | --- |
| Triage gate (`sensitive`) | 1 | $0.002 | $0.0003 |
| Ranking (`rank`) | 6 | $0.019 | $0.0025 |
| Excerpt cutting (`excerpt`) | 8 to 10 | $0.013 | $0.0017 |
| Summary (`synth`) | 1 to 2 | $0.012 | $0.0016 |
| Verifier (`verify`) | 4 to 12 | $0.008 | $0.0010 |
| **Mean per question** | 20 to 30 | **$0.054** | **$0.0070** |
| 95th percentile | | $0.061 | about $0.008 |

The after-mean is within Spec 10's target of $0.010. Its workstreams (a cheaper ranker, excerpts without a model call, one verifier call) would lower it further and are not counted here.

How it is measured: every model call made for an answer carries the answer's id in `llm_usage.answer_id`, and `query_log.llm_cost_usd` is their sum, written when the answer is stored and again when its deferred summary is. `wt report beta` prints the mean and 95th percentile; cost by stage is `select stage, sum(est_cost_usd) from llm_usage where answer_id is not null and created_at >= now() - interval '7 days' group by stage`, as the owner login. A question the triage gate stops stores no answer, so its one gate call is in the spend ledger but not in the per-question figure.

### Fixed AWS cost by service

What runs whether or not anyone asks a question. "Measured" is Cost Explorer's daily cost by service, the mean of October 14 and 15, 2026 (the first full days on the current 2 vCPU task and `db.t4g.medium`), times 30.4. "Expected" adds what was deployed after those days and is not yet on the bill, at us-east-2 list prices.

| Service | What | Measured per month | Expected per month |
| --- | --- | --- | --- |
| ECS Fargate | One web task, 2 vCPU and 8 GB, x86, always on; the nightly audit and occasional migrate and eval tasks | $86.94 | $87 |
| RDS | `db.t4g.medium`, single zone, 20 GB of storage, 7 days of backups | $49.85 | $50 |
| Elastic Load Balancing | One application load balancer and its capacity units at beta traffic | $16.48 | $17 |
| VPC | Public IPv4 addresses: two on the load balancer, one on the web task (there is no NAT gateway) | $11.42 | $11 |
| Secrets Manager | The project's secrets at $0.40 each | $2.78 | $3 |
| KMS | Keys behind the secrets and the database | $1.90 | $2 |
| Route 53 | The health check | $0.98 | $1 |
| S3, ECR, SES, SNS, CloudTrail, CloudWatch | Buckets, images, removal emails, alerts, the one free trail, logs and alarms | $0.35 | $3, once PB4.3's alarms and their logs add up |
| AWS WAF | One web ACL, its managed rule groups and the rate rule (PB3.6), plus $0.60 per million requests | not yet billed | $9 |
| GuardDuty and IAM Access Analyzer | Account-level detection (PB3.6) | not yet billed | $3 |
| **Total** | | **$170.70** | **about $186** |

October 14 also carried $3.27 of EC2 for the batch host's toggle test, which is not a fixed cost and is left out. Domain and DNS are at Cloudflare and are not in this bill. Read the table again from Cost Explorer (Billing and Cost Management, Cost Explorer, grouped by Service, daily) once WAF and GuardDuty have a full week.

### Totals by daily questions

A 30-day month. At 10,000 questions a day the one web task and the `db.t4g.medium` are not enough: the model assumes a second web task ($85, which UB3.3's two zones need anyway) and a `db.t4g.large` (about $50 more), so the fixed cost becomes about $320. Load balancer, WAF request and log charges grow with traffic but stay a few dollars at these volumes.

| Questions a day | Questions a month | Model, before | Model, after | Fixed AWS | **Total, before** | **Total, after** | All-in per question, after |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | 3,000 | $162 | $21 | $186 | **$348** | **$207** | $0.069 |
| 1,000 | 30,000 | $1,620 | $210 | $186 | **$1,806** | **$396** | $0.013 |
| 10,000 | 300,000 | $16,200 | $2,100 | $320 | **$16,520** | **$2,420** | $0.008 |

Below about 1,000 questions a day the fixed AWS cost is most of the bill; above it the model is. Haiku 5.5 moves the crossover from about 110 to about 900 questions a day.

### Monthly ceiling and the actions at 80% and 100%

During the private beta the ceiling is **$260 a month**: the account budget's $200 for AWS (`monthly_budget_usd` in `infra/terraform/account/`) and the model cap's $60 (`llm_monthly_cap_usd` in `infra/terraform/envs/v1`, which sets `WT_LLM_MONTHLY_CAP_USD`; `wt llm cap set` overrides it). The $60 cap covers about 8,500 questions a month after the switch, or about 1,100 before it. Raise either only with a week of `wt report beta` figures in hand.

| Reached | What happens by itself | What the owner does |
| --- | --- | --- |
| Model spend at 80% of the cap ($48) | The client sends one alert for the month through the owner alert (SNS once PB4.2 is deployed, otherwise a log line), recorded in `llm_alert` so it is sent once | Read `wt report beta --since 7d`: if questions per tester or cost per question jumped, look for one tester or a looping client and revoke them (`wt tester revoke`). If the use is real, decide before the month ends whether to raise the cap with `wt llm cap set` |
| Model spend at 100% of the cap ($60) | The client refuses every further model call until the month ends. Answers still serve: excerpts in fused order with the "relevance check did not finish" line, no summary, and the triage gate marked degraded, so the rules and the embedding check still route sensitive questions. Ingestion jobs that need a model wait for next month | Tell testers the summaries are paused if it is more than a week to the month's end. Raise the cap only after the 80% review; otherwise wait for the month to roll over |
| AWS spend at 80% of the budget ($160, actual) | The budget emails the owner | Check Cost Explorer by service against the fixed table above. A line well above it (a forgotten batch host, an RDS snapshot, a GPU instance, a second task) is stopped or deleted per the runbook |
| AWS spend forecast at 100% of the budget ($200) | The budget emails the owner. Nothing is turned off: AWS budgets do not stop services | If the overrun is an idle resource, remove it. If it is traffic, either raise `monthly_budget_usd` to the next row of the totals table or scale the web service to zero per the runbook and post the tester notice (PB4.4) |

## Monthly budget

| Setting | Value |
|---|---|
| Name | `dharma-gates-monthly` (keeps its old name; see [naming.md](naming.md)) |
| Type and period | Cost, monthly |
| Limit | 200.00 USD for the whole account (raised from 75 on 2026-10-13) |
| Alert at 50% | Actual spend over 100.00 USD, email to `alexandercowen@gmail.com` |
| Alert at 80% | Actual spend over 160.00 USD, email to `alexandercowen@gmail.com` |
| Alert at 100% forecast | Forecast spend over 200.00 USD, email to `alexandercowen@gmail.com` |

Budget emails arrive only when a threshold is crossed; there is no confirmation email. AWS evaluates budgets a few times a day, so an alert can lag spending by hours.

## Cost anomaly detection

**Monitor.** An account allows one AWS services monitor, and AWS created one when Cost Explorer was turned on. The account stack subscribes to it rather than creating its own.

| Setting | Value |
|---|---|
| Name | `Default-Services-Monitor` |
| Type | Dimensional, by AWS service |
| ARN | `arn:aws:ce::904883674947:anomalymonitor/d133db83-c78e-4f59-81c0-54ea20376a7b` |
| Managed by | AWS; referenced in `infra/terraform/account/terraform.tfvars` |

**Subscriptions.** Both send to the same monitor and the same address, and both are confirmed.

| Subscription | Managed by | Frequency | Alerts when | Email |
|---|---|---|---|---|
| `dharma-gates-anomalies` | `infra/terraform/account/` | Daily summary | An anomaly's total impact is 5 USD or more | `alexandercowen@gmail.com` |
| `Default-Services-Subscription` | AWS, created with the monitor | Daily summary | Total impact is 100 USD or more **and** at least 40% above expected | `alexandercowen@gmail.com` |

The 5 USD subscription is the one that matters at this account's spending level; the default one only fires for large anomalies. Email subscribers get daily or weekly summaries; immediate alerts would need an SNS topic.

## CloudTrail and the records bucket

Both were created in the October 2026 rebuild and outlive any v1 destroy. Neither should add more than cents a month; the prices are us-east-2 list prices.

| What | Managed by | What it costs |
|---|---|---|
| Trail `wt-account`: management events in every region, log file validation on | `infra/terraform/account/guardrails.tf` | Nothing for the events: the first copy of management events in an account is free. A second trail, data events (0.10 USD per 100,000) or CloudTrail Insights would be charged, and none is on |
| Bucket `wt-cloudtrail-904883674947`: the trail's log files and hourly digest files, kept 365 days, old versions 30 days | `infra/terraform/account/guardrails.tf` | S3 Standard storage at 0.023 USD per GB-month, plus PUT requests at 0.005 USD per 1,000 for each file CloudTrail delivers. At this account's activity both stay well under 1 USD a month, and the 365-day expiry caps how much builds up |
| Bucket `wt-records-904883674947`: Object Lock, GOVERNANCE default retention, every object under a legal hold | `infra/terraform/records/` | Storage only: Object Lock and legal holds cost nothing extra. It is empty until the M2 takedown code writes to it, and each takedown, exclusion or opt-out record is a few KB |

Measured on 2026-10-05: in its first 8.5 hours, which covered the whole rebuild, the trail wrote 285 files (154 of them digests) totalling 1.1 MB. At that rate a month is about 24,000 files and under 100 MB, or roughly 0.12 USD in requests, and a full year of logs is about 1 GB, or 0.03 USD a month in storage. A quiet month writes less. The records bucket held no objects.

Records are kept permanently and can never be expired by a lifecycle rule, so that bucket only grows. At a few KB per record, even thousands of records cost a fraction of a cent a month. Cost Explorer shows the two buckets under S3 and the trail under CloudTrail, and all three carry the `project` and `env` tags.

## Edge protection and detection

Added for the private beta (Spec 11, PB3.6). Spec 11 expects about 10 to 15 USD a month; this estimate, from us-east-2 list prices in October 2026, comes to about 12 to 17 USD. GuardDuty and the unused-access analyzer each start with a 30-day free trial, so the first month costs less. Check the real figure in Cost Explorer a month after the apply.

| What | Managed by | What it costs |
|---|---|---|
| WAF web ACL `wt-v1` on the load balancer: AWS Common and Known Bad Inputs managed groups and a 300-per-5-minutes rate rule | `infra/terraform/modules/service/waf.tf` | 5 USD for the web ACL plus 1 USD per rule (3), and 0.60 USD per million requests. The two AWS managed groups carry no subscription fee. About 8 USD a month at beta traffic. Count and block cost the same |
| GuardDuty: CloudTrail management events, VPC flow logs, DNS logs and S3 data events | `infra/terraform/account/detection.tf` | Billed by volume: 4 USD per million management events, 1 USD per GB of flow and DNS logs, 0.80 USD per million S3 data events. The trail's measured rate (about 24,000 files a month) suggests 1 to 4 USD a month. EKS, Lambda, runtime and malware scanning are off |
| IAM Access Analyzer `wt-external-access` and `wt-unused-access` | `infra/terraform/account/detection.tf` | External access: free. Unused access: 0.20 USD per IAM role or user analyzed each month, so 2 to 5 USD for the account's 10 to 25 roles |
| CloudTrail copy in log group `/wt/cloudtrail`, kept 30 days, with two metric filters and alarms | `infra/terraform/account/detection.tf` | 0.50 USD per GB ingested; the trail's management events come to well under 1 GB a month. Two custom metrics at 0.30 USD and two alarms at 0.10 USD: under 1.50 USD a month in all |

## Where to look in the console

Sign in through the AWS access portal as the Identity Center admin user, not as root.

| To see | Go to |
|---|---|
| Spend so far this month and the forecast | Billing and Cost Management, Home |
| The budget, its history and alert status | Billing and Cost Management, Budgets, `dharma-gates-monthly` |
| Spend by service, day or tag | Billing and Cost Management, Cost Explorer. Group by Service, or by the tag `project` once it is activated as a cost allocation tag |
| Detected anomalies and both subscriptions | Billing and Cost Management, Cost Anomaly Detection |
| Itemised charges and invoices | Billing and Cost Management, Bills |
| Free tier usage | Billing and Cost Management, Free Tier |
| Snapshots left behind after a destroy | RDS, Snapshots (manual snapshots keep costing until deleted) |

The account's home region is us-east-2 (Ohio). Billing pages are global; Cost Explorer and anomaly detection run in us-east-1, and the console handles that.

## Steps that need the root user

Root signs in at the AWS console with the account's root email and password. Everything else uses the Identity Center admin user.

| Step | Why root | Status |
|---|---|---|
| Turn on Cost Explorer | It is enabled once per account from the billing console, and anomaly detection needs it | Done on 2026-10-03. Cost data took up to 24 hours to appear |
| Turn on IAM access to billing ("IAM user and role access to billing information", on the Account page) | Only root can change it. Without it, even an administrator role cannot open billing pages or Cost Explorer | On. The Identity Center admin role reads budgets and Cost Explorer, which this setting allows |
| Add MFA to the root user (IAM, Security recommendations, or the account menu, Security credentials) | Only root can manage its own credentials | On. The account summary reports root MFA enabled |

After these, root is not needed for day-to-day work. Keep its password and MFA device somewhere safe for the rare tasks that require it, such as changing the account email, closing the account, or changing billing access.
