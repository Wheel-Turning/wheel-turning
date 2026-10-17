//! The audit's checks on things outside the database and the object store (Spec 11): the PC's
//! nightly backup in S3 (PB1.3) and the crisis help links on the support page (PB3.4). Both make
//! network calls, so each sits behind a trait; the scheduled task on AWS uses the real ones and
//! tests use fakes.

use std::time::{Duration, SystemTime};

use anyhow::Context;
use async_trait::async_trait;
use wt_db::audit::{self, AuditReport, CheckResult};

/// What `wt audit` checks outside the database.
pub(crate) struct Outside<'a> {
    /// The PC backups bucket and its reader; `None` while no PC backup is kept (the check is off).
    pub backups: Option<(&'a dyn BackupStore, &'a str)>,
    /// The newest backup's age limit.
    pub backup_max_age: Duration,
    /// The link checker; `None` leaves the links unchecked, as everywhere but the AWS task.
    pub links: Option<&'a dyn LinkProbe>,
}

/// Reads the newest object under a prefix of a bucket.
#[async_trait]
pub(crate) trait BackupStore: Send + Sync {
    /// The newest object's key and time under `prefix`, if any.
    async fn newest(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> anyhow::Result<Option<(String, SystemTime)>>;
}

/// Asks whether a web address answers.
#[async_trait]
pub(crate) trait LinkProbe: Send + Sync {
    /// `Ok` when `url` answers with a success after redirects, otherwise why not.
    async fn answers(&self, url: &str) -> Result<(), String>;
}

/// The support page's crisis help text, as the site serves it.
const SUPPORT_TEXT: &str = include_str!("../../wt-api/templates/support_text.html");

/// Every web link on the support page. `tel:` and `sms:` links cannot be asked over the web and
/// are left out.
pub(crate) fn support_links() -> Vec<&'static str> {
    links_in(SUPPORT_TEXT)
}

/// The `https://` targets of every `href` in `html`, in order.
fn links_in(html: &str) -> Vec<&str> {
    html.split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(url, _)| url))
        .filter(|url| url.starts_with("https://"))
        .collect()
}

/// Adds the outside checks that are switched on to `report`.
pub(crate) async fn add_checks(
    report: &mut AuditReport,
    outside: &Outside<'_>,
) -> anyhow::Result<()> {
    if let Some((store, bucket)) = outside.backups {
        let newest = store
            .newest(bucket, audit::PC_BACKUP_PREFIX)
            .await
            .with_context(|| format!("reading the newest backup in {bucket}"))?;
        report.checks.push(audit::pc_backup_stale(
            newest,
            SystemTime::now(),
            outside.backup_max_age,
        ));
    }
    if let Some(probe) = outside.links {
        report
            .checks
            .push(probe_links(probe, &support_links()).await);
    }
    Ok(())
}

/// Asks each link in turn.
async fn probe_links(probe: &dyn LinkProbe, links: &[&str]) -> CheckResult {
    let mut probes = Vec::with_capacity(links.len());
    for url in links {
        probes.push(((*url).to_owned(), probe.answers(url).await));
    }
    audit::crisis_link_dead(probes)
}

/// The PC backups bucket in S3, through the task role and `AWS_REGION`.
pub(crate) struct S3Backups {
    client: aws_sdk_s3::Client,
}

impl S3Backups {
    /// A reader with credentials and region from the environment. Uses rustls on ring, as the
    /// web service's SES client does.
    pub(crate) async fn new() -> Self {
        use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .http_client(http)
            .load()
            .await;
        Self {
            client: aws_sdk_s3::Client::new(&config),
        }
    }
}

