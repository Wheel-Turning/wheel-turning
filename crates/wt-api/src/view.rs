//! Answer assembly for the page (Spec 05, Answer assembly; Spec 06, Answer page structure).
//! Passage text comes only from `servable_passage` rows and the templates escape it.

use wt_core::ids::{AnswerId, PassageId, TalkId};
use wt_db::answer::{Payload, Served, TalkCard};

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

/// Where a passage's source link goes and what it says (rule 2, as Spec 03's link styles
/// amend it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLink {
    /// The link target.
    pub href: String,
    /// The visible link text, which says what opens.
    pub label: String,
    /// For sources whose player takes no time: "starts at h:mm:ss" beside the link.
    pub starts_at: Option<String>,
}

/// Rule 2: YouTube and Vimeo take a time; EngageWisdom's player does not, so the page links
/// the talk and says where the passage starts.
pub fn source_link(card: &TalkCard, start_ms: i32, lead_in_ms: i64) -> SourceLink {
    let from = (i64::from(start_ms) - lead_in_ms).max(0);
    let seconds = from / 1000;
    let at = clock(i64::from(start_ms));
    let timed = |href: String| SourceLink {
        href,
        label: format!("Listen at {at} (opens the source in a new tab)"),
        starts_at: None,
    };
    match card.platform.as_str() {
        "youtube" => timed(format!(
            "https://www.youtube.com/watch?v={}&t={seconds}s",
            card.external_id
        )),
        "vimeo" => timed(format!("{}#t={seconds}s", card.source_url)),
        "engagewisdom" => SourceLink {
            href: card.source_url.clone(),
            label: "Listen on the talk page (opens the source in a new tab)".to_owned(),
            starts_at: Some(format!("starts at {at}")),
        },
        _ => timed(format!("{}#t={seconds}", card.source_url)),
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
    /// Verbatim text.
    pub text: String,
    /// Title, date, series and part.
    pub meta: String,
    /// The source link.
    pub source: SourceLink,
    /// "Read in the transcript" target, at display level `full` only.
    pub transcript_href: Option<String>,
    /// "Give to <sangha>" target, when eligible.
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

/// A talk in the "Talks cited" list.
#[derive(Debug, Clone)]
pub struct TalkLine {
    /// The talk page.
    pub href: String,
    /// Title.
    pub title: String,
    /// Teacher.
    pub teacher: String,
    /// Date, when known.
    pub date: Option<String>,
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
    /// Verified themes; the section is left out when empty.
    pub themes: Vec<ThemeLine>,
    /// Teacher groups, ordered by their best passage.
    pub groups: Vec<Group>,
    /// Passages that no longer serve.
    pub withdrawn: usize,
    /// Talks cited.
    pub talks: Vec<TalkLine>,
    /// The reranker was unavailable for this answer.
    pub degraded: bool,
}

/// Groups the served passages by teacher in payload order, which is best first, so groups
/// are ordered by their best passage and passages inside a group by score (rule 1).
pub fn assemble(
    answer: AnswerId,
    payload: &Payload,
    served: &[Served],
    named_teacher: Option<&str>,
    lead_in_ms: i64,
) -> AnswerView {
    let mut groups: Vec<Group> = Vec::new();
    let mut talks: Vec<TalkLine> = Vec::new();
    let mut seen_talks: Vec<TalkId> = Vec::new();
    for s in served {
        let card = &s.talk;
        let view = PassageView {
            id: s.id,
            number: 0,
            text: s.text.clone(),
            meta: meta(card),
            source: source_link(card, s.start_ms, lead_in_ms),
            transcript_href: (card.display_level == "full")
                .then(|| format!("/talk/{}/transcript#p{}", card.id, s.ordinal)),
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
        if !seen_talks.contains(&card.id) {
            seen_talks.push(card.id);
            talks.push(TalkLine {
                href: format!("/talk/{}", card.id),
                title: card.title.clone(),
                teacher: card.teacher_name.clone(),
                date: card.date.map(|d| d.format("%B %-d, %Y").to_string()),
            });
        }
    }
    // Number excerpts in reading order, group by group, so the page counts 1, 2, 3 downwards.
    for (n, p) in groups
        .iter_mut()
        .flat_map(|g| g.passages.iter_mut())
        .enumerate()
    {
        p.number = n + 1;
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
        groups,
        withdrawn: payload.passage_ids.len().saturating_sub(served.len()),
        talks,
        degraded: payload.rerank_degraded,
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
        }
    }

    #[test]
    fn source_links_follow_the_platform_with_the_lead_in() {
        let l = source_link(&card("youtube", "mandate"), 65_000, 2000);
        assert_eq!(l.href, "https://www.youtube.com/watch?v=abc&t=63s");
        assert_eq!(l.label, "Listen at 1:05 (opens the source in a new tab)");
        let l = source_link(&card("vimeo", "mandate"), 1000, 2000);
        assert_eq!(l.href, "https://example.invalid/talk#t=0s");
        let l = source_link(&card("engagewisdom", "mandate"), 3_725_000, 2000);
        assert_eq!(l.href, "https://example.invalid/talk");
        assert_eq!(l.starts_at.as_deref(), Some("starts at 1:02:05"));
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
