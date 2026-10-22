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
- The Cloudflare Tunnel token (`wt-v1/cloudflare_tunnel_token`) changes only with the tunnel:
  §12, "The Cloudflare Tunnel", "Rotating the tunnel token".
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

## 10. Local backups (retired) and disk space

Retired on October 20, 2026 (Spec 11, PB1.2): the database is on RDS (PB1.1) and objects and records
are in S3 (PB1.2), so the PC holds nothing that is not already kept off the machine. RDS keeps
encrypted automated backups (procedure 6 restores one), and the transcripts and records buckets
are versioned. The nightly PC backup of October 12, 2026 is no longer needed.

1. Shut it off, in this order, so the nightly audit does not warn about a backup that stopped on
   purpose:
   1. Set `pc_backup_bucket = ""` in `infra/terraform/envs/v1` and `infra/terraform/github-oidc`
      and apply both (§12, "Terraform hygiene", for the order). The audit task then runs without
      `--backup-bucket`, and `pc_backup_stale` is no longer checked.
   2. Disable the PC's "Wheel Turning backup" scheduled task (`scripts/ops/backup-local.sh`):
      `schtasks /Change /TN "Wheel Turning backup" /DISABLE`.
2. Keep the backup bucket until the PC database is archived (§12, "Moving the pipeline to RDS",
   step 6: a final `pg_dump` to that bucket, about November 2, 2026). Then deactivate the
   `wt-backup` key (well-architected finding M3) and let the bucket's lifecycle rule expire the
   rest. Restoring that archive: `aws s3 cp s3://<bucket>/db/<file> .`, then `pg_restore -d wt
   --no-owner <file>` into a fresh pgvector container.
3. Disk. Each worktree keeps its own `target/` of 20 to 120 GB. `scripts/dev/clean-idle-targets.sh`
   clears every worktree's `target/` that has not been built for 12 hours, never the main
   checkout's; the "Wheel Turning cleanup" scheduled task runs it daily, and `--dry-run` shows
   what it would clear. This step is not retired.

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
  by hand to the load balancer; `terraform output dns_records_for_cloudflare`). From the edge
  cutover below, both are proxied by Cloudflare and the load balancer answers only Cloudflare.
  From the Cloudflare Tunnel's stage T-B, the edge stack (`infra/terraform/edge`) holds both
  records and points them at the tunnel.
- Web service: Fargate, 1 vCPU / 3 GB, one task. Its `models` container copies
  `bge-m3-int8/` and `bge-reranker-v2-m3-int8/` from `s3://wt-v1-models-904883674947/` into the
  task before `wt-api` starts; startup takes about a minute. A new model goes to that bucket
  (`aws s3 sync models/<dir> s3://wt-v1-models-904883674947/<dir>`), then a redeploy. From the
  Cloudflare Tunnel's stage T-A a third container, `cloudflared`, joins the tunnel once `wt-api`
  passes its container health check (below, "The Cloudflare Tunnel").
- Settings: `WT_ENV=prod` (Secure cookies), the session secret, database password and Claude key
  from Secrets Manager (`wt-v1/*`), the language model settings from the `llm_*` variables in
  `infra/terraform/envs/v1/variables.tf`. Config files (`config/`) are baked into the image, so a
  config change ships as a new image.
- Database: RDS `wt-v1`, Postgres 16, `db.t4g.small` (`db.t4g.medium` until October 2026; the
  comment on `db_instance_class` says when to go back), private, named `rds-v1`. Loaded on
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
  The metrics are `["wt","eval","report","--run","<id>","--json"]`, in the same log stream;
  Spec 11, PB2.2 reads `summary_use` there. The claim audit is the reviewer's: they rate the
  summary lines on the review page and save `ratings.json`, and the owner loads it through the
  batch host forward as `wt_owner_login` (the task has no way to read a local file):
  `wt eval import-ratings --run <id> --file ratings.json --reviewer "<name>"`. The report's
  `claims` then counts verdicts, and any `invented` fails synthesis.
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

