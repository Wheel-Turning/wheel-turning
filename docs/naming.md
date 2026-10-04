# Naming

The project is **Wheel Turning**. Code and docs use `wt`: crates `wt-*`, the `wt` and `wt-api` binaries, `WT_*` configuration keys. The earlier name was Dharma Gates (`dg`, `DG_*`).

These still carry the old name, one line each with why and when it changes:

- **AWS resources named `dg-v1-*`** (VPC, subnets, security groups, buckets, RDS instance, ECR repository, ECS cluster, service and task families, load balancer, IAM roles and paths, log groups, alarms, SNS topic, secrets under `dg-v1/`): renaming replaces live resources; they become `wt-*` in the planned destroy-and-rebuild test.
- **AWS tag `project = "dharma-gates"` and the budget, anomaly and state-bucket names** (`dharma-gates-monthly`, `dharma-gates-services`, `dharma-gates-anomalies`, `dharma-gates-tfstate-904883674947`): the tag, monitor and budget change at the rebuild; the state bucket cannot be renamed, so the rebuild creates a new one and the old one is emptied and removed.
- **Secrets Manager keys `db_dg_owner_login`, `db_dg_web_login`, `db_dg_pipeline_login`**: they name live secrets; renamed with the other AWS names at the rebuild.
- **RDS database name `dg` and master user `dg_admin`**: changing either replaces the database instance; set at the rebuild.
- **Database roles `dg_owner`, `dg_pipeline`, `dg_web`, `dg_analyst` and logins `dg_*_login`**: created by applied migrations, and each login starts its session with `SET ROLE dg_*`, so a rename breaks live logins; a new migration renames them during the rebuild, before any real data exists.
- **Migrations `0001` to `0009`**: applied migrations are never edited, so their text keeps the old names even after the roles are renamed.
- **Doc lines that describe the rename or the history**: `docs/PRD.md` line 3 ("Formerly Dharma Gates") and its "What changed" row, spec 00 line 147, spec 01 line 136 and spec 09 line 229 name `dg` or Dharma Gates on purpose.
- **The `dgdata` volume** in `docker-compose.yml` and spec 01: renaming it orphans the existing local database volume.
- **Git history, merged branch names and past PR titles**: history is not rewritten.
