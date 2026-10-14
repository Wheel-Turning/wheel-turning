//! Matching podcast episodes to Vimeo talks (Spec 03, Seattle Soto Zen sources item 2a): by
//! normalized title, date and duration.
//!
//! A talk qualifies when its date is within `feed_match_days` of the episode's and its length
//! within the larger of `feed_match_duration_tolerance_s` and the percentage. Among those, the
//! talk with the most similar title wins if it reaches `feed_match_title_similarity`; failing
//! that, a talk whose title states the same recording date as the episode's title wins if it is
//! the only one. Anything else stays unmatched, for a person to look at in the completeness
//! check. Dates are recording dates when the titles state them, as for rule 3.
//!
//! An episode with no length, as in SSZ's feed, needs a recording date from its title or its
//! audio file's name, and matches only a talk whose title states the same date: the only one
//! that day, or else the one with the most similar title above the threshold. Without a date
//! it stays unmatched (decision of October 5, 2026).

use chrono::NaiveDate;

/// An episode title without the speaker SSZ's feed appends in brackets, normalized as in rule
/// 1: `Who Is Sick? (A Teacher)` gives `who is sick`.
pub fn title_core(title: &str) -> String {
    let trimmed = title.trim_end();
    let core = match (trimmed.ends_with(')'), trimmed.rfind('(')) {
        (true, Some(open)) if open > 0 => &trimmed[..open],
        _ => trimmed,
    };
    crate::classify::normalize_title(core)
}

/// The recording date in an audio file's name, such as
/// `…/AudioFiles/2021-01-17-Teacher-Sunday-Dharma-Talk.mp3?sig=…`. Only the file name is read,
/// never the query, which carries signatures full of digits.
pub fn date_from_audio_url(url: &str, published: Option<NaiveDate>) -> Option<NaiveDate> {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let name = path.rsplit('/').next().unwrap_or_default();
    crate::dates::recording_date(name, published)
}

/// Chooses the talk for an episode with no length. `date` is the episode's recording date and
/// `options` are talks stored with that date; only those whose own titles state it count.
pub fn choose_by_date<T: Clone>(
    cfg: &ClassifyConfig,
    date: NaiveDate,
    options: &[Candidate<T>],
) -> Option<(T, How)> {
    let same_day: Vec<&Candidate<T>> = options
        .iter()
        .filter(|o| o.side.dated_in_title && o.side.date == Some(date))
        .collect();
    match same_day.as_slice() {
        [] => None,
        [only] => Some((only.key.clone(), How::RecordingDate)),
        several => {
            let best = several
                .iter()
                .filter(|o| o.similarity >= cfg.settings.feed_match_title_similarity)
                .max_by(|a, b| a.similarity.total_cmp(&b.similarity))?;
            let tied = several
                .iter()
                .filter(|o| o.similarity == best.similarity)
                .count();
            (tied == 1).then(|| (best.key.clone(), How::Title))
        }
    }
}

use crate::config::ClassifyConfig;

/// The facts the rule compares for one side.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    /// Its date: recording date from the title, or else the publish or upload date.
    pub date: Option<NaiveDate>,
    /// Whether that date came from the title.
    pub dated_in_title: bool,
    /// Length in seconds.
    pub duration_s: Option<i32>,
}

/// A talk the episode might belong to, with its title similarity to the episode.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate<T> {
    /// Whatever identifies the talk to the caller.
    pub key: T,
    /// Its facts.
    pub side: Side,
    /// `pg_trgm` similarity of the normalized titles.
    pub similarity: f32,
}

/// How a match was made, recorded with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The most similar title, above the threshold.
    Title,
    /// The only talk with the same recording date in its title.
    RecordingDate,
    /// The only talk with exactly the episode's title.
    ExactTitle,
}

impl How {
    /// The value stored in `raw.match.how`.
    pub fn as_str(self) -> &'static str {
        match self {
            How::Title => "title",
            How::RecordingDate => "recording_date",
            How::ExactTitle => "exact_title",
        }
    }
}

