//! Answer assembly for the page (Spec 05, Answer assembly; Spec 06, Answer page structure).
//! Passage text comes only from `servable_passage` rows and the templates escape it.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use wt_core::excerpt::{Excerpt, ExcerptPart};
use wt_core::ids::{AnswerId, PassageId, TalkId};
use wt_db::answer::{CitedWork, Payload, Served, TalkCard};

/// `h:mm:ss`, or `m:ss` under an hour.
pub fn clock(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A link the page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The target.
    pub href: String,
    /// The visible text, which says what opens.
    pub label: String,
    /// It opens another site in a new tab.
    pub external: bool,
}

/// A passage's links to the talk (Spec 05, Answer assembly, rule 2 as amended on October 12,
/// 2026, with Spec 03's link styles). These are plain fields so the excerpt's footer row can
/// lay them out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLinks {
    /// "Read the talk's transcript at `<sangha>`" for a talk whose source made its transcript,
    /// with a text fragment; "Read our transcript" at display level `full` otherwise.
    pub read: Option<Link>,
    /// The recording at the excerpt's first word, when the source's player takes a time, or
    /// the talk page for a recording whose player does not.
    pub listen: Option<Link>,
    /// "starts at h:mm:ss", for a source whose player takes no time.
    pub starts_at: Option<String>,
}

impl SourceLinks {
    /// The source's transcript comes first: it is what the reader is sent to.
    pub fn read_first(&self) -> bool {
        self.read.as_ref().is_some_and(|r| r.external)
    }
}

/// The recording at `seconds`, for players that take a time. EngageWisdom's player, a podcast
/// episode (link style `none`) and stored audio take none.
fn timed_href(card: &TalkCard, seconds: i64) -> Option<String> {
    match card.platform.as_str() {
        "youtube" => Some(format!(
            "https://www.youtube.com/watch?v={}&t={seconds}s",
            card.external_id
        )),
        "vimeo" => Some(format!("{}#t={seconds}s", card.source_url)),
        "engagewisdom" | "rss" | "storage" => None,
        _ => Some(format!("{}#t={seconds}", card.source_url)),
    }
}

