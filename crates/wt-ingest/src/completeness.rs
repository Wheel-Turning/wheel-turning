//! The completeness check (Spec 03, Seattle Soto Zen sources, item 6): the Vimeo account's
//! list against the sangha's talks page and what discovery recorded, so talks missing from
//! either are visible. The podcast feed joins the comparison once a feed source exists.
//!
//! The account list comes from the Vimeo API when a token is set. Without one it can come from
//! account video pages a person saved in their browser; this code reads those files and never
//! requests the Vimeo site. Like the ingest report, the check reads only and sets nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::report::escape;
use crate::vimeo::page_video_ids;

/// One video in the account list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedVideo {
    /// Vimeo id.
    pub id: String,
    /// Title, when the list shows it.
    pub title: Option<String>,
    /// Length in seconds, when the list shows it.
    pub duration_s: Option<i32>,
}

/// One video discovery recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedVideo {
    /// Vimeo id.
    pub id: String,
    /// Title.
    pub title: String,
    /// The talk's state.
    pub state: String,
}

/// A video card in a saved Vimeo account page: its id and title.
static CARD_TITLE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"href="https?://vimeo\.com/(\d{6,12})"[^>]*data-testid="content-card-title"[^>]*>([^<]*)<"#).ok()
});
/// A card's duration badge, `M:SS` or `H:MM:SS`.
static CARD_DURATION: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"chakra-badge[^>]*>\s*((?:\d{1,2}:)?\d{1,2}:\d{2})\s*<"#).ok());

/// Decodes the HTML entities a saved page uses in titles.
fn decode_entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// `H:MM:SS` or `M:SS` as seconds.
fn seconds(clock: &str) -> Option<i32> {
    clock
        .split(':')
        .try_fold(0i32, |acc, part| Some(acc * 60 + part.parse::<i32>().ok()?))
}

/// Reads a saved Vimeo account videos page. Each video card gives its id, title and length;
/// a page in another layout falls back to every Vimeo link it contains, ids only.
pub fn parse_saved_account_page(html: &str) -> Vec<ListedVideo> {
    let mut videos = Vec::new();
    if let (Some(title_re), Some(duration_re)) = (CARD_TITLE.as_ref(), CARD_DURATION.as_ref()) {
        for card in html.split("<article").skip(1) {
            let Some(caps) = title_re.captures(card) else {
                continue;
            };
            let (Some(id), Some(title)) = (caps.get(1), caps.get(2)) else {
                continue;
            };
            let title = decode_entities(title.as_str()).trim().to_owned();
            videos.push(ListedVideo {
                id: id.as_str().to_owned(),
                title: (!title.is_empty()).then_some(title),
                duration_s: duration_re
                    .captures(card)
                    .and_then(|c| c.get(1))
                    .and_then(|m| seconds(m.as_str())),
            });
        }
    }
    if videos.is_empty() {
        videos = page_video_ids(html)
            .into_iter()
            .map(|id| ListedVideo {
                id,
                title: None,
                duration_s: None,
            })
            .collect();
    }
    videos
}

/// Merges lists, keeping the first entry for each id.
pub fn merge_lists(lists: impl IntoIterator<Item = Vec<ListedVideo>>) -> Vec<ListedVideo> {
    let mut seen = BTreeSet::new();
    lists
        .into_iter()
        .flatten()
        .filter(|v| seen.insert(v.id.clone()))
        .collect()
}

/// A line of the completeness report: an id with whatever is known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Vimeo id.
    pub id: String,
    /// Title from the account list or from discovery.
    pub title: Option<String>,
    /// Length in seconds, when known.
    pub duration_s: Option<i32>,
    /// The talk's state, when discovery recorded it.
    pub state: Option<String>,
}

/// The completeness report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompletenessReport {
    /// Where the account list came from: `api`, `saved pages` or `none`.
    pub account_list_source: String,
    /// Videos in the account list.
    pub account_count: usize,
    /// Hours in the account list, when lengths are known.
    pub account_hours: Option<f64>,
    /// Distinct videos on the talks page.
    pub talks_page_count: usize,
    /// Videos discovery recorded.
    pub recorded_count: usize,
    /// Distinct videos known from any list.
    pub known_count: usize,
    /// On the talks page but not in the account list: usually hidden from the account page.
    pub page_not_in_account: Vec<Entry>,
    /// In the account list but not on the talks page.
    pub account_not_on_page: Vec<Entry>,
    /// Listed somewhere but never recorded: Vimeo would not describe them.
    pub listed_not_recorded: Vec<Entry>,
    /// Recorded, but in no current list: removed or hidden since discovery.
    pub recorded_not_listed: Vec<Entry>,
    /// The podcast feed comparison, when a feed source has episodes.
    pub feed: Option<FeedComparison>,
    /// Why there is no feed comparison, when there is none.
    pub feed_note: String,
}

