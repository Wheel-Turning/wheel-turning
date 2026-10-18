# Runbook

The procedures Spec 07 requires, written against the commands as built. Every `wt` command
reads its configuration from the environment (`.env.example` lists the keys). Commands that
change data print a JSON report on stdout; keep it with the record of what was done.

Real transcripts, review files and ratings never go into git. Run commands from the repository
root so `config/` and `data/` resolve.

## 1. A transcription run on the PC

Every pipeline stage, the worker and the text import write to RDS as `wt_pipeline_login`
(Spec 11, PB1.1). The PC worker reaches RDS through an SSM port forward via the batch host
(Spec 01; `docs/windows.md` has the details), and a GPU machine reaches it through its reverse
tunnel to that same forward (step 3). The batch host runs only while a worker runs. RDS is named
`rds-v1` in `database_identity`; with `WT_DATABASE_NAME=rds-v1` in `.env`, the worker, the
pipeline commands, every rights command and `wt takedown` print `database: rds-v1` at start and
refuse any other database.

1. Start the batch host: in `infra/terraform/envs/v1`, apply with `batch_host_enabled = true`
   (as a `t3.small`, `batch_host_instance_type = "t3.small"`), or start the stopped instance in
   the console. Note `terraform output batch_host_instance_id`.
2. Sign in and open the forward from PowerShell:

   ```powershell
   aws sso login --profile wheel-turning
   powershell -ExecutionPolicy Bypass -File scripts\worker-setup.ps1
   ```

   It opens `localhost:5433` to the database and prints the `DATABASE_URL` to put in `.env`
   (with `sslmode=require`; the password is the `db_dg_pipeline_login` secret). Put
   `WT_DATABASE_NAME=rds-v1` beside it.
3. Run the worker on the PC: `wt worker --kinds audio` (add `--drain` to stop when the queue is
   empty). `wt pipeline advance` queues every cleared talk first if needed. Check that it prints
   `database: rds-v1`. For a GPU machine, keep the forward open and run
   `infra/gpu-worker/run.sh ubuntu@<address> [workers] [hours]` from Git Bash: it sends the
   machine the `.env` `DATABASE_URL` and `WT_DATABASE_NAME` (or `WT_GPU_DATABASE_URL`), and
   tunnels the machine's `localhost:5433` back to the PC's forward. It refuses a URL that does
   not name `localhost:5433`.
4. When it finishes, close the forward window and stop the batch host (apply with
   `batch_host_enabled = false`, or stop the instance). It is the one costly resource.
5. Confirm no audio is left: `wt audit` must show no `audio_residue` findings. It checks that
   `WT_SCRATCH_DIR` has no talk directory older than a day and that no audio or video object is
   in object storage. Also confirm the scratch bucket is empty
   (`aws s3 ls s3://<scratch bucket> --profile wheel-turning`); its lifecycle rule expires
   objects after a day in any case.

## 2. Add a teacher and record consent

1. Add the teacher, their aliases and their sangha to the registry file (`config/registry.toml`)
   and load it: `wt registry import --file config/registry.toml`. It is safe to rerun.
2. Record the reply with a pointer to the stored evidence, never the reply text:

   ```bash
   wt consent set --teacher "<name or alias>" --status granted --evidence "<pointer>" --by "<you>"
   ```

   `--scope-note` records any limit in their words. Talks that now pass the gate move to
   `cleared`. A decline or withdrawal runs the teacher takedown and needs `--yes` (procedure 3).
3. Set the rights holder: `wt talk set-rights --teacher "<name>" --holder speaker --by "<you>"`,
   or `--talk <id>` for one talk.
4. A speaker's limits are applied by holding one talk out:
   `wt talk exclude --talk <id> --reason "<why>" --by "<you>" --yes`.

## 3. A takedown request

Target the same day.

Since the October 16 cutover (§12, "Moving the pipeline to RDS") every step below runs against
RDS only, as `wt_owner_login` through the batch host forward. Before it, while two databases held
real data (the PC's `wt-m3-ssz` and the beta's RDS), every step ran against both: once with the PC `.env`, once against RDS through the batch host
forward as `wt_owner_login` (§12, "Rights actions on both databases"). Then run the parity
check, which must exit 0. A takedown run on one side only leaves the talk served from the other.

1. Confirm the request comes from the teacher, or from someone who can act for them, through a
   channel already on record. Note how you confirmed it.
2. Dry run, which changes nothing and prints rows and objects per store:

   ```bash
   wt takedown --teacher "<name>" --actor "<you>" --dry-run
   wt takedown --talk <id> --actor "<you>" --dry-run
   ```

3. Run it: the same command with `--yes` in place of `--dry-run`. One transaction deletes the
   passages, citations, student questions, segments, transcripts and jobs, strips affected
   themes from stored answers, clears affected rating notes, sets the talks to `withdrawn`, and,
   for a teacher, records withdrawn consent. Serving stops when it commits. Then the transcript
   and citation objects are deleted, each prefix is listed to confirm it is empty, and the
   `takedown_log` row is copied to the object store (`takedowns/<id>.json`) for procedure 6.
   The stub talk and its source video stay, so discovery never recreates the talk.
4. If object deletion failed, the report says so: run `wt takedown --finish --actor "<you>"`,
   which retries every logged scope. `wt audit` fails `takedown_completeness` until nothing is
   left.
5. Send the teacher the counts from the report, and this note: database backups keep deleted
   rows until they expire, seven days after the takedown. If a backup is ever restored, every
   recorded takedown is reapplied at once (procedure 6).

A teacher who opts out of the mandate uses `wt teacher opt-out` instead, which also excludes
them for good (Spec 03, Mandated talks). Texts use `wt takedown --edition | --source | --url`
(Spec 08).

