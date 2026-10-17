//! The S3 calls behind the object and record stores (Spec 11, PB1.2). [`Bucket`] is the handful
//! of S3 operations the stores need, so the stores' logic (paging, deleting every version under a
//! prefix, legal holds) is tested against [`FakeBucket`], an in-process versioned bucket, and the
//! same logic runs against S3 through [`AwsBucket`].
//!
//! The stores are synchronous, like the local directory they replace, and are called from async
//! code. [`AwsBucket`] therefore runs each request on a small runtime of its own and waits for the
//! answer, so it works from any thread, inside a runtime or not.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier, ObjectLockLegalHoldStatus};

/// The project's home region (Spec 01), used when neither the environment nor the profile names
/// one.
const HOME_REGION: &str = "us-east-2";

/// Why an S3 call failed.
#[derive(Debug, thiserror::Error)]
#[error("S3 {op} {key}: {message}")]
pub struct S3Error {
    /// The operation, such as `GetObject`.
    pub op: &'static str,
    /// The key or prefix it concerned.
    pub key: String,
    /// What S3 or the client said.
    pub message: String,
}

impl S3Error {
    /// An error from `op` on `key`.
    fn new(op: &'static str, key: &str, message: impl std::fmt::Display) -> Self {
        Self {
            op,
            key: key.to_owned(),
            message: message.to_string(),
        }
    }
}

/// One stored version of a key. A bucket without versioning reports the version `null`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// The object key.
    pub key: String,
    /// The version id.
    pub version_id: String,
    /// Whether this version is a delete marker rather than stored bytes.
    pub delete_marker: bool,
}

/// One page of a listing and where the next one starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T, C> {
    /// The entries on this page.
    pub items: Vec<T>,
    /// The cursor for the next page; `None` on the last.
    pub next: Option<C>,
}

/// Where a version listing resumes: the key marker and the version marker.
pub type VersionCursor = (String, String);

/// The S3 operations the stores use, on one bucket.
pub trait Bucket: Send + Sync {
    /// Stores `bytes` under `key`. `legal_hold` places an Object Lock legal hold on the new
    /// version, which the records bucket requires on every write.
    fn put(&self, key: &str, bytes: &[u8], legal_hold: bool) -> Result<(), S3Error>;
    /// The current version's bytes, if the key exists.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, S3Error>;
    /// Whether the key exists.
    fn exists(&self, key: &str) -> Result<bool, S3Error>;
    /// One page of the current keys starting with `prefix`, in key order.
    fn list_page(
        &self,
        prefix: &str,
        after: Option<String>,
    ) -> Result<Page<String, String>, S3Error>;
    /// One page of every version and delete marker of the keys starting with `prefix`.
    fn versions_page(
        &self,
        prefix: &str,
        after: Option<VersionCursor>,
    ) -> Result<Page<Version, VersionCursor>, S3Error>;
    /// Permanently deletes these versions; at most 1,000 at a time.
    fn delete_versions(&self, versions: &[Version]) -> Result<(), S3Error>;
}

/// Every current key starting with `prefix`, across pages.
pub fn list_all(bucket: &dyn Bucket, prefix: &str) -> Result<Vec<String>, S3Error> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = bucket.list_page(prefix, after)?;
        out.extend(page.items);
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(out),
        }
    }
}

/// Every version and delete marker of the keys starting with `prefix`, across pages.
pub fn versions_all(bucket: &dyn Bucket, prefix: &str) -> Result<Vec<Version>, S3Error> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = bucket.versions_page(prefix, after)?;
        out.extend(page.items);
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(out),
        }
    }
}

// ---- the fake ---------------------------------------------------------------------------------

/// An in-process bucket with versioning on, for tests (rule 9). It behaves as S3 does where the
/// stores depend on it: a delete without a version id leaves a delete marker and keeps the old
/// versions, listings come in short pages, and a legal hold refuses permanent deletion.
#[derive(Debug)]
pub struct FakeBucket {
    /// Entries per listing page.
    page_size: usize,
    /// The bucket's contents.
    state: Mutex<FakeState>,
}