/// Percent-encodes a text directive term, escaping `-`, `,` and `&`, which the directive
/// reserves, and everything else but letters, digits and `.`, `_`, `~`.
fn fragment_term(words: &[&str]) -> String {
    let mut out = String::new();
    for b in words.join(" ").bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `#:~:text=<first four words>,<last four words>`, or the whole text when it is eight words
/// or fewer, so a browser that supports text fragments opens the page at the excerpt.
pub fn text_fragment(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.len() {
        0 => String::new(),
        n if n <= 8 => format!("#:~:text={}", fragment_term(&words)),
        n => format!(
            "#:~:text={},{}",
            fragment_term(&words[..4]),
            fragment_term(&words[n - 4..])
        ),
    }
}

/// Where the source's own transcript is: its transcript page, else the talk page.
fn source_transcript_url(card: &TalkCard) -> &str {
    card.transcript_url.as_deref().unwrap_or(&card.source_url)
}

/// Rule 2. `text` is the excerpt the page shows, `start_ms` when its first word is spoken and
/// `ordinal` the passage it starts in, for the anchor on this site's transcript.
pub fn source_links(
    card: &TalkCard,
    text: &str,
    start_ms: i32,
    ordinal: i32,
    lead_in_ms: i64,
) -> SourceLinks {
    let seconds = (i64::from(start_ms) - lead_in_ms).max(0) / 1000;
    let at = clock(i64::from(start_ms));
    let timed = timed_href(card, seconds);
    let starts_at = timed.is_none().then(|| format!("starts at {at}"));
    if card.transcript_from_source {
        let base = source_transcript_url(card);
        let base = base.split_once('#').map_or(base, |(b, _)| b);
        return SourceLinks {
            read: Some(Link {
                href: format!("{base}{}", text_fragment(text)),
                label: format!(
                    "Read the talk's transcript at {} (opens their page in a new tab)",
                    card.sangha_name
                ),
                external: true,
            }),
            listen: timed.map(|href| Link {
                href,
                label: format!("Listen at {at} (opens the source in a new tab)"),
                external: true,
            }),
            starts_at,
        };
    }
    let listen = match timed {
        Some(href) => Link {
            href,
            label: format!("Listen at {at} (opens the source in a new tab)"),
            external: true,
        },
        None => Link {
            href: card.source_url.clone(),
            label: "Listen on the talk page (opens the source in a new tab)".to_owned(),
            external: true,
        },
    };
    let made_by = match card.transcript_origin.as_deref() {
        Some("reviewed") => "(reviewed)",
        _ => "(made by machine, may contain errors)",
    };
    SourceLinks {
        read: (card.display_level == "full").then(|| Link {
            href: format!("/talk/{}/transcript#p{ordinal}", card.id),
            label: format!("Read our transcript {made_by}"),
            external: false,
        }),
        listen: Some(listen),
        starts_at,
    }
}

/// Spec 09, Attribution lines, for talks. A mandate line never says permission or licensed.
pub fn credit_line(card: &TalkCard) -> String {
    let base = format!("Talk by {}, {}.", card.teacher_name, card.sangha_name);
    match card.rights_basis.as_deref() {
        Some("speaker_consent") => format!("{base} Used with permission of {}.", card.teacher_name),
        Some("license") => match &card.display_notice {
            Some(notice) => format!("{} {notice}", card.title),
            None => base,
        },
        _ => base,
    }
}

/// The give link exists only for a listed sangha with a verified donation link (rule 3).
pub fn gives(card: &TalkCard) -> bool {
    card.sangha_listed && card.donation_verified && card.donation_url.is_some()
}

/// The metadata line under a passage: title, date, series and part.
pub fn meta(card: &TalkCard) -> String {
    let mut parts = vec![card.title.clone()];
    if let Some(d) = card.date {
        parts.push(d.format("%B %-d, %Y").to_string());
    }
    match (&card.series, card.part) {
        (Some(s), Some(p)) => parts.push(format!("{s}, part {p}")),
        (Some(s), None) => parts.push(s.clone()),
        _ => {}
    }
    parts.join(" · ")
}

/// One passage as the page shows it.
#[derive(Debug, Clone)]
pub struct PassageView {
    /// The passage.
    pub id: PassageId,
    /// Its number on the page, from 1 in reading order, for feedback and chips.
    pub number: usize,
    /// The excerpt cut for the question, verbatim (Spec 05, Answer assembly, rule 8).
    pub text: String,
    /// The same text split at footnote markers (rule 4).
    pub runs: Vec<TextRun>,
    /// Title, date, series and part.
    pub meta: String,
    /// The talk's References entry, `#ref-<n>`, which the metadata line links to.
    pub reference: usize,
    /// The talk's MLA citation as plain text, for the excerpt's Cite control.
    pub cite: String,
    /// Read, Listen and "starts at" (rule 2).
    pub links: SourceLinks,
    /// "Give to `<sangha>`" target, when eligible.
    pub give_href: Option<String>,
    /// The credit line.
    pub credit: String,
    /// The student's question this passage answers, lightly edited, shown above it as context
    /// and never as a quotation (Spec 04, Student questions).
    pub question: Option<String>,
}

/// The line under every student question.
pub const QUESTION_LABEL: &str = wt_core::transcript::STUDENT_QUESTION_LABEL;

/// Puts each question above the passage it stands for.
pub fn attach_questions(view: &mut AnswerView, questions: &[wt_db::questions::ShownQuestion]) {
    for p in view.groups.iter_mut().flat_map(|g| g.passages.iter_mut()) {
        p.question = questions
            .iter()
            .find(|q| q.passage_id == p.id)
            .map(|q| q.text.clone());
    }
}

/// One teacher's passages.
#[derive(Debug, Clone)]
pub struct Group {
    /// Teacher name.
    pub teacher: String,
    /// Sangha name.
    pub sangha: String,
    /// Passages, best first.
    pub passages: Vec<PassageView>,
}

/// One verified synthesis theme: model text, shown only in "How approaches differ".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeLine {
    /// The sentence.
    pub description: String,
    /// The page numbers of the passages that support it, for its footnote chips.
    pub chips: Vec<usize>,
}

/// The assembled answer.
#[derive(Debug, Clone)]
pub struct AnswerView {
    /// The result line under the question.
    pub result_line: String,
    /// The notice banner, from Spec 09's messages.
    pub notice: Option<String>,
    /// Verified themes of an answer stored before the summary; left out when empty.
    pub themes: Vec<ThemeLine>,
    /// The summary block (Spec 05, Summary and verifier), set by [`crate::summary::build`].
    pub summary: Option<crate::summary::SummaryView>,
    /// Teacher groups, ordered by their best passage.
    pub groups: Vec<Group>,
    /// Passages that no longer serve.
    pub withdrawn: usize,
    /// The References list, in MLA style and alphabetical order (rule 5).
    pub references: Vec<RefEntry>,
    /// The reranker was unavailable for this answer.
    pub degraded: bool,
}

