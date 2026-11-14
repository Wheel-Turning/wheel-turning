# Well-Architected review

> **Confidential.** These pages list unremediated security gaps. Keep them in this private repository and do not paste them into public issues, chat channels or the site.

A full AWS Well-Architected Framework review of the v1 beta ran on October 15, 2026, against `main` at d95de08. It covered every framework best practice: 6 pillars, 57 questions, 307 best practices, from the live AWS table of contents. Its remediation plan became Spec 11 (private beta) and roadmap stories UB3 and FL4. The finding ids below (H1, M1 to M12) are the ones Spec 11 and the roadmap cite. This folder holds what came after: the ledger, deeper looks and the tools that keep it current.

| File | What it is |
| --- | --- |
| `bp-ledger.csv` | One row per framework best practice: status, risk, evidence, recommendation, and the WA Tool choice id. Refreshed October 19 against `main` at 8549de5. Evidence cites paths, with line numbers as of the review; `tf/` means `infra/terraform/`. |
| `reliability.md` | Reliability pillar deep-dive: failure modes, measured availability, and the phased plan for two zones and two tasks (UB3.3) |
| `generative-ai-lens.md` | The Generative AI Lens applied on top of the framework: 51 lens best practices |
| `../../scripts/ops/wa-posture-check.sh` | Read-only check of the live account against the controls the review relies on. Run before a release |
| `../../scripts/ops/wa_tool_import.py` | Loads `bp-ledger.csv` into the AWS Well-Architected Tool |
| `../../infra/terraform/modules/*/tests/guardrails.tftest.hcl` | The offline twin of the posture check: CI fails if Terraform loses a guardrail (PR 215) |

## Where each finding stands (October 19, 2026)

| Finding | Story | Status |
| --- | --- | --- |
| H1, two live databases and objects on the PC (REL09-BP01) | PB1.1, PB1.2 | Catch-up and S3 store built; the real cutover is not done. Still **High** |
| M1, no WAF on the public load balancer (SEC05-BP03) | PB3.6 | Replaced by Cloudflare (owner, October 17). **In the live account the cutover stops after stage A**: AWS WAF is removed, but the load balancer still answers 80 and 443 from anywhere over public IPv4, with no origin-header check. Runbook §12, "The edge", stages B and C close it |
| M2, no detection services (SEC01-BP08, SEC04-BP03) | PB3.6 | Done: GuardDuty, Access Analyzer, root and IAM-change alarms, all checked live |
| M3, long-lived `wt-backup` key (SEC02-BP02) | UB3.8 | Key still active (checked live); it retires with runbook §10 once PB1.1 and PB1.2 land |
| M4, no RTO, RPO or availability target (REL13-BP01) | PB4.5 | Done: Spec 01, "Recovery and availability targets" |
| M5, single-AZ database, one task (REL10-BP01) | UB3.3 | Planned; phases in `reliability.md` |
| M6, restore never tested (REL09-BP04) | UB3.5 | Open |
| M7, no load test (REL07-BP04) | UB3.2 | Open |
| M8, no per-alarm runbook (OPS10-BP02) | PB4.4 | Done, kept in step by `runbook_alarms.rs` |
| M9, no threat model; M10, no security incident plan | UB3.7 | Incident procedure and security contacts done (PB4.4); threat model and playbooks open |
| M11, one-person dependency (OPS03-BP07) | UB3.8, FL4.2 | Open |
| M12, no staging (OPS05-BP08) | UB3.6 | Open; a non-goal for the private beta |
| Lows: headers, digest pins, Terraform hygiene | PB3.5, UB3.9 | Headers and digest pins done; egress, access logs, dashboard, memory alarm, Database Insights, storage autoscaling, cost tags in PR 215 |

The refreshed ledger counts 172 Implemented, 80 Partial, 28 Not Implemented, 24 Not Applicable and 3 Cannot Determine. Its findings are 1 High, 11 Medium and 96 Low (on October 15: 155 Implemented; 1 High, 16 Medium, 108 Low).

## The live posture check

```bash
AWS_PROFILE=wheel-turning scripts/ops/wa-posture-check.sh
```

It reads and changes nothing, and prints PASS, WARN or FAIL per control with its best-practice id. It exits 1 on any FAIL. Run on October 19 it had no FAIL and nine WARNs. Each WARN is a known open item: the `wt-backup` key, single-AZ RDS, the edge cutover's stages B and C, a single confirmed alarm subscriber, and the four things PR 215 adds. Add it to the release checklist (runbook §9) once PR 215 is applied.

## The WA Tool workload

The ledger is loaded into the AWS Well-Architected Tool as workload **`wheel-turning-v1`** (id `<WA_WORKLOAD_ID>`, us-east-2, pre-production, framework lens). All 57 questions are answered: an Implemented best practice is a selected choice, and a Not Applicable one is marked not applicable with the ledger's reason. A Partial or Not Implemented one is left unselected, with its recommendation in the question's notes. 306 of 307 best practices matched a Tool choice; SEC05-BP04's page slug is not the Tool's choice id and is left out.

**The Tool scores risk its own way.** It marks a question High when any of its key best practices is unselected, whatever the context. So it shows 29 High-risk questions, where this review, weighing likelihood and the workload's criticality, finds 1 High and 11 Medium findings. Read the Tool's counts as "questions with a gap", and this ledger for how much each gap matters.

To refresh the Tool after the ledger changes:

```bash
python scripts/ops/wa_tool_import.py --workload-id <WA_WORKLOAD_ID>          # dry run
python scripts/ops/wa_tool_import.py --workload-id <WA_WORKLOAD_ID> --apply
```

To take a milestone (a frozen copy the Tool keeps for comparison), use the console's "Save milestone" after an apply.

## Keeping the ledger current

When a story closes a gap, change its row in `bp-ledger.csv` in the same PR: status, risk, and evidence that names the file or setting. Then refresh the Tool. A full re-review, with the corpus re-read from AWS's live table of contents, is due before the public beta (UB gate), since AWS revises the framework's best practices.