/// What a [`FakeBucket`] holds.
#[derive(Debug, Default)]
struct FakeState {
    /// Every key's versions, oldest first.
    keys: BTreeMap<String, Vec<FakeVersion>>,
    /// The number behind the next version id.
    next_version: u64,
}

/// One version in a [`FakeBucket`].
#[derive(Debug, Clone)]
struct FakeVersion {
    /// The version id.
    id: String,
    /// `None` for a delete marker.
    bytes: Option<Vec<u8>>,
    /// Whether an Object Lock legal hold protects it.
    legal_hold: bool,
}

impl Default for FakeBucket {
    fn default() -> Self {
        Self::new(2)
    }
}

impl FakeBucket {
    /// An empty bucket whose listings return `page_size` entries a page, so even small tests
    /// cross page boundaries.
    pub fn new(page_size: usize) -> Self {
        Self {
            page_size: page_size.max(1),
            state: Mutex::new(FakeState::default()),
        }
    }

    /// The contents, locked.
    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        // A test that panicked while holding the lock leaves the state as it was; keep using it.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Adds a delete marker on top of `key`, as an S3 delete without a version id does in a
    /// versioned bucket. Tests use it to leave old versions behind a deleted key.
    pub fn delete_current(&self, key: &str) {
        let mut state = self.state();
        state.next_version += 1;
        let id = format!("v{}", state.next_version);
        state
            .keys
            .entry(key.to_owned())
            .or_default()
            .push(FakeVersion {
                id,
                bytes: None,
                legal_hold: false,
            });
    }

    /// How many versions and delete markers the bucket holds under `prefix`.
    pub fn version_count(&self, prefix: &str) -> usize {
        self.state()
            .keys
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, v)| v.len())
            .sum()
    }

    /// Whether the newest version of `key` carries a legal hold.
    pub fn held(&self, key: &str) -> bool {
        self.state()
            .keys
            .get(key)
            .and_then(|v| v.last())
            .is_some_and(|v| v.legal_hold)
    }
}

