# Runbook

The procedures Spec 07 requires, written against the commands as built. Every `wt` command
reads its configuration from the environment (`.env.example` lists the keys). Commands that
change data print a JSON report on stdout; keep it with the record of what was done.

Real transcripts, review files and ratings never go into git. Run commands from the repository
root so `config/` and `data/` resolve.

## 1. A transcription run on the PC

The PC worker reaches the database through an SSM port forward via the batch host (Spec 01;
`docs/windows.md` has the details).

1. Start the batch host: in `infra/terraform/envs/v1`, apply with `batch_host_enabled = true`
   (as a `t3.small`, `batch_host_instance_type = "t3.small"`), or start the stopped instance in
   the console. Note `terraform output batch_host_instance_id`.
2. Sign in and open the forward from PowerShell:

   ```powershell
   aws sso login --profile wheel-turning
   powershell -ExecutionPolicy Bypass -File scripts\worker-setup.ps1
   ```

   It opens `localhost:5433` to the database and prints the `DATABASE_URL` to put in `.env`
   (with `sslmode=require`; the password is the `db_dg_pipeline_login` secret).
3. Run the worker on the PC: `wt worker --kinds audio` (add `--drain` to stop when the queue is
   empty). `wt pipeline advance` queues every cleared talk first if needed.
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
- Web service: Fargate, 2 vCPU / 8 GB, one task. Its `models` container copies
  `bge-m3-int8/` and `bge-reranker-v2-m3-int8/` from `s3://wt-v1-models-904883674947/` into the
  task before `wt-api` starts; startup takes about a minute. A new model goes to that bucket
  (`aws s3 sync models/<dir> s3://wt-v1-models-904883674947/<dir>`), then a redeploy.
- Settings: `WT_ENV=prod` (Secure cookies), the session secret, database password and Claude key
  from Secrets Manager (`wt-v1/*`), the language model settings from the `llm_*` variables in
  `infra/terraform/envs/v1/variables.tf`. Config files (`config/`) are baked into the image, so a
  config change ships as a new image.
- Database: RDS `wt-v1`, Postgres 16, `db.t4g.medium`, private. Loaded on October 13, 2026 from
  the PC's `wt-m3-ssz` database (data only, into the same migration level) through the batch host.
- Deploys: `deploy.yml` (plan, review, apply). An infrastructure change beyond the web task and
  service is applied by hand from `infra/terraform/envs/v1` with `-var image_tag=$(terraform
  output -raw image_tag)`.
- Testers: invites are created against RDS through the batch host forward (procedure 1, step 2),
  connecting as `wt_owner_login` (its secret is `wt-v1/db_wt_owner_login`), since only the owner
  role writes `tester`:

  ```bash
  wt tester invite --label "<name>" --base-url https://app.wheelturning.org
  ```

- Cost: about $170 a month (task about $85, database about $50, load balancer and addresses
  about $30). Scale the service to 0 tasks between test rounds to save the task's share.

The PC keeps its own database: the transcription worker still writes to `wt-m3-ssz`, so new
talks reach the beta only when they are loaded again (or once the PC switches to RDS, which is
the owner's call).
