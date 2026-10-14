//! What every outbound request shares: the identifying `User-Agent`, the client settings, the
//! exclusion check, and the `robots.txt` check that runs before reading any page.
//!
//! The fetch functions take a [`FetchTarget`], which only [`Exclusions::clear`] can build, so
//! an address an active exclusion covers can never be requested (Spec 08, Rights lifecycle).

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgConnection;
use url::Url;
use wt_core::Secret;
use wt_db::rights::{self, Exclusion, RightsError};

use crate::robots::Robots;
use crate::source::IngestError;

/// The active exclusions, loaded once per run and checked before every request.
#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    rules: Arc<Vec<Exclusion>>,
}

impl Exclusions {
    /// Checks against these rules. An empty list excludes nothing.
    pub fn new(rules: Vec<Exclusion>) -> Self {
        Self {
            rules: Arc::new(rules),
        }
    }

    /// Every active exclusion in the database.
    pub async fn load(conn: &mut PgConnection) -> Result<Self, RightsError> {
        Ok(Self::new(rights::active_exclusions(conn).await?))
    }

    /// The only way to get a [`FetchTarget`]: refuses an address an active exclusion covers,
    /// citing the exclusion.
    pub fn clear(&self, url: &Url) -> Result<FetchTarget, IngestError> {
        match rights::exclusion_for_url(&self.rules, url.as_str()) {
            Some(x) => Err(IngestError::Excluded {
                url: url.to_string(),
                exclusion: x.id.to_string(),
                reason: x.reason.clone(),
            }),
            None => Ok(FetchTarget(url.clone())),
        }
    }
}

/// An address checked against the active exclusions. It has no public constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchTarget(Url);

impl FetchTarget {
    /// The address.
    pub fn url(&self) -> &Url {
        &self.0
    }
}

/// Names the project and where to reach it (Spec 03, Licensed sources, item 5).
pub const USER_AGENT: &str = concat!(
    "WheelTurning/",
    env!("CARGO_PKG_VERSION"),
    " (+https://demo.wheelturning.org)"
);

/// The shortest wait between two requests to one host.
pub const MIN_DELAY: Duration = Duration::from_secs(1);

/// A client with the project's `User-Agent` and a 30-second timeout.
pub fn client() -> Result<reqwest::Client, IngestError> {
    client_with_timeout(Duration::from_secs(30))
}

/// A client with the project's `User-Agent` and the given timeout, for long downloads.
pub fn client_with_timeout(timeout: Duration) -> Result<reqwest::Client, IngestError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(|e| IngestError::Http {
            url: String::new(),
            message: e.to_string(),
        })
}

/// Sends a GET and returns the response, whatever its status.
pub async fn get(
    client: &reqwest::Client,
    target: &FetchTarget,
) -> Result<reqwest::Response, IngestError> {
    send(client.get(target.url().clone()), target.url()).await
}

/// Sends a GET with a bearer token and an `Accept` header, as the Vimeo API wants.
pub async fn get_authorized(
    client: &reqwest::Client,
    target: &FetchTarget,
    token: &Secret,
    accept: &str,
) -> Result<reqwest::Response, IngestError> {
    let request = client
        .get(target.url().clone())
        .bearer_auth(token.expose())
        .header("Accept", accept);
    send(request, target.url()).await
}

/// Sends a request, naming the URL but not the client's own message about it in the error.
async fn send(
    request: reqwest::RequestBuilder,
    url: &Url,
) -> Result<reqwest::Response, IngestError> {
    request.send().await.map_err(|e| IngestError::Http {
        url: url.to_string(),
        message: e.without_url().to_string(),
    })
}

/// Refuses a response whose status is not a success.
pub fn ok(url: &Url, response: reqwest::Response) -> Result<reqwest::Response, IngestError> {
    if !response.status().is_success() {
        return Err(IngestError::Status {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }
    Ok(response)
}

/// Sends a GET and returns the response if its status is a success.
pub async fn get_ok(
    client: &reqwest::Client,
    target: &FetchTarget,
) -> Result<reqwest::Response, IngestError> {
    ok(target.url(), get(client, target).await?)
}

/// Reads a response body as text.
pub async fn text(url: &Url, response: reqwest::Response) -> Result<String, IngestError> {
    response.text().await.map_err(|e| IngestError::Http {
        url: url.to_string(),
        message: e.without_url().to_string(),
    })
}

/// Checks the host's `robots.txt` for `url` and refuses if it disallows the path. As RFC 9309
/// (2.3.1.3) says, a `robots.txt` the host answers with any 4xx status is unavailable and means
/// no rules; a 5xx answer is refused, since the host may be unreachable. Azure blob storage,
/// which serves SSZ's podcast audio, answers 400 (October 7, 2026).
pub async fn check_robots(
    client: &reqwest::Client,
    target: &FetchTarget,
) -> Result<(), IngestError> {
    let url = target.url();
    // The host's own robots.txt, cleared with the target since it shares the host.
    let mut robots_url = url.clone();
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    let response = send(client.get(robots_url.clone()), &robots_url).await?;
    let robots = match response.status().as_u16() {
        200..=299 => Robots::parse(&response.text().await.unwrap_or_default(), USER_AGENT),
        400..=499 => Robots::default(),
        status => {
            return Err(IngestError::Status {
                url: robots_url.to_string(),
                status,
            });
        }
    };
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    if robots.allows(&path) {
        Ok(())
    } else {
        Err(IngestError::RobotsDisallowed(url.to_string()))
    }
}

/// Parses an address, mapping the error.
pub fn parse_url(text: &str) -> Result<Url, IngestError> {
    Url::parse(text).map_err(|e| IngestError::Parse {
        url: text.to_owned(),
        message: e.to_string(),
    })
}
