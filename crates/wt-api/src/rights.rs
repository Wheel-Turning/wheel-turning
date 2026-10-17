//! `GET /rights`, `POST /rights/request` and `GET|POST /rights/confirm` (Spec 08, Rights
//! lifecycle; Spec 06, Page inventory; Spec 09, Rights and removal page). All work without a
//! session.
//!
//! A request is stored `unverified` and acted on only after its sender opens the emailed link
//! and confirms it (owner's decision of October 15, 2026), so nobody can take material down
//! with forged requests. The link opens a page with a button rather than confirming on `GET`,
//! so a mail scanner that follows links confirms nothing.

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use sqlx::types::Uuid;
use wt_db::public_forms::{self, NewRequest, SourceStatus};

use crate::chrome::{Chrome, Current};
use crate::mail::Message;
use crate::session::{new_token, token_hash};
use crate::{AppState, ClientAddress, page, server_error, too_many};

/// Where to write when the form cannot take a request.
const RIGHTS_MAILBOX: &str = "rights@wheelturning.org";

/// One line of the source list.
pub struct SourceLine {
    /// Who publishes it.
    pub name: String,
    /// `talks` or `texts`.
    pub kind: String,
    /// The plain-language status.
    pub status: String,
}

/// A date written out, such as "October 9, 2026".
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

/// The rights page: each source and its status, and the removal request form with its outcome or
/// error.
#[derive(Template)]
#[template(path = "rights.html")]
struct RightsPage {
    chrome: Chrome,
    sources: Vec<SourceLine>,
    form: RequestForm,
    excerpt: Option<String>,
    sent: bool,
    error_summary: Option<String>,
    errors: FieldErrors,
}

/// What is wrong with each field of a submitted form, if anything.
#[derive(Debug, Default)]
pub struct FieldErrors {
    name: Option<&'static str>,
    email: Option<&'static str>,
    target: Option<&'static str>,
}

impl FieldErrors {
    /// How many fields are marked.
    fn count(&self) -> usize {
        [self.name, self.email, self.target]
            .iter()
            .filter(|e| e.is_some())
            .count()
    }
}

/// Whether `s` looks like an email address: one `@` with something before it, and a domain
/// with a dot that is not at either end. Delivery is the real check.
pub fn plausible_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && !s.chars().any(char::is_whitespace)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

/// Checks the required fields: a name, an email address and what should be removed.
fn check(form: &RequestForm) -> FieldErrors {
    let email = form.email.trim();
    FieldErrors {
        name: form
            .name
            .trim()
            .is_empty()
            .then_some("Please give your name."),
        email: if email.is_empty() {
            Some("Please give your email address, so we can reply.")
        } else if !plausible_email(email) {
            Some("Please give an email address in the form name@example.org.")
        } else {
            None
        },
        target: form.target.trim().is_empty().then_some(
            "Please say what should be removed: a web address, a title or an excerpt code.",
        ),
    }
}

/// `GET /rights` parameters.
#[derive(Debug, Deserialize)]
pub struct RightsQuery {
    excerpt: Option<String>,
    talk: Option<String>,
    sent: Option<u8>,
}

/// Renders the rights page with the sources' current statuses and the form as given. `problem`
/// replaces the summary when the request could not go ahead for a reason not in its fields.
async fn render(
    state: &AppState,
    status: StatusCode,
    form: RequestForm,
    sent: bool,
    errors: FieldErrors,
    problem: Option<String>,
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
            error_summary: problem.or_else(|| match errors.count() {
                0 => None,
                1 => Some("Please fix the field marked below.".to_owned()),
                _ => Some("Please fix the fields marked below.".to_owned()),
            }),
            errors,
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
    render(
        &state,
        StatusCode::OK,
        form,
        q.sent.is_some(),
        FieldErrors::default(),
        None,
    )
    .await
}

/// A form field trimmed and cut to `max` characters, or none when empty.
fn optional(s: &str, max: usize) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then(|| {
        let end = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
        &s[..end]
    })
}

