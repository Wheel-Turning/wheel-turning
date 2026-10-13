//! `wt ingest match-storage` (Spec 03, the storage adapter; Seattle Soto Zen sources item 2c):
//! pairs the files under a storage source's root with the Vimeo talks of the same sangha, writes
//! a review table of proposed matches, unmatched files and unmatched talks, and on confirmation
//! records each match on the talk's `source_video` in `raw.storage`.
//!
//! A file matches a talk, in this order:
//!
//! 1. by a Vimeo id in its name, such as a bulk export's `Talk title (812345678).mp4`, when it
//!    names exactly one live talk;
//! 2. with a recording date in its name and a length, by the podcast rule of item 2a: a talk
//!    dated within `feed_match_days` whose length fits, the most similar title above
//!    `feed_match_title_similarity`, or else the only talk with the same recording date in its
//!    title;
//! 3. with a length, by the uniquely most similar title above the threshold among talks whose
//!    length fits, as a drive download named after the Vimeo title would be;
//! 4. with a date and no length, by the podcast rule for episodes with no length.
//!
//! Two files that choose one talk are both a conflict, left for the reviewer. Nothing changes
//! without `--apply`, which records every proposed match, or `--sheet`, which records the rows
//! the reviewer marked `yes` in the review table, including rows where they typed a Vimeo id.
//! A match is keyed by the file's fingerprint, so running it again on unchanged files changes
//! nothing (CLAUDE.md rule 8).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use wt_db::ingest::IngestDbError;
use wt_db::storage::{self, StorageRef, StorageTalk};

use crate::classify::normalize_title;
use crate::config::ClassifyConfig;
use crate::dates;
use crate::feedmatch::{self, Candidate, How, Side};
use crate::report::escape;
use crate::storage::StorageFile;

/// Errors from matching. None carries a secret.
#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    /// The database refused.
    #[error(transparent)]
    Db(#[from] IngestDbError),
    /// A database connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A reviewed sheet names a file the root no longer holds, or one that changed since the
    /// sheet was written.
    #[error(
        "the reviewed sheet's row for {key:?} {problem}; run match-storage again and review the new sheet"
    )]
    Stale {
        /// The file's key.
        key: String,
        /// What changed.
        problem: &'static str,
    },
    /// A reviewed sheet could not be read.
    #[error("cannot read the reviewed sheet: {0}")]
    Sheet(String),
}

/// Where a review row stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Two or more files chose this talk: a person decides.
    Conflict,
    /// A new match, recorded by `--apply` or by marking it `yes`.
    Proposed,
    /// A file no rule matched. A person can type the talk's Vimeo id and mark it `yes`.
    UnmatchedFile,
    /// A live talk no file matched.
    UnmatchedTalk,
    /// Already recorded, and the file is unchanged.
    Matched,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Conflict => "conflict",
            Status::Proposed => "proposed",
            Status::UnmatchedFile => "unmatched_file",
            Status::UnmatchedTalk => "unmatched_talk",
            Status::Matched => "matched",
        }
    }
}

/// One row of the review table, as the CSV holds it. The reviewer edits `accept` and, for an
/// unmatched file, `vimeo_id`; `--sheet` reads only `accept`, `file`, `fingerprint`, `vimeo_id`
/// and `how`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetRow {
    /// `conflict`, `proposed`, `unmatched_file`, `unmatched_talk` or `matched`.
    #[serde(default)]
    pub status: String,
    /// `yes` to record this row's match.
    #[serde(default)]
    pub accept: String,
    /// The file's key under the storage root.
    #[serde(default)]
    pub file: String,
    /// Its size in bytes.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Its length in seconds.
    #[serde(default)]
    pub duration_s: Option<i32>,
    /// The recording date in its name.
    #[serde(default)]
    pub file_date: Option<NaiveDate>,
    /// Its fingerprint when the table was written.
    #[serde(default)]
    pub fingerprint: String,
    /// The talk's Vimeo id.
    #[serde(default)]
    pub vimeo_id: String,
    /// The talk's title.
    #[serde(default)]
    pub talk_title: String,
    /// The talk's date.
    #[serde(default)]
    pub talk_date: Option<NaiveDate>,
    /// The talk's length in seconds.
    #[serde(default)]
    pub talk_duration_s: Option<i32>,
    /// The talk's state, and why it is held if it is.
    #[serde(default)]
    pub talk_state: String,
    /// How the match was made.
    #[serde(default)]
    pub how: String,
    /// Title similarity.
    #[serde(default)]
    pub similarity: Option<f32>,
}

