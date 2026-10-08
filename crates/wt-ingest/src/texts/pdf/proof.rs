//! The proofread gate (Spec 08, PDFs and OCR): which units a person reads against the page,
//! the standalone proof sheet they read them on, and how their answers pass or fail the
//! edition. The sample is drawn from a seed of the edition and its units, so the sheet and
//! `proof-import` agree on it without storing it.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;

use super::super::TextError;

/// Random units an OCR edition samples.
pub const OCR_RANDOM: usize = 12;
/// Lowest-confidence units an OCR edition samples.
pub const OCR_WORST: usize = 12;
/// Random units a native or text-layer edition samples.
pub const NATIVE_RANDOM: usize = 10;

/// Which part of the sample a unit is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// Chosen at random across the book.
    Random,
    /// Among the lowest OCR confidence.
    Worst,
    /// From a page range still weak after a second reading (Spec 08, Large documents).
    Range,
}

/// Units spot-checked from each weak page range.
pub const RANGE_UNITS: usize = 6;

/// A unit the sampler can choose.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Its id.
    pub id: Uuid,
    /// Its place.
    pub ordinal: i32,
    /// Its OCR confidence.
    pub confidence: Option<f32>,
    /// Its page.
    pub source_page: Option<i32>,
}

/// splitmix64, enough to draw a sample that anyone can draw again from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        #[allow(clippy::cast_possible_truncation)]
        let i = (self.next() % n.max(1) as u64) as usize;
        i
    }
}

/// The seed for an edition in a given state of its units.
pub fn seed(edition: Uuid, units_hash: &str) -> u64 {
    let digest = wt_core::records::sha256_hex(format!("{edition}:{units_hash}").as_bytes());
    u64::from_str_radix(&digest[..16], 16).unwrap_or(0)
}

/// The sample: for an OCR edition, 12 at random with one from each tenth of the pages where
/// the book is long enough, and the 12 lowest-confidence units among the rest; otherwise 10 at
/// random.
pub fn sample(units: &[Candidate], ocr: bool, seed: u64) -> Vec<(Uuid, Group)> {
    sample_with_ranges(units, ocr, seed, &[])
}

/// The sample, plus up to 6 units from each weak page range (first and last page) among the
/// units not already drawn.
pub fn sample_with_ranges(
    units: &[Candidate],
    ocr: bool,
    seed: u64,
    ranges: &[(i32, i32)],
) -> Vec<(Uuid, Group)> {
    let mut out = draw(units, ocr, seed);
    let mut rng = Rng(seed ^ 0x5241_4E47_4553);
    for &(first, last) in ranges {
        let mut pool: Vec<&Candidate> = units
            .iter()
            .filter(|u| u.source_page.is_some_and(|p| (first..=last).contains(&p)))
            .filter(|u| !out.iter().any(|(id, _)| *id == u.id))
            .collect();
        pool.sort_by_key(|u| u.ordinal);
        for _ in 0..RANGE_UNITS {
            if pool.is_empty() {
                break;
            }
            let u = pool.remove(rng.below(pool.len()));
            out.push((u.id, Group::Range));
        }
    }
    out
}

fn draw(units: &[Candidate], ocr: bool, seed: u64) -> Vec<(Uuid, Group)> {
    let mut rng = Rng(seed);
    let mut sorted: Vec<&Candidate> = units.iter().collect();
    sorted.sort_by_key(|u| u.ordinal);
    let want = if ocr { OCR_RANDOM } else { NATIVE_RANDOM };
    let mut chosen: Vec<usize> = Vec::new();
    if sorted.len() <= want {
        chosen = (0..sorted.len()).collect();
    } else {
        // One from each tenth: of the pages when they are known, else of the units.
        let pages: Vec<i32> = sorted.iter().filter_map(|u| u.source_page).collect();
        let (lo, hi) = (
            pages.iter().copied().min().unwrap_or(0),
            pages.iter().copied().max().unwrap_or(0),
        );
        let tenth = |i: usize| -> usize {
            match sorted[i].source_page {
                Some(p) if hi > lo => {
                    #[allow(clippy::cast_sign_loss)]
                    let t = ((p - lo) as usize * 10) / ((hi - lo) as usize + 1);
                    t
                }
                _ => i * 10 / sorted.len(),
            }
        };
        if ocr {
            for t in 0..10 {
                let bucket: Vec<usize> = (0..sorted.len()).filter(|&i| tenth(i) == t).collect();
                if !bucket.is_empty() && chosen.len() < want {
                    chosen.push(bucket[rng.below(bucket.len())]);
                }
            }
        }
        while chosen.len() < want {
            let rest: Vec<usize> = (0..sorted.len()).filter(|i| !chosen.contains(i)).collect();
            if rest.is_empty() {
                break;
            }
            chosen.push(rest[rng.below(rest.len())]);
        }
        chosen.sort_unstable();
    }
    let mut out: Vec<(Uuid, Group)> = chosen
        .iter()
        .map(|&i| (sorted[i].id, Group::Random))
        .collect();
    if ocr {
        let mut rest: Vec<&Candidate> = sorted
            .iter()
            .enumerate()
            .filter(|(i, _)| !chosen.contains(i))
            .map(|(_, u)| *u)
            .collect();
        rest.sort_by(|a, b| {
            a.confidence
                .unwrap_or(1.0)
                .total_cmp(&b.confidence.unwrap_or(1.0))
                .then(a.ordinal.cmp(&b.ordinal))
        });
        out.extend(rest.iter().take(OCR_WORST).map(|u| (u.id, Group::Worst)));
    }
    out
}

