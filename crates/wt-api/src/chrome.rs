//! What every page shares (Spec 06, Logo and icons, Design language): the header with its
//! menu, the footer, the inline wheel mark, the menu script and the icons.

use std::sync::LazyLock;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// The owner's themeable mark: the served copy of `docs/brand/logo-mark-themeable.svg` with its
/// provenance `<metadata>` block stripped. A test keeps the two in step.
const MARK_SOURCE: &str = include_str!("../static/brand/logo-mark-themeable.svg");
/// The favicon, a copy of `site/favicon.svg`, which carries no metadata.
const FAVICON: &str = include_str!("../static/brand/favicon.svg");
/// The menu script: Esc and the backdrop close the menu, and focus returns to its button.
const MENU_JS: &str = include_str!("../static/menu.js");

/// The disclosure line in the footer (Spec 06, Privacy modes, which replaces Logging, rule 5).
pub const LOGGING_LINE: &str = "You choose whether your questions are kept. Only totals are read, and one question is opened only if you flag it.";

/// The mark as an SVG `<symbol>` that the header and footer `<use>`. Its dots are
/// colored through `--logo-accent` by the stylesheet, since a presentation attribute cannot
/// read a custom property everywhere.
pub static MARK_SYMBOL: LazyLock<String> = LazyLock::new(|| symbol(MARK_SOURCE));

#[cfg(test)]
fn strip_metadata(svg: &str) -> String {
    match (svg.find("<metadata"), svg.find("</metadata>")) {
        (Some(start), Some(end)) if end > start => {
            format!("{}{}", &svg[..start], &svg[end + "</metadata>".len()..])
        }
        _ => svg.to_owned(),
    }
}

/// The logo's SVG as a `<symbol>` the pages reuse, with its accent colour moved into a class so the
/// stylesheet sets it.
fn symbol(svg: &str) -> String {
    let body = svg
        .find('>')
        .and_then(|open| svg.rfind("</svg>").map(|close| &svg[open + 1..close]))
        .unwrap_or_default();
    let body = body.replace("fill=\"var(--logo-accent, #B8730A)\"", "class=\"accent\"");
    format!("<symbol id=\"wt-mark\" viewBox=\"50 50 300 300\">{body}</symbol>")
}

/// Which header link is the current page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Current {
    /// Home.
    Ask,
    /// About.
    About,
    /// Rights and removal.
    Rights,
    /// The accessibility statement, linked from the footer only.
    Accessibility,
    /// Any other page.
    Other,
}

/// The header and footer's inputs.
#[derive(Debug, Clone)]
pub struct Chrome {
    /// The page's path, for the accessibility report link.
    pub path: String,
    /// The current page.
    pub current: Current,
    /// The CSRF token for the header's question field; `None` without a session, which hides
    /// the field.
    pub csrf: Option<String>,
}

impl Chrome {
    /// A page with a session.
    pub fn new(path: impl Into<String>, current: Current, csrf: Option<String>) -> Self {
        Self {
            path: path.into(),
            current,
            csrf,
        }
    }

    /// A page with no session and no path worth naming, such as an error page.
    pub fn bare() -> Self {
        Self::new("", Current::Other, None)
    }

    /// The inline symbol, rendered unescaped by the base template.
    pub fn mark(&self) -> &'static str {
        MARK_SYMBOL.as_str()
    }

    /// `aria-current` for a header link.
    pub fn is(&self, current: Current) -> bool {
        self.current == current
    }

    /// Whether the compact question field shows: on every page but home, with a session.
    pub fn header_question(&self) -> bool {
        self.csrf.is_some() && self.current != Current::Ask
    }

    /// The footer's link to the statement, carrying the page address for the report form.
    pub fn accessibility_href(&self) -> String {
        if self.path.is_empty() || self.path.starts_with("/accessibility") {
            "/accessibility".to_owned()
        } else {
            format!("/accessibility?page={}#report", encode(&self.path))
        }
    }

    /// The footer's disclosure line.
    pub fn logging_line(&self) -> &'static str {
        LOGGING_LINE
    }
}

/// Percent-encodes a query value.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `GET /static/menu.js`.
pub async fn menu_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        MENU_JS,
    )
}

/// `GET /static/brand/favicon.svg`.
pub async fn favicon_svg() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON,
    )
}

/// `GET /favicon.ico`, which browsers ask for on their own. There is no ICO file in the repo
/// yet (the owner's favicon set is not in `brand/source/`), so this answers 204 with no body
/// rather than a 404 page.
pub async fn favicon_ico() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "public, max-age=86400")],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_served_mark_has_no_metadata_and_the_source_keeps_it() {
        let original = include_str!("../../../docs/brand/logo-mark-themeable.svg");
        assert!(
            original.contains("<metadata"),
            "the delivered file is left alone"
        );
        let served =
            strip_metadata(original).replace(" xmlns:c2pa=\"http://c2pa.org/manifest\"", "");
        assert_eq!(served.trim_end(), MARK_SOURCE.trim_end());
        assert!(!MARK_SOURCE.contains("<metadata"));
        assert_eq!(FAVICON, include_str!("../../../site/favicon.svg"));
        assert!(!MARK_SYMBOL.contains("c2pa"));
        assert!(MARK_SYMBOL.starts_with("<symbol id=\"wt-mark\""));
        assert!(MARK_SYMBOL.contains("class=\"accent\""));
        assert!(!MARK_SYMBOL.contains("var(--logo-accent"));
        assert!(MARK_SYMBOL.contains("currentColor"));
        assert!(!FAVICON.contains("<metadata"));
    }

    #[test]
    fn the_report_link_carries_the_page() {
        let c = Chrome::new("/a/1 2", Current::Other, None);
        assert_eq!(
            c.accessibility_href(),
            "/accessibility?page=/a/1%202#report"
        );
        assert_eq!(Chrome::bare().accessibility_href(), "/accessibility");
    }
}
