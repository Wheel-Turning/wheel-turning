//! The shared contract suite for object and record stores.

use wt_core::objects::ObjectStore;
use wt_core::records::{RecordError, Records, sha256_hex};

/// What every [`ObjectStore`] must do. The store starts empty.
pub fn objects(store: &dyn ObjectStore) {
    let ok = |r: Result<(), wt_core::objects::ObjectError>| r.unwrap_or_else(|e| panic!("{e}"));

    // Put, get and replace.
    ok(store.put("transcripts/t1/v1.json", b"first"));
    ok(store.put("transcripts/t1/v1.json", b"second"));
    assert_eq!(
        store.get("transcripts/t1/v1.json").unwrap_or_default(),
        Some(b"second".to_vec())
    );
    assert_eq!(
        store.get("transcripts/t2/v1.json").unwrap_or_default(),
        None
    );

    // Listing: sorted, by directory or by one object, and across pages.
    for key in [
        "transcripts/t1/v3.json",
        "transcripts/t1/v2.json",
        "transcripts/t10/v1.json",
        "citations/t1.json",
        "citations/t1.json.bak",
        "takedowns/x.json",
    ] {
        ok(store.put(key, b"{}"));
    }
    assert_eq!(
        store.list("transcripts/t1/").unwrap_or_default(),
        [
            "transcripts/t1/v1.json",
            "transcripts/t1/v2.json",
            "transcripts/t1/v3.json"
        ]
    );
    assert_eq!(
        store.list("transcripts/t1").unwrap_or_default().len(),
        3,
        "a prefix without a slash names the directory, not its siblings"
    );
    assert_eq!(
        store.list("citations/t1.json").unwrap_or_default(),
        ["citations/t1.json"]
    );
    assert_eq!(store.list("").unwrap_or_default().len(), 7);
    assert!(store.list("transcripts/t9/").unwrap_or_default().is_empty());
    assert_eq!(store.versions("citations/t1.json").unwrap_or_default(), 1);

    // Deletion by prefix: exact, counted, and complete.
    assert_eq!(
        store.delete_prefix("transcripts/t1/").unwrap_or_default(),
        3
    );
    assert!(store.list("transcripts/t1/").unwrap_or_default().is_empty());
    assert_eq!(store.versions("transcripts/t1/").unwrap_or(1), 0);
    assert_eq!(store.list("transcripts/t10/").unwrap_or_default().len(), 1);
    assert_eq!(
        store.delete_prefix("citations/t1.json").unwrap_or_default(),
        1
    );
    assert_eq!(
        store.list("citations/").unwrap_or_default(),
        ["citations/t1.json.bak"]
    );
    assert_eq!(store.delete_prefix("transcripts/t9/").unwrap_or(1), 0);

    // Keys that could leave a local root are refused everywhere.
    for bad in ["", "../x", "a//b", "a/../b", "c:\\x", "a/./b"] {
        assert!(store.put(bad, b"x").is_err(), "put {bad:?}");
    }
    assert!(
        store.delete_prefix("").is_err(),
        "an empty prefix deletes nothing"
    );
}

/// What every [`Records`] store must do. The store starts empty.
pub fn records(records: &dyn Records) {
    let first = records
        .store(b"We decline.")
        .unwrap_or_else(|e| panic!("{e}"));
    let again = records
        .store(b"We decline.")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first, again);
    assert_eq!(first.sha256, sha256_hex(b"We decline."));
    assert_eq!(first.key, format!("records/{}", first.sha256));
    let second = records
        .store(b"We agree.")
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        records.get(&first.sha256).unwrap_or_default(),
        Some(b"We decline.".to_vec())
    );
    assert_eq!(
        records
            .get(&sha256_hex(b"never stored"))
            .unwrap_or_default(),
        None
    );
    let mut expected = vec![first.sha256.clone(), second.sha256.clone()];
    expected.sort();
    assert_eq!(records.list().unwrap_or_default(), expected);

    let dir = std::env::temp_dir().join(format!("wt-statement-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let empty = dir.join("empty.txt");
    std::fs::write(&empty, " \n").unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        records.store_file(&empty),
        Err(RecordError::Empty(_))
    ));
    let statement = dir.join("statement.txt");
    std::fs::write(&statement, "Yes, use the talks.").unwrap_or_else(|e| panic!("{e}"));
    let from_file = records
        .store_file(&statement)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(from_file.sha256, sha256_hex(b"Yes, use the talks."));
    let _ = std::fs::remove_dir_all(&dir);
}
