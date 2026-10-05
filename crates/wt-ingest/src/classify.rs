//! Classification rules 1 to 5 of Spec 03 as pure functions: title normalization, the non-talk
//! filter, review holds, series and part, talk type, and the duplicate test. Rule 6, the
//! speaker, lives with the registry in `wt_db::registry::assign_speaker`.

use unicode_normalization::UnicodeNormalization;

use crate::config::ClassifyConfig;

/// Rule 1: Unicode NFKC, lowercase, punctuation replaced by a space, spaces collapsed, so
/// `Part-3` and `Shobogenzo_Uji` keep their words apart (decision of October 5, 2026). The SQL
/// in `wt_db::ingest::duplicate_candidates` must give the same result, since rule 3 compares
/// them.
pub fn normalize_title(title: &str) -> String {
    let folded: String = title.nfkc().collect::<String>().to_lowercase();
    let kept: String = folded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Why a video is kept out of the pipeline at classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exclusion {
    /// A title pattern marks it as a clip, quote or trailer. Reason `non_talk`.
    NonTalk,
    /// Shorter than `min_talk_seconds`. Reason `too_short`.
    TooShort,
    /// A session excluded by decision, whatever its consent. Reason `excluded_session`.
    Session,
}

impl Exclusion {
    /// The `state_reason` recorded on the talk.
    pub fn reason(&self) -> &'static str {
        match self {
            Exclusion::NonTalk => "non_talk",
            Exclusion::TooShort => "too_short",
            Exclusion::Session => "excluded_session",
        }
    }

    /// The `talk_type` recorded with the exclusion, if the rule sets one.
    pub fn talk_type(&self) -> Option<&'static str> {
        match self {
            Exclusion::NonTalk | Exclusion::TooShort => Some("non_talk"),
            Exclusion::Session => None,
        }
    }
}

/// Rule 2. A title pattern wins over duration, so the report says which rule applied. A video
/// with no known duration is not excluded for length.
pub fn exclusion(
    cfg: &ClassifyConfig,
    normalized: &str,
    duration_s: Option<i32>,
) -> Option<Exclusion> {
    if cfg.exclude_session.iter().any(|p| p.is_match(normalized)) {
        return Some(Exclusion::Session);
    }
    if cfg.exclude_title.iter().any(|p| p.is_match(normalized)) {
        return Some(Exclusion::NonTalk);
    }
    match duration_s {
        Some(d) if d < cfg.settings.min_talk_seconds => Some(Exclusion::TooShort),
        _ => None,
    }
}

/// Rule 4: the first series whose pattern matches, with its part number.
pub fn series(cfg: &ClassifyConfig, normalized: &str) -> Option<(String, Option<i32>)> {
    cfg.series.iter().find_map(|s| {
        let caps = s.pattern.captures(normalized)?;
        let part = caps.name("part").and_then(|m| m.as_str().parse().ok());
        Some((s.name.clone(), part))
    })
}

/// What rule 5 decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecision {
    /// The `talk_type`.
    pub talk_type: String,
    /// `Some(reason)` sends the talk to `needs_review`.
    pub review: Option<String>,
}

/// Rule 5: review patterns first, then type patterns, then length.
pub fn talk_type(cfg: &ClassifyConfig, normalized: &str, duration_s: Option<i32>) -> TypeDecision {
    let typed = cfg
        .types
        .iter()
        .find(|(p, _)| p.is_match(normalized))
        .map(|(_, t)| t.clone());
    let long = duration_s.is_some_and(|d| d >= cfg.settings.long_talk_seconds);
    let talk_type = typed.unwrap_or_else(|| if long { "dharma_talk" } else { "other" }.to_owned());
    let review = cfg
        .review
        .iter()
        .find(|(p, _)| p.is_match(normalized))
        .map(|(_, reason)| reason.clone())
        .or_else(|| (talk_type == "other").then(|| "talk_type_unclear".to_owned()));
    TypeDecision { talk_type, review }
}

