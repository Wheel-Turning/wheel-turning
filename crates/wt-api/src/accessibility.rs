//! `GET /accessibility` and `POST /accessibility/report` (Spec 06, Accessibility standard,
//! rules 5 and 6; Spec 09, Accessibility statement). The statement is generated from
//! `docs/accessibility/wcag3-register.md`, so it never claims more than the register shows.

use std::sync::LazyLock;

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use wt_db::public_forms::{self, NewReport};

use crate::chrome::{Chrome, Current};
use crate::{AppState, ClientAddress, page, server_error, too_many};

/// The register, as built into the binary.
pub const REGISTER: &str = include_str!("../../../docs/accessibility/wcag3-register.md");

/// One requirement row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Its number in the draft, such as `2.2.1`.
    pub id: String,
    /// Its name.
    pub name: String,
    /// core, supplemental or assertion.
    pub kind: String,
    /// met, partly met, not met or not applicable.
    pub status: String,
    /// How it is met, or what is missing.
    pub how: String,
}

/// The register's header lines and rows.
#[derive(Debug, Clone, Default)]
pub struct Register {
    /// `Draft:`, an ISO date.
    pub draft: String,
    /// `Draft URL:`.
    pub draft_url: String,
    /// `Last evaluated:`, an ISO date.
    pub last_evaluated: String,
    /// The requirement rows, in order.
    pub rows: Vec<Row>,
}

/// The four statuses, in the order the statement lists them.
pub const STATUSES: [&str; 4] = ["met", "partly met", "not met", "not applicable"];

/// Reads the register's `Draft:`, `Draft URL:` and `Last evaluated:` lines and every row of
/// the Requirements table.
pub fn parse(text: &str) -> Register {
    let mut r = Register::default();
    let mut in_requirements = false;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Draft: ") {
            r.draft = v.trim().to_owned();
        } else if let Some(v) = line.strip_prefix("Draft URL: ") {
            r.draft_url = v.trim().to_owned();
        } else if let Some(v) = line.strip_prefix("Last evaluated: ") {
            r.last_evaluated = v.trim().to_owned();
        } else if line.starts_with("## ") {
            in_requirements = line.trim() == "## Requirements";
        } else if in_requirements && line.starts_with("| 2.") {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            if let [id, name, kind, status, how, ..] = cells.as_slice() {
                r.rows.push(Row {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    kind: (*kind).to_owned(),
                    status: (*status).to_owned(),
                    how: how.replace('`', ""),
                });
            }
        }
    }
    r
}

/// The register this binary was built with.
pub static BUILT: LazyLock<Register> = LazyLock::new(|| parse(REGISTER));

fn long_date(iso: &str) -> String {
    chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|_| iso.to_owned())
}

/// One status and the requirements that have it.
pub struct Group {
    /// The status.
    pub status: &'static str,
    /// Its heading.
    pub heading: &'static str,
    /// Its rows.
    pub rows: Vec<Row>,
}

/// The register's rows grouped by status, in [`STATUSES`] order.
pub fn groups(register: &Register) -> Vec<Group> {
    let heading = |s: &str| match s {
        "met" => "Met",
        "partly met" => "Partly met",
        "not met" => "Not met",
        _ => "Not applicable",
    };
    STATUSES
        .iter()
        .map(|status| Group {
            status,
            heading: heading(status),
            rows: register
                .rows
                .iter()
                .filter(|r| r.status == *status)
                .cloned()
                .collect(),
        })
        .collect()
}

/// What the report form holds.
#[derive(Debug, Default, Deserialize)]
pub struct ReportForm {
    #[serde(default)]
    page: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    assistive_tech: String,
    #[serde(default)]
    contact: String,
    /// The honeypot.
    #[serde(default)]
    website: String,
}

#[derive(Template)]
#[template(path = "accessibility.html")]
struct StatementPage {
    chrome: Chrome,
    draft: String,
    draft_url: String,
    last_evaluated: String,
    groups: Vec<Group>,
    gaps: Vec<Row>,
    form: ReportForm,
    sent: bool,
    description_error: Option<&'static str>,
}

