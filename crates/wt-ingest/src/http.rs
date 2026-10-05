//! What every outbound request shares: the identifying `User-Agent`, the client settings, and
//! the `robots.txt` check that runs before reading any page.

use std::time::Duration;

use url::Url;

use crate::robots::Robots;
use crate::source::IngestError;

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
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| IngestError::Http {
            url: String::new(),
            message: e.to_string(),
        })
}

/// Sends a GET and returns the response, whatever its status.
pub async fn get(client: &reqwest::Client, url: &Url) -> Result<reqwest::Response, IngestError> {
    client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| IngestError::Http {
            url: url.to_string(),
            message: e.without_url().to_string(),
        })
}

/// Sends a GET and returns the response if its status is a success.
pub async fn get_ok(client: &reqwest::Client, url: &Url) -> Result<reqwest::Response, IngestError> {
    let response = get(client, url).await?;
    if !response.status().is_success() {
        return Err(IngestError::Status {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }
    Ok(response)
}

/// Reads a response body as text.
pub async fn text(url: &Url, response: reqwest::Response) -> Result<String, IngestError> {
    response.text().await.map_err(|e| IngestError::Http {
        url: url.to_string(),
        message: e.without_url().to_string(),
    })
}

/// Checks the host's `robots.txt` for `url` and refuses if it disallows the path. A missing
/// `robots.txt` means no rules.
pub async fn check_robots(client: &reqwest::Client, url: &Url) -> Result<(), IngestError> {
    let mut robots_url = url.clone();
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    let response = get(client, &robots_url).await?;
    let robots = match response.status().as_u16() {
        200..=299 => Robots::parse(&response.text().await.unwrap_or_default(), USER_AGENT),
        404 | 410 => Robots::default(),
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