/// One answer from the proof sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    /// The unit.
    pub unit_id: Uuid,
    /// Its group.
    pub group: Group,
    /// Wrong, missing or extra words; 0 when the unit is correct.
    pub errors: u32,
    /// What was wrong.
    #[serde(default)]
    pub note: Option<String>,
}

/// The file the proof sheet saves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofFile {
    /// The edition.
    pub edition_id: Uuid,
    /// The units' hash when the sheet was made.
    pub units_hash: String,
    /// One answer per sampled unit.
    pub answers: Vec<Answer>,
}

/// The outcome of a proofread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// Random units read.
    pub random_sampled: i32,
    /// Errors in them.
    pub random_errors: i32,
    /// Lowest-confidence units read.
    pub worst_sampled: i32,
    /// Errors in them.
    pub worst_errors: i32,
    /// Units read from weak page ranges.
    pub range_sampled: i32,
    /// Errors in them.
    pub range_errors: i32,
    /// Whether the edition passes.
    pub passed: bool,
    /// The notes on units with errors, as the report's error patterns.
    pub notes: Vec<String>,
}

/// Scores answers against the sample: every sampled unit answered once in its group, nothing
/// else. Passes with at most `random_max` errors in the random units and `worst_max` in the
/// lowest-confidence ones.
pub fn score(
    sample: &[(Uuid, Group)],
    answers: &[Answer],
    random_max: u32,
    worst_max: u32,
) -> Result<Verdict, TextError> {
    score_with_ranges(sample, answers, random_max, worst_max, 0)
}

/// [`score`], with at most `range_max` errors allowed across the weak-range units.
pub fn score_with_ranges(
    sample: &[(Uuid, Group)],
    answers: &[Answer],
    random_max: u32,
    worst_max: u32,
    range_max: u32,
) -> Result<Verdict, TextError> {
    for a in answers {
        if !sample.contains(&(a.unit_id, a.group)) {
            return Err(TextError::Proof(format!(
                "unit {} is not in the {:?} sample",
                a.unit_id, a.group
            )));
        }
    }
    for (id, group) in sample {
        let n = answers
            .iter()
            .filter(|a| a.unit_id == *id && a.group == *group)
            .count();
        if n != 1 {
            return Err(TextError::Proof(format!(
                "unit {id} is answered {n} times; the sheet needs one answer for each sampled unit"
            )));
        }
    }
    let sum = |g: Group| -> u32 {
        answers
            .iter()
            .filter(|a| a.group == g)
            .map(|a| a.errors)
            .sum()
    };
    let count = |g: Group| sample.iter().filter(|(_, x)| *x == g).count();
    let (re, we, ge) = (sum(Group::Random), sum(Group::Worst), sum(Group::Range));
    let to_i32 = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
    let to_i32u = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    Ok(Verdict {
        random_sampled: to_i32u(count(Group::Random)),
        random_errors: to_i32(re),
        worst_sampled: to_i32u(count(Group::Worst)),
        worst_errors: to_i32(we),
        range_sampled: to_i32u(count(Group::Range)),
        range_errors: to_i32(ge),
        passed: re <= random_max && we <= worst_max && ge <= range_max,
        notes: answers
            .iter()
            .filter(|a| a.errors > 0)
            .map(|a| {
                format!(
                    "{} ({} error{}): {}",
                    a.unit_id,
                    a.errors,
                    if a.errors == 1 { "" } else { "s" },
                    a.note.as_deref().unwrap_or("no note")
                )
            })
            .collect(),
    })
}

