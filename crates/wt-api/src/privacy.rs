//! Question privacy (Spec 06, Privacy modes for questions; Spec 09, Question privacy
//! controls): which mode a question is kept in, removing obvious personal details in `share`
//! mode, the switch, deleting all of a tester's questions, and the purge job.

use std::sync::LazyLock;
use std::time::Duration;

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use regex::Regex;
use serde::Deserialize;
use sqlx::PgPool;
use wt_db::privacy::{COUNT_ONLY, SHARE};

use crate::chrome::{Chrome, Current};
use crate::{AppState, Session, csrf_refused, page, server_error};

/// A privacy key that does not parse.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SettingsError(String);

/// `WT_PRIVACY_DEFAULT`, `WT_QUERY_RETENTION_DAYS` and `WT_COUNT_ONLY_TTL_HOURS`.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The mode for a tester who never chose: `share` while the demo is private.
    pub default_mode: &'static str,
    /// Days a kept question's text is stored, and a flagged question's in either mode.
    pub retention_days: u32,
    /// Hours a count-only question is stored so the tester can return to the answer.
    pub count_only_ttl_hours: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_mode: SHARE,
            retention_days: 180,
            count_only_ttl_hours: 24,
        }
    }
}

impl Settings {
    /// Reads the three keys, falling back to the defaults for any that is unset.
    pub fn from_env() -> Result<Self, SettingsError> {
        let d = Self::default();
        let default_mode = match std::env::var("WT_PRIVACY_DEFAULT").ok().as_deref() {
            None | Some("") | Some("share") => SHARE,
            Some("count_only") => COUNT_ONLY,
            Some(other) => {
                return Err(SettingsError(format!(
                    "WT_PRIVACY_DEFAULT is share or count_only, not {other}"
                )));
            }
        };
        let number = |key: &str, fallback: u32| -> Result<u32, SettingsError> {
            match std::env::var(key) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse()
                    .map_err(|_| SettingsError(format!("{key} is a whole number, not {v}"))),
                _ => Ok(fallback),
            }
        };
        Ok(Self {
            default_mode,
            retention_days: number("WT_QUERY_RETENTION_DAYS", d.retention_days)?,
            count_only_ttl_hours: number("WT_COUNT_ONLY_TTL_HOURS", d.count_only_ttl_hours)?,
        })
    }

    /// The mode a question is kept in: the tester's own choice; else `count_only` when the
    /// browser sends `Sec-GPC: 1`; else the default.
    pub fn effective(&self, choice: Option<&wt_db::privacy::Choice>, gpc: bool) -> &'static str {
        match choice {
            Some(c) if c.chosen => {
                if c.mode == COUNT_ONLY {
                    COUNT_ONLY
                } else {
                    SHARE
                }
            }
            _ if gpc => COUNT_ONLY,
            _ => self.default_mode,
        }
    }

    /// When a question asked now in `mode` expires.
    pub fn expires_at(&self, mode: &str) -> chrono::DateTime<chrono::Utc> {
        let now = chrono::Utc::now();
        if mode == COUNT_ONLY {
            now + chrono::Duration::hours(i64::from(self.count_only_ttl_hours))
        } else {
            now + chrono::Duration::days(i64::from(self.retention_days))
        }
    }
}

/// Whether the request carries `Sec-GPC: 1`.
pub fn gpc(headers: &HeaderMap) -> bool {
    headers
        .get("sec-gpc")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "1")
}

/// What replaces a removed detail.
pub const REMOVED: &str = "[removed]";

/// The patterns [`scrub`] removes, in order (Spec 06, Privacy modes, rule 1).
static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Email addresses.
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+",
        // Web addresses: with a scheme, starting www., or a bare domain under a common ending.
        r"(?i)\b(?:https?://|www\.)\S+",
        r"(?i)\b[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|org|net|edu|gov|io|co|us|uk|ca|au|de|fr|info|me|app|dev|ly)\b(?:/\S*)?",
        // Phone numbers: an optional country code, then 3, 3 and 4 digits with separators.
        r"(?:\+\d{1,3}[\s.-]?)?(?:\(\d{3}\)|\b\d{3})[\s.-]?\d{3}[\s.-]?\d{4}\b",
        // Any run of nine or more digits.
        r"\d{9,}",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Removes email addresses, phone numbers, web addresses and runs of nine or more digits
/// (Spec 06, Privacy modes, rule 1).
pub fn scrub(text: &str) -> String {
    PATTERNS.iter().fold(text.to_owned(), |t, re| {
        re.replace_all(&t, REMOVED).into_owned()
    })
}

/// The switch and its help text, for home and About.
pub struct Switch {
    /// Whether "Keep my questions" is checked.
    pub keep: bool,
    /// The help text for the current state.
    pub help: &'static str,
    /// Where the form returns to.
    pub back: &'static str,
}

/// Help text when keeping is on.
pub const HELP_KEEP: &str = "Your question is saved for up to 180 days with personal details such as emails and phone numbers removed. Totals by topic are read. Your question is opened only if you flag it.";
/// Help text when keeping is off.
pub const HELP_COUNT: &str = "Your question is kept for 24 hours so you can come back to your answer, then deleted. Only counts remain.";

/// The switch as it stands for this tester and request.
pub async fn switch(
    pool: &PgPool,
    settings: &Settings,
    s: &Session,
    headers: &HeaderMap,
    back: &'static str,
) -> Result<Switch, wt_db::DbError> {
    let choice = wt_db::privacy::choice(pool, s.tester).await?;
    let keep = settings.effective(choice.as_ref(), gpc(headers)) == SHARE;
    Ok(Switch {
        keep,
        help: if keep { HELP_KEEP } else { HELP_COUNT },
        back,
    })
}