#[async_trait]
impl BackupStore for S3Backups {
    async fn newest(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> anyhow::Result<Option<(String, SystemTime)>> {
        let mut newest: Option<(String, SystemTime)> = None;
        let mut pages = self
            .client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .into_paginator()
            .send();
        while let Some(page) = pages.next().await {
            for object in page.context("listing backups")?.contents() {
                let (Some(key), Some(modified)) = (object.key(), object.last_modified()) else {
                    continue;
                };
                let at = SystemTime::try_from(*modified).context("reading an object's time")?;
                if newest.as_ref().is_none_or(|(_, t)| at > *t) {
                    newest = Some((key.to_owned(), at));
                }
            }
        }
        Ok(newest)
    }
}

/// Plain HTTPS requests with a short timeout.
pub(crate) struct HttpProbe {
    client: reqwest::Client,
}

impl HttpProbe {
    /// A probe that follows redirects and gives up after 15 seconds.
    pub(crate) fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("wheel-turning-audit")
            .build()
            .context("building the link checker")?;
        Ok(Self { client })
    }
}

#[async_trait]
impl LinkProbe for HttpProbe {
    async fn answers(&self, url: &str) -> Result<(), String> {
        match self.client.get(url).send().await {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => Err(format!("status {}", response.status().as_u16())),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Answers every address but the ones it is told are dead.
    struct FakeProbe(Vec<&'static str>);

    #[async_trait]
    impl LinkProbe for FakeProbe {
        async fn answers(&self, url: &str) -> Result<(), String> {
            if self.0.contains(&url) {
                Err("status 404".into())
            } else {
                Ok(())
            }
        }
    }

    /// Holds one backup of a given age.
    struct FakeBackups(Option<Duration>);

    #[async_trait]
    impl BackupStore for FakeBackups {
        async fn newest(
            &self,
            _bucket: &str,
            prefix: &str,
        ) -> anyhow::Result<Option<(String, SystemTime)>> {
            Ok(self
                .0
                .map(|age| (format!("{prefix}wt.dump"), SystemTime::now() - age)))
        }
    }

    fn report() -> AuditReport {
        AuditReport {
            checks: vec![],
            not_yet_checked: vec![],
        }
    }

    fn findings(report: &AuditReport, name: &str) -> i64 {
        report
            .checks
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .findings
    }

    #[test]
    fn the_support_page_links_are_its_web_addresses() {
        assert_eq!(
            support_links(),
            ["https://988lifeline.org/", "https://findahelpline.com/"]
        );
        assert_eq!(
            links_in(r#"<a href="tel:911">x</a> <a href="https://a.example/">y</a>"#),
            ["https://a.example/"]
        );
    }

    #[tokio::test]
    async fn the_link_check_reports_a_planted_dead_link() {
        let probe = FakeProbe(vec!["https://dead.example/"]);
        let check = probe_links(
            &probe,
            &["https://988lifeline.org/", "https://dead.example/"],
        )
        .await;
        assert_eq!(check.findings, 1);
        assert_eq!(check.examples, ["https://dead.example/: status 404"]);
    }

    #[tokio::test]
    async fn checks_run_only_when_switched_on() {
        let mut off = report();
        let outside = Outside {
            backups: None,
            backup_max_age: audit::PC_BACKUP_MAX_AGE,
            links: None,
        };
        add_checks(&mut off, &outside).await.unwrap();
        assert!(off.checks.is_empty());

        let stale = FakeBackups(Some(Duration::from_secs(40 * 3600)));
        let probe = FakeProbe(vec![]);
        let mut on = report();
        let outside = Outside {
            backups: Some((&stale, "backups")),
            backup_max_age: audit::PC_BACKUP_MAX_AGE,
            links: Some(&probe),
        };
        add_checks(&mut on, &outside).await.unwrap();
        assert_eq!(findings(&on, "pc_backup_stale"), 1);
        assert_eq!(findings(&on, "crisis_link_dead"), 0);

        let fresh = FakeBackups(Some(Duration::from_secs(3600)));
        let mut ok = report();
        let outside = Outside {
            backups: Some((&fresh, "backups")),
            ..outside
        };
        add_checks(&mut ok, &outside).await.unwrap();
        assert_eq!(findings(&ok, "pc_backup_stale"), 0);
    }
}