/// `POST /rights/request`: stores a request as `unverified` and emails its sender a link to
/// confirm it. At most 5 an hour from one address and 3 confirmation emails an hour to one
/// recipient. Name, email and what to remove are required (owner's decision of October 15,
/// 2026). A filled honeypot gets the same thank-you, stores nothing and sends nothing.
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
    let errors = check(&form);
    if errors.count() > 0 {
        return render(&state, StatusCode::BAD_REQUEST, form, false, errors, None).await;
    }
    let (name, email, target) = (form.name.trim(), form.email.trim(), form.target.trim());
    let mailer = &state.site.mailer;
    if !mailer.ready() {
        tracing::warn!("rights request refused: no mailer to confirm it");
        let problem = format!(
            "We cannot send the confirmation email at the moment. Please write to {RIGHTS_MAILBOX} instead."
        );
        let status = StatusCode::SERVICE_UNAVAILABLE;
        return render(
            &state,
            status,
            form,
            false,
            FieldErrors::default(),
            Some(problem),
        )
        .await;
    }
    if let Err(wait) = state
        .site
        .limits
        .confirm_emails
        .check(&email.to_lowercase())
    {
        return too_many(
            wait,
            "Several requests have gone to this email address this hour.",
        );
    }
    let text_unit = match form.excerpt.parse::<Uuid>() {
        Ok(id) => match public_forms::serving_text_unit(&state.pool, id).await {
            Ok(true) => Some(id),
            Ok(false) => None,
            Err(e) => return server_error("checking a reported excerpt", &e),
        },
        Err(_) => None,
    };
    let token = match new_token() {
        Ok(t) => t,
        Err(e) => return server_error("making a confirmation token", &e),
    };
    let hash = token_hash(&token);
    let new = NewRequest {
        name: optional(name, 200).unwrap_or(name),
        email: optional(email, 320).unwrap_or(email),
        target: optional(target, 1000).unwrap_or(target),
        message: optional(&form.message, 4000),
        text_unit,
        token_hash: &hash,
    };
    if let Err(e) = public_forms::request_removal(&state.pool, &new).await {
        return server_error("storing a rights request", &e);
    }
    let message = confirmation(new.email, &state.site.public_url, &token);
    if let Err(e) = mailer.send(&message).await {
        // The stored request stays unverified and is purged a week after its link expires.
        tracing::error!(error = %e, "rights request: confirmation email not sent");
        let problem = format!(
            "We could not send the confirmation email. Please try again later, or write to {RIGHTS_MAILBOX}."
        );
        let status = StatusCode::SERVICE_UNAVAILABLE;
        return render(
            &state,
            status,
            form,
            false,
            FieldErrors::default(),
            Some(problem),
        )
        .await;
    }
    tracing::info!("rights request stored; confirmation sent");
    Redirect::to("/rights?sent=1#sent").into_response()
}

/// The confirmation email. It carries nothing the sender typed, so the form cannot be used to
/// put someone else's words or links in front of whoever owns the address.
pub fn confirmation(to: &str, public_url: &str, token: &str) -> Message {
    Message {
        to: to.to_owned(),
        subject: "Confirm your removal request to Wheel Turning".to_owned(),
        text: format!(
            "Someone asked Wheel Turning to remove material, and gave this email address.\n\n\
             If it was you, open this link and confirm the request. It works once, for 24 hours:\n\
             {public_url}/rights/confirm?token={token}\n\n\
             Nothing is removed until you confirm. Once you confirm, we stop showing the material \
             you named. Requests are handled in the order they arrive. The site is run by one \
             person as a side project, so timing can vary, but each request is handled as \
             promptly as possible.\n\n\
             If it was not you, ignore this email. The request lapses and nothing changes.\n\n\
             Wheel Turning\n"
        ),
    }
}

/// The confirmation page and its outcome.
#[derive(Template)]
#[template(path = "rights_confirm.html")]
struct ConfirmPage {
    chrome: Chrome,
    /// The token from the link, for the button's form; none when the link is incomplete.
    token: Option<String>,
    /// After the button: whether the request was confirmed.
    outcome: Option<bool>,
}

