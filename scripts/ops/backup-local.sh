#!/usr/bin/env bash
# Nightly off-site backup of the owner's local stack, until the database moves to RDS
# (docs/runbook.md, procedure 10). Run from Git Bash; it always backs up the main checkout's data:
#
#   scripts/ops/backup-local.sh        (bucket from WT_BACKUP_BUCKET or the main checkout's .env)
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

# The real data lives in the main checkout, whichever worktree this copy of the script is in.
ROOT=${WT_ROOT:-$(git -C "$(dirname "$0")" worktree list --porcelain | awk '/^worktree /{print substr($0, 10); exit}')}
cd "$ROOT"
# The bucket name may come from .env; nothing else is read from it.
if [ -z "${WT_BACKUP_BUCKET:-}" ] && [ -f .env ]; then
  WT_BACKUP_BUCKET=$(grep -E '^WT_BACKUP_BUCKET=' .env | cut -d= -f2- | tr -d '\r')
fi
BUCKET=${WT_BACKUP_BUCKET:?set WT_BACKUP_BUCKET to the backup bucket name}
PROFILE=${WT_BACKUP_PROFILE:-wt-backup}
CONTAINER=${WT_BACKUP_CONTAINER:-wt-m3-ssz}
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