/// One feed episode, as the completeness check sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedEpisode {
    /// The episode's guid.
    pub id: String,
    /// Title.
    pub title: String,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// The Vimeo talk it matched, if any.
    pub matched_vimeo_id: Option<String>,
}

/// Vimeo against the podcast feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedComparison {
    /// The feed source's name.
    pub source: String,
    /// Episodes in the feed.
    pub episodes: usize,
    /// Episodes matched to a Vimeo talk.
    pub matched: usize,
    /// Live Vimeo talks with no episode: their audio must come from elsewhere.
    pub vimeo_missing_from_feed: Vec<Entry>,
    /// Episodes that matched no Vimeo talk.
    pub episodes_unmatched: Vec<FeedEpisode>,
}

/// Compares the lists. `account` is `None` when no account list is available.
pub fn compare(
    account: Option<&[ListedVideo]>,
    page_ids: &[String],
    recorded: &[RecordedVideo],
    feed: Option<(&str, &[FeedEpisode])>,
    feed_note: &str,
) -> CompletenessReport {
    let account_map: BTreeMap<&str, &ListedVideo> = account
        .unwrap_or_default()
        .iter()
        .map(|v| (v.id.as_str(), v))
        .collect();
    let page: BTreeSet<&str> = page_ids.iter().map(String::as_str).collect();
    let rec: BTreeMap<&str, &RecordedVideo> = recorded.iter().map(|r| (r.id.as_str(), r)).collect();

    let entry = |id: &str| {
        let listed = account_map.get(id);
        let recorded = rec.get(id);
        Entry {
            id: id.to_owned(),
            title: listed
                .and_then(|l| l.title.clone())
                .or_else(|| recorded.map(|r| r.title.clone())),
            duration_s: listed.and_then(|l| l.duration_s),
            state: recorded.map(|r| r.state.clone()),
        }
    };
    let sorted = |ids: BTreeSet<&str>| -> Vec<Entry> {
        let mut entries: Vec<Entry> = ids.into_iter().map(entry).collect();
        // Newest first: Vimeo ids grow over time.
        entries.sort_by_key(|e| std::cmp::Reverse(e.id.parse::<u64>().unwrap_or(0)));
        entries
    };

    let acc: BTreeSet<&str> = account_map.keys().copied().collect();
    let listed: BTreeSet<&str> = acc.union(&page).copied().collect();
    let recorded_ids: BTreeSet<&str> = rec.keys().copied().collect();
    let have_account = account.is_some();

    let account_hours = (have_account
        && !account_map.is_empty()
        && account_map.values().all(|v| v.duration_s.is_some()))
    .then(|| {
        let total: i64 = account_map
            .values()
            .filter_map(|v| v.duration_s)
            .map(i64::from)
            .sum();
        (total as f64 / 360.0).round() / 10.0
    });

    CompletenessReport {
        account_list_source: String::new(),
        account_count: acc.len(),
        account_hours,
        talks_page_count: page.len(),
        recorded_count: recorded_ids.len(),
        known_count: listed.union(&recorded_ids).count(),
        page_not_in_account: if have_account {
            sorted(page.difference(&acc).copied().collect())
        } else {
            Vec::new()
        },
        account_not_on_page: sorted(acc.difference(&page).copied().collect()),
        listed_not_recorded: sorted(listed.difference(&recorded_ids).copied().collect()),
        recorded_not_listed: sorted(recorded_ids.difference(&listed).copied().collect()),
        feed: feed.map(|(source, episodes)| {
            let matched: BTreeSet<&str> = episodes
                .iter()
                .filter_map(|e| e.matched_vimeo_id.as_deref())
                .collect();
            let live: BTreeSet<&str> = rec
                .values()
                .filter(|r| !matches!(r.state.as_str(), "excluded" | "withdrawn"))
                .map(|r| r.id.as_str())
                .collect();
            FeedComparison {
                source: source.to_owned(),
                episodes: episodes.len(),
                matched: matched.len(),
                vimeo_missing_from_feed: sorted(live.difference(&matched).copied().collect()),
                episodes_unmatched: episodes
                    .iter()
                    .filter(|e| e.matched_vimeo_id.is_none())
                    .cloned()
                    .collect(),
            }
        }),
        feed_note: feed_note.to_owned(),
    }
}

/// Seconds as H:MM:SS, or empty when unknown.
fn clock(seconds: Option<i32>) -> String {
    seconds.map_or_else(String::new, |s| {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    })
}

