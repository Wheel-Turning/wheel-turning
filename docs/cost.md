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
