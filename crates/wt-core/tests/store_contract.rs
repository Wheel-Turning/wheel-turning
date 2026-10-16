//! The contract every object and record store keeps (Spec 11, PB1.2), run against the local
//! directory and the S3 store over the in-process fake bucket. The S3 store's own client runs it
//! too, against a local S3-compatible container, in `s3_container.rs`.

use wt_core::objects::{LocalObjects, ObjectStore, S3Objects};
use wt_core::records::{LocalRecords, RecordError, Records, S3Records, sha256_hex};
use wt_core::s3::FakeBucket;

mod contract;

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wt-{name}-{}", uuid::Uuid::new_v4()))
}

#[test]
fn the_local_object_store_keeps_the_contract() {
    let root = temp("objects");
    contract::objects(&LocalObjects::new(&root));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_s3_object_store_keeps_the_contract_on_the_fake() {
    contract::objects(&S3Objects::new(FakeBucket::new(2)));
}

#[test]
fn the_local_records_keep_the_contract() {
    let dir = temp("records");
    contract::records(&LocalRecords::new(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_s3_records_keep_the_contract_on_the_fake() {
    contract::records(&S3Records::new(FakeBucket::new(2)));
}

#[test]
fn deleting_a_prefix_in_s3_removes_every_version_and_delete_marker() {
    let store = S3Objects::new(FakeBucket::new(2));
    for n in 1..=3 {
        store
            .put(
                "transcripts/t1/v1.json",
                format!("{{\"n\":{n}}}").as_bytes(),
            )
            .unwrap_or_else(|e| panic!("{e}"));
    }
    store
        .put("transcripts/t1/v2.json", b"{}")
        .unwrap_or_else(|e| panic!("{e}"));
    // A key deleted without its versions, as a plain S3 delete leaves it in a versioned bucket.
    store
        .put("transcripts/t1/old.json", b"{}")
        .unwrap_or_else(|e| panic!("{e}"));
    store.bucket().delete_current("transcripts/t1/old.json");
    store
        .put("transcripts/t10/v1.json", b"{}")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(store.versions("transcripts/t1/").unwrap_or_default(), 6);

    assert_eq!(
        store.delete_prefix("transcripts/t1/").unwrap_or_default(),
        2
    );
    assert_eq!(store.bucket().version_count("transcripts/t1/"), 0);
    assert_eq!(store.versions("transcripts/t1/").unwrap_or(1), 0);
    assert_eq!(
        store.list("").unwrap_or_default(),
        ["transcripts/t10/v1.json"]
    );
}

#[test]
fn s3_records_are_written_once_with_a_legal_hold() {
    let records = S3Records::new(FakeBucket::new(2));
    let stored = records
        .store(b"We agree.")
        .unwrap_or_else(|e| panic!("{e}"));
    records
        .store(b"We agree.")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(records.bucket().held(&stored.key));
    assert_eq!(records.bucket().version_count("records/"), 1);
    assert_eq!(stored.sha256, sha256_hex(b"We agree."));
    assert!(matches!(
        records.get("not-a-hash"),
        Ok(None) | Err(RecordError::S3(_))
    ));
}
