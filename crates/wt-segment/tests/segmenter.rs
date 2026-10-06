//! Spec 04, Diarization and segmentation: the segmenter on synthetic timelines. Every word is
//! invented: the teacher says `t<n>`, a student says `s<n>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use wt_core::transcript::Word;
use wt_segment::{Label, Review, SegmentSettings, Segmentation, SpanKind, Turn, plan};

/// Speech by `speaker` from `start_s` to `end_s` at `wpm` words a minute, with words from
/// `vocab`; returns its words and its turn.
fn speech(
    speaker: &str,
    start_s: f64,
    end_s: f64,
    wpm: f64,
    vocab: &mut dyn FnMut(usize) -> String,
) -> (Vec<Word>, Turn) {
    let step = 60_000.0 / wpm;
    let start = (start_s * 1000.0) as i32;
    let end = (end_s * 1000.0) as i32;
    let mut words = Vec::new();
    let mut at = f64::from(start);
    let mut n = 0;
    while at + step * 0.8 <= f64::from(end) {
        words.push(Word::new(
            vocab(n),
            at as i32,
            (at + step * 0.8) as i32,
            0.9,
        ));
        at += step;
        n += 1;
    }
    (
        words,
        Turn {
            speaker: speaker.into(),
            start_ms: start,
            end_ms: end,
        },
    )
}

struct Timeline {
    words: Vec<Word>,
    turns: Vec<Turn>,
    next: usize,
}

impl Timeline {
    fn new() -> Self {
        Self {
            words: Vec::new(),
            turns: Vec::new(),
            next: 0,
        }
    }

    /// The teacher talking: distinct words at 150 a minute.
    fn teacher(&mut self, start_s: f64, end_s: f64) -> &mut Self {
        let base = self.next;
        let (w, t) = speech("S1", start_s, end_s, 150.0, &mut |n| {
            format!("t{}", base + n)
        });
        self.next += w.len();
        self.words.extend(w);
        self.turns.push(t);
        self
    }

    fn student(&mut self, start_s: f64, end_s: f64) -> &mut Self {
        let base = self.next;
        let (w, t) = speech("S2", start_s, end_s, 150.0, &mut |n| {
            format!("s{}", base + n)
        });
        self.next += w.len();
        self.words.extend(w);
        self.turns.push(t);
        self
    }

    /// Slow, repeated chanting by the teacher.
    fn chant(&mut self, start_s: f64, end_s: f64) -> &mut Self {
        let line = ["namu", "kie", "butsu"];
        let (w, t) = speech("S1", start_s, end_s, 40.0, &mut |n| line[n % 3].to_owned());
        self.words.extend(w);
        self.turns.push(t);
        self
    }

    fn run(&self, audio_s: i32) -> Result<Segmentation, Review> {
        let p = plan(
            &self.words,
            &self.turns,
            audio_s * 1000,
            &["namu kie butsu".to_owned()],
            &SegmentSettings::default(),
        )?;
        let labels: BTreeMap<usize, Label> = p
            .ambiguous()
            .iter()
            .map(|w| (w.index, Label::Other))
            .collect();
        Ok(p.finish(&labels).unwrap())
    }
}

fn kinds(s: &Segmentation) -> Vec<SpanKind> {
    s.segments.iter().map(|g| g.kind).collect()
}

#[test]
fn a_silence_of_two_minutes_is_sitting_and_splits_the_talk() {
    let s = Timeline::new()
        .teacher(0.0, 120.0)
        .teacher(260.0, 380.0)
        .run(380)
        .unwrap();
    let sitting: Vec<_> = s
        .spans
        .iter()
        .filter(|sp| sp.kind == SpanKind::Sitting)
        .collect();
    assert_eq!(sitting.len(), 1);
    assert!(
        sitting[0].start_ms >= 119_000 && sitting[0].end_ms == 260_000,
        "{sitting:?}"
    );
    assert_eq!(
        kinds(&s),
        [SpanKind::Talk, SpanKind::Talk],
        "no segment spans the sitting"
    );
    assert!(s.segments[0].end_ms <= sitting[0].start_ms);
}

