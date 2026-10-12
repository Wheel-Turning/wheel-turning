//! The dana split calculator (Spec 05, Dana calculator).
//!
//! A pure function: it handles no money, performs no I/O and has no payment dependency. The
//! donor enters a gift total, which is divided equally among the eligible sanghas of one answer
//! in integer cents, and an optional site amount, which is a separate line and never taken out
//! of the sangha total.

use url::Url;
use wt_core::ids::SanghaId;

/// The default ceiling on what one split may add up to: $100,000.
pub const MAX_TOTAL_CENTS: i64 = 10_000_000;

/// One sangha cited in the answer, as the registry has it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recipient {
    /// The sangha.
    pub sangha_id: SanghaId,
    /// Its name, as the panel shows it.
    pub name: String,
    /// Its donation page.
    pub url: Option<String>,
    /// The same page with `{amount_cents}` or `{amount}` in it, when the page takes an amount.
    pub url_template: Option<String>,
    /// A person checked the donation page.
    pub verified: bool,
    /// False when the sangha asked for attribution only.
    pub listed: bool,
}

impl Recipient {
    /// Rule 2: only a listed sangha with a verified donation page shares in the split.
    pub fn eligible(&self) -> bool {
        self.listed && self.verified && self.url.is_some()
    }
}

/// What the donor entered, and who the answer cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaInput {
    /// The total to share among the sanghas.
    pub gift_total_cents: i64,
    /// The optional, separate gift to the site.
    pub site_cents: i64,
    /// Every sangha the answer cites, duplicates allowed.
    pub recipients: Vec<Recipient>,
    /// Decides which sanghas get the leftover cents; the caller derives it from the answer id.
    pub tiebreak_seed: u64,
}

/// One sangha's share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaLine {
    /// The sangha.
    pub sangha_id: SanghaId,
    /// Its name.
    pub name: String,
    /// Its share.
    pub cents: i64,
    /// Its giving page, with the amount filled in when the page takes one.
    pub link: String,
}

/// The computed split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaSplit {
    /// One line per eligible sangha, in rotated order. Empty when none is eligible.
    pub lines: Vec<DanaLine>,
    /// The site line, unchanged from the input.
    pub site_cents: i64,
    /// Everything the donor would give: the sangha lines plus the site line.
    pub total_outlay_cents: i64,
    /// Sanghas left out because they are unlisted, unverified or have no page, each once.
    pub excluded: Vec<SanghaId>,
}

/// Why a split was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DanaError {
    /// An amount below zero.
    #[error("amounts cannot be negative")]
    Negative,
    /// More than the ceiling in all.
    #[error("the total is above the limit of {max_cents} cents")]
    TooLarge {
        /// The ceiling.
        max_cents: i64,
    },
}

/// Why a donation URL template was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The donation URL or the template is not a valid address.
    #[error("not a valid address: {0}")]
    Invalid(String),
    /// The template is not HTTPS.
    #[error("the template must use https")]
    NotHttps,
    /// The template points at another host than the donation page.
    #[error("the template's host {template} differs from the donation page's host {page}")]
    HostMismatch {
        /// The donation page's host.
        page: String,
        /// The template's host.
        template: String,
    },
    /// The template has neither `{amount_cents}` nor `{amount}`.
    #[error("the template has no {{amount_cents}} or {{amount}} placeholder")]
    NoPlaceholder,
}

const PLACEHOLDERS: [&str; 2] = ["{amount_cents}", "{amount}"];

/// The host of a URL, which must parse after the placeholders are filled with a sample amount.
fn host_of(raw: &str) -> Result<(String, String), TemplateError> {
    let mut filled = raw.to_owned();
    for p in PLACEHOLDERS {
        filled = filled.replace(p, "1");
    }
    let url = Url::parse(&filled).map_err(|_| TemplateError::Invalid(raw.to_owned()))?;
    let host = url
        .host_str()
        .ok_or_else(|| TemplateError::Invalid(raw.to_owned()))?
        .to_ascii_lowercase();
    Ok((url.scheme().to_owned(), host))
}

