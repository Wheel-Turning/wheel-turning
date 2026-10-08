//! The document fetcher for URL ingestion (Spec 08, Ingesting from URLs, fetching rules): the
//! project's `User-Agent`, one connection and at least `fetch_delay_ms` between requests per
//! host, `robots.txt` obeyed with no override, only `http` and `https`, every address checked
//! after DNS resolution and again on each of at most 5 redirects so private, loopback and
//! link-local ranges are never reached, a 30-second timeout, a size cap, no cookies, and audio
//! and video refused. Every URL is cleared against the exclusions first.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;
use url::Url;
use wt_core::records::sha256_hex;

use crate::http::{self, Exclusions, USER_AGENT};
use crate::robots::Robots;
use crate::source::IngestError;

/// How the fetcher behaves.
#[derive(Debug, Clone)]
pub struct WebSettings {
    /// The least time between two requests to one host.
    pub fetch_delay: Duration,
    /// The largest body accepted.
    pub max_download_bytes: u64,
    /// Per request.
    pub timeout: Duration,
    /// Redirects followed.
    pub max_redirects: usize,
    /// Addresses allowed although private or loopback: only a test's local server.
    pub allow_addresses: Vec<IpAddr>,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            fetch_delay: Duration::from_millis(1000),
            max_download_bytes: 200 * 1024 * 1024,
            timeout: Duration::from_secs(30),
            max_redirects: 5,
            allow_addresses: Vec::new(),
        }
    }
}

/// Whether an address is on the public internet: not loopback, private, link-local, shared,
/// unspecified, multicast or reserved, in IPv4 or IPv6 (and IPv4 inside IPv6).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// A body fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fetched {
    /// The address asked for.
    pub url: String,
    /// Where it ended after redirects.
    pub final_url: String,
    /// The status.
    pub status: u16,
    /// The content type the server named.
    pub content_type: Option<String>,
    /// For conditional refetches.
    pub etag: Option<String>,
    /// For conditional refetches.
    pub last_modified: Option<String>,
    /// SHA-256 of the body.
    pub sha256: String,
    /// The body.
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

/// What a `HEAD` request learned: all the importer asks of a URL no evidence covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Head {
    /// Where it ended after redirects.
    pub final_url: String,
    /// The status.
    pub status: u16,
    /// The content type.
    pub content_type: Option<String>,
    /// The size, if the server said.
    pub length: Option<u64>,
}

/// A GET's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    /// A body.
    Body(Fetched),
    /// The conditional request found it unchanged.
    NotModified,
}

/// Validators from an earlier fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    /// `ETag`.
    pub etag: Option<String>,
    /// `Last-Modified`.
    pub last_modified: Option<String>,
}

/// The fetcher. One per run, shared by every task, so its per-host slots hold.
pub struct WebFetcher {
    settings: WebSettings,
    exclusions: Exclusions,
    hosts: Mutex<HashMap<String, Arc<Mutex<Option<Instant>>>>>,
    robots: Mutex<HashMap<String, Robots>>,
}

fn http_err(url: &Url, message: impl ToString) -> IngestError {
    IngestError::Http {
        url: url.to_string(),
        message: message.to_string(),
    }
}

/// Audio and video go through Spec 03's path, never this one.
pub fn is_media(content_type: &str) -> bool {
    let t = content_type.trim().to_ascii_lowercase();
    t.starts_with("audio/") || t.starts_with("video/")
}

impl WebFetcher {
    /// A fetcher clearing every URL against `exclusions`.
    pub fn new(settings: WebSettings, exclusions: Exclusions) -> Self {
        Self {
            settings,
            exclusions,
            hosts: Mutex::new(HashMap::new()),
            robots: Mutex::new(HashMap::new()),
        }
    }

    async fn slot(&self, host: &str) -> Arc<Mutex<Option<Instant>>> {
        self.hosts
            .lock()
            .await
            .entry(host.to_owned())
            .or_default()
            .clone()
    }

