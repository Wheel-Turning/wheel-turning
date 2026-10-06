//! A smoke test of the real tools on this machine: whisper-cli and the diarization sidecar on a
//! WAV you name, then the segmenter. It is ignored in CI. Run it with
//! `cargo test -p wt-ingest --test real_tools -- --ignored` and set WT_WHISPER_CLI,
//! WT_WHISPER_MODEL, WT_DIARIZE_URL (a running sidecar) and WT_SMOKE_WAV, a 16 kHz mono WAV that
//! is public domain or yours. It prints counts and timings only, never transcript text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use wt_asr::{AsrHints, AsrSettings, Transcriber, WhisperCli};
use wt_core::ids::TalkId;
use wt_core::scratch::{ScratchDir, WAV_NAME};
use wt_segment::{Diarizer, Label, SegmentSettings, SidecarDiarizer};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this test"))
}

#[tokio::test]
#[ignore = "needs whisper-cli, a model, a running sidecar and a WAV"]
async fn whisper_and_the_sidecar_run_on_a_real_wav() {
    let root = std::env::temp_dir().join("wt-real-tools");
    let scratch = ScratchDir::create(&root, TalkId::from(sqlx::types::Uuid::new_v4())).unwrap();
    let wav = scratch.file(WAV_NAME).unwrap();
    std::fs::copy(var("WT_SMOKE_WAV"), &wav).unwrap();

    let whisper = WhisperCli::new(
        PathBuf::from(var("WT_WHISPER_CLI")),
        PathBuf::from(var("WT_WHISPER_MODEL")),
        &AsrSettings::default(),
    );
    let hints = AsrHints {
        glossary_prompt: "Dogen, shikantaza, Genjokoan".into(),
        language: "en".into(),
    };
    let t = Instant::now();
    let raw = whisper.transcribe(&scratch, &wav, &hints).await.unwrap();
    println!(
        "whisper: {} words over {} ms in {:?}, model {}",
        raw.words.len(),
        raw.audio_ms,
        t.elapsed(),
        raw.model
    );
    assert!(!raw.words.is_empty());
    assert!(raw.words.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));

    let t = Instant::now();
    let turns = SidecarDiarizer::new(&var("WT_DIARIZE_URL"))
        .unwrap()
        .diarize(&wav)
        .await
        .unwrap();
    println!("sidecar: {} turns in {:?}", turns.len(), t.elapsed());
    assert!(!turns.is_empty());

    let plan = wt_segment::plan(
        &raw.words,
        &turns,
        i32::try_from(raw.audio_ms).unwrap(),
        &SegmentSettings::default().chant_phrases,
        &SegmentSettings::default(),
    );
    match plan {
        Ok(plan) => {
            let labels: BTreeMap<usize, Label> = plan
                .ambiguous()
                .iter()
                .map(|w| (w.index, Label::Talk))
                .collect();
            let seg = plan.finish(&labels).unwrap();
            println!(
                "segmenter: teacher {}, {} words kept, {} segments, {} student turns",
                seg.teacher,
                seg.words.len(),
                seg.segments.len(),
                seg.student_turns.len()
            );
        }
        Err(review) => println!("segmenter: held for review, {}", review.reason()),
    }
    scratch.finish().unwrap();
}