/// `GET /rights/confirm` and `POST /rights/confirm` parameters.
#[derive(Debug, Default, Deserialize)]
pub struct ConfirmForm {
    #[serde(default)]
    token: String,
}

/// A token as the link carries it: 43 URL-safe base64 characters (32 bytes).
fn well_formed(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `GET /rights/confirm?token=...`: a page with one button. Following the link changes nothing.
pub async fn confirm_page(Query(q): Query<ConfirmForm>) -> Response {
    let token = q.token.trim();
    let ok = well_formed(token);
    page(
        if ok {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        &ConfirmPage {
            chrome: Chrome::new("/rights/confirm", Current::Rights, None),
            token: ok.then(|| token.to_owned()),
            outcome: None,
        },
    )
}

/// `POST /rights/confirm`: confirms the request the token belongs to, once and within 24 hours.
pub async fn confirm(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    Form(form): Form<ConfirmForm>,
) -> Response {
    if let Err(wait) = state.site.limits.public_forms.check(&address) {
        return too_many(wait, "You have sent several requests this hour.");
    }
    let token = form.token.trim();
    let confirmed = if well_formed(token) {
        match public_forms::confirm_removal(&state.pool, &token_hash(token)).await {
            Ok(c) => c,
            Err(e) => return server_error("confirming a rights request", &e),
        }
    } else {
        false
    };
    if confirmed {
        tracing::info!("rights request confirmed");
    }
    page(
        if confirmed {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        &ConfirmPage {
            chrome: Chrome::new("/rights/confirm", Current::Rights, None),
            token: None,
            outcome: Some(confirmed),
        },
    )
}

/// Deletes unconfirmed requests a week after their link expired, once an hour.
pub async fn purge_loop(pool: sqlx::PgPool) {
    let mut every = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        every.tick().await;
        match public_forms::purge_unverified(&pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(requests = n, "purged unconfirmed rights requests"),
            Err(e) => tracing::warn!(error = %e, "purging unconfirmed rights requests"),
        }
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
    fn an_email_address_needs_one_at_and_a_dotted_domain() {
        for ok in ["a@b.org", "first.last+tag@mail.example.co.uk"] {
            assert!(plausible_email(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "a@",
            "@b.org",
            "a@b",
            "a@.org",
            "a@b.",
            "a@b@c.org",
            "a b@c.org",
        ] {
            assert!(!plausible_email(bad), "{bad}");
        }
    }

    #[test]
    fn the_confirmation_email_carries_the_link_and_nothing_the_sender_typed() {
        let m = confirmation("holder@example.org", "https://wt.test", "tok");
        assert_eq!(m.to, "holder@example.org");
        assert!(
            m.text
                .contains("https://wt.test/rights/confirm?token=tok\n")
        );
        assert!(m.text.contains("Nothing is removed until you confirm."));
        assert!(!m.text.contains("holder@example.org"));
    }

    #[test]
    fn only_a_43_character_url_safe_token_is_well_formed() {
        let good = "A".repeat(43);
        assert!(well_formed(&good));
        assert!(!well_formed(&"A".repeat(42)));
        assert!(!well_formed(&format!("{}=", "A".repeat(42))));
        assert!(!well_formed(""));
    }

    #[test]
    fn name_email_and_target_are_each_required() {
        let empty = check(&RequestForm::default());
        assert!(empty.name.is_some() && empty.email.is_some() && empty.target.is_some());
        let full = check(&RequestForm {
            name: "A Holder".into(),
            email: "holder@example.org".into(),
            target: "Excerpt 1".into(),
            ..RequestForm::default()
        });
        assert_eq!(full.count(), 0);
        let bad = check(&RequestForm {
            name: " ".into(),
            email: "holder".into(),
            target: "t".into(),
            ..RequestForm::default()
        });
        assert_eq!(bad.count(), 2);
        assert!(bad.email.is_some_and(|e| e.contains("name@example.org")));
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
