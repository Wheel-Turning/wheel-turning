//! The S3 client itself against a local S3-compatible server in Docker (owner's answer to Spec 11
//! open question 4: allowed while it stays cheap). Like the Postgres tests, it starts its own
//! container and makes no call outside this machine. MinIO's image left Docker Hub in 2026 and
//! Adobe's S3Mock has no version listing, so this uses RustFS (Apache-2.0), an S3-compatible
//! server that keeps versions and Object Lock legal holds as S3 does. It starts in about a
//! second.

use std::time::{Duration, Instant};

use aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use wt_core::Secret;
use wt_core::objects::{ObjectStore, S3Objects};
use wt_core::records::{Records, S3Records};
use wt_core::s3::{AwsBucket, Bucket, Endpoint, versions_all};

mod contract;

const IMAGE: &str = "rustfs/rustfs";
const TAG: &str = "1.0.0";
const PORT: u16 = 9000;
const ACCESS_KEY: &str = "wt-test";
const SECRET_KEY: &str = "wt-test-secret";

/// Starts the server and returns it with its endpoint.
async fn server() -> (ContainerAsync<GenericImage>, Endpoint) {
    let container = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(PORT.tcp())
        .with_env_var("RUSTFS_ACCESS_KEY", ACCESS_KEY)
        .with_env_var("RUSTFS_SECRET_KEY", SECRET_KEY)
        .start()
        .await
        .unwrap_or_else(|e| panic!("starting {IMAGE}:{TAG}: {e}"));
    let host = container.get_host().await.unwrap_or_else(|e| panic!("{e}"));
    let port = container
        .get_host_port_ipv4(PORT.tcp())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let endpoint = Endpoint {
        url: format!("http://{host}:{port}"),
        access_key: ACCESS_KEY.to_owned(),
        secret_key: Secret::new(SECRET_KEY),
    };
    (container, endpoint)
}

/// Creates a versioned bucket, waiting for the server to answer. `object_lock` creates it with
/// Object Lock, as the records bucket is.
async fn create(bucket: &AwsBucket, object_lock: bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let made = bucket
            .client()
            .create_bucket()
            .bucket(bucket.name())
            .object_lock_enabled_for_bucket(object_lock)
            .send()
            .await;
        match made {
            Ok(_) => break,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("creating {}: {e:?}", bucket.name()),
        }
    }
    bucket
        .client()
        .put_bucket_versioning()
        .bucket(bucket.name())
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|e| panic!("versioning {}: {e:?}", bucket.name()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_s3_client_keeps_the_store_contract_on_a_local_server() {
    let (_container, endpoint) = server().await;
    let connect = |name: &str| {
        AwsBucket::connect(name, Some(endpoint.clone()))
            .unwrap_or_else(|e| panic!("{e}"))
            .with_page_size(2)
    };

    let objects = connect("wt-objects");
    create(&objects, false).await;
    let store = S3Objects::new(objects);
    contract::objects(&store);

    // A versioned bucket keeps old versions; deleting the prefix removes them all.
    for n in 0..3 {
        store
            .put("transcripts/t1/v1.json", format!("{n}").as_bytes())
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert_eq!(store.versions("transcripts/t1/").unwrap_or_default(), 3);
    assert_eq!(
        store.delete_prefix("transcripts/t1/").unwrap_or_default(),
        1
    );
    assert!(
        versions_all(store.bucket(), "transcripts/t1/")
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty()
    );

    let records = connect("wt-records");
    create(&records, true).await;
    let records = S3Records::new(records);
    contract::records(&records);
    // Every record is under a legal hold, so even a permanent delete of its version fails.
    let held = versions_all(records.bucket(), "records/").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(held.len(), records.list().unwrap_or_default().len());
    assert!(records.bucket().delete_versions(&held[..1]).is_err());
}