/// What was read back for one stored answer, the input to [`assemble`].
#[derive(Debug, Clone, Copy)]
pub struct AnswerRows<'a> {
    /// The answer being shown.
    pub answer: AnswerId,
    /// Its stored payload.
    pub payload: &'a Payload,
    /// Its passages still served, through the serving view.
    pub served: &'a [Served],
    /// The rebuilt excerpt of each passage that has one.
    pub excerpts: &'a BTreeMap<PassageId, Excerpt>,
    /// The resolved and probable citations of the passages the excerpts cover.
    pub cited: &'a [CitedWork],
    /// The teacher the question named, if any.
    pub named_teacher: Option<&'a str>,
    /// The day the answer was made, the References' access date.
    pub accessed: NaiveDate,
}

/// Groups the served passages by teacher in payload order, which is best first, so groups
/// are ordered by their best passage and passages inside a group by score (rule 1). Each shows
/// its excerpt; "Listen at" and the transcript anchor follow the excerpt's first word (rule 8),
/// starting `lead_in_ms` early.
pub fn assemble(rows: &AnswerRows<'_>, lead_in_ms: i64) -> AnswerView {
    let AnswerRows {
        answer,
        payload,
        served,
        excerpts,
        cited,
        named_teacher,
        accessed,
    } = *rows;
    let mut groups: Vec<Group> = Vec::new();
    let mut entries: BTreeMap<RefKey, RefEntry> = BTreeMap::new();
    // Per passage: its talk's key, then each footnote's offset and key.
    let mut keyed: BTreeMap<PassageId, (RefKey, Vec<(usize, RefKey)>)> = BTreeMap::new();
    for s in served {
        let card = &s.talk;
        let whole;
        let (text, start_ms, ordinal, parts) = match excerpts.get(&s.id) {
            Some(e) => (&e.text, e.start_ms, e.first_ordinal, e.parts.as_slice()),
            None => {
                whole = [ExcerptPart {
                    passage_id: s.id,
                    passage_from: 0,
                    passage_to: s.text.chars().count(),
                    excerpt_from: 0,
                }];
                (&s.text, s.start_ms, s.ordinal, whole.as_slice())
            }
        };
        let talk_key = RefKey::Talk(card.id);
        let talk_ref = entries
            .entry(talk_key.clone())
            .or_insert_with(|| talk_entry(card, accessed));
        let cite = talk_ref.plain();
        let notes = marks(s.id, parts, text.chars().count(), cited)
            .into_iter()
            .map(|(at, i)| {
                let c = &cited[i];
                let key = RefKey::Work(c.work_id, c.locator.clone());
                entries.entry(key.clone()).or_insert_with(|| text_entry(c));
                (at, key)
            })
            .collect();
        keyed.insert(s.id, (talk_key, notes));
        let view = PassageView {
            id: s.id,
            number: 0,
            text: text.clone(),
            runs: Vec::new(),
            meta: meta(card),
            reference: 0,
            cite,
            links: source_links(card, text, start_ms, ordinal, lead_in_ms),
            give_href: gives(card).then(|| format!("/give/{answer}/{}", card.sangha_id)),
            credit: credit_line(card),
            question: None,
        };
        match groups.iter_mut().find(|g| g.teacher == card.teacher_name) {
            Some(g) => g.passages.push(view),
            None => groups.push(Group {
                teacher: card.teacher_name.clone(),
                sangha: card.sangha_name.clone(),
                passages: vec![view],
            }),
        }
    }
    // References sort alphabetically, as MLA's works cited do, and are numbered in that order.
    let mut references: Vec<(RefKey, RefEntry)> = entries.into_iter().collect();
    references.sort_by(|a, b| a.1.sort_key.cmp(&b.1.sort_key).then(a.0.cmp(&b.0)));
    for (n, (_, e)) in references.iter_mut().enumerate() {
        e.number = n + 1;
    }
    let number_of = |key: &RefKey| {
        references
            .iter()
            .find(|(k, _)| k == key)
            .map_or(0, |(_, e)| e.number)
    };
    // Number excerpts in reading order, group by group, so the page counts 1, 2, 3 downwards,
    // and footnote markers in the same order (rule 4).
    let mut footnote = 0;
    for (n, p) in groups
        .iter_mut()
        .flat_map(|g| g.passages.iter_mut())
        .enumerate()
    {
        p.number = n + 1;
        if let Some((talk_key, notes)) = keyed.get(&p.id) {
            p.reference = number_of(talk_key);
            p.runs = split_runs(
                &p.text,
                notes.iter().map(|(at, key)| {
                    footnote += 1;
                    (
                        *at,
                        Marker {
                            number: footnote,
                            reference: number_of(key),
                        },
                    )
                }),
            );
        }
    }
    // A theme renders only while every passage it cites still serves on this page.
    let numbers: Vec<(PassageId, usize)> = groups
        .iter()
        .flat_map(|g| g.passages.iter().map(|p| (p.id, p.number)))
        .collect();
    let themes = payload
        .themes
        .iter()
        .filter_map(|t| {
            let chips: Option<Vec<usize>> = t
                .passage_ids
                .iter()
                .map(|id| numbers.iter().find(|(p, _)| p == id).map(|(_, n)| *n))
                .collect();
            Some(ThemeLine {
                description: t.description.clone(),
                chips: chips?,
            })
        })
        .collect();
    let teachers = groups.len();
    let result_line = match teachers {
        0 => "No excerpts found".to_owned(),
        1 => "Passages from 1 teacher".to_owned(),
        n => format!("Passages from {n} teachers"),
    };
    let name = named_teacher.unwrap_or("this teacher");
    let notice = match payload.notice.as_deref() {
        Some("none_found") => Some(
            "We found no excerpts that match this question well enough. Try describing the \
             situation in different words."
                .to_owned(),
        ),
        Some("thin_coverage") => Some(format!(
            "Found excerpts from {} teachers for this question.",
            payload.notice_count.unwrap_or(0)
        )),
        Some("named_none") => Some(format!("This collection has no excerpts from {name}.")),
        Some("named_few") => Some(format!(
            "This collection has {} excerpts from {name}.",
            payload.notice_count.unwrap_or(0)
        )),
        Some("multi_named") => {
            Some("Searching by teacher works with one teacher at a time.".to_owned())
        }
        _ => None,
    };
    AnswerView {
        result_line,
        notice,
        themes,
        summary: None,
        groups,
        withdrawn: payload.passage_ids.len().saturating_sub(served.len()),
        references: references.into_iter().map(|(_, e)| e).collect(),
        degraded: payload.rerank_degraded,
    }
}