/// The widest length gap any match can have, to narrow the database search.
pub fn max_gap_s(cfg: &ClassifyConfig, duration_s: i32) -> i32 {
    let s = &cfg.settings;
    // Twice the percentage of this side: the percentage applies to the longer side.
    let pct = (f64::from(duration_s) * f64::from(s.feed_match_duration_tolerance_pct) / 50.0)
        .ceil()
        .min(f64::from(i32::MAX)) as i32;
    s.feed_match_duration_tolerance_s.max(pct)
}

/// Whether two lengths are within the larger of `feed_match_duration_tolerance_s` and the
/// percentage of the longer one.
pub fn lengths_fit(cfg: &ClassifyConfig, a: i32, b: i32) -> bool {
    let s = &cfg.settings;
    let tolerance = f64::from(s.feed_match_duration_tolerance_s)
        .max(f64::from(a.max(b)) * f64::from(s.feed_match_duration_tolerance_pct) / 100.0);
    f64::from((a - b).abs()) <= tolerance
}

/// Whether a talk can be the episode's: both dated within `feed_match_days` of each other and both
/// with lengths that fit. Missing facts never qualify.
fn qualifies(cfg: &ClassifyConfig, episode: &Side, talk: &Side) -> bool {
    let (Some(de), Some(dt)) = (episode.date, talk.date) else {
        return false;
    };
    let (Some(le), Some(lt)) = (episode.duration_s, talk.duration_s) else {
        return false;
    };
    (de - dt).num_days().abs() <= cfg.settings.feed_match_days && lengths_fit(cfg, le, lt)
}

