//! `wt objects sync` and `wt records sync` (Spec 11, PB1.2): the one-time copy of the PC's
//! `data/objects` into the transcripts bucket and of `data/records` into the records bucket.
//! Each is a dry run unless `--apply` is given, and each verifies every key by SHA-256 after
//! copying, listing any key that is missing or differs.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_core::records::{LocalRecords, Records, sha256_hex};

use crate::Ctx;
use crate::commands::print_json;

/// `wt objects`.
#[derive(Debug, Subcommand)]
pub enum ObjectsCommand {
    /// Copy a local object directory into the transcripts bucket (WT_TRANSCRIPT_BUCKET) and
    /// verify every key by SHA-256. A dry run unless --apply is given.
    Sync(SyncArgs),
}

/// `wt records`.
#[derive(Debug, Subcommand)]
pub enum RecordsCommand {
    /// Copy a local records directory into the records bucket (WT_RECORDS_BUCKET) and verify
    /// every record by SHA-256. A dry run unless --apply is given.
    Sync(SyncArgs),
}

/// Where to copy from, and whether to copy.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// The local directory, such as data/objects or data/records.
    #[arg(long)]
    from: PathBuf,
    /// Copy. Without it, only report what would be copied.
    #[arg(long)]
    apply: bool,
}

/// What a sync found and did. Keys are listed, never contents.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct SyncReport {
    /// Whether this run copied.
    pub applied: bool,
    /// Keys in the local directory.
    pub source: usize,
    /// Keys the bucket already held with the same SHA-256.
    pub matched_before: usize,
    /// Keys copied, or that a dry run would copy.
    pub copied: usize,
    /// Keys the bucket held with different bytes. Objects are replaced by `--apply`; records
    /// never are, since a record's key is its hash.
    pub differing: Vec<String>,
    /// Local records whose bytes do not hash to their file name; never copied.
    pub corrupt_source: Vec<String>,
    /// After copying: keys missing from the bucket or whose SHA-256 differs.
    pub mismatched: Vec<String>,
    /// Keys in the bucket's listing that the local directory lacks.
    pub only_in_bucket: usize,
    /// Keys verified in the bucket with matching SHA-256 (after an apply, or before a dry run).
    pub verified: usize,
    /// True when every local key is in the bucket with the same SHA-256.
    pub complete: bool,
}

/// `wt objects ...`.
pub async fn objects(ctx: &Ctx, command: ObjectsCommand) -> anyhow::Result<ExitCode> {
    let ObjectsCommand::Sync(args) = command;
    if ctx.config.transcript_bucket.is_none() {
        bail!("WT_TRANSCRIPT_BUCKET is not set: there is no bucket to copy into");
    }
    let source = LocalObjects::new(&args.from);
    let bucket = crate::stages::objects(&ctx.config)?;
    let report = sync_objects(&source, &bucket, args.apply).context("syncing objects")?;
    finish(&report)
}

/// `wt records ...`.
pub async fn records(ctx: &Ctx, command: RecordsCommand) -> anyhow::Result<ExitCode> {
    let RecordsCommand::Sync(args) = command;
    if ctx.config.records_bucket.is_none() {
        bail!("WT_RECORDS_BUCKET is not set: there is no bucket to copy into");
    }
    let source = LocalRecords::new(&args.from);
    let report = sync_records(&source, &ctx.records, args.apply).context("syncing records")?;
    finish(&report)
}

