//! `GET /rights` and `POST /rights/request` (Spec 08, Rights lifecycle; Spec 06, Page
//! inventory; Spec 09, Rights and removal page). Both work without a session.

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use sqlx::types::Uuid;
use wt_db::public_forms::{self, NewRequest, SourceStatus};

use crate::chrome::{Chrome, Current};
use crate::{AppState, ClientAddress, page, server_error, too_many};

/// One line of the source list.
pub struct SourceLine {
    /// Who publishes it.
    pub name: String,
    /// `talks` or `texts`.
    pub kind: String,
    /// The plain-language status.
    pub status: String,
}

fn date(d: chrono::NaiveDate) -> String {
    d.format("%B %-d, %Y").to_string()
}

/// Spec 09's four statuses, in plain words. A mandate never reads as permission: the project's
/// own decision is named as such. A source switched off for any other reason is not described
/// as removed at anyone's request.
pub fn status_line(s: &SourceStatus) -> String {
    if !s.enabled {
        return if s.removal_requested {
            "Removed at the rights holder's request.".to_owned()
        } else {
            "Not shown at present.".to_owned()
        };
    }
    match s.basis.as_str() {
        "permission" => match s.permission_recorded_on.or(s.license_recorded_on) {
            Some(d) => format!("Used with permission, recorded on {}.", date(d)),
            None => "Used with permission.".to_owned(),
        },
        "license" => {
            let name = s.license_name.as_deref().unwrap_or("a license");
            match s.license_recorded_on {
                Some(d) => format!("Used under {name}, recorded on {}.", date(d)),
                None => format!("Used under {name}."),
            }
        }
        _ => match s.request_sent_on {
            Some(d) => format!(
                "Used on the project's own decision. Permission requested on {}.",
                date(d)
            ),
            None => "Used on the project's own decision. Permission not yet requested.".to_owned(),
        },
    }
}

/// What the form holds, to refill it after an error.
#[derive(Debug, Default, Deserialize)]
pub struct RequestForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    message: String,
    /// The honeypot. People never see it, so anything in it came from a bot.
    #[serde(default)]
    website: String,
    /// The reported excerpt's id, from "Report this excerpt".
    #[serde(default)]
    excerpt: String,
}

#[derive(Template)]
#[template(path = "rights.html")]
struct RightsPage {
    chrome: Chrome,
    sources: Vec<SourceLine>,
    form: RequestForm,
    excerpt: Option<String>,
    sent: bool,
    error_summary: Option<&'static str>,
    target_error: Option<&'static str>,
}

/// `GET /rights` parameters.
#[derive(Debug, Deserialize)]
pub struct RightsQuery {
    excerpt: Option<String>,
    talk: Option<String>,
    sent: Option<u8>,
}

async fn render(
    state: &AppState,
    status: StatusCode,
    form: RequestForm,
    sent: bool,
    target_error: Option<&'static str>,
) -> Response {
    let sources = match public_forms::source_statuses(&state.pool).await {
        Ok(s) => s,
        Err(e) => return server_error("loading source statuses", &e),
    };
    let sources = sources
        .iter()
        .map(|s| SourceLine {
            name: s.publisher.clone(),
            kind: s.kind.clone(),
            status: status_line(s),
        })
        .collect();
    let excerpt = (!form.excerpt.is_empty()).then(|| form.excerpt.clone());
    page(
        status,
        &RightsPage {
            chrome: Chrome::new("/rights", Current::Rights, None),
            sources,
            excerpt,
            sent,
            error_summary: target_error.map(|_| "Please fix the field marked below."),
            target_error,
            form,
        },
    )
}

/// `GET /rights`, with `?excerpt=<id>` from "Report this excerpt" or `?talk=<id>` from
/// "Report this transcript" prefilling what to remove.
pub async fn rights_page(State(state): State<AppState>, Query(q): Query<RightsQuery>) -> Response {
    let mut form = RequestForm::default();
    if let Some(id) = q.excerpt.as_deref().and_then(|x| x.parse::<Uuid>().ok()) {
        form.target = format!("Excerpt {id}");
        form.excerpt = id.to_string();
    } else if let Some(id) = q.talk.as_deref().and_then(|x| x.parse::<Uuid>().ok()) {
        form.target = format!("Transcript of talk {id}");
    }
    render(&state, StatusCode::OK, form, q.sent.is_some(), None).await
}

fn optional(s: &str, max: usize) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then(|| {
        let end = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
        &s[..end]
    })
}

/// `POST /rights/request`: stores a request, at most 5 an hour from one address. A filled
/// honeypot gets the same thank-you and stores nothing.
pub async fn request(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    Form(form): Form<RequestForm>,
) -> Response {
    if let Err(wait) = state.site.limits.public_forms.check(&address) {
        return too_many(wait, "You have sent several requests this hour.");
    }
    if !form.website.is_empty() {
        tracing::info!("rights request: honeypot filled, nothing stored");
        return Redirect::to("/rights?sent=1#sent").into_response();
    }
    let target = form.target.trim();
    if target.is_empty() {
        return render(
            &state,
            StatusCode::BAD_REQUEST,
            form,
            false,
            Some("Please say what should be removed: a web address, a title or an excerpt code."),
        )
        .await;
    }
    let text_unit = match form.excerpt.parse::<Uuid>() {
        Ok(id) => match public_forms::serving_text_unit(&state.pool, id).await {
            Ok(true) => Some(id),
            Ok(false) => None,
            Err(e) => return server_error("checking a reported excerpt", &e),
        },
        Err(_) => None,
    };
    let new = NewRequest {
        name: optional(&form.name, 200),
        email: optional(&form.email, 320),
        target: optional(target, 1000).unwrap_or(target),
        message: optional(&form.message, 4000),
        text_unit,
    };
    match public_forms::request_removal(&state.pool, &new).await {
        Ok(()) => {
            tracing::info!("rights request stored");
            Redirect::to("/rights?sent=1#sent").into_response()
        }
        Err(e) => server_error("storing a rights request", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(basis: &str) -> SourceStatus {
        SourceStatus {
            key: "k".into(),
            publisher: "P".into(),
            basis: basis.into(),
            kind: "texts".into(),
            enabled: true,
            request_sent_on: None,
            license_name: None,
            permission_recorded_on: None,
            license_recorded_on: None,
            removal_requested: false,
        }
    }

    #[test]
    fn each_basis_reads_in_plain_words() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let mut m = source("mandate");
        assert_eq!(
            status_line(&m),
            "Used on the project's own decision. Permission not yet requested."
        );
        m.request_sent_on = Some(d);
        let line = status_line(&m);
        assert_eq!(
            line,
            "Used on the project's own decision. Permission requested on October 5, 2026."
        );
        assert!(!line.contains("with permission") && !line.contains("icensed"));

        let mut p = source("permission");
        p.permission_recorded_on = Some(d);
        assert_eq!(
            status_line(&p),
            "Used with permission, recorded on October 5, 2026."
        );

        let mut l = source("license");
        l.license_name = Some("cc-by-4.0".into());
        l.license_recorded_on = Some(d);
        assert_eq!(
            status_line(&l),
            "Used under cc-by-4.0, recorded on October 5, 2026."
        );

        m.enabled = false;
        assert_eq!(status_line(&m), "Not shown at present.");
        m.removal_requested = true;
        assert_eq!(status_line(&m), "Removed at the rights holder's request.");
    }
}