- Usage: what each tester did, by label, is `wt report usage --since 7d` in the same window, as
  the analyst (Spec 07, Usage report). It prints JSON with each tester's page views, visits,
  days active, questions, flags, feedback and dana clicks, page views per route and activity
  per day, and never a question's text. Page views go back 30 days; older ones are purged. A
  flagged question is read with `wt report flagged`.

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

### The edge: Cloudflare in front of the load balancer

Owner's decision of October 17, 2026 (`docs/adr-edge-cost.md`): Cloudflare proxies `app` and
`demo`, its Free Managed Ruleset and one rate rule replace the AWS WAF web ACL, and the load
balancer loses its public IPv4 addresses. The edge drops from about $37 to about $21 a month.

Done: stage A on October 19, 2026, and the Cloudflare settings, proxying, stage B and stage C on
October 20. `cloudflare_only = true` and `alb_public_ipv4 = false` are now the defaults in
`variables.tf`, so plain applies keep them; the stage steps below are kept for a rebuild or a
rollback. Step 8's next-day checks and burst test are still to do.

Three Terraform variables in `infra/terraform/envs/v1` carry it, applied by hand in this order,
since the deploy role cannot change the VPC, the load balancer's address type or listener rules:

| Stage | Variables | What changes |
| --- | --- | --- |
| A | defaults (`cloudflare_only = false`, `alb_public_ipv4 = true`) | IPv6 block on the VPC and public subnets, `::/0` route, load balancer `dualstack`, IPv6 ingress, the origin header and its listener rule (forwarding, so nothing changes for testers), the WAF web ACL removed |
| B | `cloudflare_only = true` | Requests without the origin header get a 403; ingress only from Cloudflare's ranges on 443; `WT_FORWARDED_HOPS=2` |
| C | `cloudflare_only = true`, `alb_public_ipv4 = false` | Load balancer `dualstack-without-public-ipv4`: no public IPv4 |

Every apply below runs from `infra/terraform/envs/v1` with both image tags of the running stack,
so it moves no image:

```bash
T="-var image_tag=$(terraform output -raw image_tag) -var migrate_image_tag=<the migrate tag the admin task runs>"
```

**Cutover.** Pick a quiet hour; the whole run takes about an hour.

1. Stage A, right after the PR merges and before the next deploy (`deploy.yml`'s apply would
   otherwise fail on the VPC changes): `terraform apply $T`. Check `/healthz` and one question.
2. Deploy the image with this change through `deploy.yml`, so `wt-api` reads
   `WT_FORWARDED_HOPS` and the About page names Cloudflare. Then `terraform apply` again with
   the new `image_tag` if the deploy did not already.
3. At Cloudflare, zone `wheelturning.org`, before anything is proxied:
   - SSL/TLS: encryption mode **Full (strict)**; Edge Certificates: minimum TLS **1.2**, **Always
     Use HTTPS** on. The load balancer's ACM certificate already covers both names.
   - Security, WAF, Managed rules: the **Cloudflare Free Managed Ruleset** deployed.
   - Security, WAF, Rate limiting rules: the Free plan's one rule, "wt forms and asks". Free
     rules can match only the path, count by IP over 10 seconds and block for 10 seconds, with no
     count mode. Match
     `(http.request.uri.path in {"/ask" "/ask/curated" "/rights/request" "/rights/confirm" "/accessibility/report"} or http.request.uri.path wildcard "/join/*")`,
     **20 requests per 10 seconds**, action **Block**. The app's own limits (asks per tester, 5
     public forms an hour per address) stay the main limits; this rule stops floods and invite
     token guessing. The static site's paths do not overlap these.
   - Rules, Transform Rules, Modify Request Header: one rule for the same two hosts that **sets**
     the header named in `terraform output -json cloudflare_origin_header` to its value.
   - Leave Bot Fight Mode off: it would challenge the Route 53 health checkers.
4. Proxy `app` and `demo` (orange cloud), keeping the CNAME to the load balancer. The two
   `_…acm-validations.aws` records stay DNS-only. Check from a browser and with
   `curl -sI https://app.wheelturning.org/healthz` (a `cf-ray` header shows it came through
   Cloudflare), ask one question and watch the progress stream reach the answer page, and send
   the rights form once.