/// A unit on the proof sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetUnit {
    /// Its id.
    pub id: Uuid,
    /// Its group.
    pub group: Group,
    /// Its label.
    pub label: String,
    /// Its text.
    pub text: String,
    /// Its page, when known.
    pub page: Option<i32>,
    /// A PNG crop of its page, base64.
    pub crop_png_base64: Option<String>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The standalone proof sheet. It needs no server: the reader marks each unit and saves a
/// `proof.json` for `wt text proof-import`.
pub fn sheet(title: &str, edition: Uuid, units_hash: &str, units: &[SheetUnit]) -> String {
    let mut rows = String::new();
    for (i, u) in units.iter().enumerate() {
        let image = u.crop_png_base64.as_ref().map_or_else(
            || "<p class=\"none\">No page image: read against the source.</p>".to_owned(),
            |b| format!("<img alt=\"page crop\" src=\"data:image/png;base64,{b}\">"),
        );
        let page = u.page.map(|p| format!(", page {p}")).unwrap_or_default();
        let group = match u.group {
            Group::Random => "random",
            Group::Worst => "lowest confidence",
            Group::Range => "weak page range",
        };
        let _ = writeln!(
            rows,
            "<section data-unit=\"{id}\" data-group=\"{g}\"><h2>{n}. {label}{page} \
             <small>{group}</small></h2><div class=\"pair\"><pre>{text}</pre>{image}</div>\
             <label>Wrong, missing or extra words <input type=\"number\" min=\"0\" value=\"0\">\
             </label><label>Note <input type=\"text\" class=\"note\"></label></section>",
            id = u.id,
            g = serde_json::to_string(&u.group)
                .unwrap_or_default()
                .trim_matches('"'),
            n = i + 1,
            label = esc(&u.label),
            text = esc(&u.text),
        );
    }
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Proof: {title}</title>
<style>
body {{ font: 16px/1.5 system-ui, sans-serif; margin: 0 auto; max-width: 1200px; padding: 16px; background: #fff; color: #111; }}
.pair {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }}
pre {{ white-space: pre-wrap; font: 15px/1.5 Georgia, serif; margin: 0; }}
img {{ max-width: 100%; border: 1px solid #ccc; }}
section {{ border-top: 1px solid #ddd; padding: 12px 0; }}
small {{ color: #666; font-weight: normal; }}
label {{ display: block; margin-top: 8px; }}
.note {{ width: 60%; }}
@media (max-width: 700px) {{ .pair {{ grid-template-columns: 1fr; }} }}
</style></head><body>
<h1>Proof: {title}</h1>
<p>Read each unit against its page. Count any wrong, missing or extra word as an error. Then save the answers and run <code>wt text proof-import</code>.</p>
{rows}<button id="save">Save proof.json</button>
<script>
document.getElementById("save").addEventListener("click", () => {{
  const answers = [...document.querySelectorAll("section[data-unit]")].map(s => ({{
    unit_id: s.dataset.unit, group: s.dataset.group,
    errors: Number(s.querySelector("input[type=number]").value) || 0,
    note: s.querySelector(".note").value || null }}));
  const body = JSON.stringify({{ edition_id: "{edition}", units_hash: "{units_hash}", answers }}, null, 2);
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([body], {{ type: "application/json" }}));
  a.download = "proof.json"; a.click();
}});
</script></body></html>
"#,
        title = esc(title),
        units_hash = esc(units_hash),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(n: i32) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                id: Uuid::from_u128(u128::try_from(i).unwrap() + 1),
                ordinal: i,
                confidence: Some(0.99 - f32::from(u16::try_from(i % 50).unwrap()) / 100.0),
                source_page: Some(i / 3 + 1),
            })
            .collect()
    }

    #[test]
    fn ocr_sample_is_twelve_random_across_the_book_and_twelve_worst() {
        let us = units(300);
        let s = sample(&us, true, 7);
        let random: Vec<Uuid> = s
            .iter()
            .filter(|(_, g)| *g == Group::Random)
            .map(|(id, _)| *id)
            .collect();
        let worst: Vec<Uuid> = s
            .iter()
            .filter(|(_, g)| *g == Group::Worst)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!((random.len(), worst.len()), (12, 12));
        assert!(random.iter().all(|id| !worst.contains(id)));
        // One from each tenth of the pages.
        for t in 0..10 {
            assert!(
                random.iter().any(|id| {
                    let u = us.iter().find(|u| u.id == *id).unwrap();
                    (u.source_page.unwrap() - 1) * 10 / 100 == t
                }),
                "tenth {t}"
            );
        }
        // The worst are the lowest confidence among the rest.
        let floor = us
            .iter()
            .filter(|u| !random.contains(&u.id) && !worst.contains(&u.id))
            .map(|u| u.confidence.unwrap())
            .fold(f32::MAX, f32::min);
        assert!(
            worst
                .iter()
                .all(|id| us.iter().find(|u| u.id == *id).unwrap().confidence.unwrap() <= floor)
        );
        assert_eq!(
            s,
            sample(&us, true, 7),
            "the same seed draws the same sample"
        );
    }

    #[test]
    fn native_sample_is_ten_random() {
        let s = sample(&units(50), false, 1);
        assert_eq!(s.len(), 10);
        assert!(s.iter().all(|(_, g)| *g == Group::Random));
        assert_eq!(sample(&units(4), false, 1).len(), 4);
    }

    fn answers(s: &[(Uuid, Group)], random_errors: u32, worst_errors: u32) -> Vec<Answer> {
        let mut out: Vec<Answer> = s
            .iter()
            .map(|(id, g)| Answer {
                unit_id: *id,
                group: *g,
                errors: 0,
                note: None,
            })
            .collect();
        if let Some(a) = out.iter_mut().find(|a| a.group == Group::Random) {
            a.errors = random_errors;
            a.note = Some("ligature".into());
        }
        if let Some(a) = out.iter_mut().find(|a| a.group == Group::Worst) {
            a.errors = worst_errors;
        }
        out
    }

    #[test]
    fn scores_pass_and_fail() {
        let s = sample(&units(300), true, 3);
        let v = score(&s, &answers(&s, 1, 3), 1, 3).unwrap();
        assert!(v.passed);
        assert_eq!(
            (
                v.random_sampled,
                v.random_errors,
                v.worst_sampled,
                v.worst_errors
            ),
            (12, 1, 12, 3)
        );
        assert!(!score(&s, &answers(&s, 2, 0), 1, 3).unwrap().passed);
        assert!(!score(&s, &answers(&s, 0, 4), 1, 3).unwrap().passed);
        let failed = score(&s, &answers(&s, 2, 0), 1, 3).unwrap();
        assert!(failed.notes[0].contains("ligature"));
    }

    #[test]
    fn refuses_answers_that_are_not_the_sample() {
        let s = sample(&units(300), true, 3);
        let mut a = answers(&s, 0, 0);
        a.pop();
        assert!(score(&s, &a, 1, 3).is_err());
        let mut b = answers(&s, 0, 0);
        b.push(Answer {
            unit_id: Uuid::from_u128(9999),
            group: Group::Random,
            errors: 0,
            note: None,
        });
        assert!(score(&s, &b, 1, 3).is_err());
    }

    #[test]
    fn sheet_shows_each_unit_beside_its_crop() {
        let html = sheet(
            "A <Work>",
            Uuid::from_u128(1),
            "abc",
            &[SheetUnit {
                id: Uuid::from_u128(2),
                group: Group::Worst,
                label: "Work, paragraph 3".into(),
                text: "The moon & the water".into(),
                page: Some(14),
                crop_png_base64: Some("iVBORw0KGgo=".into()),
            }],
        );
        assert!(html.contains("A &lt;Work&gt;"));
        assert!(html.contains("data-group=\"worst\""));
        assert!(html.contains("page 14"));
        assert!(html.contains("The moon &amp; the water"));
        assert!(html.contains("data:image/png;base64,iVBORw0KGgo="));
    }

    #[test]
    fn weak_ranges_add_six_units_each() {
        let us = units(300);
        let s = sample_with_ranges(&us, true, 5, &[(40, 45)]);
        let range: Vec<&Uuid> = s
            .iter()
            .filter(|(_, g)| *g == Group::Range)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(range.len(), 6);
        assert!(range.iter().all(|id| {
            let p = us
                .iter()
                .find(|u| u.id == **id)
                .unwrap()
                .source_page
                .unwrap();
            (40..=45).contains(&p)
        }));
        let mut a: Vec<Answer> = s
            .iter()
            .map(|(id, g)| Answer {
                unit_id: *id,
                group: *g,
                errors: 0,
                note: None,
            })
            .collect();
        a.iter_mut()
            .find(|x| x.group == Group::Range)
            .unwrap()
            .errors = 2;
        assert!(!score_with_ranges(&s, &a, 1, 3, 1).unwrap().passed);
        a.iter_mut()
            .find(|x| x.group == Group::Range && x.errors == 2)
            .unwrap()
            .errors = 1;
        let v = score_with_ranges(&s, &a, 1, 3, 1).unwrap();
        assert!(v.passed);
        assert_eq!((v.range_sampled, v.range_errors), (6, 1));
    }
}