/// The review: every row, with its counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Review {
    /// The storage source.
    pub source: String,
    /// Rows, conflicts first, then proposals, unmatched files, unmatched talks and matches.
    pub rows: Vec<SheetRow>,
    /// Files listed.
    pub files: usize,
    /// Rows per status.
    pub counts: BTreeMap<String, usize>,
}

/// What `apply` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Applied {
    /// Matches recorded or changed.
    pub recorded: usize,
    /// Matches already recorded as they are.
    pub unchanged: usize,
}

/// The matcher's facts about a file.
#[derive(Debug, Clone, PartialEq)]
struct FileFacts {
    normalized: String,
    date: Option<NaiveDate>,
    duration_s: Option<i32>,
    numbers: BTreeSet<String>,
}

fn facts(file: &StorageFile) -> FileFacts {
    let stem = file.stem();
    let numbers = stem
        .split(|c: char| !c.is_ascii_digit())
        .filter(|n| (6..=12).contains(&n.len()))
        .map(str::to_owned)
        .collect();
    FileFacts {
        normalized: normalize_title(stem),
        date: dates::recording_date(stem, None).or_else(|| dates::recording_date(&file.key, None)),
        duration_s: file.duration_s,
        numbers,
    }
}

fn talk_side(t: &StorageTalk) -> Side {
    let uploaded = t.published_at.map(|p| p.date_naive());
    let titled = dates::recording_date(&t.title, uploaded);
    Side {
        date: titled.or(t.talk_date).or(uploaded),
        dated_in_title: titled.is_some(),
        duration_s: t.duration_s,
    }
}

/// The value stored in `raw.storage.how` for a rule.
fn how_str(how: How) -> &'static str {
    how.as_str()
}

/// Chooses the talk for one file among `talks` (each with its title similarity to the file),
/// by the rules in the module comment. Returns the talk's Vimeo id and how.
fn choose(
    cfg: &ClassifyConfig,
    file: &FileFacts,
    talks: &[StorageTalk],
) -> Option<(String, &'static str)> {
    let by_id: Vec<&StorageTalk> = talks
        .iter()
        .filter(|t| file.numbers.contains(&t.vimeo_id))
        .collect();
    if let [only] = by_id.as_slice() {
        return Some((only.vimeo_id.clone(), "vimeo_id"));
    }
    let options: Vec<Candidate<String>> = talks
        .iter()
        .map(|t| Candidate {
            key: t.vimeo_id.clone(),
            side: talk_side(t),
            similarity: t.similarity,
        })
        .collect();
    let me = Side {
        date: file.date,
        dated_in_title: file.date.is_some(),
        duration_s: file.duration_s,
    };
    match (file.date, file.duration_s) {
        (Some(_), Some(len)) => feedmatch::choose(cfg, &me, &options)
            .or_else(|| by_title(cfg, len, &options))
            .map(|(k, h)| (k, how_str(h))),
        (None, Some(len)) => by_title(cfg, len, &options).map(|(k, h)| (k, how_str(h))),
        (Some(date), None) => {
            let same_day: Vec<Candidate<String>> = options
                .into_iter()
                .filter(|o| o.side.date == Some(date))
                .collect();
            feedmatch::choose_by_date(cfg, date, &same_day).map(|(k, h)| (k, how_str(h)))
        }
        (None, None) => None,
    }
}

