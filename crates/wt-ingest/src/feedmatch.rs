//! Matching podcast episodes to Vimeo talks (Spec 03, Seattle Soto Zen sources item 2a): by
//! normalized title, date and duration.
//!
//! A talk qualifies when its date is within `feed_match_days` of the episode's and its length
//! within the larger of `feed_match_duration_tolerance_s` and the percentage. Among those, the
//! talk with the most similar title wins if it reaches `feed_match_title_similarity`; failing
//! that, a talk whose title states the same recording date as the episode's title wins if it is
//! the only one. Anything else stays unmatched, for a person to look at in the completeness
//! check. Dates are recording dates when the titles state them, as for rule 3.

use chrono::NaiveDate;

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
}

impl How {
    /// The value stored in `raw.match.how`.
    pub fn as_str(self) -> &'static str {
        match self {
            How::Title => "title",
            How::RecordingDate => "recording_date",
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

fn qualifies(cfg: &ClassifyConfig, episode: &Side, talk: &Side) -> bool {
    let s = &cfg.settings;
    let (Some(de), Some(dt)) = (episode.date, talk.date) else {
        return false;
    };
    let (Some(le), Some(lt)) = (episode.duration_s, talk.duration_s) else {
        return false;
    };
    let tolerance = f64::from(s.feed_match_duration_tolerance_s)
        .max(f64::from(le.max(lt)) * f64::from(s.feed_match_duration_tolerance_pct) / 100.0);
    (de - dt).num_days().abs() <= s.feed_match_days && f64::from((le - lt).abs()) <= tolerance
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