5. Stage B: `terraform apply $T -var cloudflare_only=true`. Check: the site still works through
   Cloudflare; `curl -sk https://<alb dns name>/healthz` answers 403 over IPv4 and IPv6 (or does
   not connect, once the security group drops the old rules).
6. Stage C: `terraform apply $T -var cloudflare_only=true -var alb_public_ipv4=false`. Check the
   site again: Cloudflare now reaches the load balancer over IPv6 only. `nslookup <alb dns name>`
   returns no A record.
7. Commit `cloudflare_only = true` and `alb_public_ipv4 = false` as the defaults in
   `variables.tf`, so the next deploy keeps them.
8. The next day: no `wt-v1-healthz-unreachable` or `wt-v1-alb-5xx-rate` alarm, and Cloudflare's
   Security Events show the rate rule only on real floods. Burst-test it once (Spec 11, PB3.6):
   400 POSTs to `/rights/request` from one address as fast as they go; Cloudflare blocks them
   once 20 arrive within 10 seconds, and Security Events records the block.

**Rollback.** Undo the latest stage first; each takes a few minutes.

- After C: `terraform apply $T -var cloudflare_only=true` (public IPv4 back; the DNS name gains A
  records within minutes).
- After B: `terraform apply $T` (open to all again, `WT_FORWARDED_HOPS=1`), then, if Cloudflare is
  the problem, set `app` and `demo` back to DNS-only. Turn the proxy off only once public IPv4
  is back, or IPv4-only testers lose the site.
- After step 4 alone: set `app` and `demo` back to DNS-only.
- AWS WAF does not come back by rollback; it would need `waf.tf` restored from git history.

Cloudflare's ranges change rarely; `cloudflare_ranges` holds the copy read on October 17, 2026.
Compare it with `https://www.cloudflare.com/ips-v4` and `ips-v6` before each edge change and
every few months, since a new Cloudflare range missing here makes the site fail for some
testers.

### The Cloudflare Tunnel

Owner's decisions of October 17 and 19, 2026 (`docs/adr-edge-cost.md`, "Follow-on", T1 (a) and
T2): a `cloudflared` container in the web task carries `app` and `demo` from Cloudflare, the load
balancer goes, and last the task gives up its public IPv4 address. The edge drops from about $21
to about $0.40 a month. Cloudflare's proxy, managed ruleset and rate rule stay as they are.

Two stacks carry it, both applied only by the owner:

- `infra/terraform/edge`: the tunnel, its routes, the `app`, `demo` and `tunnel-test` DNS
  records and the maintenance Worker. It needs a Cloudflare API token in `CLOUDFLARE_API_TOKEN`,
  kept in the password manager and set only in the shell that applies. Create it once in the
  Cloudflare dashboard (My Profile, API Tokens, Custom token) with Account: Cloudflare Tunnel
  Edit and Workers Scripts Edit; Zone `wheelturning.org`: Zone Read, DNS Edit and Workers Routes
  Edit. CI never sees it.
- `infra/terraform/envs/v1`, with the variables `edge`, `db_dual_stack` and `web_ipv6_only`.

| Stage | edge stack | envs/v1 | What changes |
| --- | --- | --- | --- |
| T-A | first apply | `edge = "tunnel_test"` | The tunnel exists; the web task runs `cloudflared`, the container health check and the mirrored images; only `tunnel-test.wheelturning.org` goes through the tunnel |
| T-B | `app_via_tunnel = true` | `edge = "tunnel_live"` | `app` and `demo` go through the tunnel; `WT_FORWARDED_HOPS=1`; the load balancer stays, idle |
| T-C, a week later | | `edge = "tunnel"` | The load balancer, its certificate, its listeners and its two alarms are gone; the web security group has no inbound rule |
| T-D1, a quiet hour | | `db_dual_stack = true` | IPv6 on the private subnets, two IPv6-only subnets, the database in dual-stack mode |
| T-D2, the next day | | `web_ipv6_only = true` | The web task in the IPv6-only subnets, with no public IPv4 address |

Done so far: T-A and T-B on October 22, 2026 (T-B at 17:34 UTC). `edge = "tunnel_live"` and
`app_via_tunnel = true` are the defaults. T-C is due no earlier than October 29, if the week
passes with no alarm the tunnel caused.