// ---- References (Spec 05, Answer assembly, rule 5) ------------------------------------------

/// MLA's date: `3 Mar. 1970`, with May, June and July written out and September as `Sept.`.
pub fn mla_date(d: NaiveDate) -> String {
    const MONTHS: [&str; 12] = [
        "Jan.", "Feb.", "Mar.", "Apr.", "May", "June", "July", "Aug.", "Sept.", "Oct.", "Nov.",
        "Dec.",
    ];
    let month = usize::try_from(d.month0()).map_or("", |m| MONTHS[m % 12]);
    format!("{} {month} {}", d.day(), d.year())
}

/// An address as MLA prints it: without the scheme.
fn bare_url(url: &str) -> &str {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
}

/// Ends an element with a period unless it already ends with a mark of its own.
fn closed(s: &str) -> String {
    let s = s.trim();
    if s.ends_with(['.', '?', '!']) {
        s.to_owned()
    } else {
        format!("{s}.")
    }
}

/// The website a talk is on, MLA's container: the platform's name, or the sangha's for a
/// center's own archive.
fn site_name(card: &TalkCard) -> &str {
    match card.platform.as_str() {
        "youtube" => "YouTube",
        "vimeo" => "Vimeo",
        _ => &card.sangha_name,
    }
}

/// One entry of the References list. The work or site title is set apart so the template can
/// italicize it without marking up stored text; everything is escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    /// Its place in the list, from 1: the target `#ref-<n>`.
    pub number: usize,
    /// The text before the italic title.
    pub before: String,
    /// The italic title: the website for a talk, the work for a text.
    pub italic: String,
    /// The text after it.
    pub after: String,
    /// For a text: `resolved` or `probable`, shown after the entry in words.
    pub confidence: Option<String>,
    /// What the list sorts by.
    sort_key: String,
}

