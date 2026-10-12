#!/usr/bin/env bash
# Nightly off-site backup of the owner's local stack, until the database moves to RDS
# (docs/runbook.md, procedure 9). Run from Git Bash in the main checkout:
#
#   WT_BACKUP_BUCKET=<bucket> scripts/ops/backup-local.sh
#
# What it copies to s3://$WT_BACKUP_BUCKET:
#   db/wt-<UTC stamp>.dump   a pg_dump of the real-data database (custom format, compressed)
#   objects/                 data/objects (transcripts, staging), mirrored with deletions, so a
#                            takedown leaves the backup too
#   records/                 data/records (statements, drafts, mandate decisions), never deleted
#
# It never copies .env or any secret. The bucket keeps noncurrent versions and database dumps for
# seven days (scripts/ops/backup-bucket-lifecycle.json), which is the backup limit the runbook's
# takedown note promises teachers. The AWS profile ($WT_BACKUP_PROFILE, default wt-backup) may
# list, read, write and delete current objects, but not delete old versions.
set -euo pipefail

BUCKET=${WT_BACKUP_BUCKET:?set WT_BACKUP_BUCKET to the backup bucket name}
PROFILE=${WT_BACKUP_PROFILE:-wt-backup}
CONTAINER=${WT_BACKUP_CONTAINER:-wt-m3-ssz}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
OUT=data/backups
mkdir -p "$OUT"
LOG="$OUT/backup.log"
stamp=$(date -u +%Y%m%dT%H%MZ)
log() { echo "$(date -u +%FT%TZ) $*" | tee -a "$LOG"; }
s3() { aws --profile "$PROFILE" --only-show-errors "$@"; }

log "backup $stamp to s3://$BUCKET"
dump="$OUT/wt-$stamp.dump"
docker exec "$CONTAINER" pg_dump -U wt -d wt -Fc > "$dump"
# A dump that pg_restore cannot list is not a backup.
docker exec -i "$CONTAINER" pg_restore -l < "$dump" > /dev/null
log "database dump $(du -h "$dump" | cut -f1)"
s3 s3 cp "$dump" "s3://$BUCKET/db/$(basename "$dump")"
rm -f "$dump"

s3 s3 sync data/objects "s3://$BUCKET/objects" --delete
s3 s3 sync data/records "s3://$BUCKET/records"

date -u +%FT%TZ > "$OUT/last-success"
log "backup $stamp done"