Every envs/v1 apply below runs from `infra/terraform/envs/v1` with `$T` from "The edge" above,
plus the stage's variables. Once a stage holds, commit its variables as the defaults in that
stack's `variables.tf`, so `deploy.yml` and the next plain apply keep them.

**Before T-A.**

1. Merge and deploy this change through `deploy.yml`, so the image has `wt-api --healthcheck`
   and writes `http_response` events. Then `terraform apply $T` once, which adds the
   `wt-v1/cloudflare_tunnel_token` secret (empty), the two new alarms and their metric filters,
   and moves the load balancer's pieces in state without touching them. From here
   `wt-v1-web-5xx-rate` and `wt-v1-no-running-web-task` run beside the load balancer's alarms.
2. Copy the two outside images into the `wt-v1` repository, from a machine with Docker (the
   lifecycle rule never expires `mirror-` tags):

   ```bash
   R=904883674947.dkr.ecr.us-east-2.amazonaws.com/wt-v1
   aws ecr get-login-password | docker login --username AWS --password-stdin ${R%%/*}
   docker pull --platform linux/amd64 cloudflare/cloudflared:2026.10.0@sha256:9b49eed8f62806d5d45ddf59ecefb5710429598ea6d3fcccd2af938f621b2b07
   docker tag cloudflare/cloudflared:2026.10.0@sha256:9b49eed8f62806d5d45ddf59ecefb5710429598ea6d3fcccd2af938f621b2b07 $R:mirror-cloudflared-2026.10.0
   docker push $R:mirror-cloudflared-2026.10.0
   docker pull --platform linux/amd64 public.ecr.aws/aws-cli/aws-cli:2.31.0
   docker tag public.ecr.aws/aws-cli/aws-cli:2.31.0 $R:mirror-aws-cli-2.31.0
   docker push $R:mirror-aws-cli-2.31.0
   ```

   A new version later is a new tag here and in `mirror_tags` (`modules/service`).
3. The edge stack, first time: `terraform init`, then bring the two existing records into its
   state so it does not try to create them again. The zone ID is in `cf zones list --name
   wheelturning.org`, each record's ID in
   `cf dns records list -z wheelturning.org --name app.wheelturning.org --type CNAME` (and
   `demo`):

   ```bash
   terraform import 'module.edge.cloudflare_dns_record.app["app.wheelturning.org"]' <zone id>/<record id>
   terraform import 'module.edge.cloudflare_dns_record.app["demo.wheelturning.org"]' <zone id>/<record id>
   terraform plan
   ```

   The plan must change nothing on those two records (a proxied CNAME to the load balancer) and
   create the tunnel, its configuration, the `tunnel-test` record and the maintenance Worker.

**Cutover.**

1. T-A, edge stack: `terraform apply`. Then put the token into Secrets Manager without it
   touching a file or the shell history:

   ```bash
   aws secretsmanager put-secret-value --secret-id wt-v1/cloudflare_tunnel_token --secret-string "$(cf tunnels token get $(terraform output -raw tunnel_id) -q)"
   ```

   Check the secret holds the token alone (`aws secretsmanager get-secret-value ... --query
   'length(SecretString)'` prints about 180, not a JSON document).
2. T-A, envs/v1: `terraform apply $T -var edge=tunnel_test`. The new task starts `cloudflared`
   only once `wt-api` passes its health check, about two minutes in. Check: the service reaches a
   steady state; `cf tunnels connections list <tunnel id>` shows four connections;
   `curl -sI https://tunnel-test.wheelturning.org/healthz` answers 200 with a `cf-ray` header;
   from a browser, `https://tunnel-test.wheelturning.org/join/<a test invite>` signs in and one
   question streams to its answer page (the invite cookie is per hostname, so this is a separate
   test sign-in). Testers still use the load balancer.