    /// Resolves the host and refuses any address that is not public, so the request goes to
    /// the checked addresses only.
    async fn checked_client(&self, url: &Url) -> Result<reqwest::Client, IngestError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(http_err(url, "only http and https are fetched"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| http_err(url, "the address has no host"))?
            .to_owned();
        let port = url.port_or_known_default().unwrap_or(80);
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| http_err(url, format!("resolving {host}: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(http_err(url, format!("{host} resolves to nothing")));
        }
        if let Some(bad) = addrs
            .iter()
            .find(|a| !is_public(a.ip()) && !self.settings.allow_addresses.contains(&a.ip()))
        {
            return Err(http_err(
                url,
                format!(
                    "{host} resolves to {}, which is not a public address",
                    bad.ip()
                ),
            ));
        }
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(self.settings.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| http_err(url, e))
    }

    /// Clears a URL against the exclusions and its host's `robots.txt`, which is fetched once
    /// per host and kept. A `robots.txt` that is missing, or answers with any 4xx, means no
    /// rules (RFC 9309); a 5xx refuses the host.
    async fn cleared(
        &self,
        url: &Url,
        client: &reqwest::Client,
    ) -> Result<http::FetchTarget, IngestError> {
        let target = self.exclusions.clear(url)?;
        let origin = url.origin().ascii_serialization();
        // Under the host's slot, so concurrent first requests read robots.txt once.
        let slot = self.slot(&origin).await;
        let mut last = slot.lock().await;
        let known = self.robots.lock().await.get(&origin).cloned();
        let robots = match known {
            Some(r) => r,
            None => {
                let mut robots_url = url.clone();
                robots_url.set_path("/robots.txt");
                robots_url.set_query(None);
                robots_url.set_fragment(None);
                self.wait(&mut last).await;
                let response = client.get(robots_url.clone()).send().await;
                *last = Some(Instant::now());
                let response = response.map_err(|e| http_err(&robots_url, e.without_url()))?;
                let robots = match response.status().as_u16() {
                    200..=299 => {
                        Robots::parse(&response.text().await.unwrap_or_default(), USER_AGENT)
                    }
                    400..=499 => Robots::default(),
                    status => {
                        return Err(IngestError::Status {
                            url: robots_url.to_string(),
                            status,
                        });
                    }
                };
                self.robots.lock().await.insert(origin, robots.clone());
                robots
            }
        };
        drop(last);
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        if !robots.allows(&path) {
            return Err(IngestError::RobotsDisallowed(url.to_string()));
        }
        Ok(target)
    }

    async fn wait(&self, last: &mut Option<Instant>) {
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < self.settings.fetch_delay {
                tokio::time::sleep(self.settings.fetch_delay - since).await;
            }
        }
    }

    /// Sends one request per hop under the host's slot, following redirects by hand so each
    /// hop is checked again.
    async fn send(
        &self,
        start: &Url,
        head: bool,
        validators: &Validators,
    ) -> Result<(Url, reqwest::Response), IngestError> {
        let mut url = start.clone();
        for _ in 0..=self.settings.max_redirects {
            let client = self.checked_client(&url).await?;
            let target = self.cleared(&url, &client).await?;
            let slot = self.slot(&url.origin().ascii_serialization()).await;
            let mut last = slot.lock().await;
            self.wait(&mut last).await;
            let mut request = if head {
                client.head(target.url().clone())
            } else {
                client.get(target.url().clone())
            };
            if let Some(e) = &validators.etag {
                request = request.header("If-None-Match", e);
            }
            if let Some(m) = &validators.last_modified {
                request = request.header("If-Modified-Since", m);
            }
            let response = request
                .send()
                .await
                .map_err(|e| http_err(&url, e.without_url()));
            *last = Some(Instant::now());
            drop(last);
            let response = response?;
            if response.status().is_redirection() && response.status().as_u16() != 304 {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| http_err(&url, "a redirect without a location"))?;
                url = url
                    .join(location)
                    .map_err(|e| http_err(&url, format!("redirect to {location:?}: {e}")))?;
                continue;
            }
            return Ok((url, response));
        }
        Err(http_err(
            start,
            format!("more than {} redirects", self.settings.max_redirects),
        ))
    }

    /// A `HEAD` request only: the type and size of a URL no evidence covers.
    pub async fn head(&self, url: &Url) -> Result<Head, IngestError> {
        let (final_url, response) = self.send(url, true, &Validators::default()).await?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        Ok(Head {
            final_url: final_url.to_string(),
            status: response.status().as_u16(),
            content_type: header("content-type"),
            length: header("content-length").and_then(|l| l.parse().ok()),
        })
    }

    /// A GET, conditional when validators are given, with the body capped at the size limit
    /// and audio and video refused before the body is read.
    pub async fn get(&self, url: &Url, validators: &Validators) -> Result<Got, IngestError> {
        let (final_url, response) = self.send(url, false, validators).await?;
        if response.status().as_u16() == 304 {
            return Ok(Got::NotModified);
        }
        let response = http::ok(&final_url, response)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header("content-type");
        if let Some(t) = &content_type
            && is_media(t)
        {
            return Err(http_err(
                &final_url,
                format!("{t} is audio or video; talks go through Spec 03's path"),
            ));
        }
        let max = self.settings.max_download_bytes;
        if response.content_length().is_some_and(|l| l > max) {
            return Err(http_err(&final_url, format!("larger than {max} bytes")));
        }
        let (etag, last_modified) = (header("etag"), header("last-modified"));
        let status = response.status().as_u16();
        let mut bytes: Vec<u8> = Vec::new();
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| http_err(&final_url, e.without_url()))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > max {
                return Err(http_err(&final_url, format!("larger than {max} bytes")));
            }
        }
        Ok(Got::Body(Fetched {
            url: url.to_string(),
            final_url: final_url.to_string(),
            status,
            content_type,
            etag,
            last_modified,
            sha256: sha256_hex(&bytes),
            bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(blocked.parse().unwrap()), "{blocked}");
        }
        for open in ["93.184.216.34", "2606:2800:220:1::1"] {
            assert!(is_public(open.parse().unwrap()), "{open}");
        }
    }

    #[test]
    fn audio_and_video_are_media() {
        assert!(is_media("audio/mpeg"));
        assert!(is_media("Video/MP4; codecs=x"));
        assert!(!is_media("application/pdf"));
    }
}