/// Prints the report; exits 1 when an applied sync is incomplete or anything differs.
fn finish(report: &SyncReport) -> anyhow::Result<ExitCode> {
    print_json(report)?;
    let bad = !report.mismatched.is_empty()
        || !report.corrupt_source.is_empty()
        || (report.applied && !report.complete);
    Ok(if bad {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Copies every object from `source` to `dest` that `dest` lacks or holds with other bytes,
/// then reads each back and compares SHA-256.
pub fn sync_objects(
    source: &dyn ObjectStore,
    dest: &dyn ObjectStore,
    apply: bool,
) -> anyhow::Result<SyncReport> {
    let keys = source.list("")?;
    let mut report = SyncReport {
        applied: apply,
        source: keys.len(),
        ..SyncReport::default()
    };
    let mut hashes = Vec::with_capacity(keys.len());
    for key in &keys {
        let bytes = source
            .get(key)?
            .with_context(|| format!("{key} vanished from the source while syncing"))?;
        let sha = sha256_hex(&bytes);
        match dest.get(key)? {
            Some(held) if sha256_hex(&held) == sha => report.matched_before += 1,
            held => {
                if held.is_some() {
                    report.differing.push(key.clone());
                }
                report.copied += 1;
                if apply {
                    dest.put(key, &bytes)?;
                }
            }
        }
        hashes.push((key.clone(), sha));
    }
    if apply {
        for (key, sha) in &hashes {
            match dest.get(key)? {
                Some(held) if sha256_hex(&held) == *sha => report.verified += 1,
                _ => report.mismatched.push(key.clone()),
            }
        }
    } else {
        report.verified = report.matched_before;
    }
    let local: BTreeSet<&String> = keys.iter().collect();
    report.only_in_bucket = dest.list("")?.iter().filter(|k| !local.contains(k)).count();
    report.complete = report.verified == report.source && report.mismatched.is_empty();
    Ok(report)
}

/// Copies every record from `source` to `dest` that `dest` lacks, then reads each back and
/// compares SHA-256. A record whose bytes do not match its name is reported and not copied.
pub fn sync_records(
    source: &dyn Records,
    dest: &dyn Records,
    apply: bool,
) -> anyhow::Result<SyncReport> {
    let names = source.list()?;
    let mut report = SyncReport {
        applied: apply,
        source: names.len(),
        ..SyncReport::default()
    };
    let mut good = Vec::with_capacity(names.len());
    for name in &names {
        let bytes = source
            .get(name)?
            .with_context(|| format!("record {name} vanished from the source while syncing"))?;
        if sha256_hex(&bytes) != *name {
            report.corrupt_source.push(name.clone());
            continue;
        }
        match dest.get(name)? {
            Some(held) if sha256_hex(&held) == *name => report.matched_before += 1,
            Some(_) => report.differing.push(name.clone()),
            None => {
                report.copied += 1;
                if apply {
                    dest.store(&bytes)?;
                }
            }
        }
        good.push(name.clone());
    }
    if apply {
        for name in &good {
            match dest.get(name)? {
                Some(held) if sha256_hex(&held) == *name => report.verified += 1,
                _ => report.mismatched.push(name.clone()),
            }
        }
    } else {
        report.verified = report.matched_before;
    }
    let local: BTreeSet<&String> = names.iter().collect();
    report.only_in_bucket = dest.list()?.iter().filter(|k| !local.contains(k)).count();
    report.complete = report.verified == report.source && report.mismatched.is_empty();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wt_core::objects::S3Objects;
    use wt_core::records::S3Records;
    use wt_core::s3::FakeBucket;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wt-sync-{name}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn objects_sync_is_a_dry_run_until_applied_then_verifies_every_key() {
        let dir = temp("objects");
        let local = LocalObjects::new(&dir);
        for (key, body) in [
            ("transcripts/t1/v1.json", "{\"a\":1}"),
            ("transcripts/t2/v1.json", "{\"b\":2}"),
            ("citations/t1.json", "[]"),
        ] {
            local
                .put(key, body.as_bytes())
                .unwrap_or_else(|e| panic!("{e}"));
        }
        let bucket = S3Objects::new(FakeBucket::new(2));
        bucket
            .put("citations/t1.json", b"[\"stale\"]")
            .unwrap_or_else(|e| panic!("{e}"));
        bucket
            .put("takedowns/x.json", b"{}")
            .unwrap_or_else(|e| panic!("{e}"));

        let dry = sync_objects(&local, &bucket, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((dry.source, dry.copied, dry.verified), (3, 3, 0));
        assert_eq!(dry.differing, ["citations/t1.json"]);
        assert!(!dry.complete);
        assert!(
            bucket
                .get("transcripts/t1/v1.json")
                .unwrap_or_default()
                .is_none()
        );

        let applied = sync_objects(&local, &bucket, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((applied.copied, applied.verified), (3, 3));
        assert!(applied.complete && applied.mismatched.is_empty());
        assert_eq!(applied.only_in_bucket, 1);

        let again = sync_objects(&local, &bucket, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((again.matched_before, again.copied), (3, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_sync_copies_by_hash_and_refuses_a_corrupt_record() {
        let dir = temp("records");
        let local = LocalRecords::new(&dir);
        let kept = local.store(b"We agree.").unwrap_or_else(|e| panic!("{e}"));
        local
            .store(b"We decline.")
            .unwrap_or_else(|e| panic!("{e}"));
        let forged = sha256_hex(b"something else");
        std::fs::write(dir.join(&forged), b"tampered").unwrap_or_else(|e| panic!("{e}"));
        let bucket = S3Records::new(FakeBucket::new(2));

        let dry = sync_records(&local, &bucket, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((dry.source, dry.copied), (3, 2));
        assert_eq!(dry.corrupt_source, std::slice::from_ref(&forged));
        assert!(bucket.list().unwrap_or_default().is_empty());

        let applied = sync_records(&local, &bucket, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(applied.verified, 2);
        assert!(!applied.complete, "the corrupt record is never copied");
        assert!(bucket.bucket().held(&kept.key));
        assert_eq!(bucket.list().unwrap_or_default().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
