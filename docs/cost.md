# Cost guardrails

What watches the AWS bill, where to see it, and the steps only the root user can take. Values below were read from the account on 2026-10-05. The budget and anomaly subscription are managed in `infra/terraform/account/`; the AWS-created monitor and its default subscription are not.

## Monthly budget

| Setting | Value |
|---|---|
| Name | `dharma-gates-monthly` (keeps its old name; see [naming.md](naming.md)) |
| Type and period | Cost, monthly |
| Limit | 75.00 USD for the whole account |
| Alert at 50% | Actual spend over 37.50 USD, email to `alexandercowen@gmail.com` |
| Alert at 80% | Actual spend over 60.00 USD, email to `alexandercowen@gmail.com` |
| Alert at 100% forecast | Forecast spend over 75.00 USD, email to `alexandercowen@gmail.com` |

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