impl RefEntry {
    /// The entry as plain text, for copying.
    pub fn plain(&self) -> String {
        format!("{}{}{}", self.before, self.italic, self.after)
    }
}

/// The MLA entry of a talk. The URL is the source transcript for a talk whose source made it,
/// and otherwise the recording; the access date is the day the answer was made.
pub fn talk_entry(card: &TalkCard, accessed: NaiveDate) -> RefEntry {
    let name = card.citation_name.as_deref().unwrap_or(&card.teacher_name);
    let mut before = format!("{} \"{}\" ", closed(name), closed(&card.title));
    match (&card.series, card.part) {
        (Some(s), Some(p)) => before.push_str(&format!("{s}, part {p}. ")),
        (Some(s), None) => before.push_str(&format!("{} ", closed(s))),
        _ => {}
    }
    before.push_str("Dharma talk. ");
    let site = site_name(card);
    let mut after = Vec::new();
    if card.sangha_name != site {
        after.push(card.sangha_name.clone());
    }
    if let Some(d) = card.date {
        after.push(mla_date(d));
    }
    let url = if card.transcript_from_source {
        source_transcript_url(card)
    } else {
        &card.source_url
    };
    after.push(bare_url(url).to_owned());
    let sort = card
        .citation_sort
        .as_deref()
        .or(card.citation_name.as_deref())
        .unwrap_or(&card.teacher_name);
    RefEntry {
        number: 0,
        before,
        italic: site.to_owned(),
        after: format!(
            ", {} Accessed {}.",
            closed(&after.join(", ")),
            mla_date(accessed)
        ),
        confidence: None,
        sort_key: format!("{}\u{0}{}", sort.to_lowercase(), card.title.to_lowercase()),
    }
}

/// The MLA entry of a cited work and locator; the work-level form has no locator. Elements
/// with no value are left out.
pub fn text_entry(c: &CitedWork) -> RefEntry {
    let before = c.author.as_deref().map(|a| format!("{} ", closed(a)));
    let mut tail: Vec<String> = Vec::new();
    if let Some(t) = &c.translator {
        tail.push(format!("Translated by {t}"));
    }
    if c.locator != WORK_LEVEL {
        tail.push(c.label.clone());
    }
    let mut after = ".".to_owned();
    if !tail.is_empty() {
        after.push(' ');
        after.push_str(&closed(&tail.join(", ")));
    }
    if let Some(u) = &c.link_out_url {
        after.push(' ');
        after.push_str(&closed(bare_url(u)));
    }
    let sort = c.author.as_deref().unwrap_or(&c.work_title);
    RefEntry {
        number: 0,
        before: before.unwrap_or_default(),
        italic: c.work_title.trim_end_matches('.').to_owned(),
        after,
        confidence: Some(c.confidence.clone()),
        sort_key: format!(
            "{}\u{0}{}\u{0}{}",
            sort.to_lowercase(),
            c.work_title.to_lowercase(),
            c.locator
        ),
    }
}

/// The work-level locator (Spec 08, Work-level citations).
const WORK_LEVEL: &str = "*";

/// What a References entry stands for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RefKey {
    Talk(TalkId),
    Work(sqlx::types::Uuid, String),
}

/// A run of an excerpt: verbatim text, then a footnote marker when a citation ends there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// Escaped by the template.
    pub text: String,
    /// The marker after it.
    pub marker: Option<Marker>,
}

/// A footnote marker (rule 4), numbered in order of appearance across the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    /// Its number.
    pub number: usize,
    /// The References entry it links to.
    pub reference: usize,
}

/// Where the citations of the passages an excerpt covers fall in the excerpt's text, in
/// characters, with the work each cites. A citation outside the shown words has no marker and
/// no entry; one with no span, in the selected passage, is marked at the end.
fn marks(
    selected: PassageId,
    parts: &[ExcerptPart],
    text_chars: usize,
    cited: &[CitedWork],
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, c) in cited.iter().enumerate() {
        let at = match (c.span_start, c.span_end) {
            (Some(s), Some(e)) => {
                let (s, e) = (
                    usize::try_from(s).unwrap_or(usize::MAX),
                    usize::try_from(e).unwrap_or(usize::MAX),
                );
                parts.iter().find_map(|p| {
                    (p.passage_id == c.passage_id && s >= p.passage_from && e <= p.passage_to)
                        .then(|| p.excerpt_from + (e - p.passage_from))
                })
            }
            _ => (c.passage_id == selected).then_some(text_chars),
        };
        if let Some(at) = at {
            out.push((at.min(text_chars), i));
        }
    }
    out.sort_unstable();
    out
}