impl Bucket for FakeBucket {
    fn put(&self, key: &str, bytes: &[u8], legal_hold: bool) -> Result<(), S3Error> {
        let mut state = self.state();
        state.next_version += 1;
        let id = format!("v{}", state.next_version);
        state
            .keys
            .entry(key.to_owned())
            .or_default()
            .push(FakeVersion {
                id,
                bytes: Some(bytes.to_vec()),
                legal_hold,
            });
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, S3Error> {
        Ok(self
            .state()
            .keys
            .get(key)
            .and_then(|v| v.last())
            .and_then(|v| v.bytes.clone()))
    }

    fn exists(&self, key: &str) -> Result<bool, S3Error> {
        Ok(self.get(key)?.is_some())
    }

    fn list_page(
        &self,
        prefix: &str,
        after: Option<String>,
    ) -> Result<Page<String, String>, S3Error> {
        let state = self.state();
        let mut current = state
            .keys
            .iter()
            .filter(|(k, v)| {
                k.starts_with(prefix)
                    && after.as_ref().is_none_or(|a| k.as_str() > a.as_str())
                    && v.last().is_some_and(|v| v.bytes.is_some())
            })
            .map(|(k, _)| k.clone());
        let items: Vec<String> = current.by_ref().take(self.page_size).collect();
        let next = if current.next().is_some() {
            items.last().cloned()
        } else {
            None
        };
        Ok(Page { items, next })
    }

    fn versions_page(
        &self,
        prefix: &str,
        after: Option<VersionCursor>,
    ) -> Result<Page<Version, VersionCursor>, S3Error> {
        let state = self.state();
        let mut all = state
            .keys
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .flat_map(|(k, versions)| {
                versions.iter().map(move |v| Version {
                    key: k.clone(),
                    version_id: v.id.clone(),
                    delete_marker: v.bytes.is_none(),
                })
            })
            .skip_while(|v| {
                after
                    .as_ref()
                    .is_some_and(|(k, id)| (&v.key, &v.version_id) != (k, id))
            })
            .skip(usize::from(after.is_some()));
        let items: Vec<Version> = all.by_ref().take(self.page_size).collect();
        let next = if all.next().is_some() {
            items.last().map(|v| (v.key.clone(), v.version_id.clone()))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    fn delete_versions(&self, versions: &[Version]) -> Result<(), S3Error> {
        if versions.len() > 1000 {
            return Err(S3Error::new("DeleteObjects", "", "more than 1,000 keys"));
        }
        let mut state = self.state();
        for v in versions {
            if let Some(stored) = state.keys.get(&v.key)
                && stored.iter().any(|s| s.id == v.version_id && s.legal_hold)
            {
                return Err(S3Error::new(
                    "DeleteObjects",
                    &v.key,
                    "AccessDenied: legal hold",
                ));
            }
        }
        for v in versions {
            if let Some(stored) = state.keys.get_mut(&v.key) {
                stored.retain(|s| s.id != v.version_id);
                if stored.is_empty() {
                    state.keys.remove(&v.key);
                }
            }
        }
        Ok(())
    }
}

// ---- the AWS client ---------------------------------------------------------------------------

/// The runtime every [`AwsBucket`] request runs on. It lives for the whole process, because a
/// runtime may not be dropped from async code.
fn runtime() -> Result<&'static tokio::runtime::Runtime, S3Error> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("wt-s3")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| S3Error::new("runtime", "", e))
}

/// Runs `fut` on the S3 runtime and waits for it. Inside a multi-threaded runtime the wait tells
/// tokio this thread is blocked, so its other tasks move elsewhere.
fn block<T: Send + 'static>(
    fut: impl Future<Output = Result<T, S3Error>> + Send + 'static,
) -> Result<T, S3Error> {
    let (tx, rx) = std::sync::mpsc::channel();
    runtime()?.spawn(async move {
        let _ = tx.send(fut.await);
    });
    let wait = move || {
        rx.recv()
            .map_err(|_| S3Error::new("runtime", "", "the request stopped before answering"))
    };
    let in_multi_thread = tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
    if in_multi_thread {
        tokio::task::block_in_place(wait)?
    } else {
        wait()?
    }
}

/// An S3-compatible server other than AWS, such as MinIO in tests, with fixed credentials.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Its URL, such as `http://127.0.0.1:9000`.
    pub url: String,
    /// The access key id.
    pub access_key: String,
    /// The secret key.
    pub secret_key: crate::config::Secret,
}

/// One bucket in S3. Credentials come from the standard chain: the ECS task role, the batch
/// host's instance profile, or the AWS profile on the PC (`AWS_PROFILE`).
#[derive(Debug, Clone)]
pub struct AwsBucket {
    /// The SDK client.
    client: aws_sdk_s3::Client,
    /// The bucket's name.
    bucket: String,
    /// Keys per listing page; `None` is S3's own 1,000.
    page_size: Option<i32>,
}

impl AwsBucket {
    /// A client for `bucket`. No request is made until the store is used.
    pub fn connect(bucket: &str, endpoint: Option<Endpoint>) -> Result<Self, S3Error> {
        let client = block(async move {
            use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
            let http = aws_smithy_http_client::Builder::new()
                .tls_provider(Provider::Rustls(CryptoMode::Ring))
                .build_https();
            // The loader resolves the region itself, so its lookups (the profile, the instance
            // metadata service) use this client too; the home region is the fallback below.
            let mut loader =
                aws_config::defaults(aws_config::BehaviorVersion::latest()).http_client(http);
            if let Some(e) = &endpoint {
                loader = loader
                    .region(aws_config::Region::new(HOME_REGION))
                    .endpoint_url(&e.url)
                    .credentials_provider(aws_sdk_s3::config::Credentials::new(
                        &e.access_key,
                        e.secret_key.expose(),
                        None,
                        None,
                        "wt-endpoint",
                    ));
            }
            let shared = loader.load().await;
            let mut config =
                aws_sdk_s3::config::Builder::from(&shared).force_path_style(endpoint.is_some());
            if shared.region().is_none() {
                config = config.region(aws_config::Region::new(HOME_REGION));
            }
            let config = config.build();
            Ok(aws_sdk_s3::Client::from_conf(config))
        })?;
        Ok(Self {
            client,
            bucket: bucket.to_owned(),
            page_size: None,
        })
    }