/// Rule 6: a template is HTTPS, has the donation page's host and names an amount. Registry
/// import runs this, and [`split`] runs it again before substituting.
pub fn validate_template(donation_url: &str, template: &str) -> Result<(), TemplateError> {
    let (_, page) = host_of(donation_url)?;
    let (scheme, host) = host_of(template)?;
    if scheme != "https" {
        return Err(TemplateError::NotHttps);
    }
    if host != page {
        return Err(TemplateError::HostMismatch {
            page,
            template: host,
        });
    }
    if !PLACEHOLDERS.iter().any(|p| template.contains(p)) {
        return Err(TemplateError::NoPlaceholder);
    }
    Ok(())
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Dollars with two decimals.
pub fn dollars(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.unsigned_abs();
    format!("{sign}{}.{:02}", c / 100, c % 100)
}

/// The giving link for one share: the template with the amount filled in when it passes the
/// re-check, otherwise the donation page itself.
fn link(page: &str, template: Option<&str>, cents: i64) -> String {
    match template {
        Some(t) if validate_template(page, t).is_ok() => t
            .replace("{amount_cents}", &encode(&cents.to_string()))
            .replace("{amount}", &encode(&dollars(cents))),
        _ => page.to_owned(),
    }
}

/// Splits with the default ceiling of [`MAX_TOTAL_CENTS`].
pub fn split(input: &DanaInput) -> Result<DanaSplit, DanaError> {
    split_with_limit(input, MAX_TOTAL_CENTS)
}

/// Spec 05's split, with the ceiling on gift plus site amount given.
pub fn split_with_limit(input: &DanaInput, max_total_cents: i64) -> Result<DanaSplit, DanaError> {
    if input.gift_total_cents < 0 || input.site_cents < 0 {
        return Err(DanaError::Negative);
    }
    let outlay = input
        .gift_total_cents
        .checked_add(input.site_cents)
        .filter(|t| *t <= max_total_cents)
        .ok_or(DanaError::TooLarge {
            max_cents: max_total_cents,
        })?;

    // Rule 2: one entry per sangha. Sorting the whole record first makes the surviving copy
    // the same whatever order the recipients came in.
    let mut all = input.recipients.clone();
    all.sort();
    all.dedup_by(|a, b| a.sangha_id == b.sangha_id);
    let (mut eligible, ineligible): (Vec<Recipient>, Vec<Recipient>) =
        all.into_iter().partition(Recipient::eligible);
    let excluded: Vec<SanghaId> = ineligible.iter().map(|r| r.sangha_id).collect();

    if eligible.is_empty() {
        return Ok(DanaSplit {
            lines: Vec::new(),
            site_cents: input.site_cents,
            total_outlay_cents: input.site_cents,
            excluded,
        });
    }

    // Rule 3: sorted by sangha id (already), then rotated by the seed.
    let n = eligible.len();
    let shift = usize::try_from(input.tiebreak_seed % n as u64).unwrap_or(0);
    eligible.rotate_left(shift);

    // Rule 4: equal shares in integer cents; the first `rem` in rotated order get one more.
    let count = i64::try_from(n).unwrap_or(i64::MAX);
    let base = input.gift_total_cents / count;
    let rem = usize::try_from(input.gift_total_cents % count).unwrap_or(0);
    let lines = eligible
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let cents = base + i64::from(i < rem);
            let page = r.url.unwrap_or_default();
            DanaLine {
                sangha_id: r.sangha_id,
                link: link(&page, r.url_template.as_deref(), cents),
                name: r.name,
                cents,
            }
        })
        .collect();
    Ok(DanaSplit {
        lines,
        site_cents: input.site_cents,
        total_outlay_cents: outlay,
        excluded,
    })
}

