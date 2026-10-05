//! The ingest review page (Spec 03): duplicate clusters with the kept record marked, excluded
//! items with reasons, talks in `needs_review`, series coverage by part number, a month-by-month
//! count with its gaps, and donation-looking links found in descriptions for a person to verify.
//! The report reads only; it sets nothing.
//!
//! The page is one self-contained HTML file: inline styles, no scripts, nothing loaded from
//! another host.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::LazyLock;

use chrono::Datelike;
use regex::Regex;
use serde::Serialize;
use sqlx::types::chrono::{DateTime, Utc};
use wt_core::ids::TalkId;
use wt_db::ingest::ReportRow;

use crate::config::Series;

/// Words that make a link in a description look like a donation page. `donat` is a stem for
/// donate, donation and donations.
const DONATION_WORDS: [&str; 12] = [
    "donat",
    "dana",
    "give",
    "giving",
    "support",
    "contribute",
    "contribution",
    "contributing",
    "paypal",
    "venmo",
    "patreon",
    "tithe.ly",
];

static LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'()\[\]]+"#).ok());

/// One talk as a line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// The platform and its id, such as `vimeo 672990540`.
    pub source: String,
    /// Canonical page address.
    pub url: String,
    /// Title.
    pub title: String,
    /// The talk's date, `YYYY-MM-DD`: its recording date where known, else the upload date.
    pub date: Option<String>,
    /// Publish date, `YYYY-MM-DD`.
    pub uploaded: Option<String>,
    /// Length as `h:mm:ss`.
    pub duration: Option<String>,
    /// Whether the source holds captions.
    pub captions: bool,
    /// State and reason, such as `needs_review: untitled`.
    pub status: String,
    /// Talk type.
    pub talk_type: String,
    /// Resolved teacher, or the speaker guess in brackets.
    pub speaker: Option<String>,
}

/// A kept record and the duplicates merged into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cluster {
    /// The record kept.
    pub kept: Item,
    /// The records merged into it.
    pub duplicates: Vec<Item>,
}

/// Which parts of a series are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SeriesCoverage {
    /// Series name.
    pub name: String,
    /// Number of parts, when the series config states it.
    pub known_parts: Option<i32>,
    /// Live talks by part number.
    pub parts: BTreeMap<i32, Vec<Item>>,
    /// Part numbers from 1 to the last known or seen part with no live talk.
    pub missing: Vec<i32>,
    /// Live talks recognized as the series with no part number.
    pub unnumbered: Vec<Item>,
}

/// Videos published in one month.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonthCount {
    /// `YYYY-MM`.
    pub month: String,
    /// Every listed video.
    pub videos: usize,
    /// Talks still live: not excluded or withdrawn.
    pub talks: usize,
}

/// A link in a description that looks like a donation page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DonationLink {
    /// The address as written.
    pub url: String,
    /// How many descriptions carry it.
    pub found_in: usize,
    /// The first video that carries it.
    pub example: Item,
}

/// Counts by where talks stand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// Every talk.
    pub talks: usize,
    /// Not yet classified.
    pub discovered: usize,
    /// Waiting for a rights basis.
    pub awaiting_consent: usize,
    /// Cleared or further along the pipeline.
    pub cleared_or_later: usize,
    /// Held for a person.
    pub needs_review: usize,
    /// Excluded, duplicates included.
    pub excluded: usize,
    /// Withdrawn after a takedown.
    pub withdrawn: usize,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestReport {
    /// When it was built.
    pub generated_at: String,
    /// Counts by state.
    pub totals: Totals,
    /// Duplicate clusters, kept record first.
    pub duplicate_clusters: Vec<Cluster>,
    /// Exclusions other than duplicates.
    pub excluded: Vec<Item>,
    /// Talks held for a person.
    pub needs_review: Vec<Item>,
    /// Coverage per series.
    pub series: Vec<SeriesCoverage>,
    /// Videos per month, every month from the first to the last.
    pub months: Vec<MonthCount>,
    /// Runs of months with no video, as `(first, last)`.
    pub month_gaps: Vec<(String, String)>,
    /// Donation-looking links in descriptions, to verify by hand.
    pub donation_links: Vec<DonationLink>,
}

fn live(state: &str) -> bool {
    !matches!(state, "excluded" | "withdrawn")
}

