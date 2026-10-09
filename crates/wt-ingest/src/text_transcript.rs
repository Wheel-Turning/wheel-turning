//! Text-only source transcripts (Spec 04, Text-only source transcripts; owner's decision of
//! October 8, 2026). When a source's own transcript is all there is, because the talk has no
//! audio or its recording does not align with the text, the transcript is built from the text:
//! speaker labels say whose words are whose, `[mm:ss]` markers give times where present, and
//! steady speech fills in the rest. The words then go through segmentation as recognized words
//! do, so student turns are dropped by the same rules.

use wt_asr::align::labeled_source_words;
use wt_core::transcript::Word;
use wt_segment::Turn;
use wt_segment::scope::{self, Scope};

/// The pace assumed between markers, or without any: 150 words a minute.
pub const MS_PER_WORD: i32 = 400;

/// The teacher's speaker name in the turns built from text.
const TEACHER: &str = "T";
/// Speech after questions are invited in an unlabeled text, whose speakers are unknown.
const UNKNOWN: &str = "U";

/// What the text gives.
#[derive(Debug, Clone, PartialEq)]
pub enum TextPlan {
    /// Words and turns ready for segmentation.
    Ready {
        /// The source's words with estimated times.
        words: Vec<Word>,
        /// Speaker turns from the labels.
        turns: Vec<Turn>,
        /// The talk's length as estimated.
        audio_ms: i32,
        /// Whether the text labels its speakers.
        labeled: bool,
    },
    /// The text cannot be used alone: the talk waits for a person with this reason.
    Held(&'static str),
}

/// The labels that name the teacher: each name and alias as written, its words, and the
/// initials of a name of two or more words ("Suzuki Roshi" gives `SR`), plus `A` for answer
/// and `Teacher`. Compared without case.
pub fn teacher_labels(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec!["a".into(), "teacher".into()];
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        out.push(name.to_lowercase());
        let parts: Vec<&str> = name.split_whitespace().collect();
        for p in &parts {
            if p.chars().count() >= 3 {
                out.push(p.to_lowercase());
            }
        }
        if parts.len() >= 2 {
            out.push(
                parts
                    .iter()
                    .filter_map(|p| p.chars().next())
                    .flat_map(char::to_lowercase)
                    .collect(),
            );
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Builds words and turns from a source's text. In a labeled text, words before any label are
/// the teacher's, a label naming the teacher (`teacher_labels`) starts the teacher's speech,
/// any other label starts another speaker's, and an unlabeled paragraph continues whoever spoke
/// last, so an unlabeled paragraph after a student's is dropped with it. An unlabeled text is
/// read as the audio path reads its words: a monologue is the teacher's throughout; one that
/// invites questions is the teacher's until the invitation and of unknown speakers after it,
/// so the questions and answers are dropped; a conversation with no invitation is held, since
/// nothing can tell its speakers apart.
pub fn plan(text: &str, teacher_labels: &[String], monologue_max_per_hour: f64) -> TextPlan {
    let source = labeled_source_words(text);
    if source.is_empty() {
        return TextPlan::Held("no_speech");
    }
    let labeled = source.iter().any(|(_, l)| l.is_some());

    // Times: markers where present, a steady pace between and beyond them.
    let mut anchors: Vec<(usize, i32)> = Vec::new();
    for (i, (w, _)) in source.iter().enumerate() {
        if let Some(ms) = w.marker_ms
            && anchors.last().is_none_or(|&(_, last)| ms > last)
        {
            anchors.push((i, ms));
        }
    }
    let starts = times(source.len(), &anchors);
    let words: Vec<Word> = source
        .iter()
        .zip(&starts)
        .enumerate()
        .map(|(i, ((w, _), &s))| {
            let next = starts.get(i + 1).copied().unwrap_or(s + MS_PER_WORD);
            let e = (s + MS_PER_WORD * 4 / 5).min(next.max(s + 1));
            Word::new(w.text.clone(), s, e.max(s + 1), 1.0)
        })
        .collect();
    let audio_ms = words.last().map_or(0, |w| w.end_ms);

    let speakers: Vec<String> = if labeled {
        let mut current = TEACHER.to_owned();
        source
            .iter()
            .map(|(_, label)| {
                if let Some(l) = label.as_deref() {
                    current = if teacher_labels.contains(&l.to_lowercase()) {
                        TEACHER.to_owned()
                    } else {
                        format!("L:{l}")
                    };
                }
                current.clone()
            })
            .collect()
    } else {
        match scope::scope(&words, audio_ms, 0, monologue_max_per_hour) {
            Scope::Monologue => vec![TEACHER.to_owned(); words.len()],
            Scope::From { cue_ms, .. } => words
                .iter()
                .map(|w| {
                    if w.start_ms < cue_ms {
                        TEACHER
                    } else {
                        UNKNOWN
                    }
                    .to_owned()
                })
                .collect(),
            Scope::Full => return TextPlan::Held("unlabeled_dialogue"),
        }
    };

    let mut turns: Vec<Turn> = Vec::new();
    for (w, speaker) in words.iter().zip(&speakers) {
        match turns.last_mut() {
            Some(t) if t.speaker == *speaker => t.end_ms = w.end_ms,
            _ => turns.push(Turn {
                speaker: speaker.clone(),
                start_ms: w.start_ms,
                end_ms: w.end_ms,
            }),
        }
    }
    TextPlan::Ready {
        words,
        turns,
        audio_ms,
        labeled,
    }
}

/// Each word's start: interpolated between anchors, at `MS_PER_WORD` before the first and after
/// the last.
fn times(n: usize, anchors: &[(usize, i32)]) -> Vec<i32> {
    let pace = |count: usize| {
        i32::try_from(count)
            .unwrap_or(i32::MAX)
            .saturating_mul(MS_PER_WORD)
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let next = anchors.iter().position(|&(a, _)| a >= i);
        let ms = match next {
            Some(k) if anchors[k].0 == i => anchors[k].1,
            Some(0) => {
                let (a, ms) = anchors[0];
                (ms - pace(a - i)).max(0)
            }
            Some(k) => {
                let (a0, m0) = anchors[k - 1];
                let (a1, m1) = anchors[k];
                let span = i32::try_from(a1 - a0).unwrap_or(i32::MAX).max(1);
                let pos = i32::try_from(i - a0).unwrap_or(0);
                m0 + (m1 - m0) / span * pos
            }
            None => match anchors.last() {
                Some(&(a, ms)) => ms.saturating_add(pace(i - a)),
                None => pace(i),
            },
        };
        out.push(ms);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentence(prefix: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("{prefix}{i}"))
            .collect::<Vec<_>>()
            .join(" ")
            + "."
    }

    #[test]
    fn labels_name_the_teacher_by_name_alias_and_initials() {
        let l = teacher_labels(&["Suzuki Roshi".into(), "Shunryu Suzuki".into()]);
        for want in [
            "sr",
            "ss",
            "suzuki",
            "roshi",
            "suzuki roshi",
            "a",
            "teacher",
        ] {
            assert!(l.contains(&want.to_owned()), "{want} in {l:?}");
        }
        assert!(!l.contains(&"student".to_owned()));
    }

    #[test]
    fn a_labeled_text_keeps_the_teacher_and_drops_the_students_paragraphs() {
        let text = format!(
            "{}\n\nStudent A: {}\n\n{}\n\nSR: {}\n\nPS: {}",
            sentence("t", 50),
            sentence("s", 10),
            sentence("s", 5),
            sentence("a", 20),
            sentence("p", 3),
        );
        let TextPlan::Ready {
            words,
            turns,
            labeled,
            ..
        } = plan(&text, &teacher_labels(&["Suzuki Roshi".into()]), 4.0)
        else {
            panic!("expected a plan");
        };
        assert!(labeled);
        let speakers: Vec<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
        assert_eq!(speakers, vec!["T", "L:Student A", "T", "L:PS"]);
        // The unlabeled paragraph after the student's stays the student's.
        assert_eq!(turns[1].end_ms, words[64].end_ms);
        assert_eq!(turns[2].start_ms, words[65].start_ms);
    }

    #[test]
    fn markers_set_times_and_the_pace_fills_the_rest() {
        let text = format!(
            "[00:00]\n\n{}\n\n[02:00]\n\n{}",
            sentence("t", 100),
            sentence("u", 10)
        );
        let TextPlan::Ready {
            words, audio_ms, ..
        } = plan(&text, &[], 4.0)
        else {
            panic!("expected a plan");
        };
        assert_eq!(words[0].start_ms, 0);
        assert_eq!(words[50].start_ms, 60_000, "halfway between the markers");
        assert_eq!(words[100].start_ms, 120_000);
        assert_eq!(words[105].start_ms, 120_000 + 5 * MS_PER_WORD);
        assert!(audio_ms > 120_000);
        assert!(words.windows(2).all(|p| p[0].start_ms <= p[1].start_ms));
    }

    #[test]
    fn an_unlabeled_monologue_is_the_teachers() {
        let text = sentence("t", 3000);
        let TextPlan::Ready { turns, labeled, .. } = plan(&text, &[], 4.0) else {
            panic!("expected a plan");
        };
        assert!(!labeled);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, "T");
    }

    #[test]
    fn an_unlabeled_text_that_invites_questions_drops_everything_after() {
        let text = format!(
            "{} So are there any questions? {}",
            sentence("t", 3000),
            sentence("q", 200)
        );
        let TextPlan::Ready { turns, .. } = plan(&text, &[], 4.0) else {
            panic!("expected a plan");
        };
        let speakers: Vec<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
        assert_eq!(speakers, vec!["T", "U"]);
    }

    #[test]
    fn an_unlabeled_conversation_is_held() {
        let text = "What do you think? Yes. And the pond? No. It is still. ".repeat(60);
        assert_eq!(plan(&text, &[], 4.0), TextPlan::Held("unlabeled_dialogue"));
    }
}