/// A tiebreak seed from an answer id's bytes, so one answer always splits the same way and
/// different answers rotate the leftover cents differently.
pub fn seed_from(id: &[u8]) -> u64 {
    // FNV-1a: stable across builds and platforms, unlike the standard hasher.
    id.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Parses a dollar amount as a donor types it: digits with at most two decimals and an
/// optional leading `$`. Blank is zero. `None` for anything else.
pub fn parse_dollars(raw: &str) -> Option<i64> {
    let s = raw.trim().trim_start_matches('$').trim().replace(',', "");
    if s.is_empty() {
        return Some(0);
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s.as_str(), ""),
    };
    let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty()) || !digits(whole) || !digits(frac) || frac.len() > 2 {
        return None;
    }
    let w: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let f: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<2}").parse().ok()?
    };
    w.checked_mul(100)?.checked_add(f)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> SanghaId {
        SanghaId::from_uuid(Uuid::from_u128(n))
    }

    fn ok(n: u128) -> Recipient {
        Recipient {
            sangha_id: id(n),
            name: format!("Sangha {n}"),
            url: Some(format!("https://s{n}.example.org/give")),
            url_template: None,
            verified: true,
            listed: true,
        }
    }

    fn input(total: i64, rs: Vec<Recipient>) -> DanaInput {
        DanaInput {
            gift_total_cents: total,
            site_cents: 0,
            recipients: rs,
            tiebreak_seed: 0,
        }
    }

    #[test]
    fn splits_equally_with_the_extra_cent_first_in_rotated_order() {
        let mut i = input(100, vec![ok(3), ok(1), ok(2)]);
        let s = split(&i).unwrap();
        let got: Vec<_> = s.lines.iter().map(|l| (l.sangha_id, l.cents)).collect();
        assert_eq!(got, vec![(id(1), 34), (id(2), 33), (id(3), 33)]);
        i.tiebreak_seed = 4; // 4 mod 3 = 1
        let s = split(&i).unwrap();
        let got: Vec<_> = s.lines.iter().map(|l| (l.sangha_id, l.cents)).collect();
        assert_eq!(got, vec![(id(2), 34), (id(3), 33), (id(1), 33)]);
    }

    #[test]
    fn excludes_unlisted_unverified_and_pageless_sanghas() {
        let mut unlisted = ok(1);
        unlisted.listed = false;
        let mut unverified = ok(2);
        unverified.verified = false;
        let mut pageless = ok(3);
        pageless.url = None;
        let s = split(&input(1000, vec![unlisted, unverified, pageless, ok(4)])).unwrap();
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.lines[0].cents, 1000);
        assert_eq!(s.excluded, vec![id(1), id(2), id(3)]);
    }

    #[test]
    fn no_eligible_sangha_gives_no_lines_and_only_the_site_outlay() {
        let mut r = ok(1);
        r.verified = false;
        let mut i = input(500, vec![r]);
        i.site_cents = 300;
        let s = split(&i).unwrap();
        assert!(s.lines.is_empty());
        assert_eq!(s.total_outlay_cents, 300);
        assert_eq!(s.excluded, vec![id(1)]);
    }

    #[test]
    fn site_line_is_extra() {
        let mut i = input(1000, vec![ok(1), ok(2)]);
        i.site_cents = 250;
        let s = split(&i).unwrap();
        assert_eq!(s.lines.iter().map(|l| l.cents).sum::<i64>(), 1000);
        assert_eq!(s.site_cents, 250);
        assert_eq!(s.total_outlay_cents, 1250);
    }

    #[test]
    fn rejects_negative_and_oversized_amounts() {
        assert_eq!(split(&input(-1, vec![ok(1)])), Err(DanaError::Negative));
        let mut i = input(0, vec![ok(1)]);
        i.site_cents = -5;
        assert_eq!(split(&i), Err(DanaError::Negative));
        assert_eq!(
            split(&input(MAX_TOTAL_CENTS + 1, vec![ok(1)])),
            Err(DanaError::TooLarge {
                max_cents: MAX_TOTAL_CENTS
            })
        );
        let mut i = input(MAX_TOTAL_CENTS, vec![ok(1)]);
        assert!(split(&i).is_ok());
        i.site_cents = 1;
        assert!(split(&i).is_err());
        assert!(split_with_limit(&input(i64::MAX, vec![ok(1)]), i64::MAX).is_ok());
        let mut i = input(i64::MAX, vec![ok(1)]);
        i.site_cents = 1;
        assert!(
            split_with_limit(&i, i64::MAX).is_err(),
            "overflow is refused"
        );
    }

    #[test]
    fn template_amounts_are_substituted() {
        let mut r = ok(1);
        r.url = Some("https://s1.example.org/give".into());
        r.url_template = Some("https://s1.example.org/give?amt={amount}&c={amount_cents}".into());
        let s = split(&input(1234, vec![r])).unwrap();
        assert_eq!(
            s.lines[0].link,
            "https://s1.example.org/give?amt=12.34&c=1234"
        );
    }

    #[test]
    fn a_template_failing_the_recheck_falls_back_to_the_page() {
        for t in [
            "http://s1.example.org/give?a={amount}",
            "https://elsewhere.example.org/give?a={amount}",
            "https://s1.example.org/give",
        ] {
            let mut r = ok(1);
            r.url_template = Some(t.into());
            let s = split(&input(500, vec![r])).unwrap();
            assert_eq!(s.lines[0].link, "https://s1.example.org/give", "{t}");
        }
    }

    #[test]
    fn template_validation() {
        let page = "https://give.example.org/donate";
        assert!(validate_template(page, "https://give.example.org/donate?amount={amount}").is_ok());
        assert!(validate_template(page, "https://GIVE.example.org/d/{amount_cents}").is_ok());
        assert_eq!(
            validate_template(page, "http://give.example.org/donate?amount={amount}"),
            Err(TemplateError::NotHttps)
        );
        assert!(matches!(
            validate_template(page, "https://evil.example.com/donate?amount={amount}"),
            Err(TemplateError::HostMismatch { .. })
        ));
        assert!(matches!(
            validate_template(page, "https://give.example.org.evil.com/?a={amount}"),
            Err(TemplateError::HostMismatch { .. })
        ));
        assert_eq!(
            validate_template(page, "https://give.example.org/donate"),
            Err(TemplateError::NoPlaceholder)
        );
        assert!(matches!(
            validate_template("not a url", "https://give.example.org/?a={amount}"),
            Err(TemplateError::Invalid(_))
        ));
    }

    #[test]
    fn dollars_and_parsing() {
        assert_eq!(dollars(0), "0.00");
        assert_eq!(dollars(5), "0.05");
        assert_eq!(dollars(123_456), "1234.56");
        assert_eq!(parse_dollars(""), Some(0));
        assert_eq!(parse_dollars(" $25 "), Some(2500));
        assert_eq!(parse_dollars("12.5"), Some(1250));
        assert_eq!(parse_dollars("0.07"), Some(7));
        assert_eq!(parse_dollars("1,000.00"), Some(100_000));
        assert_eq!(parse_dollars(".5"), Some(50));
        for bad in ["abc", "1.234", "-5", "1.2.3", ".", "1e3"] {
            assert_eq!(parse_dollars(bad), None, "{bad}");
        }
    }

    #[test]
    fn seeds_are_stable_and_vary() {
        assert_eq!(seed_from(b"abc"), seed_from(b"abc"));
        assert_ne!(seed_from(b"abc"), seed_from(b"abd"));
    }
}