/// Rule 3: the uniquely most similar title above the threshold among talks whose length fits.
fn by_title(
    cfg: &ClassifyConfig,
    len: i32,
    options: &[Candidate<String>],
) -> Option<(String, How)> {
    let fit: Vec<&Candidate<String>> = options
        .iter()
        .filter(|o| {
            o.side
                .duration_s
                .is_some_and(|d| feedmatch::lengths_fit(cfg, len, d))
        })
        .collect();
    let best = fit
        .iter()
        .filter(|o| o.similarity >= cfg.settings.feed_match_title_similarity)
        .max_by(|a, b| a.similarity.total_cmp(&b.similarity))?;
    let tied = fit
        .iter()
        .filter(|o| o.similarity == best.similarity)
        .count();
    (tied == 1).then(|| (best.key.clone(), How::Title))
}

fn talk_state(t: &StorageTalk) -> String {
    match &t.state_reason {
        Some(reason) => format!("{} ({reason})", t.state),
        None => t.state.clone(),
    }
}

fn row(status: Status, file: Option<&StorageFile>, talk: Option<&StorageTalk>) -> SheetRow {
    let mut r = SheetRow {
        status: status.as_str().to_owned(),
        ..SheetRow::default()
    };
    if let Some(f) = file {
        r.file = f.key.clone();
        r.bytes = Some(f.bytes);
        r.duration_s = f.duration_s;
        r.file_date = facts(f).date;
        r.fingerprint = f.fingerprint();
    }
    if let Some(t) = talk {
        r.vimeo_id = t.vimeo_id.clone();
        r.talk_title = t.title.clone();
        r.talk_date = t.talk_date;
        r.talk_duration_s = t.duration_s;
        r.talk_state = talk_state(t);
    }
    r
}

fn is_current(source: &str, file: &StorageFile, held: Option<&StorageRef>) -> bool {
    held.is_some_and(|s| {
        s.source == source && s.key == file.key && s.fingerprint == file.fingerprint()
    })
}