/// `GET /accessibility` parameters: `page` from the footer link, `sent` after a report.
#[derive(Debug, Deserialize)]
pub struct StatementQuery {
    page: Option<String>,
    sent: Option<u8>,
}

fn render(
    status: StatusCode,
    form: ReportForm,
    sent: bool,
    error: Option<&'static str>,
) -> Response {
    let r = &*BUILT;
    let groups = groups(r);
    let gaps = r
        .rows
        .iter()
        .filter(|row| row.status == "partly met" || row.status == "not met")
        .cloned()
        .collect();
    page(
        status,
        &StatementPage {
            chrome: Chrome::new("/accessibility", Current::Accessibility, None),
            draft: long_date(&r.draft),
            draft_url: r.draft_url.clone(),
            last_evaluated: long_date(&r.last_evaluated),
            groups,
            gaps,
            form,
            sent,
            description_error: error,
        },
    )
}

/// `GET /accessibility`.
pub async fn statement(Query(q): Query<StatementQuery>) -> Response {
    let form = ReportForm {
        page: q
            .page
            .filter(|p| p.starts_with('/') && p.len() <= 500)
            .unwrap_or_default(),
        ..ReportForm::default()
    };
    render(StatusCode::OK, form, q.sent.is_some(), None)
}

fn optional(s: &str, max: usize) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then(|| {
        let end = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
        &s[..end]
    })
}

/// `POST /accessibility/report`: stores a report, at most 5 an hour from one address, with
/// the same honeypot as the rights form.
pub async fn report(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    Form(form): Form<ReportForm>,
) -> Response {
    if let Err(wait) = state.site.limits.public_forms.check(&address) {
        return too_many(wait, "You have sent several reports this hour.");
    }
    if !form.website.is_empty() {
        tracing::info!("accessibility report: honeypot filled, nothing stored");
        return Redirect::to("/accessibility?sent=1#sent").into_response();
    }
    let chars = form.description.trim().chars().count();
    if !(5..=4000).contains(&chars) {
        return render(
            StatusCode::BAD_REQUEST,
            form,
            false,
            Some("Please describe what happened in 5 to 4000 characters."),
        );
    }
    let new = NewReport {
        page_path: optional(&form.page, 500),
        description: form.description.trim(),
        assistive_tech: optional(&form.assistive_tech, 500),
        contact: optional(&form.contact, 500),
    };
    match public_forms::report_accessibility(&state.pool, &new).await {
        Ok(()) => {
            tracing::info!("accessibility report stored");
            Redirect::to("/accessibility?sent=1#sent").into_response()
        }
        Err(e) => server_error("storing an accessibility report", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_register_parses_and_every_row_has_a_known_status_and_type() {
        let r = &*BUILT;
        assert!(r.rows.len() > 40, "{}", r.rows.len());
        assert!(r.draft_url.starts_with("https://www.w3.org/TR/"));
        for row in &r.rows {
            assert!(STATUSES.contains(&row.status.as_str()), "{row:?}");
            assert!(
                ["core", "supplemental", "assertion"].contains(&row.kind.as_str()),
                "{row:?}"
            );
        }
        let grouped: usize = groups(r).iter().map(|g| g.rows.len()).sum();
        assert_eq!(grouped, r.rows.len());
    }

    #[test]
    fn the_register_names_the_september_2026_draft_or_a_later_one() {
        let draft = chrono::NaiveDate::parse_from_str(&BUILT.draft, "%Y-%m-%d").unwrap();
        assert!(draft >= chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap());
        let evaluated =
            chrono::NaiveDate::parse_from_str(&BUILT.last_evaluated, "%Y-%m-%d").unwrap();
        assert!(evaluated >= draft);
        assert!(REGISTER.contains("Latest published draft checked on: "));
    }
}