/// A table of entries, or `empty` when there are none.
fn table(out: &mut String, entries: &[Entry], empty: &str) {
    if entries.is_empty() {
        let _ = writeln!(out, r#"<p class="muted">{}</p>"#, escape(empty));
        return;
    }
    out.push_str(
        "<table><tr><th>Vimeo id</th><th>Title</th><th>Length</th><th>Recorded as</th></tr>\n",
    );
    for e in entries {
        let _ = writeln!(
            out,
            r#"<tr><td><a href="https://vimeo.com/{id}" rel="noopener noreferrer">{id}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>"#,
            escape(e.title.as_deref().unwrap_or("")),
            escape(&clock(e.duration_s)),
            escape(e.state.as_deref().unwrap_or("not recorded")),
            id = escape(&e.id),
        );
    }
    out.push_str("</table>\n");
}

/// The report page's inline stylesheet, light and dark, so the page is one file.
const STYLE: &str = "body{font:15px/1.45 system-ui,sans-serif;margin:24px auto;max-width:1100px;\
padding:0 16px;color:#1d1d1f;background:#fff}h1{font-size:24px}h2{font-size:19px;margin-top:32px;\
border-bottom:1px solid #ddd;padding-bottom:4px}table{border-collapse:collapse;width:100%;margin:8px 0}\
th,td{text-align:left;padding:4px 8px;border-bottom:1px solid #eee;vertical-align:top}\
th{font-weight:600;background:#f6f6f6}.muted{color:#666}\
@media (prefers-color-scheme:dark){body{background:#151515;color:#e6e6e6}th{background:#222}\
th,td{border-color:#333}.muted{color:#aaa}a{color:#8ab4f8}}";

/// Renders the report as one self-contained HTML page, like the ingest report.
pub fn render_html(r: &CompletenessReport, generated_at: &str) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Completeness check</title><style>{STYLE}</style></head><body>\n\
         <h1>Completeness check</h1>\n<p class=\"muted\">Generated {}. Account list from {}. \
         This page sets nothing.</p>\n",
        escape(generated_at),
        escape(&r.account_list_source)
    );
    let hours = r
        .account_hours
        .map(|h| format!(" ({h:.1} h)"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "<table><tr><th>Account list</th><th>Talks page</th><th>Recorded</th><th>Known in all</th></tr>\
         <tr><td>{}{}</td><td>{}</td><td>{}</td><td>{}</td></tr></table>",
        r.account_count,
        escape(&hours),
        r.talks_page_count,
        r.recorded_count,
        r.known_count
    );
    let sections: [(&str, &str, &[Entry], &str); 4] = [
        (
            "page-not-in-account",
            "On the talks page, not in the account list",
            &r.page_not_in_account,
            "None, or no account list to compare with.",
        ),
        (
            "account-not-on-page",
            "In the account list, not on the talks page",
            &r.account_not_on_page,
            "None.",
        ),
        (
            "listed-not-recorded",
            "Listed, but not recorded by discovery",
            &r.listed_not_recorded,
            "None.",
        ),
        (
            "recorded-not-listed",
            "Recorded, but in no current list",
            &r.recorded_not_listed,
            "None.",
        ),
    ];
    for (id, heading, entries, empty) in sections {
        let _ = writeln!(
            out,
            "<h2 id=\"{id}\">{} ({})</h2>",
            escape(heading),
            entries.len()
        );
        table(&mut out, entries, empty);
    }
    match &r.feed {
        None => {
            let _ = writeln!(
                out,
                "<h2 id=\"feed\">Podcast feed</h2>\n<p class=\"muted\">{}</p>",
                escape(&r.feed_note)
            );
        }
        Some(f) => {
            let _ = writeln!(
                out,
                "<h2 id=\"feed\">Podcast feed: {}</h2>\n<p>{} episodes, {} matched to a Vimeo talk.</p>",
                escape(&f.source),
                f.episodes,
                f.matched
            );
            let _ = writeln!(
                out,
                "<h2 id=\"vimeo-missing-from-feed\">Vimeo talks missing from the feed ({})</h2>",
                f.vimeo_missing_from_feed.len()
            );
            table(&mut out, &f.vimeo_missing_from_feed, "None.");
            let _ = writeln!(
                out,
                "<h2 id=\"episodes-unmatched\">Feed episodes with no Vimeo match ({})</h2>",
                f.episodes_unmatched.len()
            );
            if f.episodes_unmatched.is_empty() {
                out.push_str("<p class=\"muted\">None.</p>\n");
            } else {
                out.push_str("<table><tr><th>Episode</th><th>Title</th><th>Length</th></tr>\n");
                for e in &f.episodes_unmatched {
                    let _ = writeln!(
                        out,
                        "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                        escape(&e.id),
                        escape(&e.title),
                        escape(&clock(e.duration_s))
                    );
                }
                out.push_str("</table>\n");
            }
        }
    }
    out.push_str("</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two cards in the layout of Vimeo's account videos page, with invented titles.
    const SAVED: &str = r#"<section>
      <article class="Grid"><a href="https://vimeo.com/900000002" aria-label="Video card"></a>
        <span class="chakra-badge css-1">1:02:14</span>
        <a class="chakra-text" href="https://vimeo.com/900000002" data-testid="content-card-title">Talk on &quot;Fixtures&quot; &amp; Tests</a></article>
      <article class="Grid"><a href="https://vimeo.com/900000001" aria-label="Video card"></a>
        <span class="chakra-badge css-1">00:11</span>
        <a class="chakra-text" href="https://vimeo.com/900000001" data-testid="content-card-title">A Short Clip</a></article>
    </section>"#;

    fn listed(id: &str) -> ListedVideo {
        ListedVideo {
            id: id.into(),
            title: Some(format!("title {id}")),
            duration_s: Some(3600),
        }
    }

    fn recorded(id: &str) -> RecordedVideo {
        RecordedVideo {
            id: id.into(),
            title: format!("title {id}"),
            state: "awaiting_consent".into(),
        }
    }

    #[test]
    fn a_saved_account_page_gives_ids_titles_and_lengths() {
        let videos = parse_saved_account_page(SAVED);
        assert_eq!(
            videos,
            [
                ListedVideo {
                    id: "900000002".into(),
                    title: Some("Talk on \"Fixtures\" & Tests".into()),
                    duration_s: Some(3734)
                },
                ListedVideo {
                    id: "900000001".into(),
                    title: Some("A Short Clip".into()),
                    duration_s: Some(11)
                },
            ]
        );
        let ids_only = parse_saved_account_page(r#"<a href="https://vimeo.com/900000003">x</a>"#);
        assert_eq!(
            ids_only,
            [ListedVideo {
                id: "900000003".into(),
                title: None,
                duration_s: None
            }]
        );
        let both = merge_lists([videos.clone(), parse_saved_account_page(SAVED), ids_only]);
        assert_eq!(both.len(), 3, "a video on two saved pages counts once");
    }

    #[test]
    fn each_difference_lands_in_its_section() {
        let account = [listed("10"), listed("11"), listed("12"), listed("13")];
        let page = [
            "11".to_owned(),
            "12".to_owned(),
            "20".to_owned(),
            "21".to_owned(),
        ];
        let rec = [
            recorded("11"),
            recorded("12"),
            recorded("20"),
            recorded("30"),
        ];
        let feed = [
            FeedEpisode {
                id: "ep-a".into(),
                title: "A".into(),
                duration_s: Some(60),
                matched_vimeo_id: Some("11".into()),
            },
            FeedEpisode {
                id: "ep-b".into(),
                title: "B".into(),
                duration_s: None,
                matched_vimeo_id: None,
            },
        ];
        let r = compare(
            Some(&account),
            &page,
            &rec,
            Some(("fixture-podcast", &feed)),
            "",
        );
        let ids = |e: &[Entry]| e.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&r.page_not_in_account),
            ["21", "20"],
            "hidden from the account page"
        );
        assert_eq!(ids(&r.account_not_on_page), ["13", "10"]);
        assert_eq!(ids(&r.listed_not_recorded), ["21", "13", "10"]);
        assert_eq!(ids(&r.recorded_not_listed), ["30"]);
        assert_eq!(
            (
                r.account_count,
                r.talks_page_count,
                r.recorded_count,
                r.known_count
            ),
            (4, 4, 4, 7)
        );
        assert_eq!(r.account_hours, Some(4.0));
        assert_eq!(
            r.page_not_in_account[1].state.as_deref(),
            Some("awaiting_consent")
        );

        let f = r.feed.as_ref().unwrap();
        assert_eq!((f.episodes, f.matched), (2, 1));
        assert_eq!(
            ids(&f.vimeo_missing_from_feed),
            ["30", "20", "12"],
            "every live talk without an episode"
        );
        assert_eq!(f.episodes_unmatched[0].id, "ep-b");

        let without = compare(None, &page, &rec, None, "No feed source is set up yet.");
        assert!(without.feed.is_none());
        assert!(
            without.page_not_in_account.is_empty(),
            "nothing to compare the page with"
        );
        assert_eq!(without.account_hours, None);

        let html = render_html(&r, "2026-10-05 12:00 UTC");
        for id in [
            "page-not-in-account",
            "account-not-on-page",
            "listed-not-recorded",
            "recorded-not-listed",
            "feed",
            "vimeo-missing-from-feed",
            "episodes-unmatched",
        ] {
            assert!(html.contains(&format!("id=\"{id}\"")), "section {id}");
        }
        assert!(!html.to_lowercase().contains("<script"));
    }
}