/// `POST /about/privacy`.
#[derive(Debug, Deserialize)]
pub struct PrivacyForm {
    csrf: String,
    /// Present when the box is checked.
    keep: Option<String>,
    /// `home` or `about`.
    back: Option<String>,
}

/// Saves the switch; applies to the next question.
pub async fn save(
    State(state): State<AppState>,
    s: Session,
    Form(f): Form<PrivacyForm>,
) -> Response {
    if !s.csrf_ok(&state.site.keys, &f.csrf) {
        return csrf_refused();
    }
    let mode = if f.keep.is_some() { SHARE } else { COUNT_ONLY };
    if let Err(e) = wt_db::privacy::choose(&state.pool, s.tester, mode).await {
        return server_error("saving the privacy choice", &e);
    }
    let to = match f.back.as_deref() {
        Some("home") => "/?saved=1#privacy",
        _ => "/about?saved=1#privacy",
    };
    Redirect::to(to).into_response()
}

/// The page that confirms "Delete my data".
#[derive(Template)]
#[template(path = "delete.html")]
struct ConfirmPage {
    chrome: Chrome,
    csrf: String,
}

/// `POST /about/delete-my-data`.
#[derive(Debug, Deserialize)]
pub struct DeleteForm {
    csrf: String,
    /// `yes` once the tester confirmed.
    confirm: Option<String>,
}

/// Asks for confirmation, then deletes every question, answer, feedback row and dana event
/// that belongs to the tester.
pub async fn delete(
    State(state): State<AppState>,
    s: Session,
    Form(f): Form<DeleteForm>,
) -> Response {
    if !s.csrf_ok(&state.site.keys, &f.csrf) {
        return csrf_refused();
    }
    if f.confirm.as_deref() != Some("yes") {
        let csrf = s.csrf(&state.site.keys);
        return page(
            StatusCode::OK,
            &ConfirmPage {
                chrome: Chrome::new("/about/delete-my-data", Current::Other, Some(csrf.clone())),
                csrf,
            },
        );
    }
    match wt_db::privacy::delete_questions(&state.pool, s.tester).await {
        Ok(n) => {
            tracing::info!(questions = n, "a tester deleted their questions");
            Redirect::to("/about?deleted=1#privacy").into_response()
        }
        Err(e) => server_error("deleting a tester's questions", &e),
    }
}

/// How long a deferred summary may stay pending before the purge marks it lost (Spec 11,
/// PB2.4). Writing one takes at most four model calls of 20 seconds each.
pub const SUMMARY_LOST_AFTER: Duration = Duration::from_secs(600);

/// The purge, every hour from start-up. Expiry is to the hour; Spec 06 asks for at least
/// nightly. It also marks deferred summaries still pending after [`SUMMARY_LOST_AFTER`] as
/// lost, so no answer stays pending (Spec 11, PB2.4).
pub async fn purge_loop(pool: PgPool, retention_days: u32) {
    let mut every = tokio::time::interval(Duration::from_secs(3600));
    loop {
        every.tick().await;
        match wt_db::privacy::purge(&pool, retention_days).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(questions = n, "purged expired question text"),
            Err(e) => tracing::warn!(error = %e, "purging expired question text"),
        }
        settle_lost_summaries(&pool).await;
    }
}

/// Marks deferred summaries pending past [`SUMMARY_LOST_AFTER`] as `deferred_lost`, with one
/// `summary_settled` event each.
pub async fn settle_lost_summaries(pool: &PgPool) {
    match wt_db::answer::settle_lost_summaries(pool, SUMMARY_LOST_AFTER).await {
        Ok(lost) => {
            for answer in lost {
                crate::ask::settled(answer, wt_core::answer::SummaryStatus::DeferredLost);
            }
        }
        Err(e) => tracing::warn!(error = %e, "marking lost summaries"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wt_db::privacy::Choice;

    #[test]
    fn each_kind_of_personal_detail_is_removed() {
        let cases = [
            (
                "write to me at jo.doe+zen@example.org please",
                "write to me at [removed] please",
            ),
            ("call 415-555-0123 tonight", "call [removed] tonight"),
            ("call (415) 555 0123", "call [removed]"),
            ("or +1 415.555.0123", "or [removed]"),
            (
                "see https://example.org/x?y=1 for it",
                "see [removed] for it",
            ),
            ("see www.sangha.org", "see [removed]"),
            ("my site zendo.com/about", "my site [removed]"),
            ("account 123456789012", "account [removed]"),
        ];
        for (input, want) in cases {
            assert_eq!(scrub(input), want, "{input}");
        }
        let plain = "I sat for 45 minutes in 2024 and my mind wandered 100 times.";
        assert_eq!(scrub(plain), plain, "ordinary numbers stay");
        assert_eq!(scrub("12345678"), "12345678", "eight digits stay");
    }

    #[test]
    fn a_choice_beats_gpc_and_gpc_beats_the_default() {
        let s = Settings::default();
        assert_eq!(s.effective(None, false), SHARE);
        assert_eq!(s.effective(None, true), COUNT_ONLY);
        let unchosen = Choice {
            mode: SHARE.into(),
            chosen: false,
        };
        assert_eq!(s.effective(Some(&unchosen), true), COUNT_ONLY);
        let kept = Choice {
            mode: SHARE.into(),
            chosen: true,
        };
        assert_eq!(s.effective(Some(&kept), true), SHARE);
        let counted = Choice {
            mode: COUNT_ONLY.into(),
            chosen: true,
        };
        assert_eq!(s.effective(Some(&counted), false), COUNT_ONLY);
        let public = Settings {
            default_mode: COUNT_ONLY,
            ..Settings::default()
        };
        assert_eq!(public.effective(None, false), COUNT_ONLY);
    }
}