/// Builds the review for `files` of storage source `source` against the live talks of
/// `vimeo_sources`. Reads only.
pub async fn review(
    conn: &mut PgConnection,
    cfg: &ClassifyConfig,
    source: &str,
    vimeo_sources: &[String],
    files: &[StorageFile],
) -> Result<Review, MatchError> {
    let talks = storage::storage_talks(conn, vimeo_sources, "").await?;
    let mut rows = Vec::new();
    // Talks already holding a listed, unchanged file keep it, and no other file can take them.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut open_files = Vec::new();
    for file in files {
        match talks
            .iter()
            .find(|t| is_current(source, file, t.storage.as_ref()))
        {
            Some(t) => {
                taken.insert(t.vimeo_id.clone());
                let mut r = row(Status::Matched, Some(file), Some(t));
                r.how = "recorded".to_owned();
                rows.push(r);
            }
            None => open_files.push(file),
        }
    }
    let mut chosen: BTreeMap<String, Vec<(&StorageFile, &'static str, f32)>> = BTreeMap::new();
    let mut unmatched_files = Vec::new();
    for file in open_files {
        let f = facts(file);
        let options: Vec<StorageTalk> = storage::storage_talks(conn, vimeo_sources, &f.normalized)
            .await?
            .into_iter()
            .filter(|t| !taken.contains(&t.vimeo_id))
            .collect();
        match choose(cfg, &f, &options) {
            Some((vimeo_id, how)) => {
                let similarity = options
                    .iter()
                    .find(|t| t.vimeo_id == vimeo_id)
                    .map_or(0.0, |t| t.similarity);
                chosen
                    .entry(vimeo_id)
                    .or_default()
                    .push((file, how, similarity));
            }
            None => unmatched_files.push(file),
        }
    }
    for (vimeo_id, picks) in &chosen {
        let talk = talks.iter().find(|t| &t.vimeo_id == vimeo_id);
        let status = if picks.len() == 1 {
            Status::Proposed
        } else {
            Status::Conflict
        };
        for (file, how, similarity) in picks {
            let mut r = row(status, Some(file), talk);
            r.how = (*how).to_owned();
            r.similarity = Some(*similarity);
            if status == Status::Proposed {
                r.accept = "yes".to_owned();
            }
            rows.push(r);
        }
    }
    for file in unmatched_files {
        rows.push(row(Status::UnmatchedFile, Some(file), None));
    }
    for talk in &talks {
        if !taken.contains(&talk.vimeo_id) && !chosen.contains_key(&talk.vimeo_id) {
            rows.push(row(Status::UnmatchedTalk, None, Some(talk)));
        }
    }
    rows.sort_by_key(|r| status_rank(&r.status));
    let mut counts = BTreeMap::new();
    for r in &rows {
        *counts.entry(r.status.clone()).or_insert(0) += 1;
    }
    Ok(Review {
        source: source.to_owned(),
        rows,
        files: files.len(),
        counts,
    })
}

fn status_rank(status: &str) -> u8 {
    match status {
        "conflict" => 0,
        "proposed" => 1,
        "unmatched_file" => 2,
        "unmatched_talk" => 3,
        _ => 4,
    }
}

fn accepted(r: &SheetRow) -> bool {
    matches!(
        r.accept.trim().to_ascii_lowercase().as_str(),
        "yes" | "y" | "x" | "true" | "1"
    )
}

/// The rows to record: with `sheet`, the reviewed rows marked `yes` that name a file and a
/// Vimeo id; without, every proposed row of `review`.
pub fn confirmed(review: &Review, sheet: Option<&[SheetRow]>) -> Vec<SheetRow> {
    match sheet {
        Some(rows) => rows
            .iter()
            .filter(|r| accepted(r) && !r.file.trim().is_empty() && !r.vimeo_id.trim().is_empty())
            .cloned()
            .collect(),
        None => review
            .rows
            .iter()
            .filter(|r| r.status == Status::Proposed.as_str())
            .cloned()
            .collect(),
    }
}

/// Records `rows` in one transaction, each checked against the current listing: a file the root
/// no longer holds, or one changed since the sheet was written, stops the run before anything is
/// recorded.
pub async fn apply(
    conn: &mut PgConnection,
    source: &str,
    vimeo_sources: &[String],
    files: &[StorageFile],
    rows: &[SheetRow],
    actor: &str,
) -> Result<Applied, MatchError> {
    let by_key: BTreeMap<&str, &StorageFile> = files.iter().map(|f| (f.key.as_str(), f)).collect();
    let mut planned = Vec::with_capacity(rows.len());
    for r in rows {
        let key = r.file.trim();
        let Some(file) = by_key.get(key) else {
            return Err(MatchError::Stale {
                key: key.to_owned(),
                problem: "names a file the storage root no longer holds",
            });
        };
        let fp = r.fingerprint.trim();
        if !fp.is_empty() && fp != file.fingerprint() {
            return Err(MatchError::Stale {
                key: key.to_owned(),
                problem: "names a file that changed since the sheet was written",
            });
        }
        let how = match r.how.trim() {
            "" => "reviewed",
            how => how,
        };
        planned.push((r.vimeo_id.trim().to_owned(), file, how.to_owned()));
    }
    let mut out = Applied::default();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    for (vimeo_id, file, how) in planned {
        let target = StorageRef {
            source: source.to_owned(),
            key: file.key.clone(),
            bytes: file.bytes,
            fingerprint: file.fingerprint(),
        };
        if storage::set_storage(&mut tx, vimeo_sources, &vimeo_id, &target, &how, actor).await? {
            out.recorded += 1;
        } else {
            out.unchanged += 1;
        }
    }
    tx.commit().await?;
    Ok(out)
}

/// Reads a reviewed sheet.
pub fn read_sheet(text: &str) -> Result<Vec<SheetRow>, MatchError> {
    csv::Reader::from_reader(text.as_bytes())
        .deserialize()
        .collect::<Result<_, _>>()
        .map_err(|e| MatchError::Sheet(e.to_string()))
}

/// The review table as CSV, for a spreadsheet.
pub fn to_csv(review: &Review) -> Result<String, MatchError> {
    let mut w = csv::Writer::from_writer(Vec::new());
    for r in &review.rows {
        w.serialize(r)
            .map_err(|e| MatchError::Sheet(e.to_string()))?;
    }
    let bytes = w
        .into_inner()
        .map_err(|e| MatchError::Sheet(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| MatchError::Sheet(e.to_string()))
}

const STYLE: &str = "body{font:15px/1.45 system-ui,sans-serif;margin:24px auto;max-width:1200px;\
padding:0 16px;color:#1d1d1f;background:#fff}h1{font-size:24px}h2{font-size:19px;margin-top:32px;\
border-bottom:1px solid #ddd;padding-bottom:4px}table{border-collapse:collapse;width:100%;margin:8px 0}\
th,td{text-align:left;padding:4px 8px;border-bottom:1px solid #eee;vertical-align:top}\
th{font-weight:600;background:#f6f6f6}.muted{color:#666}\
@media (prefers-color-scheme:dark){body{background:#151515;color:#e6e6e6}th{background:#222}\
th,td{border-color:#333}.muted{color:#aaa}a{color:#8ab4f8}}";

fn clock(s: Option<i32>) -> String {
    s.map_or_else(String::new, |s| {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    })
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map_or_else(String::new, |v| v.to_string())
}

/// The review table as one self-contained HTML page, like the completeness check.
pub fn render_html(review: &Review, generated_at: &str, csv_name: &str) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Storage matches</title><style>{STYLE}</style></head><body>\n\
         <h1>Storage matches: {}</h1>\n<p class=\"muted\">Generated {}. {} files listed. \
         Nothing is recorded until you run <code>wt ingest match-storage --apply</code>, or \
         mark rows <code>yes</code> in <code>{}</code> and run it with <code>--sheet</code>.</p>\n",
        escape(&review.source),
        escape(generated_at),
        review.files,
        escape(csv_name)
    );
    for (status, heading) in [
        ("conflict", "Conflicts: several files chose one talk"),
        ("proposed", "Proposed matches"),
        ("unmatched_file", "Files with no talk"),
        ("unmatched_talk", "Talks with no file"),
        ("matched", "Already matched"),
    ] {
        let rows: Vec<&SheetRow> = review.rows.iter().filter(|r| r.status == status).collect();
        let _ = writeln!(out, "<h2>{} ({})</h2>", heading, rows.len());
        if rows.is_empty() {
            continue;
        }
        out.push_str(
            "<table><tr><th>File</th><th>Length</th><th>File date</th><th>Vimeo id</th>\
             <th>Talk</th><th>Talk date</th><th>Talk length</th><th>State</th><th>How</th>\
             <th>Similarity</th></tr>\n",
        );
        for r in rows {
            let _ = writeln!(
                out,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                 <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape(&r.file),
                clock(r.duration_s),
                opt(r.file_date),
                escape(&r.vimeo_id),
                escape(&r.talk_title),
                opt(r.talk_date),
                clock(r.talk_duration_s),
                escape(&r.talk_state),
                escape(&r.how),
                r.similarity.map_or_else(String::new, |s| format!("{s:.2}")),
            );
        }
        out.push_str("</table>\n");
    }
    out.push_str("</body></html>\n");
    out
}