fn item(r: &ReportRow) -> Item {
    Item {
        source: format!("{} {}", r.platform, r.external_id),
        url: r.url.clone(),
        title: r.title.clone(),
        date: r.talk_date.map(|d| d.format("%Y-%m-%d").to_string()),
        uploaded: r.published_at.map(|t| t.format("%Y-%m-%d").to_string()),
        duration: r
            .duration_s
            .map(|s| format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)),
        captions: r.has_captions,
        status: match &r.state_reason {
            Some(reason) => format!("{}: {reason}", r.state),
            None => r.state.clone(),
        },
        talk_type: r.talk_type.clone(),
        speaker: r
            .teacher
            .clone()
            .or_else(|| r.speaker_guess.as_ref().map(|g| format!("[{g}]"))),
    }
}

fn month_index(at: DateTime<Utc>) -> i32 {
    at.year() * 12 + i32::try_from(at.month0()).unwrap_or(0)
}

fn month_label(index: i32) -> String {
    format!(
        "{:04}-{:02}",
        index.div_euclid(12),
        index.rem_euclid(12) + 1
    )
}

/// Builds the report from every talk. `series` is the configured list, so a series with no
/// talks yet still shows as missing.
pub fn build(rows: &[ReportRow], series: &[Series], now: DateTime<Utc>) -> IngestReport {
    let by_id: BTreeMap<TalkId, &ReportRow> = rows.iter().map(|r| (r.talk, r)).collect();

    let mut totals = Totals {
        talks: rows.len(),
        ..Totals::default()
    };
    for r in rows {
        match r.state.as_str() {
            "discovered" => totals.discovered += 1,
            "awaiting_consent" => totals.awaiting_consent += 1,
            "needs_review" => totals.needs_review += 1,
            "excluded" => totals.excluded += 1,
            "withdrawn" => totals.withdrawn += 1,
            _ => totals.cleared_or_later += 1,
        }
    }

    let mut clusters: BTreeMap<TalkId, Vec<Item>> = BTreeMap::new();
    for r in rows {
        if let Some(kept) = r.duplicate_of {
            clusters.entry(kept).or_default().push(item(r));
        }
    }
    let mut duplicate_clusters: Vec<Cluster> = clusters
        .into_iter()
        .filter_map(|(kept, duplicates)| {
            Some(Cluster {
                kept: item(by_id.get(&kept)?),
                duplicates,
            })
        })
        .collect();
    // Oldest kept record first, as a reviewer reads them.
    duplicate_clusters
        .sort_by(|a, b| (&a.kept.date, &a.kept.source).cmp(&(&b.kept.date, &b.kept.source)));

    let excluded = rows
        .iter()
        .filter(|r| r.state == "excluded" && r.duplicate_of.is_none())
        .map(item)
        .collect();
    let needs_review = rows
        .iter()
        .filter(|r| r.state == "needs_review")
        .map(item)
        .collect();

    // Series: the configured ones in order, then any others found on talks.
    let mut names: Vec<(String, Option<i32>)> =
        series.iter().map(|s| (s.name.clone(), s.parts)).collect();
    let extra: BTreeSet<&str> = rows
        .iter()
        .filter_map(|r| r.series.as_deref())
        .filter(|n| !series.iter().any(|s| s.name == *n))
        .collect();
    names.extend(extra.into_iter().map(|n| (n.to_owned(), None)));
    let series = names
        .into_iter()
        .map(|(name, known_parts)| {
            let mut parts: BTreeMap<i32, Vec<Item>> = BTreeMap::new();
            let mut unnumbered = Vec::new();
            for r in rows
                .iter()
                .filter(|r| live(&r.state) && r.series.as_deref() == Some(name.as_str()))
            {
                match r.part {
                    Some(p) => parts.entry(p).or_default().push(item(r)),
                    None => unnumbered.push(item(r)),
                }
            }
            let last = known_parts
                .into_iter()
                .chain(parts.keys().next_back().copied())
                .max()
                .unwrap_or(0);
            let missing = (1..=last).filter(|p| !parts.contains_key(p)).collect();
            SeriesCoverage {
                name,
                known_parts,
                parts,
                missing,
                unnumbered,
            }
        })
        .collect();

    let mut counts: BTreeMap<i32, (usize, usize)> = BTreeMap::new();
    for r in rows {
        if let Some(at) = r.published_at {
            let entry = counts.entry(month_index(at)).or_default();
            entry.0 += 1;
            entry.1 += usize::from(live(&r.state));
        }
    }
    let mut months = Vec::new();
    let mut month_gaps = Vec::new();
    if let (Some(&first), Some(&last)) = (counts.keys().next(), counts.keys().next_back()) {
        let mut gap_start: Option<i32> = None;
        for m in first..=last {
            let (videos, talks) = counts.get(&m).copied().unwrap_or_default();
            months.push(MonthCount {
                month: month_label(m),
                videos,
                talks,
            });
            match (videos, gap_start) {
                (0, None) => gap_start = Some(m),
                (0, Some(_)) => {}
                (_, Some(start)) => {
                    month_gaps.push((month_label(start), month_label(m - 1)));
                    gap_start = None;
                }
                (_, None) => {}
            }
        }
    }

    let mut links: BTreeMap<String, DonationLink> = BTreeMap::new();
    if let Some(re) = LINK.as_ref() {
        for r in rows {
            let Some(description) = &r.description else {
                continue;
            };
            let found: BTreeSet<&str> = re
                .find_iter(description)
                .map(|m| m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']))
                .filter(|url| {
                    let lower = url.to_lowercase();
                    DONATION_WORDS.iter().any(|w| lower.contains(w))
                })
                .collect();
            for url in found {
                links
                    .entry(url.to_owned())
                    .and_modify(|l| l.found_in += 1)
                    .or_insert_with(|| DonationLink {
                        url: url.to_owned(),
                        found_in: 1,
                        example: item(r),
                    });
            }
        }
    }

    IngestReport {
        generated_at: now.format("%Y-%m-%d %H:%M UTC").to_string(),
        totals,
        duplicate_clusters,
        excluded,
        needs_review,
        series,
        months,
        month_gaps,
        donation_links: links.into_values().collect(),
    }
}

/// Escapes text for HTML element content and quoted attributes.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A link only for http and https addresses; anything else is shown as text.
fn link(url: &str, text: &str) -> String {
    if url.starts_with("https://") || url.starts_with("http://") {
        format!(
            r#"<a href="{}" rel="noopener noreferrer">{}</a>"#,
            escape(url),
            escape(text)
        )
    } else {
        escape(text)
    }
}

const STYLE: &str = "body{font:15px/1.45 system-ui,sans-serif;margin:24px auto;max-width:1100px;\
padding:0 16px;color:#1d1d1f;background:#fff}h1{font-size:24px}h2{font-size:19px;margin-top:32px;\
border-bottom:1px solid #ddd;padding-bottom:4px}table{border-collapse:collapse;width:100%;\
margin:8px 0}th,td{text-align:left;padding:4px 8px;border-bottom:1px solid #eee;vertical-align:top}\
th{font-weight:600;background:#f6f6f6}.kept{font-weight:600}.muted{color:#666}.gap{color:#a33}\
.note{background:#fff8e1;padding:8px 12px;border-left:3px solid #e0a800}\
@media (prefers-color-scheme:dark){body{background:#151515;color:#e6e6e6}th{background:#222}\
th,td{border-color:#333}.muted{color:#aaa}.note{background:#2b2615}a{color:#8ab4f8}}";

fn items_table(out: &mut String, items: &[Item], empty: &str) {
    if items.is_empty() {
        let _ = writeln!(out, r#"<p class="muted">{}</p>"#, escape(empty));
        return;
    }
    out.push_str("<table><tr><th>Title</th><th>Status</th><th>Type</th><th>Speaker</th><th>Recorded</th><th>Uploaded</th><th>Length</th><th>Source</th></tr>\n");
    for i in items {
        item_row(out, i, "");
    }
    out.push_str("</table>\n");
}

fn item_row(out: &mut String, i: &Item, class: &str) {
    let _ = writeln!(
        out,
        r#"<tr class="{class}"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}{}</td></tr>"#,
        link(&i.url, &i.title),
        escape(&i.status),
        escape(&i.talk_type),
        escape(i.speaker.as_deref().unwrap_or("")),
        escape(i.date.as_deref().unwrap_or("")),
        escape(i.uploaded.as_deref().unwrap_or("")),
        escape(i.duration.as_deref().unwrap_or("")),
        escape(&i.source),
        if i.captions { " · captions" } else { "" },
    );
}

/// Renders the report as one self-contained HTML page.
pub fn render_html(r: &IngestReport) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Ingest review</title><style>{STYLE}</style></head><body>\n\
         <h1>Ingest review</h1>\n<p class=\"muted\">Generated {}. This page sets nothing: every \
         change is made by a person with the wt commands.</p>\n",
        escape(&r.generated_at)
    );
    let t = &r.totals;
    let _ = writeln!(
        out,
        "<table><tr><th>Talks</th><th>Discovered</th><th>Awaiting consent</th><th>Cleared or later</th>\
         <th>Needs review</th><th>Excluded</th><th>Withdrawn</th></tr><tr><td>{}</td><td>{}</td><td>{}</td>\
         <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr></table>",
        t.talks,
        t.discovered,
        t.awaiting_consent,
        t.cleared_or_later,
        t.needs_review,
        t.excluded,
        t.withdrawn
    );

    let _ = writeln!(
        out,
        "<h2 id=\"duplicates\">Duplicate clusters ({})</h2>",
        r.duplicate_clusters.len()
    );
    if r.duplicate_clusters.is_empty() {
        out.push_str("<p class=\"muted\">No duplicates found.</p>\n");
    } else {
        out.push_str(
            "<p class=\"muted\">The first row of each cluster, in bold, is the record kept.</p>\n",
        );
        out.push_str("<table><tr><th>Title</th><th>Status</th><th>Type</th><th>Speaker</th><th>Recorded</th><th>Uploaded</th><th>Length</th><th>Source</th></tr>\n");
        for c in &r.duplicate_clusters {
            item_row(&mut out, &c.kept, "kept");
            for d in &c.duplicates {
                item_row(&mut out, d, "");
            }
        }
        out.push_str("</table>\n");
    }

    let _ = writeln!(
        out,
        "<h2 id=\"excluded\">Excluded ({})</h2>",
        r.excluded.len()
    );
    items_table(
        &mut out,
        &r.excluded,
        "Nothing excluded apart from duplicates.",
    );

    let _ = writeln!(
        out,
        "<h2 id=\"review\">Needs review ({})</h2>",
        r.needs_review.len()
    );
    items_table(&mut out, &r.needs_review, "Nothing is waiting for review.");

    out.push_str("<h2 id=\"series\">Series coverage</h2>\n<table><tr><th>Series</th><th>Parts present</th><th>Missing parts</th><th>Without a part number</th></tr>\n");
    for s in &r.series {
        let present: Vec<String> = s
            .parts
            .iter()
            .map(|(p, items)| {
                if items.len() > 1 {
                    format!("{p} (×{})", items.len())
                } else {
                    p.to_string()
                }
            })
            .collect();
        let missing: Vec<String> = s.missing.iter().map(ToString::to_string).collect();
        let total = s
            .known_parts
            .map(|n| format!(" of {n}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            r#"<tr><td>{}</td><td>{}{}</td><td class="gap">{}</td><td>{}</td></tr>"#,
            escape(&s.name),
            escape(&if present.is_empty() {
                "none".to_owned()
            } else {
                present.join(", ")
            }),
            escape(&total),
            escape(&missing.join(", ")),
            s.unnumbered.len()
        );
    }
    out.push_str("</table>\n");

    out.push_str("<h2 id=\"months\">Videos by month</h2>\n");
    if r.month_gaps.is_empty() {
        out.push_str("<p class=\"muted\">No month without a video.</p>\n");
    } else {
        let gaps: Vec<String> = r
            .month_gaps
            .iter()
            .map(|(a, b)| {
                if a == b {
                    a.clone()
                } else {
                    format!("{a} to {b}")
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "<p class=\"gap\">Months with no video: {}.</p>",
            escape(&gaps.join("; "))
        );
    }
    out.push_str("<table><tr><th>Month</th><th>Videos</th><th>Talks kept</th></tr>\n");
    for m in &r.months {
        let class = if m.videos == 0 { "gap" } else { "" };
        let _ = writeln!(
            out,
            r#"<tr class="{class}"><td>{}</td><td>{}</td><td>{}</td></tr>"#,
            escape(&m.month),
            m.videos,
            m.talks
        );
    }
    out.push_str("</table>\n");

    let _ = writeln!(
        out,
        "<h2 id=\"donation-links\">Donation-looking links ({})</h2>",
        r.donation_links.len()
    );
    out.push_str("<p class=\"note\">Found in video descriptions. None is used until a person opens it, \
                  confirms it belongs to the sangha and takes gifts, and records it with the registry \
                  commands.</p>\n");
    if r.donation_links.is_empty() {
        out.push_str("<p class=\"muted\">None found.</p>\n");
    } else {
        out.push_str("<table><tr><th>Link</th><th>Descriptions</th><th>First seen on</th></tr>\n");
        for l in &r.donation_links {
            let _ = writeln!(
                out,
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                link(&l.url, &l.url),
                l.found_in,
                link(&l.example.url, &l.example.title)
            );
        }
        out.push_str("</table>\n");
    }
    out.push_str("</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_covers_markup_and_quotes() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn only_web_addresses_become_links() {
        assert!(link("https://example.invalid/give", "x").starts_with("<a href="));
        assert_eq!(link("javascript:alert(1)", "x"), "x");
    }

    #[test]
    fn months_count_from_zero() {
        let at: DateTime<Utc> = "2022-04-15T00:00:00Z".parse().unwrap_or_default();
        assert_eq!(month_label(month_index(at)), "2022-04");
        assert_eq!(month_label(month_index(at) + 9), "2023-01");
    }
}