    /// Asks for `n` keys per listing page, so a test crosses page boundaries with a few keys.
    pub fn with_page_size(mut self, n: i32) -> Self {
        self.page_size = Some(n);
        self
    }

    /// The underlying client, for setup such as creating a test bucket.
    pub fn client(&self) -> &aws_sdk_s3::Client {
        &self.client
    }

    /// The bucket's name.
    pub fn name(&self) -> &str {
        &self.bucket
    }
}

/// The service's own message for a failed call, which the SDK's `Display` leaves out.
fn describe<E>(e: &aws_sdk_s3::error::SdkError<E>) -> String
where
    E: aws_sdk_s3::error::ProvideErrorMetadata + std::error::Error + 'static,
{
    use aws_sdk_s3::error::ProvideErrorMetadata as _;
    match (e.code(), e.message()) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_owned(),
        _ => aws_sdk_s3::error::DisplayErrorContext(e).to_string(),
    }
}

impl Bucket for AwsBucket {
    fn put(&self, key: &str, bytes: &[u8], legal_hold: bool) -> Result<(), S3Error> {
        let (client, bucket, key, body) = (
            self.client.clone(),
            self.bucket.clone(),
            key.to_owned(),
            bytes.to_vec(),
        );
        block(async move {
            let mut req = client
                .put_object()
                .bucket(bucket)
                .key(&key)
                .body(ByteStream::from(body));
            if legal_hold {
                req = req.object_lock_legal_hold_status(ObjectLockLegalHoldStatus::On);
            }
            req.send()
                .await
                .map(drop)
                .map_err(|e| S3Error::new("PutObject", &key, describe(&e)))
        })
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, S3Error> {
        let (client, bucket, key) = (self.client.clone(), self.bucket.clone(), key.to_owned());
        block(async move {
            match client.get_object().bucket(bucket).key(&key).send().await {
                Ok(out) => out
                    .body
                    .collect()
                    .await
                    .map(|b| Some(b.into_bytes().to_vec()))
                    .map_err(|e| S3Error::new("GetObject", &key, e)),
                Err(e) if e.as_service_error().is_some_and(|s| s.is_no_such_key()) => Ok(None),
                Err(e) => Err(S3Error::new("GetObject", &key, describe(&e))),
            }
        })
    }

    fn exists(&self, key: &str) -> Result<bool, S3Error> {
        let (client, bucket, key) = (self.client.clone(), self.bucket.clone(), key.to_owned());
        block(async move {
            match client.head_object().bucket(bucket).key(&key).send().await {
                Ok(_) => Ok(true),
                Err(e) if e.as_service_error().is_some_and(|s| s.is_not_found()) => Ok(false),
                Err(e) => Err(S3Error::new("HeadObject", &key, describe(&e))),
            }
        })
    }

    fn list_page(
        &self,
        prefix: &str,
        after: Option<String>,
    ) -> Result<Page<String, String>, S3Error> {
        let (client, bucket, prefix) =
            (self.client.clone(), self.bucket.clone(), prefix.to_owned());
        let page_size = self.page_size;
        block(async move {
            let out = client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(&prefix)
                .set_max_keys(page_size)
                .set_continuation_token(after)
                .send()
                .await
                .map_err(|e| S3Error::new("ListObjectsV2", &prefix, describe(&e)))?;
            let items = out
                .contents()
                .iter()
                .filter_map(|o| o.key().map(str::to_owned))
                .collect();
            let next = if out.is_truncated() == Some(true) {
                out.next_continuation_token().map(str::to_owned)
            } else {
                None
            };
            Ok(Page { items, next })
        })
    }

    fn versions_page(
        &self,
        prefix: &str,
        after: Option<VersionCursor>,
    ) -> Result<Page<Version, VersionCursor>, S3Error> {
        let (client, bucket, prefix) =
            (self.client.clone(), self.bucket.clone(), prefix.to_owned());
        let page_size = self.page_size;
        block(async move {
            let (key_marker, version_marker) = after.unzip();
            let out = client
                .list_object_versions()
                .bucket(bucket)
                .prefix(&prefix)
                .set_max_keys(page_size)
                .set_key_marker(key_marker)
                .set_version_id_marker(version_marker)
                .send()
                .await
                .map_err(|e| S3Error::new("ListObjectVersions", &prefix, describe(&e)))?;
            let version = |key: Option<&str>, id: Option<&str>, delete_marker| {
                key.map(|k| Version {
                    key: k.to_owned(),
                    version_id: id.unwrap_or("null").to_owned(),
                    delete_marker,
                })
            };
            let mut items: Vec<Version> = out
                .versions()
                .iter()
                .filter_map(|v| version(v.key(), v.version_id(), false))
                .chain(
                    out.delete_markers()
                        .iter()
                        .filter_map(|m| version(m.key(), m.version_id(), true)),
                )
                .collect();
            items.sort();
            let next = if out.is_truncated() == Some(true) {
                out.next_key_marker().map(|k| {
                    (
                        k.to_owned(),
                        out.next_version_id_marker().unwrap_or_default().to_owned(),
                    )
                })
            } else {
                None
            };
            Ok(Page { items, next })
        })
    }

    fn delete_versions(&self, versions: &[Version]) -> Result<(), S3Error> {
        if versions.is_empty() {
            return Ok(());
        }
        let objects = versions
            .iter()
            .map(|v| {
                ObjectIdentifier::builder()
                    .key(&v.key)
                    .version_id(&v.version_id)
                    .build()
                    .map_err(|e| S3Error::new("DeleteObjects", &v.key, e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let delete = Delete::builder()
            .set_objects(Some(objects))
            .quiet(true)
            .build()
            .map_err(|e| S3Error::new("DeleteObjects", "", e))?;
        let (client, bucket) = (self.client.clone(), self.bucket.clone());
        block(async move {
            let out = client
                .delete_objects()
                .bucket(bucket)
                .delete(delete)
                .send()
                .await
                .map_err(|e| S3Error::new("DeleteObjects", "", describe(&e)))?;
            match out.errors().first() {
                Some(err) => Err(S3Error::new(
                    "DeleteObjects",
                    err.key().unwrap_or_default(),
                    format!(
                        "{}: {}",
                        err.code().unwrap_or_default(),
                        err.message().unwrap_or_default()
                    ),
                )),
                None => Ok(()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_pages_listings_and_keeps_versions_behind_a_delete_marker() {
        let bucket = FakeBucket::new(2);
        for key in ["a/1", "a/2", "a/3", "b/1"] {
            bucket
                .put(key, b"x", false)
                .unwrap_or_else(|e| panic!("{e}"));
        }
        bucket
            .put("a/1", b"y", false)
            .unwrap_or_else(|e| panic!("{e}"));
        let first = bucket
            .list_page("a/", None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.items, ["a/1", "a/2"]);
        assert!(first.next.is_some());
        assert_eq!(
            list_all(&bucket, "a/").unwrap_or_default(),
            ["a/1", "a/2", "a/3"]
        );
        bucket.delete_current("a/2");
        assert_eq!(list_all(&bucket, "a/").unwrap_or_default(), ["a/1", "a/3"]);
        let versions = versions_all(&bucket, "a/").unwrap_or_default();
        assert_eq!(versions.len(), 5);
        assert_eq!(versions.iter().filter(|v| v.delete_marker).count(), 1);
        bucket
            .delete_versions(&versions)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bucket.version_count("a/"), 0);
        assert_eq!(bucket.version_count(""), 1);
    }

    #[test]
    fn the_fake_refuses_to_delete_a_held_version() {
        let bucket = FakeBucket::default();
        bucket
            .put("r/1", b"x", true)
            .unwrap_or_else(|e| panic!("{e}"));
        let versions = versions_all(&bucket, "r/").unwrap_or_default();
        assert!(bucket.delete_versions(&versions).is_err());
        assert!(bucket.held("r/1"));
    }
}
