//! Recording dates written in titles (Spec 03, rule 3). Uploads often come days or weeks after a
//! talk is given, so the date in the title is the better date for a talk and for finding a
//! re-upload of the same recording.
//!
//! Recognized, in this order: ISO dates (`2021-11-07`, `2019-3-31`, also between underscores),
//! month names (`September 20, 2026`, `Apr 16, 2023`, `10 Mar 2024`, `24 March 2022`), and US
//! numeric dates, month first (`6-06-2021`, `4/9/2021`, `9-18-21`, `03-13-2022`). A date in the
//! future of the upload is a typo, not a recording date, and is ignored.

use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;

/// Month abbreviations in order, as a regex alternation; a name matches by its first three letters.
const MONTHS: &str = "jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec";

/// A recording is taken to be at most this many days after its upload date, for time zones.
const UPLOAD_TOLERANCE_DAYS: i64 = 1;

/// `2023-04-09`, also with `_`, `/` or `.`, not inside a longer number.
static ISO: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9])(20\d{2})[-_/.](\d{1,2})[-_/.](\d{1,2})(?:[^0-9]|$)").ok()
});
/// `April 9th, 2023` and `Apr. 9 2023`.
static MONTH_DAY_YEAR: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b({MONTHS})[a-z]*\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?,?\s+(\d{{4}})\b"
    ))
    .ok()
});
/// `9 April 2023`.
static DAY_MONTH_YEAR: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b(\d{{1,2}})\s+({MONTHS})[a-z]*\.?,?\s+(\d{{4}})\b"
    ))
    .ok()
});
/// `4/9/2023` or `4-9-23`, month first.
static US_NUMERIC: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9])(\d{1,2})[-/](\d{1,2})[-/](\d{4}|\d{2})(?:[^0-9]|$)").ok()
});

/// A month's number from its name or abbreviation.
fn month_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    MONTHS
        .split('|')
        .position(|m| lower.starts_with(m))
        .and_then(|i| u32::try_from(i + 1).ok())
}

/// A date in this century, or none, so a stray number never makes a year like 1907.
fn date(year: i32, month: u32, day: u32) -> Option<NaiveDate> {
    (2000..=2099)
        .contains(&year)
        .then(|| NaiveDate::from_ymd_opt(year, month, day))
        .flatten()
}

/// A capture group parsed as a number.
fn number<T: std::str::FromStr>(caps: &regex::Captures<'_>, i: usize) -> Option<T> {
    caps.get(i)?.as_str().parse().ok()
}

/// Every date the title states, in the order of the patterns above.
fn candidates(title: &str) -> Vec<NaiveDate> {
    let mut found = Vec::new();
    if let Some(re) = ISO.as_ref() {
        found.extend(
            re.captures_iter(title)
                .filter_map(|c| date(number(&c, 1)?, number(&c, 2)?, number(&c, 3)?)),
        );
    }
    if let Some(re) = MONTH_DAY_YEAR.as_ref() {
        found.extend(re.captures_iter(title).filter_map(|c| {
            date(
                number(&c, 3)?,
                month_number(c.get(1)?.as_str())?,
                number(&c, 2)?,
            )
        }));
    }
    if let Some(re) = DAY_MONTH_YEAR.as_ref() {
        found.extend(re.captures_iter(title).filter_map(|c| {
            date(
                number(&c, 3)?,
                month_number(c.get(2)?.as_str())?,
                number(&c, 1)?,
            )
        }));
    }
    if let Some(re) = US_NUMERIC.as_ref() {
        found.extend(re.captures_iter(title).filter_map(|c| {
            let year: i32 = number(&c, 3)?;
            let year = if year < 100 { 2000 + year } else { year };
            date(year, number(&c, 1)?, number(&c, 2)?)
        }));
    }
    found
}

/// The recording date a title states, if any is plausible: not after the upload date. With no
/// upload date, the first date found.
pub fn recording_date(title: &str, uploaded: Option<NaiveDate>) -> Option<NaiveDate> {
    candidates(title)
        .into_iter()
        .find(|d| uploaded.is_none_or(|up| (*d - up).num_days() <= UPLOAD_TOLERANCE_DAYS))
}

/// A talk's date: the recording date from its title, or else its upload date.
pub fn talk_date(title: &str, uploaded: Option<NaiveDate>) -> Option<NaiveDate> {
    recording_date(title, uploaded).or(uploaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap_or_default()
    }

    #[test]
    fn the_archive_title_formats_give_their_dates() {
        let upload = Some(d(2030, 1, 1));
        for (title, want) in [
            ("2021-11-07_Sunday_Fixture_Uji_Part-4", d(2021, 11, 7)),
            ("2021-10-17 Fixture - Sunday - Part-1.mp4", d(2021, 10, 17)),
            ("Disarming Speech_Fixture_2019-3-31", d(2019, 3, 31)),
            ("kanshin 2020-05-10", d(2020, 5, 10)),
            (
                "Dharma Talk on Sangha, by A Teacher on September 20, 2026",
                d(2026, 9, 20),
            ),
            (
                "Dharma Talk on Two Kinds of Thought by A Teacher on Apr 9, 2023",
                d(2023, 4, 9),
            ),
            (
                "Dharma Talk on Hidden Lamp #50, by A Teacher on 10 Mar 2024",
                d(2024, 3, 10),
            ),
            (
                "Dharma Talk on Caring by A Teacher on 24 March 2022",
                d(2022, 3, 24),
            ),
            ("Meg Fixture Q&A March 24 2019", d(2019, 3, 24)),
            ("Genjokoan - Paragraph 3 (4-25-2021)", d(2021, 4, 25)),
            ("Genjokoan Paragraph 1 (4/10/2021)", d(2021, 4, 10)),
            (
                "A Teacher - Ocean Mudra Samadhi (9-18-21).mp4",
                d(2021, 9, 18),
            ),
            (
                "Dharma Talk on Letting Go by A Teacher on 03-13-2022",
                d(2022, 3, 13),
            ),
            ("01-17-21 Sunday Dharma Talk: A Teacher", d(2021, 1, 17)),
            ("Genzen Fixture Sunday Dharma Talk 7-11/21", d(2021, 7, 11)),
        ] {
            assert_eq!(recording_date(title, upload), Some(want), "{title}");
        }
    }

    #[test]
    fn titles_without_a_full_date_give_none() {
        for title in [
            "Sunday Dharma Talk: A Teacher - Bendowa Series",
            "Sunday service 5-31",
            "zoom_0",
            "Blue Cliff Record Case #12, verse 3",
            "Tuesday Class #5 on silence",
            "Platform Sutra Ch 20 21",
        ] {
            assert_eq!(recording_date(title, None), None, "{title}");
        }
    }

    #[test]
    fn a_date_after_the_upload_is_a_typo_and_the_upload_date_stands() {
        let title = "Dharma Talk on Endings by A Teacher on May 30, 2023";
        assert_eq!(recording_date(title, Some(d(2023, 5, 10))), None);
        assert_eq!(talk_date(title, Some(d(2023, 5, 10))), Some(d(2023, 5, 10)));
        assert_eq!(
            recording_date(title, Some(d(2023, 6, 2))),
            Some(d(2023, 5, 30))
        );
        assert_eq!(
            recording_date(title, Some(d(2023, 5, 29))),
            Some(d(2023, 5, 30)),
            "a day ahead is a time zone, not a typo"
        );
        assert_eq!(talk_date("zoom_0", None), None);
    }
}
