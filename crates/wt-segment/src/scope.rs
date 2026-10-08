//! How much of a talk to diarize (Spec 04, Diarization and segmentation; owner's decision of
//! October 8, 2026). Diarization is the slow step, and a dharma talk is one voice until its
//! questions begin. So the words, already transcribed, are read for the point where questions
//! are invited, and only the audio from a minute before it is diarized. A talk with no such
//! cue and few signs of conversation is a monologue and is not diarized at all; one with no cue
//! but many such signs is diarized whole.

use std::sync::LazyLock;

use regex::Regex;
use wt_core::transcript::Word;

use crate::segmenter::Turn;

/// What to diarize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The whole recording.
    Full,
    /// From this point to the end; the teacher holds everything before it.
    From {
        /// Where diarization starts.
        start_ms: i32,
        /// Where the cue that invited questions was heard.
        cue_ms: i32,
    },
    /// Nothing: one voice throughout.
    Monologue,
}

/// Phrases that open the floor to questions.
static INVITATION: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"\b(any questions|questions or comments|comments or questions|i invite you to unmute|unmute yourself|raise your hand|raise a hand|open (?:it|the floor|things) up|time for (?:some |a few )?(?:questions|discussion)|(?:does|did) anyone have (?:a|any) questions?|anybody have (?:a|any) questions?|questions anyone|let's (?:have|take) (?:some )?questions|we have (?:some |a little )?time for)\b",
    )
    .ok()
});

/// A sentence that is only a short reply: a sign of conversation.
static REPLY: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"^(yes|yeah|no|okay|ok|right|mm+-?hmm+|uh-huh|thank you|thanks|sure|i see|oh)[.!?]?$",
    )
    .ok()
});

fn clean(word: &str) -> String {
    word.to_lowercase()
}

/// Reads the words for the point questions are invited, ignoring the first fifth of the talk,
/// where "questions" are announced rather than asked for.
pub fn scope(words: &[Word], audio_ms: i32, preroll_ms: i32, monologue_max_per_hour: f64) -> Scope {
    // The text with each character's word, so a match finds its word's time.
    let mut text = String::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let t = clean(&w.text);
        text.push_str(&t);
        text.push(' ');
        owner.extend(std::iter::repeat_n(i, t.len() + 1));
    }
    let earliest = audio_ms / 5;
    if let Some(re) = INVITATION.as_ref() {
        for m in re.find_iter(&text) {
            let Some(&i) = owner.get(m.start()) else {
                continue;
            };
            let cue_ms = words[i].start_ms;
            if cue_ms >= earliest {
                return Scope::From {
                    start_ms: (cue_ms - preroll_ms).max(0),
                    cue_ms,
                };
            }
        }
    }
    // No invitation: how often does the text sound like a conversation?
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    for w in words {
        current.push_str(&clean(&w.text));
        current.push(' ');
        if w.text.trim_end().ends_with(['.', '?', '!']) {
            sentences.push(current.trim().to_owned());
            current.clear();
        }
    }
    let replies = REPLY.as_ref().map_or(0, |re| {
        sentences
            .windows(2)
            .filter(|p| p[0].ends_with('?') && re.is_match(&p[1]))
            .count()
            + sentences.iter().filter(|s| re.is_match(s)).count()
    });
    #[allow(clippy::cast_precision_loss)]
    let hours = (f64::from(audio_ms) / 3_600_000.0).max(0.25);
    #[allow(clippy::cast_precision_loss)]
    let per_hour = replies as f64 / hours;
    if per_hour <= monologue_max_per_hour {
        Scope::Monologue
    } else {
        Scope::Full
    }
}

/// The label the teacher gets in a scoped diarization.
pub const TEACHER: &str = "T";