The same rule holds for every other rights action while two databases hold real data: `wt
teacher opt-out` (with or without `--suspend-only`), `wt teacher display` for a teacher or one
talk, `wt talk exclude`, `wt consent set`, `wt source disable | enable`, `wt rights decline`,
`wt rights set-talks-basis`, `wt exclusion lift` and `wt takedown requests suspend | remove`. Run
it with the PC `.env`, run it again against RDS, then run the parity check (§12). A `--talk
<id>` names the same talk on both for talks loaded on October 13, 2026; a talk found later
exists only on the PC, and the dry run's title confirms which talk an id is.

Removal requests from the beta's `/rights` form are written to RDS only. Each business day, list
them against RDS (`wt takedown requests list`, as `wt_owner_login` through the forward) and handle
each one on both databases. `wt audit` against RDS warns `takedown_request_overdue` once one has
waited more than a business day; the audit against the PC never sees them.

## 4. A blocking quality failure

For a talk whose served text is wrong, for example garbled, mis-attributed or holding student
speech.

1. Quarantine it, which stops serving at once and deletes nothing:
   `wt talk quarantine --talk <id> --reason "<what failed>" --by "<you>"`.
2. Repair it: a reviewed transcript (`wt transcript export-review`, then import the edited file),
   a corrected speaker or rights holder, or a rerun of the failed stage (`wt reindex --talk <id>`,
   `wt pipeline retry`).
3. Re-check: `wt audit` must pass, and read the talk's passages on its talk page.
4. Release it: `wt talk release --talk <id> --note "<repair and decision>" --by "<you>"`. It
   refuses unless the talk still has passages and a valid rights basis. A talk the pipeline
   quarantined before it was ready goes back through `wt pipeline retry` instead.
5. Both steps are recorded in `correction_log`; the `--note` is the record of the decision. If
   the talk cannot be repaired, take it down (procedure 3).

## 5. Rotate secrets and invite tokens

- Database passwords, the Anthropic and YouTube keys and the session secret live in Secrets
  Manager (Spec 01). Put a new value with
  `aws secretsmanager put-secret-value --secret-id <arn> --secret-string <value> --profile wheel-turning`
  (`terraform output secret_arns` lists them), then redeploy the web service so tasks read it.
  A database password changes in the database first (`alter role ... password`), then in the
  secret. On the PC, update `.env`.
- A new `WT_SESSION_SECRET` signs out every tester; their invite links still work.
- An invite token is shown once and stored only as a hash. To replace one, revoke the tester and
  invite them again, then send the new link privately:

  ```bash
  wt tester revoke --label "<label>"
  wt tester invite --label "<label>" --base-url https://demo.wheelturning.org
  ```

  The label `eval` is reserved for evaluation runs.

## 6. Restore the database, then replay takedowns

1. Restore the RDS snapshot or point-in-time copy (console or `aws rds restore-db-instance-*`),
   and point the services at it.
2. Before anyone uses it, run `wt db migrate`, then:

   ```bash
   wt takedown --replay --actor "<you>" --yes
   ```

   It reads every takedown from the restored `takedown_log` and from the copies in the object
   store, which a snapshot taken before a takedown does not hold, and takes each talk down
   again if it is not withdrawn or has rows. A teacher's withdrawal is recorded again unless a
   later grant of consent stands. Running it twice changes nothing the second time.
3. `missing_talks` in the report lists recorded talks the restored database does not have,
   each with its source video (`platform:external_id`). They hold no text here. The next
   discovery run recreates each as a new talk: find it by that source video and run
   `wt talk exclude --talk <new id> --reason "taken down before the restore" --by "<you>" --yes`
   before the worker runs. A teacher-scope takedown also records withdrawn consent again, which
   keeps that teacher's rediscovered talks at the gate.
4. Run `wt audit`; it must pass.

## 7. Change the embedding model or glossary

1. Load the change: a new model directory under `WT_MODEL_DIR`, or a glossary update
   (`wt glossary ...`).
2. Reindex: `wt reindex --all` (with `--model <dir>` for a new model). `wt audit` reports
   `mixed_embeddings` until every passage uses one model.
3. Evaluate on the same set as the last accepted run:

   ```bash
   wt eval run --set practitioner@2
   wt eval report --run <new run id>
   wt eval compare --a <last accepted run> --b <new run id>
   ```

   For a change worth a reviewer's time, `wt eval export-review --run <id> --out data/eval/<id>/review.html`,
   send the file privately, then `wt eval import-ratings --run <id> --file ratings.json --reviewer "<name>"`
   and report again.

   After a change to `config/sensitive.toml`, the exemplars or the triage prompt, score the gates:
   `wt eval gate --file eval/gate_questions_v1.toml`. A missed notice page blocks the change.

## 8. Before real transcripts reach the language model

1. Check the API key belongs to the project's organization and workspace, and that `.env` sets
   `ANTHROPIC_WORKSPACE_ID`; without it every call fails.
2. Set the monthly cap in `WT_LLM_MONTHLY_CAP_USD`, or with `wt llm cap set <usd> --by "<you>"`.
3. Set `WT_LLM_REAL_DATA_OK=true`. The standard commercial terms apply; zero retention is not
   available (decision of October 9, 2026).