/// Splits text into runs at the given character offsets, each run followed by its marker.
fn split_runs(text: &str, marks: impl Iterator<Item = (usize, Marker)>) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut chars = text.chars();
    let mut at = 0;
    for (offset, marker) in marks {
        let take = offset.saturating_sub(at);
        runs.push(TextRun {
            text: chars.by_ref().take(take).collect(),
            marker: Some(marker),
        });
        at += take;
    }
    let tail: String = chars.collect();
    if !tail.is_empty() || runs.is_empty() {
        runs.push(TextRun {
            text: tail,
            marker: None,
        });
    }
    runs
}

// ---- dana panel (Spec 05, Dana calculator; Spec 06, Answer page structure, item 8) ----------

/// One sangha's line of the split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaLineView {
    /// The sangha.
    pub name: String,
    /// Its share, in dollars with two decimals.
    pub amount: String,
    /// The app's `/give` route, which logs the click and redirects.
    pub href: String,
}

/// The site's own line, separate from the sanghas'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteLineView {
    /// The amount, in dollars with two decimals.
    pub amount: String,
    /// The app's `/give/<answer>/site` route.
    pub href: String,
}

/// What the dana result region shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DanaView {
    /// One line per eligible sangha, in the calculator's order.
    pub lines: Vec<DanaLineView>,
    /// The site line, when the donor entered an amount and the site has a giving page.
    pub site: Option<SiteLineView>,
    /// Everything the donor would give.
    pub outlay: String,
    /// "Sangha: note" for each cited sangha left out that has a note, such as a contact page.
    pub notes: Vec<String>,
    /// A problem with what was entered, shown instead of a split.
    pub error: Option<String>,
}

impl DanaView {
    /// A view that shows only a message.
    pub fn message(text: &str) -> Self {
        Self {
            error: Some(text.to_owned()),
            ..Self::default()
        }
    }
}

/// The calculator's recipients: each cited sangha, as its registry row has it.
pub fn recipients(sanghas: &[wt_db::answer::DanaSangha]) -> Vec<wt_dana::Recipient> {
    sanghas
        .iter()
        .map(|s| wt_dana::Recipient {
            sangha_id: s.id,
            name: s.name.clone(),
            url: s.donation_url.clone(),
            url_template: s.donation_url_template.clone(),
            verified: s.donation_verified,
            listed: s.listed,
        })
        .collect()
}

/// The tiebreak seed for an answer, so its leftover cents always land the same way.
pub fn dana_seed(answer: AnswerId) -> u64 {
    wt_dana::seed_from(answer.as_uuid().as_bytes())
}