3. T-B, in a quiet hour, envs/v1 first, then the edge stack, within a few minutes of each
   other: `terraform apply $T -var edge=tunnel_live` (the client address is then counted one
   hop from the end, which through the load balancer is Cloudflare's edge: coarse for the few
   minutes until the next step, never forgeable), then in `infra/terraform/edge`
   `terraform apply -var app_via_tunnel=true`. Check: `curl -sI https://app.wheelturning.org/healthz`
   answers 200; the load balancer's `RequestCount` falls to zero within minutes; one question
   streams to its answer page on `app` and on `demo`; the rights form sends its email.
4. A week on T-B with no `wt-v1-web-5xx-rate`, `wt-v1-no-running-web-task` or
   `wt-v1-healthz-unreachable` alarm that the tunnel caused: T-C,
   `terraform apply $T -var edge=tunnel`. Check the site as in step 3. Leave the two
   `_…acm-validations.aws` CNAMEs at Cloudflare: they cost nothing, and a rollback's new
   certificate for the same names validates through them.
5. T-D1, in a quiet hour: take a manual snapshot
   (`aws rds create-db-snapshot --db-instance-identifier wt-v1 --db-snapshot-identifier wt-v1-before-dual-stack`)
   and wait for it to be `available`; put up the maintenance page (§13); then
   `terraform apply $T -var edge=tunnel -var db_dual_stack=true`. AWS lists the network type
   change as one that may cause downtime. Wait until
   `aws rds describe-db-instances --db-instance-identifier wt-v1 --query 'DBInstances[0].[DBInstanceStatus,NetworkType]'`
   prints `available` and `DUAL`, take the maintenance page down, and check one question.
6. T-D2, the next day: `terraform apply $T -var edge=tunnel -var db_dual_stack=true -var web_ipv6_only=true`.
   The new task pulls through ECR's dual-stack name and has no IPv4 address at all. If it never
   turns healthy, the circuit breaker keeps the old task; read the stopped task's reason. One
   possible cause is the account setting AWS's guide asks for IPv6 on Fargate:
   `aws ecs put-account-setting-default --name dualStackIPv6 --value enabled`, then apply again.
   Check: one question with a summary (the model API over IPv6), the rights form's email (SES),
   an answer page that reads a transcript (S3), and `WT_ALERT_TOPIC_ARN=… wt alerts test` from
   the PC still reaches the topic; the next day, the `wt-v1-alarms` topic shows no failures.

**Rollback.** Undo the latest stage first.

- After T-D2: `terraform apply $T -var edge=tunnel -var db_dual_stack=true` (the task goes back
  to the public subnets with an address).
- After T-D1: with the maintenance page up, `terraform apply $T -var edge=tunnel`
  (`network_type` back to `IPV4`).
- After T-C: `terraform apply $T -var edge=tunnel_live` recreates the load balancer under a new
  DNS name, its certificate (compare its validation records, `terraform output
  dns_records_for_cloudflare`, with the CNAMEs left at Cloudflare) and its alarms, in about 10
  minutes; then roll back T-B. The origin header gets a new value: update the transform rule.
- After T-B: in `infra/terraform/edge`, `terraform apply -var app_via_tunnel=false` (the records
  point at the load balancer again within seconds), then `terraform apply $T -var edge=tunnel_test`.
- After T-A: `terraform apply $T` (edge `alb`). The tunnel itself can stay.
- Rolling back to an image older than this change fails its health check from T-A on, since it
  has no `--healthcheck`: roll back to the first image that has it, or set `edge = "alb"` first.

**Rotating the tunnel token.** `cf tunnels token get` prints the same token until the tunnel is
replaced; to change it, `terraform apply -replace=module.edge.cloudflare_zero_trust_tunnel_cloudflared.web`
in `infra/terraform/edge` (the records follow the new tunnel id), put the new token as in step
1, then force a new web deployment
(`aws ecs update-service --cluster wt-v1 --service wt-v1-web --force-new-deployment`). The site is
down between the replace and the new task joining, about three minutes, so do it in a quiet hour
with the maintenance page up.

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
   of the database cutover (PB1.1). Keep `data/objects` and `data/records` until the PC database
   is archived (§10, step 2).

### Terraform hygiene (roadmap UB3.9)

Egress by port, load balancer access logs, the `wt-v1` dashboard, the `wt-v1-web-memory` alarm,
Database Insights (Standard), storage autoscaling to 100 GB and cost allocation tags. Applied by
the owner, in this order, since the deploy role must be able to read each new resource before
`deploy.yml` can plan again:

1. `infra/terraform/github-oidc`: the deploy role may manage the `wt-v1-alb-logs-<account>`
   bucket and the `wt-v1` dashboard.
2. `infra/terraform/envs/v1`, with `-var image_tag=$(terraform output -raw image_tag)`. The plan
   replaces the web and batch security groups' all-protocol egress rules with port rules; open
   connections survive (security groups are stateful), and a request in the second between the
   two may fail once. Expect no restart of the database: Database Insights and the storage
   ceiling apply online.
3. `infra/terraform/account`: activates `project` and `env` as cost allocation tags. They show in
   Cost Explorer's "Tag" grouping from the next day's data.
4. Check: `/healthz` answers through Cloudflare; one question gets an answer with a summary (the
   web task still reaches the Claude API, SES and the database); after 10 minutes,
   `aws s3 ls s3://wt-v1-alb-logs-904883674947/alb/ --recursive` lists log files; the `wt-v1`
   dashboard shows data in every panel that has traffic.

To undo the egress change, restore the two `-1` rules in `modules/network/main.tf` and apply.

Service quotas worth knowing (WA REL01-BP01), read with the owner's profile and recorded here
when checked:

```bash
aws service-quotas get-service-quota --service-code fargate --quota-code L-3032A538  # on-demand vCPUs
aws sesv2 get-account                                                                # sending quota, sandbox status
aws service-quotas list-service-quotas --service-code rds --query "Quotas[?QuotaName=='Total storage for all DB instances'].Value"
```

| Quota | Value | Checked |
| --- | --- | --- |
| Fargate on-demand vCPUs | not yet recorded | |
| SES daily sending quota and rate | not yet recorded | |
| RDS total storage | not yet recorded | |

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