/// The facts the duplicate rule compares for one video.
#[derive(Debug, Clone, PartialEq)]
pub struct DupFacts {
    /// Normalized title.
    pub title: String,
    /// Days since the Unix epoch of the publish date, when known.
    pub day: Option<i64>,
    /// Duration in seconds, when known.
    pub duration_s: Option<i32>,
    /// Series and part, so two parts of one series are never merged.
    pub series_part: Option<(String, i32)>,
}

/// Rule 3, given the `pg_trgm` similarity of the two normalized titles. Two talks match only
/// when their dates are within a day, and then either their durations are within the larger of
/// the minimum and the percentage and their titles similar enough, or their titles are
/// identical and their durations within `dedupe_title_identical_tolerance_s`. The date limit
/// on identical titles keeps weekly talks with one generic title apart (decision of October 5,
/// 2026). Two different parts of one series never match, so a lecture series recorded weekly
/// at similar lengths is not merged.
pub fn is_duplicate(cfg: &ClassifyConfig, a: &DupFacts, b: &DupFacts, similarity: f32) -> bool {
    if let (Some((sa, pa)), Some((sb, pb))) = (&a.series_part, &b.series_part)
        && sa == sb
        && pa != pb
    {
        return false;
    }
    let (Some(da), Some(db)) = (a.duration_s, b.duration_s) else {
        return false;
    };
    let close_dates = matches!((a.day, b.day), (Some(x), Some(y)) if (x - y).abs() <= 1);
    if !close_dates {
        return false;
    }
    let gap = (da - db).abs();
    let s = &cfg.settings;
    if a.title == b.title && gap <= s.dedupe_title_identical_tolerance_s {
        return true;
    }
    let tolerance = f64::from(s.dedupe_duration_tolerance_min_s)
        .max(f64::from(da.max(db)) * f64::from(s.dedupe_duration_tolerance_pct) / 100.0);
    f64::from(gap) <= tolerance && similarity >= s.dedupe_title_similarity
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPELINE: &str = r#"
        [ingest]
        exclude_title_patterns = ['\bquote\b', '\bclip\b', '\btrailer\b']
        exclude_session_patterns = ['antiracist 12 steps']
        review_title_patterns = [
          { pattern = '^zoom ?\d+$', reason = "untitled" },
          { pattern = '\bq ?a\b|\bquestions and answers\b', reason = "student_voices" },
        ]
        type_patterns = [
          { pattern = '\bclass\b', talk_type = "class" },
          { pattern = '\bsesshin\b|\bretreat\b', talk_type = "retreat_talk" },
          { pattern = '\bservice\b', talk_type = "service" },
        ]
    "#;
    const SERIES: &str = r#"
        [[series]]
        name = "Genjokoan"
        pattern = 'genjo ?koan\D*(?P<part>\d+)'
    "#;

    fn cfg() -> ClassifyConfig {
        ClassifyConfig::parse(PIPELINE, "pipeline", SERIES, "series").unwrap()
    }

    fn facts(title: &str, day: i64, d: i32) -> DupFacts {
        DupFacts {
            title: normalize_title(title),
            day: Some(day),
            duration_s: Some(d),
            series_part: None,
        }
    }

    #[test]
    fn titles_normalize_to_lowercase_words() {
        assert_eq!(
            normalize_title("  Genjōkōan — Part 3!  "),
            "genjōkōan part 3"
        );
        assert_eq!(normalize_title("Q&A: Zazen"), "q a zazen");
        assert_eq!(
            normalize_title("ﬁrst   Talk"),
            "first talk",
            "NFKC folds the ligature"
        );
        assert_eq!(normalize_title("zoom_0"), "zoom 0");
        assert_eq!(
            normalize_title("2021-11-03_Shobogenzo-Uji_Part-3.mp4"),
            "2021 11 03 shobogenzo uji part 3 mp4",
            "separators keep words apart"
        );
    }

    #[test]
    fn a_title_pattern_wins_over_length_and_unknown_length_is_kept() {
        let c = cfg();
        assert_eq!(
            exclusion(&c, "buddhist quote", Some(11)),
            Some(Exclusion::NonTalk)
        );
        assert_eq!(
            exclusion(&c, "zen protocols", Some(28)),
            Some(Exclusion::TooShort)
        );
        assert_eq!(
            exclusion(&c, "antiracist 12 steps session", Some(5400)),
            Some(Exclusion::Session)
        );
        assert_eq!(exclusion(&c, "zen protocols", None), None);
        assert_eq!(exclusion(&c, "uji part 4", Some(2400)), None);
        assert_eq!(Exclusion::TooShort.reason(), "too_short");
        assert_eq!(Exclusion::TooShort.talk_type(), Some("non_talk"));
    }

    #[test]
    fn review_patterns_hold_long_recordings_and_short_unmatched_ones_are_unclear() {
        let c = cfg();
        let untitled = talk_type(&c, &normalize_title("zoom_0"), Some(2700));
        assert_eq!(untitled.review.as_deref(), Some("untitled"));
        for title in ["Q&A with the teacher", "Q & A", "Questions and Answers"] {
            let qa = talk_type(&c, &normalize_title(title), Some(2000));
            assert_eq!(qa.review.as_deref(), Some("student_voices"), "{title}");
        }
        let short = talk_type(&c, "a brief word", Some(600));
        assert_eq!(
            (short.talk_type.as_str(), short.review.as_deref()),
            ("other", Some("talk_type_unclear"))
        );
        let long = talk_type(&c, "facing the jewel mirror", Some(2400));
        assert_eq!(
            (long.talk_type.as_str(), long.review),
            ("dharma_talk", None)
        );
        let class = talk_type(&c, "class on the precepts", Some(600));
        assert_eq!((class.talk_type.as_str(), class.review), ("class", None));
    }

    #[test]
    fn series_records_the_part_number() {
        let c = cfg();
        assert_eq!(
            series(&c, &normalize_title("Genjokoan, Part 7")),
            Some(("Genjokoan".to_owned(), Some(7)))
        );
        assert_eq!(series(&c, "uji part 4"), None);
    }

    #[test]
    fn a_series_without_a_part_capture_is_refused() {
        let err =
            ClassifyConfig::parse("", "p", "[[series]]\nname = \"Uji\"\npattern = 'uji'", "s")
                .unwrap_err();
        assert!(err.to_string().contains("named capture `part`"));
    }

    #[test]
    fn the_duplicate_rule_needs_close_dates_lengths_and_titles() {
        let c = cfg();
        let a = facts("Uji, Part 4", 100, 2400);
        assert!(is_duplicate(
            &c,
            &a,
            &facts("Uji Part 4 (re-upload)", 101, 2430),
            0.7
        ));
        assert!(
            !is_duplicate(&c, &a, &facts("Uji Part 4 (re-upload)", 102, 2430), 0.7),
            "two days apart"
        );
        assert!(
            !is_duplicate(&c, &a, &facts("Uji Part 4 (re-upload)", 100, 2500), 0.7),
            "100 s off on 2400 s"
        );
        assert!(
            !is_duplicate(&c, &a, &facts("Something else", 100, 2400), 0.2),
            "titles differ"
        );
        assert!(
            is_duplicate(&c, &a, &facts("UJI — part 4", 101, 2500), 1.0),
            "identical title a day later, 100 s apart"
        );
        assert!(
            !is_duplicate(&c, &a, &facts("Uji part 4", 101, 2600), 1.0),
            "identical title, 200 s apart"
        );
        assert!(
            !is_duplicate(&c, &a, &facts("Uji, Part 4", 107, 2400), 1.0),
            "identical title a week apart: weekly talks with one generic title"
        );
        let mut p3 = facts("Genjokoan part 3", 10, 2400);
        let mut p4 = facts("Genjokoan part 4", 10, 2400);
        p3.series_part = Some(("Genjokoan".into(), 3));
        p4.series_part = Some(("Genjokoan".into(), 4));
        assert!(
            !is_duplicate(&c, &p3, &p4, 0.9),
            "different parts never merge"
        );
    }
}