/// Chooses the talk an episode belongs to, if the rule allows exactly one.
pub fn choose<T: Clone>(
    cfg: &ClassifyConfig,
    episode: &Side,
    options: &[Candidate<T>],
) -> Option<(T, How)> {
    let fit: Vec<&Candidate<T>> = options
        .iter()
        .filter(|o| qualifies(cfg, episode, &o.side))
        .collect();
    let best = fit
        .iter()
        .filter(|o| o.similarity >= cfg.settings.feed_match_title_similarity)
        .max_by(|a, b| a.similarity.total_cmp(&b.similarity));
    if let Some(best) = best {
        let tied = fit
            .iter()
            .filter(|o| o.similarity == best.similarity)
            .count();
        if tied == 1 {
            return Some((best.key.clone(), How::Title));
        }
    }
    if episode.dated_in_title {
        let same_day: Vec<&&Candidate<T>> = fit
            .iter()
            .filter(|o| o.side.dated_in_title && o.side.date == episode.date)
            .collect();
        if let [only] = same_day.as_slice() {
            return Some((only.key.clone(), How::RecordingDate));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ClassifyConfig {
        ClassifyConfig::parse("", "pipeline", "", "series").unwrap_or_else(|e| panic!("{e}"))
    }

    fn day(n: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2024, 5, n)
    }

    fn side(d: u32, titled: bool, secs: i32) -> Side {
        Side {
            date: day(d),
            dated_in_title: titled,
            duration_s: Some(secs),
        }
    }

    fn option(key: &str, d: u32, titled: bool, secs: i32, similarity: f32) -> Candidate<String> {
        Candidate {
            key: key.into(),
            side: side(d, titled, secs),
            similarity,
        }
    }

    #[test]
    fn the_most_similar_title_within_the_date_and_length_window_wins() {
        let c = cfg();
        let ep = side(20, false, 2400);
        let opts = [
            option("same", 12, false, 2460, 0.8),
            option("weaker", 13, false, 2400, 0.6),
            option("too long ago", 1, false, 2400, 0.95),
            option("too long", 12, false, 2700, 0.95),
        ];
        assert_eq!(
            choose(&c, &ep, &opts),
            Some(("same".to_owned(), How::Title))
        );
    }

    #[test]
    fn a_shared_recording_date_matches_when_titles_differ() {
        let c = cfg();
        let ep = side(12, true, 2400);
        let opts = [
            option("dated", 12, true, 2420, 0.2),
            option("other", 14, false, 2400, 0.2),
        ];
        assert_eq!(
            choose(&c, &ep, &opts),
            Some(("dated".to_owned(), How::RecordingDate))
        );
    }

    #[test]
    fn ties_and_weak_titles_stay_unmatched() {
        let c = cfg();
        let ep = side(20, false, 2400);
        assert_eq!(
            choose(
                &c,
                &ep,
                &[
                    option("a", 19, false, 2400, 0.7),
                    option("b", 18, false, 2400, 0.7)
                ]
            ),
            None
        );
        assert_eq!(
            choose(&c, &ep, &[option("weak", 19, false, 2400, 0.3)]),
            None
        );
        let dated = side(12, true, 2400);
        assert_eq!(
            choose(
                &c,
                &dated,
                &[
                    option("a", 12, true, 2400, 0.1),
                    option("b", 12, true, 2410, 0.1)
                ]
            ),
            None,
            "two talks on the day"
        );
        assert!(max_gap_s(&c, 3600) >= 360);
    }
}

#[cfg(test)]
mod no_length {
    use super::*;

    fn cfg() -> ClassifyConfig {
        ClassifyConfig::parse("", "pipeline", "", "series").unwrap_or_else(|e| panic!("{e}"))
    }

    fn talk(key: &str, d: Option<NaiveDate>, titled: bool, similarity: f32) -> Candidate<String> {
        Candidate {
            key: key.into(),
            side: Side {
                date: d,
                dated_in_title: titled,
                duration_s: Some(2400),
            },
            similarity,
        }
    }

    #[test]
    fn the_title_core_drops_the_bracketed_speaker_only_at_the_end() {
        assert_eq!(title_core("Who Is Sick? (A Teacher)"), "who is sick");
        assert_eq!(title_core("Mother's Day (and More)"), "mother s day");
        assert_eq!(title_core("(Untitled)"), "untitled");
        assert_eq!(
            title_core("Ten (10) Grave Precepts"),
            "ten 10 grave precepts"
        );
    }

    #[test]
    fn the_audio_file_name_gives_the_date_and_the_query_is_ignored() {
        let url = "https://files.example.invalid/ssz/AudioFiles/2021-01-17-A-Teacher-Sunday-Talk.mp3\
                   ?sr=b&sig=2020-12-31x";
        assert_eq!(
            date_from_audio_url(url, None),
            NaiveDate::from_ymd_opt(2021, 1, 17)
        );
        assert_eq!(
            date_from_audio_url("https://x.invalid/talk.mp3?d=2020-01-01", None),
            None
        );
    }

    #[test]
    fn one_talk_dated_that_day_matches_and_two_need_a_title() {
        let c = cfg();
        let day = NaiveDate::from_ymd_opt(2021, 1, 17).unwrap_or_default();
        let uploaded_that_day = talk("upload", Some(day), false, 0.9);
        assert_eq!(
            choose_by_date(
                &c,
                day,
                &[
                    talk("titled", Some(day), true, 0.1),
                    uploaded_that_day.clone()
                ]
            ),
            Some(("titled".to_owned(), How::RecordingDate)),
            "a talk merely uploaded that day does not count"
        );
        assert_eq!(choose_by_date(&c, day, &[uploaded_that_day]), None);
        let two = [
            talk("sunday", Some(day), true, 0.7),
            talk("zazenkai", Some(day), true, 0.2),
        ];
        assert_eq!(
            choose_by_date(&c, day, &two),
            Some(("sunday".to_owned(), How::Title))
        );
        let unclear = [
            talk("a", Some(day), true, 0.3),
            talk("b", Some(day), true, 0.3),
        ];
        assert_eq!(
            choose_by_date(&c, day, &unclear),
            None,
            "an ambiguous day stays unmatched"
        );
    }
}