/// Turns a computed split into what the page shows. Lines link to the app's `/give` route with
/// the share, which the route uses only to fill a verified page's amount template.
pub fn dana_view(
    answer: AnswerId,
    split: &wt_dana::DanaSplit,
    sanghas: &[wt_db::answer::DanaSangha],
    site_giving: bool,
) -> DanaView {
    let lines = split
        .lines
        .iter()
        .map(|l| DanaLineView {
            name: l.name.clone(),
            amount: wt_dana::dollars(l.cents),
            href: format!("/give/{answer}/{}?amount_cents={}", l.sangha_id, l.cents),
        })
        .collect();
    let site = (site_giving && split.site_cents > 0).then(|| SiteLineView {
        amount: wt_dana::dollars(split.site_cents),
        href: format!("/give/{answer}/site"),
    });
    let notes = sanghas
        .iter()
        .filter(|s| s.listed && split.excluded.contains(&s.id))
        .filter_map(|s| s.donation_note.as_ref().map(|n| format!("{}: {n}", s.name)))
        .collect();
    DanaView {
        lines,
        site,
        outlay: wt_dana::dollars(split.total_outlay_cents),
        notes,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use sqlx::types::Uuid;
    use wt_core::ids::{SanghaId, TeacherId};

    fn card(platform: &str, basis: &str) -> TalkCard {
        TalkCard {
            id: TalkId::from_uuid(Uuid::from_u128(1)),
            title: "On sitting".into(),
            date: None,
            series: None,
            part: None,
            platform: platform.into(),
            external_id: "abc".into(),
            source_url: "https://example.invalid/talk".into(),
            teacher_id: TeacherId::from_uuid(Uuid::from_u128(2)),
            teacher_name: "Ada Fixture".into(),
            sangha_id: SanghaId::from_uuid(Uuid::from_u128(3)),
            sangha_name: "North Fixture Zendo".into(),
            sangha_listed: true,
            donation_url: Some("https://example.invalid/give".into()),
            donation_verified: false,
            rights_basis: Some(basis.into()),
            display_notice: None,
            display_level: "full".into(),
            transcript_from_source: false,
            transcript_url: None,
            transcript_origin: Some("asr".into()),
            citation_name: None,
            citation_sort: None,
        }
    }

    const EXCERPT: &str = "When the mind wanders, come back - gently, without blame & go on.";

    #[test]
    fn source_links_follow_the_platform_with_the_lead_in() {
        let l = source_links(&card("youtube", "mandate"), EXCERPT, 65_000, 4, 2000);
        let listen = l.listen.unwrap();
        assert_eq!(listen.href, "https://www.youtube.com/watch?v=abc&t=63s");
        assert_eq!(
            listen.label,
            "Listen at 1:05 (opens the source in a new tab)"
        );
        assert_eq!(l.starts_at, None);
        let l = source_links(&card("vimeo", "mandate"), EXCERPT, 1000, 4, 2000);
        assert_eq!(l.listen.unwrap().href, "https://example.invalid/talk#t=0s");
    }

    #[test]
    fn link_styles_without_a_timed_player_say_where_the_excerpt_starts() {
        // Spec 03: `engagewisdom`, and `none` for a talk with only a podcast match.
        for platform in ["engagewisdom", "rss"] {
            let l = source_links(&card(platform, "mandate"), EXCERPT, 3_725_000, 4, 2000);
            let listen = l.listen.unwrap();
            assert_eq!(listen.href, "https://example.invalid/talk", "{platform}");
            assert!(!listen.href.contains("#t="), "{platform}");
            assert_eq!(l.starts_at.as_deref(), Some("starts at 1:02:05"));
        }
    }

    #[test]
    fn a_machine_transcript_links_the_recording_and_our_transcript_at_full() {
        let l = source_links(&card("youtube", "mandate"), EXCERPT, 65_000, 4, 2000);
        assert!(!l.read_first());
        let read = l.read.unwrap();
        assert_eq!(
            read.href,
            "/talk/00000000-0000-0000-0000-000000000001/transcript#p4"
        );
        assert_eq!(
            read.label,
            "Read our transcript (made by machine, may contain errors)"
        );
        assert!(!read.external);
        let mut c = card("youtube", "mandate");
        c.transcript_origin = Some("reviewed".into());
        let read = source_links(&c, EXCERPT, 0, 4, 2000).read.unwrap();
        assert_eq!(read.label, "Read our transcript (reviewed)");
        c.display_level = "excerpts".into();
        assert_eq!(source_links(&c, EXCERPT, 0, 4, 2000).read, None);
    }

    #[test]
    fn a_source_transcript_links_the_source_with_a_text_fragment() {
        let mut c = card("engagewisdom", "mandate");
        c.transcript_from_source = true;
        c.transcript_origin = Some("aligned".into());
        c.display_level = "excerpts".into();
        let l = source_links(&c, EXCERPT, 125_000, 4, 2000);
        assert!(l.read_first());
        let read = l.read.unwrap();
        assert_eq!(
            read.href,
            "https://example.invalid/talk#:~:text=When%20the%20mind%20wanders%2C,blame%20%26%20go%20on."
        );
        assert_eq!(
            read.label,
            "Read the talk's transcript at North Fixture Zendo (opens their page in a new tab)"
        );
        assert!(read.external);
        assert_eq!(l.listen, None, "EngageWisdom's player takes no time");
        assert_eq!(l.starts_at.as_deref(), Some("starts at 2:05"));

        // A separate transcript page wins over the talk page, and a timed player adds Listen.
        let mut c = card("youtube", "mandate");
        c.transcript_from_source = true;
        c.transcript_url = Some("https://example.invalid/transcripts/7#top".into());
        let l = source_links(&c, "Just sit - and breathe.", 65_000, 4, 2000);
        let read = l.read.unwrap();
        assert_eq!(
            read.href,
            "https://example.invalid/transcripts/7#:~:text=Just%20sit%20%2D%20and%20breathe."
        );
        assert!(
            !read.href.contains("/transcript#p"),
            "never this site's transcript"
        );
        assert_eq!(
            l.listen.unwrap().href,
            "https://www.youtube.com/watch?v=abc&t=63s"
        );
        assert_eq!(l.starts_at, None);
    }

    #[test]
    fn mla_dates_abbreviate_months_as_mla_does() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        assert_eq!(mla_date(d(3, 3)), "3 Mar. 2026");
        assert_eq!(mla_date(d(6, 1)), "1 June 2026");
        assert_eq!(mla_date(d(9, 30)), "30 Sept. 2026");
    }

    #[test]
    fn a_talk_entry_follows_mla_with_the_citation_name() {
        let mut c = card("youtube", "mandate");
        c.date = NaiveDate::from_ymd_opt(1970, 3, 3);
        c.series = Some("Sesshin".into());
        c.part = Some(2);
        let accessed = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let e = talk_entry(&c, accessed);
        assert_eq!(
            e.plain(),
            "Ada Fixture. \"On sitting.\" Sesshin, part 2. Dharma talk. YouTube, North Fixture \
             Zendo, 3 Mar. 1970, example.invalid/talk. Accessed 12 Oct. 2026."
        );
        assert_eq!(e.italic, "YouTube");
        c.citation_name = Some("Fixture Roshi".into());
        c.platform = "engagewisdom".into();
        c.transcript_from_source = true;
        c.transcript_url = Some("https://example.invalid/t/1".into());
        c.series = None;
        assert_eq!(
            talk_entry(&c, accessed).plain(),
            "Fixture Roshi. \"On sitting.\" Dharma talk. North Fixture Zendo, 3 Mar. 1970, \
             example.invalid/t/1. Accessed 12 Oct. 2026."
        );
    }

    #[test]
    fn a_text_entry_leaves_out_what_has_no_value() {
        let c = CitedWork {
            passage_id: PassageId::from_uuid(Uuid::from_u128(9)),
            span_start: Some(0),
            span_end: Some(4),
            confidence: "probable".into(),
            work_id: Uuid::from_u128(10),
            work_title: "Fixture Sutra".into(),
            author: Some("Old Fixture".into()),
            translator: Some("A. Translator".into()),
            locator: "fixture:5".into(),
            label: "chapter 5".into(),
            link_out_url: Some("https://example.invalid/sutra".into()),
        };
        let e = text_entry(&c);
        assert_eq!(
            e.plain(),
            "Old Fixture. Fixture Sutra. Translated by A. Translator, chapter 5. \
             example.invalid/sutra."
        );
        assert_eq!(e.confidence.as_deref(), Some("probable"));
        let whole = CitedWork {
            author: None,
            translator: None,
            locator: "*".into(),
            link_out_url: None,
            ..c
        };
        assert_eq!(text_entry(&whole).plain(), "Fixture Sutra.");
    }

    #[test]
    fn runs_split_at_markers() {
        let m = |n| Marker {
            number: n,
            reference: 3,
        };
        let runs = split_runs("abcdef", [(2, m(1)), (6, m(2))].into_iter());
        let texts: Vec<_> = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["ab", "cdef"]);
        assert_eq!(runs[1].marker, Some(m(2)));
        assert_eq!(split_runs("", std::iter::empty()).len(), 1);
    }

    #[test]
    fn a_mandate_credit_line_never_claims_permission() {
        let line = credit_line(&card("youtube", "mandate"));
        assert_eq!(line, "Talk by Ada Fixture, North Fixture Zendo.");
        assert!(!line.contains("permission") && !line.contains("licensed"));
        assert!(credit_line(&card("youtube", "speaker_consent")).contains("Used with permission"));
    }

    #[test]
    fn no_give_link_without_a_verified_listed_sangha() {
        let mut c = card("youtube", "mandate");
        assert!(!gives(&c), "unverified");
        c.donation_verified = true;
        assert!(gives(&c));
        c.sangha_listed = false;
        assert!(!gives(&c), "unlisted");
    }

    #[test]
    fn clock_formats_hours_minutes_and_seconds() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(61_000), "1:01");
        assert_eq!(clock(3_661_000), "1:01:01");
    }
}