#[test]
fn a_chant_window_is_labeled_chant_and_not_indexed() {
    let s = Timeline::new()
        .teacher(0.0, 120.0)
        .chant(120.0, 240.0)
        .teacher(240.0, 360.0)
        .run(360)
        .unwrap();
    let chant: Vec<_> = s
        .spans
        .iter()
        .filter(|sp| sp.kind == SpanKind::Chant)
        .collect();
    assert!(!chant.is_empty(), "{:?}", s.spans);
    for seg in &s.segments {
        assert!(
            !seg.text.contains("namu"),
            "chant text never becomes a segment"
        );
    }
    assert_eq!(kinds(&s), [SpanKind::Talk, SpanKind::Talk]);
}

#[test]
fn a_student_turn_is_dropped_and_the_answer_is_qa_teacher() {
    let s = Timeline::new()
        .teacher(0.0, 300.0)
        .student(300.0, 320.0)
        .teacher(320.0, 400.0)
        .run(400)
        .unwrap();
    assert_eq!(s.teacher, "S1");
    assert_eq!(kinds(&s), [SpanKind::Talk, SpanKind::QaTeacher]);
    assert_eq!(s.student_turns.len(), 1);
    let (from, to) = s.student_turns[0];
    assert!(from >= 300_000 && to <= 320_000);
    // No student word survives anywhere that could be stored.
    assert!(s.words.iter().all(|w| w.text.starts_with('t')));
    assert!(s.segments.iter().all(|g| !g.text.contains('s')));
    assert!(s.spans.iter().all(|sp| sp.start_ms >= 320_000
        || sp.end_ms <= 300_000 + 1000
        || sp.words.is_none()
        || sp.kind != SpanKind::QaTeacher));
}

#[test]
fn a_second_speaker_at_thirty_five_percent_goes_to_review() {
    let err = Timeline::new()
        // 90 of 240 seconds: 37.5 percent.
        .teacher(0.0, 120.0)
        .student(120.0, 210.0)
        .teacher(210.0, 240.0)
        .run(240)
        .map(|_| ())
        .unwrap_err();
    match err {
        Review::MultiSpeaker { share } => assert!(share >= 0.35, "{share}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(err.reason(), "multi_speaker");
    // Just under the threshold is fine.
    assert!(
        Timeline::new()
            .teacher(0.0, 200.0)
            .student(200.0, 260.0)
            .teacher(260.0, 300.0)
            .run(300)
            .is_ok()
    );
}

#[test]
fn ambiguous_windows_need_a_label_and_carry_only_the_teachers_words() {
    let mut t = Timeline::new();
    t.teacher(0.0, 120.0);
    // 75 words a minute: neither talk nor chant by the rules.
    let (w, turn) = speech("S1", 120.0, 180.0, 75.0, &mut |n| format!("slow{n}"));
    t.words.extend(w);
    t.turns.push(turn);
    t.teacher(180.0, 300.0).student(300.0, 305.0);
    let p = plan(
        &t.words,
        &t.turns,
        305_000,
        &[],
        &SegmentSettings::default(),
    )
    .unwrap();
    let ambiguous = p.ambiguous().to_vec();
    assert!(!ambiguous.is_empty());
    for w in &ambiguous {
        assert!(
            !w.text.contains('s') || w.text.contains("slow"),
            "{}",
            w.text
        );
    }
    let missing = p.clone().finish(&BTreeMap::new()).unwrap_err();
    assert_eq!(missing.0, ambiguous[0].index);

    let labels: BTreeMap<usize, Label> = ambiguous
        .iter()
        .map(|w| (w.index, Label::Announcement))
        .collect();
    let first = p.clone().finish(&labels).unwrap();
    assert!(
        first
            .spans
            .iter()
            .any(|sp| sp.kind == SpanKind::Announcement)
    );
    assert!(first.segments.iter().all(|g| !g.text.contains("slow")));
    assert_eq!(first, p.finish(&labels).unwrap(), "deterministic");
}

#[test]
fn no_speech_and_unaligned_words() {
    assert_eq!(
        plan(&[], &[], 60_000, &[], &SegmentSettings::default()).unwrap_err(),
        Review::NoSpeech
    );
    let mut t = Timeline::new();
    t.teacher(0.0, 60.0);
    t.words.push(Word::new("stray", 70_000, 70_300, 0.5));
    let s = t.run(71).unwrap();
    assert_eq!(s.unaligned_words, 1);
    assert!(s.words.iter().all(|w| w.text != "stray"));
}