/// Turns for the whole talk from a scope and, for `From`, the turns diarized from its start
/// (times relative to that start). The tail's speaker who holds most of the pre-roll, before
/// questions are invited, is the teacher, and holds everything before the start too.
pub fn assemble(scope: Scope, audio_ms: i32, tail: &[Turn]) -> Vec<Turn> {
    match scope {
        Scope::Full => tail.to_vec(),
        Scope::Monologue => vec![Turn {
            speaker: TEACHER.into(),
            start_ms: 0,
            end_ms: audio_ms,
        }],
        Scope::From { start_ms, cue_ms } => {
            let pre = cue_ms - start_ms;
            let mut held: Vec<(String, i32)> = Vec::new();
            for t in tail {
                let overlap = t.end_ms.min(pre) - t.start_ms.max(0);
                if overlap > 0 {
                    match held.iter_mut().find(|(s, _)| *s == t.speaker) {
                        Some((_, ms)) => *ms += overlap,
                        None => held.push((t.speaker.clone(), overlap)),
                    }
                }
            }
            let teacher = held
                .iter()
                .max_by_key(|(_, ms)| *ms)
                .map(|(s, _)| s.clone());
            let mut out = Vec::with_capacity(tail.len() + 1);
            if start_ms > 0 {
                out.push(Turn {
                    speaker: TEACHER.into(),
                    start_ms: 0,
                    end_ms: start_ms,
                });
            }
            for t in tail {
                let speaker = if Some(&t.speaker) == teacher.as_ref() {
                    TEACHER.to_owned()
                } else {
                    format!("Q{}", t.speaker)
                };
                out.push(Turn {
                    speaker,
                    start_ms: t.start_ms + start_ms,
                    end_ms: t.end_ms + start_ms,
                });
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str, every_ms: i32) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let i = i32::try_from(i).unwrap();
                Word::new(w, i * every_ms, i * every_ms + every_ms - 10, 0.9)
            })
            .collect()
    }

    #[test]
    fn finds_the_invitation_and_starts_a_minute_before() {
        let mut talk = "the pond is still and the moon rests on it. ".repeat(300);
        talk.push_str("So now are there any questions? Yes. Thank you for the talk. ");
        let w = words(&talk, 1000);
        let audio = i32::try_from(w.len()).unwrap() * 1000;
        let Scope::From { start_ms, cue_ms } = scope(&w, audio, 60_000, 4.0) else {
            panic!("expected a Q&A scope");
        };
        assert_eq!(cue_ms - start_ms, 60_000);
        assert!(cue_ms > audio * 9 / 10);
    }

    #[test]
    fn an_early_mention_of_questions_does_not_count() {
        let talk = format!(
            "today we will leave time at the end so any questions can wait. {}",
            "the pond is still and the moon rests on it. ".repeat(300)
        );
        let w = words(&talk, 1000);
        let audio = i32::try_from(w.len()).unwrap() * 1000;
        assert_eq!(scope(&w, audio, 60_000, 4.0), Scope::Monologue);
    }

    #[test]
    fn conversation_without_an_invitation_is_diarized_whole() {
        let talk = "what do you think about that? yes. and the pond? no. it is still. ".repeat(40);
        let w = words(&talk, 1000);
        let audio = i32::try_from(w.len()).unwrap() * 1000;
        assert_eq!(scope(&w, audio, 60_000, 4.0), Scope::Full);
    }

    #[test]
    fn the_tail_speaker_of_the_preroll_becomes_the_teacher() {
        let tail = vec![
            Turn {
                speaker: "S2".into(),
                start_ms: 0,
                end_ms: 60_000,
            },
            Turn {
                speaker: "S1".into(),
                start_ms: 60_000,
                end_ms: 70_000,
            },
            Turn {
                speaker: "S2".into(),
                start_ms: 70_000,
                end_ms: 120_000,
            },
        ];
        let turns = assemble(
            Scope::From {
                start_ms: 1_000_000,
                cue_ms: 1_060_000,
            },
            1_120_000,
            &tail,
        );
        assert_eq!(
            turns[0],
            Turn {
                speaker: "T".into(),
                start_ms: 0,
                end_ms: 1_000_000
            }
        );
        assert_eq!(turns[1].speaker, "T");
        assert_eq!(
            turns[2],
            Turn {
                speaker: "QS1".into(),
                start_ms: 1_060_000,
                end_ms: 1_070_000
            }
        );
        assert_eq!(turns[3].speaker, "T");
    }
}