**Scale to zero** stops the web task, so testers get an error page (the load balancer's 503,
or Cloudflare's 530 behind the tunnel) and nothing is served. Use it only when serving is itself the harm: a passage that must not be served is being
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

- **Means:** more than 5% of requests returned a 5xx over two 5-minute periods. It exists only
  while the load balancer does (until the tunnel's stage T-C); `wt-v1-web-5xx-rate` counts the
  same from the web service's own log.
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
- **Fix:** storage autoscaling grows the volume on its own once free space stays under 10% for
  5 minutes, at most once every 6 hours, up to `max_allocated_storage_gb` (100 GB). So a single
  alarm that clears within the hour needs nothing more. If it stays in alarm, the volume is at
  its ceiling or still inside the 6-hour wait: raise `max_allocated_storage_gb` or
  `allocated_storage_gb` in the `database` module and apply. Drop leftovers only after a
  snapshot.
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

- **Means:** no web task has passed `/healthz` for 5 minutes. The site is down. It exists only
  while the load balancer does (until the tunnel's stage T-C); `wt-v1-no-running-web-task`
  replaces it.
- **First check:** the ECS service's events and the stopped task's reason, then the `models`
  and `web` log streams for the start-up error: a missing model, a secret without a value, the
  database refusing the login, a setting the web server refuses to start without.
- **Fix:** deploy the last good `image_tag`, or fix the setting the error names (procedure 12).
  Put up the maintenance page while it is down.
- **Scale to zero:** it already serves nothing.

#### `wt-v1-web-5xx-rate`

- **Means:** more than 5% of the web service's responses were 5xx over two 5-minute periods,
  counted from its `http_response` log events. Errors that Cloudflare or `cloudflared` answer
  because the task cannot be reached (502, 530) are not in the log; `wt-v1-no-running-web-task`
  and `wt-v1-healthz-unreachable` catch those.
- **First check:** `/wt-v1/web` for `"level":"ERROR"` lines in the window, and which routes
  failed: `fields route, status | filter event = "http_response" and status >= 500 | stats count() by route`.
- **Fix:** as for `wt-v1-alb-5xx-rate`: deploy the previous `image_tag` for a bad deploy; a
  database error goes to the database alarms.
- **Scale to zero:** no. Put up the maintenance page if it lasts more than 15 minutes.

#### `wt-v1-no-running-web-task`

- **Means:** the web service has had no running task for 5 minutes: the site is down. Behind the
  tunnel ECS replaces a task whose `/healthz` fails its container health check, so a task that
  keeps failing shows as gaps here. It also fires while the service is scaled to zero.
- **First check:** the ECS service's events and the stopped task's reason (an essential
  container exited, or failed its health check), then the `models`, `web` and `cloudflared` log
  streams: a missing model, a secret without a value (the tunnel token included), the database
  refusing the login, `cloudflared` unable to reach Cloudflare on 7844.
- **Fix:** deploy the last good `image_tag` (no older than the first with `--healthcheck`), or
  fix the setting the error names. Put up the maintenance page while it is down.
- **Scale to zero:** it already serves nothing.

#### `wt-v1-web-memory`

- **Means:** the web task's memory has been above 85% of its 3 GB for 15 minutes. Fargate stops
  a task that runs out, and the embedder lives in that memory.
- **First check:** the `wt-v1` dashboard's CPU and memory panel against requests: a climb with
  traffic is load, a climb without it is a leak. Then `config/retrieval.toml`: a cross-encoder
  ranker (`ranker = "onnx"`) needs 2 vCPU / 8 GB (Spec 01, "The beta deployment").
- **Fix:** raise `web_memory` (with a matching `web_cpu`) in `infra/terraform/envs/v1` and
  deploy. A leak: deploy the last good `image_tag` and record it in `docs/bugs.md`.
- **Scale to zero:** no.

#### `wt-v1-healthz-unreachable`

- **Means:** Route 53 has not reached `https://app.wheelturning.org/healthz` from outside AWS
  for 3 minutes. It also fires while the maintenance page is up, by design.
- **First check:** whether `wt-v1-no-running-web-task` (or, with the load balancer,
  `wt-v1-no-healthy-web-task`) fired too (the app) or not (Cloudflare, DNS, the tunnel, the
  certificate or the load balancer). Cloudflare's status page and its Security Events for
  blocked `/healthz` requests come next; behind the tunnel, `cf tunnels connections list <tunnel
  id>` shows whether `cloudflared` is connected.
- **Fix:** the app: as above. DNS: the CNAME at Cloudflare must be proxied and match
  `terraform output dns_records_for_cloudflare`, and the transform rule must still set the origin
  header (without it the load balancer answers 403). The certificate: ACM renews it while its
  validation CNAME stays DNS-only at Cloudflare. The tunnel: no connections means `cloudflared`
  is down or cannot reach Cloudflare (its log stream says which); a forced new deployment
  restarts it. A Cloudflare outage takes the site down; if it lasts, roll the edge back (§12,
  "The edge"; behind the tunnel only once the load balancer is back, "The Cloudflare Tunnel").
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
  above. If the database's ReadIOPS rose with the slow answers, the search indexes no longer fit
  in its memory: set `db_instance_class` back to `db.t4g.medium`, apply in a quiet hour, then
  restart the web service (`aws ecs update-service --force-new-deployment`) so its warm-up runs.
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
  `pc_backup_stale` means the PC's 03:00 backup has not run (§10: retired, so clear `pc_backup_bucket` instead of fixing the backup), `crisis_link_dead` means a help
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

A fixed 503 page saying the site is down for maintenance, on every path, `/healthz` included,
while the web task keeps running. Which stack serves it depends on what carries the traffic
(§12, "The Cloudflare Tunnel").

From the tunnel's stage T-B on, Cloudflare serves it: `maintenance = true` in
`infra/terraform/edge` routes `app` and `demo` to the `wt-v1-maintenance` Worker. From that
directory, with `CLOUDFLARE_API_TOKEN` set:

```bash
terraform apply -var maintenance=true
```

Apply again without it to end it. `terraform test` in `infra/terraform/modules/edge` checks the
switch offline.

Before T-B, the load balancer serves it: from `infra/terraform/envs/v1`,

```bash
terraform apply -var maintenance=true -var image_tag=$(terraform output -raw image_tag)
```

and `-var maintenance=false` to end it; `terraform test` in `infra/terraform/modules/service`
checks that switch.
