# Naming

The project is **Wheel Turning**. Code and docs use `wt`: crates `wt-*`, the `wt` and `wt-api` binaries, `WT_*` configuration keys. The earlier name was Dharma Gates (`dg`, `DG_*`).

AWS resources, secrets, the RDS database (`wt`, master user `wt_admin`) and database roles take `wt` names from the October 2026 rebuild. These still carry the old name, one line each with why and when it changes:

- **The budget and anomaly names** `dharma-gates-monthly`, `dharma-gates-services` and `dharma-gates-anomalies`: they live in `infra/terraform/account/`, which the destroy-and-rebuild test left alone on purpose; renaming the budget replaces it and loses its history, so they keep the old names (docs/cost.md lists them).
- **Database roles `dg_*` in a database built before migration `0010`**: `0010` renames the group roles to `wt_*`; logins are not renamed, so any `dg_*_login` made before it is dropped and recreated as `wt_*_login`.
- **Migrations `0001` to `0009`**: applied migrations are never edited, so their text keeps the old names even after the roles are renamed.
- **Doc lines that describe the rename or the history**: `docs/PRD.md` line 3 ("Formerly Dharma Gates") and its "What changed" row, the naming notes in specs 00 and 01 and the mandate commands note in spec 09 name `dg` or Dharma Gates on purpose.
- **The `dgdata` volume** in `docker-compose.yml` and spec 01: renaming it orphans the existing local database volume.
- **Git history, merged branch names and past PR titles**: history is not rewritten.