/// Writes `<dir>/<source>.csv` and `<dir>/<source>.html` and returns the CSV's path.
pub fn write_review(
    dir: &Path,
    review: &Review,
    generated_at: &str,
) -> Result<std::path::PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let csv_path = dir.join(format!("{}.csv", review.source));
    let csv = to_csv(review).map_err(std::io::Error::other)?;
    std::fs::write(&csv_path, csv)?;
    let name = csv_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    std::fs::write(
        dir.join(format!("{}.html", review.source)),
        render_html(review, generated_at, &name),
    )?;
    Ok(csv_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wt_core::ids::TalkId;

    fn cfg() -> ClassifyConfig {
        ClassifyConfig::parse("", "pipeline", "", "series").unwrap_or_else(|e| panic!("{e}"))
    }

    fn file(key: &str, secs: Option<i32>) -> StorageFile {
        StorageFile {
            key: key.into(),
            bytes: 10,
            modified_s: 1,
            duration_s: secs,
        }
    }

    fn talk(id: &str, title: &str, date: (i32, u32, u32), secs: i32, sim: f32) -> StorageTalk {
        StorageTalk {
            talk: TalkId::from(sqlx::types::Uuid::nil()),
            vimeo_id: id.into(),
            title: title.into(),
            talk_date: NaiveDate::from_ymd_opt(date.0, date.1, date.2),
            published_at: None,
            duration_s: Some(secs),
            state: "needs_review".into(),
            state_reason: Some("no_media".into()),
            storage: None,
            similarity: sim,
        }
    }

    #[test]
    fn a_vimeo_id_in_the_name_matches_that_talk() {
        let f = facts(&file("export/Some Talk (812345678).mp4", Some(10)));
        let talks = [
            talk("812345678", "Other Words", (2021, 1, 1), 3000, 0.1),
            talk("812345679", "Some Talk", (2021, 1, 1), 10, 0.9),
        ];
        assert_eq!(
            choose(&cfg(), &f, &talks),
            Some(("812345678".to_owned(), "vimeo_id"))
        );
    }

    #[test]
    fn a_dated_file_matches_by_date_and_length() {
        let f = facts(&file("2021-11-07_Sunday_Talk.mp4", Some(2400)));
        assert_eq!(f.date, NaiveDate::from_ymd_opt(2021, 11, 7));
        let talks = [
            talk("1", "Unrelated 2021-11-07", (2021, 11, 7), 2410, 0.1),
            talk("2", "Unrelated", (2021, 11, 7), 5000, 0.1),
        ];
        assert_eq!(
            choose(&cfg(), &f, &talks),
            Some(("1".to_owned(), "recording_date"))
        );
    }

    #[test]
    fn an_undated_file_needs_a_unique_title_of_a_fitting_length() {
        let f = facts(&file("Shobogenzo-Uji_Part-3.mp4", Some(2400)));
        let near = talk("1", "Shobogenzo Uji Part 3", (2021, 1, 1), 2450, 0.9);
        let long = talk("2", "Shobogenzo Uji Part 3", (2021, 1, 1), 4000, 1.0);
        let weak = talk("3", "Something else", (2021, 1, 1), 2400, 0.2);
        assert_eq!(
            choose(&cfg(), &f, &[near.clone(), long, weak.clone()]),
            Some(("1".to_owned(), "title"))
        );
        let twin = talk("4", "Shobogenzo Uji Part 3", (2022, 1, 1), 2400, 0.9);
        assert_eq!(
            choose(&cfg(), &f, &[near, twin]),
            None,
            "a tie stays unmatched"
        );
        assert_eq!(
            choose(&cfg(), &f, &[weak]),
            None,
            "a weak title stays unmatched"
        );
        assert_eq!(
            choose(&cfg(), &facts(&file("x.mp4", None)), &[]),
            None,
            "no date and no length"
        );
    }

    #[test]
    fn a_sheet_round_trips_and_only_yes_rows_with_ids_are_confirmed() {
        let review = Review {
            source: "s".into(),
            rows: vec![
                SheetRow {
                    status: "proposed".into(),
                    accept: "yes".into(),
                    file: "a.mp4".into(),
                    vimeo_id: "1".into(),
                    talk_title: "A, \"quoted\"".into(),
                    ..SheetRow::default()
                },
                SheetRow {
                    status: "unmatched_file".into(),
                    file: "b.mp4".into(),
                    ..SheetRow::default()
                },
            ],
            files: 2,
            counts: BTreeMap::new(),
        };
        let csv = to_csv(&review).unwrap();
        let mut sheet = read_sheet(&csv).unwrap();
        assert_eq!(sheet, review.rows);
        assert_eq!(confirmed(&review, None).len(), 1);
        sheet[1].accept = "Y".into();
        assert_eq!(
            confirmed(&review, Some(&sheet)).len(),
            1,
            "no Vimeo id typed"
        );
        sheet[1].vimeo_id = "2".into();
        assert_eq!(confirmed(&review, Some(&sheet)).len(), 2);
        let html = render_html(&review, "now", "s.csv");
        assert!(html.contains("A, &quot;quoted&quot;"));
    }
}