4. After the first run, confirm `llm_usage` has rows (`wt llm cap` shows the month's spend).

## 9. Release checklist

- [ ] `wt audit` passes against the release database (the nightly workflow's last run, or a
      manual run).
- [ ] An evaluation run on the current build is attached: its id and `wt eval report` output.
- [ ] `docs/gate-a.md` records a pass with a named reviewer.
- [ ] The consent coverage report is reviewed: `wt consent list`, and `v_consent_coverage`.
- [ ] The disclosure copy (Spec 09) matches what the logging code stores.
- [ ] SSZ's permission note and speaker-release line are recorded (Spec 00, blockers).

## 10. Local backups and disk space, until the database moves to RDS

While the real data lives on the owner's PC (the `wt-m3-ssz` container and `data/`), a nightly job
copies it off the machine (owner's decision of October 12, 2026).

1. Setup, once. An S3 bucket in us-east-2 with versioning on and all public access blocked; its
   lifecycle from `scripts/ops/backup-bucket-lifecycle.json`
   (`aws s3api put-bucket-lifecycle-configuration --bucket <bucket> --lifecycle-configuration
   file://scripts/ops/backup-bucket-lifecycle.json`); an IAM user `wt-backup` allowed only
   `s3:ListBucket` on the bucket and `s3:PutObject`, `s3:GetObject`, `s3:DeleteObject` on its
   objects, never `s3:DeleteObjectVersion`; and its key in the AWS CLI profile `wt-backup`.
2. Nightly. `WT_BACKUP_BUCKET=<bucket> scripts/ops/backup-local.sh` dumps the database, checks the
   dump lists, uploads it under `db/`, mirrors `data/objects` with deletions and copies
   `data/records`. A scheduled task runs it at 03:00; `data/backups/last-success` holds the time
   of the last good run and `data/backups/backup.log` the history.
3. Takedowns. Database dumps and every older object version expire after seven days, so taken-down
   material leaves the backups within the seven days the takedown note (procedure 3) promises.
4. Restore. Download a dump with `aws s3 cp s3://<bucket>/db/<file> .`, restore it into a fresh
   pgvector container with `pg_restore -d wt --no-owner <file>`, copy `objects/` and `records/`
   back into `data/`, then follow procedure 6 to replay takedowns recorded after the dump.
5. Disk. Each worktree keeps its own `target/` of 20 to 120 GB. `scripts/dev/clean-idle-targets.sh`
   clears every worktree's `target/` that has not been built for 12 hours, never the main
   checkout's; a scheduled task runs it daily, and `--dry-run` shows what it would clear.

## 11. When SSZ's files arrive

SSZ's Vimeo plan gives the API no file links, so SSZ's talks get their audio from files SSZ hands
the owner: a folder, a drive download or a bulk export (Spec 03, the storage adapter, route c).
The files are the owner's copy. The pipeline reads them and never deletes or changes them; the
owner deletes them after the run (step 5).

1. Put the files in `data/storage/ssz/` in the main checkout (the `root` of `ssz-storage` in
   `config/sources.toml`; `data/` is gitignored and the nightly backup does not copy this folder).
   Subfolders are fine. Keep the names SSZ gave them: a name holding the Vimeo id, the talk's
   title or its date is what the matcher reads. `ffprobe` (part of ffmpeg) must be on the path.
2. Match them to the Vimeo talks:

   ```bash
   wt ingest match-storage
   ```

   It changes nothing and writes `data/storage-match/ssz-storage.html` and `.csv`: proposed
   matches (marked `yes`), conflicts where several files chose one talk, files with no talk, and
   talks with no file, with each talk's state. Read the page. In the CSV, clear `accept` on a wrong
   proposal, and for a conflict or an unmatched file type the talk's `vimeo_id` and `yes`. Then
   record the reviewed rows, or every proposal if all are right:

   ```bash
   wt ingest match-storage --sheet data/storage-match/ssz-storage.csv
   ```

   ```bash
   wt ingest match-storage --apply
   ```

   Running it again shows the recorded matches as `matched` and records nothing twice. A sheet
   naming a file that has since moved or changed is refused whole; run step 2 again.
3. Send the held talks back and queue the rest, against the database the run writes to:

   ```bash
   wt pipeline retry-held --reason no_media
   ```

   ```bash
   wt pipeline advance
   ```

   Every talk held as `no_media` is retried, not only matched ones: an unmatched talk is held
   again at once, after one Vimeo API call when the token is set.
4. Transcribe. On the GPU machine (`infra/gpu-worker/run.sh ubuntu@<address> 3 <hours>`), run.sh
   copies `data/storage` to the machine first and prints its size, so allow for the upload; the
   machine's copy is removed when its workers stop. Or run the audio worker on the PC as in
   procedure 1. A matched talk is tried on the Vimeo API first and copies its file into scratch
   only when the API offers none; scratch is deleted after every job. A file missing from the
   folder, or changed since it was matched, holds its talk as `no_media` again: put it back or
   repeat step 2, then step 3.
5. Afterwards: `wt audit` must show no `audio_residue`. Once the SSZ talks you expected are past
   `transcribing`, delete the files: `rm -rf data/storage/ssz`. The review table in
   `data/storage-match/` holds titles and file names only.

## 12. The beta on AWS

Owner's decision of October 13, 2026: invited testers use the AWS v1 stack, not the PC alpha.
Everything below runs with `AWS_PROFILE=wheel-turning` after `aws sso login --profile
wheel-turning` (on Windows, `--use-device-code` works when the browser callback does not).

- Site: `https://app.wheelturning.org`, also `https://demo.wheelturning.org` (Cloudflare DNS, set
  by hand to the load balancer; `terraform output dns_records_for_cloudflare`).
- Web service: Fargate, 1 vCPU / 3 GB, one task. Its `models` container copies
  `bge-m3-int8/` and `bge-reranker-v2-m3-int8/` from `s3://wt-v1-models-904883674947/` into the
  task before `wt-api` starts; startup takes about a minute. A new model goes to that bucket
  (`aws s3 sync models/<dir> s3://wt-v1-models-904883674947/<dir>`), then a redeploy.
- Settings: `WT_ENV=prod` (Secure cookies), the session secret, database password and Claude key
  from Secrets Manager (`wt-v1/*`), the language model settings from the `llm_*` variables in
  `infra/terraform/envs/v1/variables.tf`. Config files (`config/`) are baked into the image, so a
  config change ships as a new image.
- Database: RDS `wt-v1`, Postgres 16, `db.t4g.medium`, private, named `rds-v1`. Loaded on
  October 13, 2026 from the PC's `wt-m3-ssz` database (data only, into the same migration level)
  through the batch host, and caught up on October 16 (below). It is the one database every
  pipeline and rights command writes.
- Deploys: `deploy.yml` (plan, review, apply). An infrastructure change beyond the web task and
  service is applied by hand from `infra/terraform/envs/v1` with `-var image_tag=$(terraform
  output -raw image_tag)`.
- Testers: invites are created against RDS through the batch host forward (procedure 1, step 2),
  connecting as `wt_owner_login` (its secret is `wt-v1/db_wt_owner_login`), since only the owner
  role writes `tester`:

  ```bash
  wt tester invite --label "<name>" --base-url https://app.wheelturning.org
  ```

- Evaluation on the live stack: the `wt-v1-eval` task runs `wt` on the deployed image with the
  site's models, config and RDS, as `wt_owner_login`. Give it the command as an override:

  ```bash
  aws ecs run-task --cluster wt-v1 --task-definition wt-v1-eval --launch-type FARGATE     --network-configuration "awsvpcConfiguration={subnets=[<public subnets>],securityGroups=[<web sg>],assignPublicIp=ENABLED}"     --overrides '{"containerOverrides":[{"name":"eval","command":["wt","eval","run","--set","practitioner@2"]}]}'
  ```

  Its output is in the `/wt-v1/admin` log group under `eval/eval/<task id>`. For the review page,
  pass `["wt","eval","export-review","--run","<id>","--out","/out/<id>/review.html"]`; the task
  copies `/out` to `s3://wt-v1-transcripts-904883674947/eval-reviews/`, and the owner downloads it
  with `aws s3 cp` (the page quotes transcripts: never share it by public link).
- Logins: `wt_analyst_login` (aggregate reports) and `wt_audit_login` (the read-only nightly
  audit) take their passwords from `wt-v1/db_wt_analyst_login` and `wt-v1/db_wt_audit_login`.
  Create or reset each once with the `wt-v1-admin` task, which injects every login password:

  ```bash
  aws ecs run-task --cluster wt-v1 --task-definition wt-v1-admin --launch-type FARGATE     --network-configuration "awsvpcConfiguration={subnets=[<public subnets>],securityGroups=[<web sg>],assignPublicIp=ENABLED}"     --overrides '{"containerOverrides":[{"name":"admin","command":["wt","db","create-login","--role","wt_analyst","--password-env","WT_LOGIN_PASSWORD_WT_ANALYST_LOGIN"]}]}'
  ```

  and again with `"--role","wt_audit","--password-env","WT_LOGIN_PASSWORD_WT_AUDIT_LOGIN"`.
- Feedback: testers' feedback is read on RDS as the analyst, which sees counts and notes but
  never a question unless the tester flagged it. Open the batch host forward (procedure 1,
  steps 1 and 2), then in the same PowerShell window (the password is the
  `wt-v1/db_wt_analyst_login` secret, percent-encoded):

  ```powershell
  $env:DATABASE_URL_ANALYST = "postgres://wt_analyst_login:<password>@localhost:5433/wt?sslmode=require"
  wt report feedback --since 7d --notes
  ```

- Nightly audit: the `wt-v1-audit` task runs `wt audit --fail-on warning --check-links` (plus
  `--backup-bucket` while the PC keeps its database) against RDS as `wt_audit_login`, started by
  the `wt-v1-audit-nightly` schedule at 10:17 UTC. Its JSON report is in the `/wt-v1/admin` log
  group under `audit/audit/<task id>`. Any finding, warning or fail, exits 1, and the
  `wt-v1-audit-failed` EventBridge rule emails the alarm topic; so does a task that fails to
  start, and the `wt-v1-audit-not-started` alarm fires when the schedule cannot start it. To run
  it now:

  ```bash
  aws ecs run-task --cluster wt-v1 --task-definition wt-v1-audit --launch-type FARGATE     --network-configuration "awsvpcConfiguration={subnets=[<public subnets>],securityGroups=[<web sg>],assignPublicIp=ENABLED}"
  ```

  To test the alarm, add an override with a command that exits 1. The image has no shell, so use
  a bucket the task may not list:
  `--overrides '{"containerOverrides":[{"name":"audit","command":["wt","audit","--backup-bucket","wt-no-such-bucket"]}]}'`.

  The GitHub workflow "audit smoke test" (`nightly.yml`) only proves the binary on a fresh
  database; it is not the audit of the beta.
- Cost: about $125 a month (task about $40 since October 17, 2026, database about $50, load
  balancer and addresses about $30). Scale the service to 0 tasks between test rounds to save
  the task's share.

### The database's name

Each database holds its name in `database_identity`, written once: `rds-v1` on RDS, `pc` on the
PC's `wt-m3-ssz`. Set it with `wt db set-identity --name <name>` as the owner (on RDS through the
forward as `wt_owner_login`, or as an admin task override); naming it again with the same name
changes nothing, and another name is refused. `wt db identity` prints it. The ECS tasks run with
`WT_DATABASE_NAME=rds-v1`, and in `WT_ENV=prod` the worker, pipeline, rights and takedown
commands refuse to run without the key.

### Moving the pipeline to RDS (the one-time catch-up)

Done once, on October 16, 2026 (Spec 11, PB1.1). RDS was loaded from the PC on October 13; the
PC worker kept writing to `wt-m3-ssz` until this cutover. `wt db catch-up` copies the pipeline
rows the PC changed since then, and nothing else:

1. Name both databases (above): `pc` on the PC, `rds-v1` on RDS. Both must be at the same
   migration; run `wt db migrate` on the PC first if RDS is ahead.
2. Open the forward (procedure 1, steps 1 and 2) and set, in one PowerShell window, the source
   and target as owner logins (percent-encoded passwords; the environment keeps them out of
   shell history):

   ```powershell
   $env:WT_CATCH_UP_FROM = "postgres://wt:<password>@localhost:55433/wt"
   $env:WT_CATCH_UP_TO = "postgres://wt_owner_login:<password>@localhost:5433/wt?sslmode=require"
   wt db catch-up --actor "<you>" > catch-up-dry.json
   ```

   The dry run copies each talk inside a transaction and rolls it back, so its counts and
   failures are those of the real run. It reports talks changed on the PC after `--since`
   (default `2026-10-13T00:00:00Z`) as new, updated, unchanged, skipped or failed, rows per
   table, the untouched tables' counts, and `wt rights parity` between RDS and the PC. It never
   writes `tester`, `query_log`, `feedback`, `takedown_request`, `rights_event` or any other table
   the web service or a rights command writes, keeps RDS's rights columns on every talk, and
   skips any talk withdrawn, quarantined, excluded, taken down or opted out on RDS, or whose
   passages an answer or feedback row on RDS names but the PC has replaced. Read the skipped and
   failed lists before going on.
3. Run it: `wt db catch-up --actor "<you>" --apply > catch-up.json`. It replays RDS's takedowns
   first (procedure 6), then copies, then runs the parity check, and exits 1 on any failed copy,
   any untouched table that lost rows, or any `"unsafe": true` difference. Fix an unsafe
   difference by replaying the rights action on the side that lacks it, then rerun; rerunning is
   safe and copies only what still differs.
4. Talks listed under `in_pipeline_without_job` have no job on RDS; requeue them there (`wt
   pipeline retry --talk <id>` for a dead job, `wt pipeline advance` for cleared talks).
5. Point the worker at RDS: in the PC `.env`, put the forward's `DATABASE_URL` as
   `wt_pipeline_login` and `WT_DATABASE_NAME=rds-v1`, and rename the old PC URL to
   `PC_DATABASE_URL_READ_ONLY` so no command reaches it by accident.
6. Set the PC database read-only for 14 days, then archive it (a final `pg_dump` to the backups
   bucket) and remove it from this runbook:

   ```powershell
   docker exec wt-m3-ssz psql -U wt -d postgres -c "alter database wt set default_transaction_read_only = on"
   ```

   New sessions refuse writes (`cannot execute ... in a read-only transaction`); reads, backups
   and `wt rights parity` still work. Undo with `= off` only to replay a rights action there.

### Email: removal confirmations, bounces and complaints

SES sends one kind of email, the link confirming a removal request, from `WT_MAIL_FROM`
(`rights@wheelturning.org`). The web server refuses to start in prod without `WT_MAIL_FROM` and
`WT_PUBLIC_URL` (Spec 11, PB1.6). The identity, the MAIL FROM domain and the feedback topic live
in `infra/terraform/account/email.tf`.

- Sender authentication: Easy DKIM signs as `wheelturning.org` (three CNAMEs,
  `terraform output ses_dkim_records_for_cloudflare`), the MAIL FROM domain
  `mail.wheelturning.org` carries SPF for SES (`terraform output
  ses_mail_from_records_for_cloudflare`), and `_dmarc.wheelturning.org` holds the DMARC policy.
  All are DNS-only records at Cloudflare. To check a message, open it in Gmail, choose "Show
  original", and look for `DKIM: 'PASS'`, `SPF: 'PASS'` and `DMARC: 'PASS'`.
- Bounces and complaints go to the SNS topic `wt-ses-feedback`, which emails the owner (confirm
  the subscription link AWS sends once). Each notice is JSON: `notificationType` is `Bounce` or
  `Complaint`; for a bounce, `bounce.bounceType` is `Permanent` (the address does not exist) or
  `Transient` (a full mailbox, try later), and `bounce.bouncedRecipients[].diagnosticCode` says
  why; for a complaint, `complaint.complainedRecipients[].emailAddress` marked the mail as spam.
  `mail.commonHeaders.subject` and `mail.timestamp` identify the message.
- What to do: SES adds every permanent bounce and complaint to the account's suppression list,
  so nothing more is sent there. For a bounce on a removal request, the requester never got
  their link: find the unconfirmed request in `takedown_request` by its time, and if the
  address looks mistyped, answer from the rights mailbox. For a complaint, check that the request was really
  made from /rights; repeated complaints from addresses nobody typed point to form abuse.
  `aws sesv2 get-suppressed-destination --email-address <address>` shows why an address is
  suppressed, and `aws sesv2 delete-suppressed-destination` lifts it once it is known good.
- Keep the bounce rate under 5% and the complaint rate under 0.1%: SES reviews an account above
  them. The SES console's "Reputation metrics" page shows both.

### Rights actions on both databases

Until the catch-up above, both databases held real data. After it the PC is read-only and every
rights action runs once, against RDS; this section and the next are retired once objects move to
S3 too (Spec 11, PB1.2). Before the cutover, every `wt` command
acts on the one `DATABASE_URL` names. So each rights action (procedure 3 and the list after it)
runs twice:

1. With the PC `.env` as it is, against `wt-m3-ssz`.
2. Against RDS: open the batch host forward (procedure 1, steps 1 and 2), then, in the same
   PowerShell window, point this shell only at RDS as the owner login (its password is the
   `wt-v1/db_wt_owner_login` secret, percent-encoded):

   ```powershell
   $env:DATABASE_URL = "postgres://wt_owner_login:<password>@localhost:5433/wt?sslmode=require"
   ```

   Object deletion uses the PC's object store both times, which is the only one until objects
   move to S3; the second run finds nothing left to delete there and says so. Once they have
   moved ("Moving objects and records to S3" below), set `WT_TRANSCRIPT_BUCKET` and
   `WT_RECORDS_BUCKET` for both runs.
3. Compare the two, read-only on both. It exits 1 when one side could serve something the other
   has removed (a talk, teacher, source, exclusion, takedown or `talks_basis` that differs that
   way), and lists every other difference too, such as talks the PC found after the load or a
   removal request that reached RDS only:

   ```powershell
   $env:WT_OTHER_DATABASE_URL = "postgres://wt_owner_login:<password>@localhost:5433/wt?sslmode=require"
   wt rights parity --this-name pc --other-name rds   # with DATABASE_URL back on the PC database
   ```

   Every `"unsafe": true` difference is fixed by replaying the action on the side that lacks it,
   never by editing rows. Run the check after each rights action, and at least once each
   business day with the removal-request check (procedure 3).

### Reloading RDS from the PC

> **Warning.** A PC-to-RDS reload overwrites RDS. Since October 13, 2026, rows exist only on RDS:
> `tester`, `query_log`, `feedback` and `takedown_request` from the beta, and any takedown,
> opt-out or display change run there. Do not reload until (1) every takedown and rights action
> recorded on RDS is replayed on the PC, and `wt rights parity` exits 0; and (2) the RDS rows of
> `tester`, `query_log`, `feedback` and `takedown_request` are exported and carried into the
> load. Otherwise testers lose their invites, logs and feedback, and a talk taken down on RDS
> comes back. After the load, run `wt takedown --replay --actor "<you>" --yes` against RDS
> (procedure 6) and the parity check again.

### Moving objects and records to S3 (Spec 11, PB1.2)

`wt` uses the transcripts bucket when `WT_TRANSCRIPT_BUCKET` is set and the records bucket when
`WT_RECORDS_BUCKET` is set; otherwise `data/objects` and `--records-dir` (`data/records`). Both
are one-time copies, done once, with the owner's go-ahead.

1. Before. The owner applies `infra/terraform/github-oidc` (the boundary gains the version and
   legal-hold actions and the records bucket), then `infra/terraform/envs/v1` (the admin task
   role, the batch role's version deletes and records access, `WT_RECORDS_BUCKET` on the admin
   task). Stop the worker, so `data/objects` does not change during the copy.
2. Dry run, on the PC from the repository root, after `aws sso login --profile wheel-turning`:

   ```powershell
   $env:AWS_PROFILE = "wheel-turning"
   $env:WT_TRANSCRIPT_BUCKET = "wt-v1-transcripts-904883674947"
   $env:WT_RECORDS_BUCKET = "wt-records-904883674947"
   wt objects sync --from data/objects
   wt records sync --from data/records
   ```

   Each prints JSON: `source` (local keys), `matched_before`, `copied` (what `--apply` would
   copy), `differing` (keys the bucket holds with other bytes), `only_in_bucket` and, for
   records, `corrupt_source` (a file whose bytes do not hash to its name). Nothing is written.
3. Copy. Rerun both with `--apply`. Each key is read back and compared by SHA-256; `verified`
   must equal `source`, `mismatched` must be empty and `complete` true, or the command exits 1.
   Rerunning is safe: it copies only what is missing or differs. Objects that differ are
   replaced from the PC. Records are never replaced, since a record's key is its hash, and each
   is written under a legal hold, so a `differing` or `corrupt_source` record is looked at by
   hand.
4. Remove what was taken down. The PC's objects may predate a takedown run only on RDS. Against
   RDS (the batch host forward, as in "Rights actions on both databases"), with the two bucket
   variables still set, run `wt takedown --finish`, which deletes every version under each
   logged talk's prefixes, then `wt audit`: `takedown_completeness` must have no findings.
5. After. Keep both variables set for every later `wt` command, in `.env` too, so no command
   writes to `data/objects` or `data/records` again; pointing the worker at the buckets is part
   of the database cutover (PB1.1). Keep `data/objects` and `data/records` until runbook §10 is
   retired with PB1.1.

## 13. Alarms, incidents and outages

Spec 11, PB4.2 to PB4.4. Every alarm emails the `wt-v1-alarms` topic (us-east-2; the health
check's alarm uses the topic of the same name in us-east-1), and so does the language-model
spend alert at 80 percent of the monthly cap. Both topics email `alert_email` and, once the
owner names one, `second_alert_email` (`infra/terraform/envs/v1/variables.tf`). Each new address
must click the confirmation link AWS sends it, or nothing arrives. Check delivery after any
change, with the owner's profile:

```bash
WT_ALERT_TOPIC_ARN=$(terraform -chdir=infra/terraform/envs/v1 output -raw alert_topic_arn) wt alerts test
```

`wt` loads its whole configuration first, so `DATABASE_URL` must be set (the PC's `.env` has it), though this command never connects. The web task and the batch host publish with their own roles, which may publish to this topic
and nothing else in SNS. A worker on the batch host or the PC needs `WT_ALERT_TOPIC_ARN` in its
environment to publish; without it the alert is only a log line.

The web service writes one JSON object per line to the `/wt-v1/web` log group. No line carries
question or passage text. To read one event in CloudWatch Logs Insights:

```text
fields @timestamp, event, stage, error, ms | filter event = "llm_call_failed" | sort @timestamp desc
```

**Scale to zero** stops the web task, so testers get the load balancer's 503 and nothing is
served. Use it only when serving is itself the harm: a passage that must not be served is being
served, a secret has leaked, or spend keeps climbing past the cap. For anything else prefer the
maintenance page below, which tells testers what is happening.

```bash
aws ecs update-service --cluster wt-v1 --service wt-v1-web --desired-count 0
```

Bring it back with `--desired-count 1` once fixed; the next `terraform apply` also sets it to one
task.

### The alarms

`crates/wt-core/tests/runbook_alarms.rs` checks this list against Terraform: every `alarm_name`
under `infra/terraform` has a heading here, and every heading names an alarm there.

#### `wt-v1-alb-5xx-rate`

- **Means:** more than 5% of requests returned a 5xx over two 5-minute periods.
- **First check:** `/wt-v1/web` for `"level":"ERROR"` lines in the window, and the ECS service's
  events for task restarts.
- **Fix:** a bad deploy is rolled back by deploying the previous `image_tag`; a database error
  goes to the two database alarms below.
- **Scale to zero:** no. Put up the maintenance page if it lasts more than 15 minutes.

#### `wt-v1-db-cpu`

- **Means:** database CPU above 80% for 15 minutes.
- **First check:** `pg_stat_activity` through the batch host forward for a long query, and
  whether an evaluation, reload or backfill is running.
- **Fix:** cancel the runaway query (`pg_cancel_backend`) or stop the job. If the load is real,
  raise `db_instance_class` and apply in a quiet hour.
- **Scale to zero:** no.

#### `wt-v1-db-free-storage`

- **Means:** less than 2 GiB free on the database volume.
- **First check:** the largest tables (`pg_total_relation_size`) and whether a reload left
  copies behind.
- **Fix:** raise the allocated storage in the `database` module and apply (RDS grows it
  online). Drop leftovers only after a snapshot.
- **Scale to zero:** no. If writes have started failing, put up the maintenance page.

#### `wt-v1-scratch-not-empty`

- **Means:** the scratch bucket has held objects for more than 24 hours. Audio must not persist
  (CLAUDE.md rule 6).
- **First check:** `aws s3 ls s3://wt-v1-scratch-904883674947/ --recursive`, and which job wrote
  the objects.
- **Fix:** delete the objects, then find why the pipeline did not, from the stage's logs. Record
  it in `docs/incidents/` as a data-handling incident.
- **Scale to zero:** no. The site does not read scratch.

#### `wt-v1-no-healthy-web-task`

- **Means:** no web task has passed `/healthz` for 5 minutes. The site is down.
- **First check:** the ECS service's events and the stopped task's reason, then the `models`
  and `web` log streams for the start-up error: a missing model, a secret without a value, the
  database refusing the login, a setting the web server refuses to start without.
- **Fix:** deploy the last good `image_tag`, or fix the setting the error names (procedure 12).
  Put up the maintenance page while it is down.
- **Scale to zero:** it already serves nothing.

#### `wt-v1-healthz-unreachable`

- **Means:** Route 53 has not reached `https://app.wheelturning.org/healthz` from outside AWS
  for 3 minutes. It also fires while the maintenance page is up, by design.
- **First check:** whether `wt-v1-no-healthy-web-task` fired too (the app) or not (DNS at
  Cloudflare, the certificate or the load balancer).
- **Fix:** the app: as above. DNS: the CNAME at Cloudflare must be DNS-only and match
  `terraform output dns_records_for_cloudflare`. The certificate: ACM renews it while its
  validation CNAME stays at Cloudflare.
- **Scale to zero:** no.

#### `wt-v1-model-failures`

- **Means:** more than 20% of language-model calls failed in 15 minutes, with at least 5 calls.
  Answers still show passages, but without a summary, and the relevance check falls back.
- **First check:** the `llm_call_failed` lines (query above) and their `error`: `status` is the
  provider's answer (401 or 403: the key or workspace; 429: the rate limit; 5xx: the provider),
  `http` the network, `parse` or `truncated` a prompt or model change, `ledger` the database.
- **Fix:** a key problem: rotate it (procedure 5). The provider down: wait, and tell testers if
  it lasts an hour. Parse failures after a deploy: roll back.
- **Scale to zero:** no.

#### `wt-v1-cap-reached`

- **Means:** the monthly spend cap (`llm_monthly_cap_usd`, or the owner's value in the
  database) refused a call. Answers are passages only until the month ends or the cap rises.
- **First check:** this month's `llm_usage` by stage, and whether one tester or job accounts for
  most of it. The 80 percent alert should have arrived first.
- **Fix:** expected spend: raise `llm_monthly_cap_usd` and apply. Unexpected: find the caller
  first.
- **Scale to zero:** only if spend still climbs with the cap reached, which would be a bug.

#### `wt-v1-summaries`

- **Means:** more than 30% of answers in an hour, with at least 5, ended with a summary status
  other than `shown` or `not_attempted` (Spec 11, PB2.4).
- **First check:** the `summary_settled` lines and their `summary_status`: `model_error` or
  `timeout` go with `wt-v1-model-failures`; `cap_reached` with `wt-v1-cap-reached`; `no_key` is
  the API key secret; `all_sentences_rejected` is the summary's check refusing every sentence;
  `deferred_lost` is the web task restarting with summaries pending.
- **Fix:** the matching alarm's fix. `all_sentences_rejected` after a deploy or model change:
  roll back the prompt or model and run the evaluation set. `deferred_lost`: find why the task
  restarted (ECS events).
- **Scale to zero:** no. Answers still show passages.

#### `wt-v1-ranker-fallback`

- **Means:** an answer in the last hour was served with ranker `none (fallback)`: the relevance
  check did not run and passages came in search order (Spec 11, PB2.1).
- **First check:** the `ranker_served` lines, then `llm_call_failed` lines for the ranker's
  stage around the same time (a timeout or provider error), and whether
  `config/retrieval.toml` in the deployed image names the model ranker.
- **Fix:** a provider problem: as `wt-v1-model-failures`. A config problem: fix
  `config/retrieval.toml` and ship a new image.
- **Scale to zero:** no.

#### `wt-v1-slow-answers`

- **Means:** the 95th percentile time to the answer page was above 8 seconds over an hour.
- **First check:** the `ask_completed` lines and their `ms`, the `answer stored` line's
  `synth_ms`, and `wt-v1-db-cpu`.
- **Fix:** a slow provider: wait, or set a faster model for the slow stage. The database: as
  above.
- **Scale to zero:** no.

#### `wt-account-root-activity`

- **Means:** the AWS root user signed in to the console or made an API call (CIS 4.3, from the
  CloudTrail copy in `/wt/cloudtrail`). Nobody should use root day to day.
- **First check:** if the owner did it on purpose (billing, account settings), note it and stop.
  If not, read the root events in `/wt/cloudtrail` (source IP, user agent, what was called).
- **Fix:** if it was not the owner: treat it as a security incident (incident procedure, step
  5). Change the root password, check root MFA, look for new IAM users, keys or roles, and
  revoke anything unknown.
- **Scale to zero:** only if the calls touched the database, buckets or secrets.

#### `wt-account-iam-policy-change`

- **Means:** an IAM policy was created, deleted, versioned, put, attached or detached (CIS 4.4).
  A Terraform apply that changes IAM fires it, which is expected.
- **First check:** did an apply or deploy run at that time? If so, match the change to its plan.
  If not, read the event in `/wt/cloudtrail`: who made it and which policy.
- **Fix:** an unexplained change is a security incident: revert it with `terraform apply`,
  and find and revoke the credentials that made it.
- **Scale to zero:** only if the change widened access to data or secrets.

#### The nightly audit failed (EventBridge rule `wt-v1-audit-failed`)

Not a CloudWatch alarm: an EventBridge rule that emails the topic when a `wt-v1-audit` task stops
with a nonzero exit code or fails to start (Spec 11, PB1.4; §12, "Nightly audit").

- **Means:** exit 1 is a finding: the audit runs with `--fail-on warning`, so any check with
  findings, fail or warn, raises it. Another code, or "failed to start", means the task itself
  broke: a missing secret, a refused database login, or an image that will not pull.
- **First check:** the report in `/wt-v1/admin` under `audit/audit/<task id>` (the email names
  the task). Each check with `findings > 0` lists up to 20 examples.
- **Fix:** a fail check (`consent_drift`, `serving_parity`, `opted_out_served`,
  `takedown_completeness` and the rest in Spec 07) is a rights incident: follow the incident
  procedure below. A warning is upkeep: `takedown_request_overdue` is procedure 3,
  `pc_backup_stale` is §10 (the PC's 03:00 backup has not run), `crisis_link_dead` means a help
  link on the support page needs correcting in `support_text.html`, and the rest are in Spec 07.
  A broken task: `aws ecs describe-tasks` for its `stoppedReason`, and `wt db create-login --role
  wt_audit` (§12) if the login was refused.
- **Scale to zero:** only for a fail check showing that something which must not be served is
  being served.

#### `wt-v1-audit-not-started`

- **Means:** the `wt-v1-audit-nightly` schedule could not start the audit task, so no audit ran
  and no task event was sent.
- **First check:** the schedule's target in the EventBridge Scheduler console, and whether the
  `wt-v1-audit` task definition is active and its scheduler role can still run it and pass its
  roles.
- **Fix:** reapply Terraform, then run the audit once by hand (§12) to see it pass.
- **Scale to zero:** no.

### Incident procedure

1. Note the time and the alarm in a new `docs/incidents/<date>-<slug>.md` from `TEMPLATE.md`.
2. If anything is served that must not be (a removed talk, someone's question), scale to zero.
3. If the site is down or wrong for testers, put up the maintenance page.
4. Follow the alarm's entry above: the first check, then the fix.
5. If a tester's data or a secret may be exposed, tell the security contacts the same day.
6. If testers saw it or it lasts over an hour, email them (`docs/incidents/outage-email.md`).
7. Deploy the fix and watch the alarm return to OK.
8. Take the maintenance page down or scale back up; check `/healthz` and ask one question.
9. Email testers that it is resolved, if step 6 sent the first email.
10. Within a week, finish the incident file: the cause, what changed, and follow-up items.

**Security contacts.** The list of whom to tell about a security incident (the owner, anyone
else with access, and the affected sanghas' contacts for a rights breach) is kept privately,
out of git, in `data/private/security-contacts.md` on the owner's PC (`data/` is ignored by
git), with a copy in the owner's password manager under "Wheel Turning security contacts".

### The maintenance page

`maintenance = true` makes the load balancer answer every path, `/healthz` included, with a fixed
503 page saying the site is down for maintenance. The web task keeps running behind it. From
`infra/terraform/envs/v1`:

```bash
terraform apply -var maintenance=true -var image_tag=$(terraform output -raw image_tag)
```

Apply again with `-var maintenance=false` to end it. `terraform test` in
`infra/terraform/modules/service` checks the switch offline.
