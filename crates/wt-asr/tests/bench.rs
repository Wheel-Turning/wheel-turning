//! Spec 04: `wt asr benchmark` on a fixture with known text, with a fake engine in place of
//! whisper-cli. Every word is invented; the "audio" is ten seconds of silence in scratch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use wt_asr::bench::{self, BenchEngine, BenchModel, EngineRun, Route, Setup};
use wt_asr::{AsrError, AsrHints};
use wt_core::ids::TalkId;
use wt_core::scratch::ScratchDir;
use wt_core::transcript::Word;

const REFERENCE: &str = "This morning Ada Fixture spoke about Dogen and the Genjokoan. \
    To study the self is to forget the self, as Dogen says, and Ada laughed.";

/// Returns a scripted text per model, and a speed per setup.
struct FakeEngine;

#[async_trait]
impl BenchEngine for FakeEngine {
    async fn run(
        &self,
        setup: &Setup,
        model: &BenchModel,
        _wav: &Path,
        _scratch: &ScratchDir,
        _hints: &AsrHints,
    ) -> Result<EngineRun, AsrError> {
        if setup.name == "broken" {
            return Err(AsrError::Failed("exit code -1".into()));
        }
        if setup.name == "garbage" {
            // Fastest of all, and useless.
            return Ok(EngineRun {
                words: vec![Word::new("[music]", 0, 1, 0.1); 3],
                wall: Duration::from_millis(500),
                peak_bytes: None,
            });
        }
        let text = match model.name.as_str() {
            "large-v3" => REFERENCE.to_owned(),
            // Faster, but mishears a term and a name.
            _ => REFERENCE
                .replace("Genjokoan", "gen jo cone")
                .replace("Ada laughed", "Ida laughed"),
        };
        let secs = match (setup.name.as_str(), model.name.as_str()) {
            ("gpu", "large-v3") => 3.0,
            ("gpu", _) => 1.0,
            (_, "large-v3") => 12.0,
            _ => 4.0,
        };
        Ok(EngineRun {
            words: text
                .split_whitespace()
                .map(|w| Word::new(w, 0, 1, 0.9))
                .collect(),
            wall: Duration::from_secs_f64(secs),
            peak_bytes: Some(4_300_000_000),
        })
    }
}

/// Ten seconds of 16 kHz mono silence.
fn silence(path: &Path) {
    let samples = 160_000u32;
    let data = samples * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    b.resize(b.len() + data as usize, 0);
    std::fs::write(path, b).unwrap();
}

fn setup(name: &str) -> Setup {
    Setup {
        name: name.into(),
        binary: PathBuf::from("fake"),
        env: Default::default(),
        use_gpu: true,
        args: Vec::new(),
        path_prepend: Vec::new(),
    }
}

#[tokio::test]
async fn the_benchmark_prints_three_error_rates_for_both_models_and_keeps_the_vocabulary() {
    let root = std::env::temp_dir().join(format!("wt-bench-{}", sqlx_free_id()));
    let scratch = ScratchDir::create(&root, TalkId::from(uuid_nil())).unwrap();
    let wav = scratch.file("clip.wav").unwrap();
    silence(&wav);
    let models = vec![
        BenchModel {
            name: "large-v3".into(),
            path: wav.clone(),
        },
        BenchModel {
            name: "distil-large-v3".into(),
            path: wav.clone(),
        },
    ];
    let (gpu, cpu, broken, garbage) = (
        setup("gpu"),
        setup("cpu"),
        setup("broken"),
        setup("garbage"),
    );
    let results = bench::run(
        &FakeEngine,
        &[&gpu, &cpu, &broken, &garbage],
        &models,
        &wav,
        &scratch,
        &AsrHints {
            glossary_prompt: String::new(),
            language: "en".into(),
        },
        Some(REFERENCE),
        &["Dogen".into(), "Genjokoan".into()],
        &["Ada Fixture".into(), "Ada".into()],
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 8);

    let large = &results[0];
    assert_eq!(
        (large.setup.as_str(), large.model.as_str()),
        ("gpu", "large-v3")
    );
    let r = large.rates.unwrap();
    assert_eq!(
        (r.overall.errors, r.glossary.errors, r.names.errors),
        (0, 0, 0)
    );
    assert_eq!((r.glossary.total, r.names.total), (3, 3));
    assert!((large.rtf.unwrap() - 10.0 / 3.0).abs() < 1e-6);

    let distil = &results[1];
    let d = distil.rates.unwrap();
    assert_eq!((d.glossary.errors, d.names.errors), (1, 1));
    assert!(d.overall.errors >= 2);
    assert!(
        results[4]
            .error
            .as_deref()
            .unwrap()
            .contains("exit code -1")
    );

    // The fixture's reference is a few dozen words, so each misheard word costs several points:
    // a loose limit keeps the distil run in and tests the vocabulary rule. The configured 5% is
    // tested on the run of October 6, 2026 in `wt_asr::bench`.
    let decision = bench::decide(
        &results,
        &["large-v3".into(), "distil-large-v3".into()],
        50.0,
    );
    let (_, best_model, best_rtf, best_route) = decision.best.clone().unwrap();
    assert_eq!(
        (best_model.as_str(), best_route),
        ("distil-large-v3", Route::ThisPc)
    );
    assert!((best_rtf - 10.0).abs() < 1e-6);
    assert_eq!(decision.model.as_ref().unwrap().0, "large-v3");
    let (setup_name, _, route) = decision.chosen.clone().unwrap();
    assert_eq!((setup_name.as_str(), route), ("gpu", Route::ThisPc));

    let table = bench::render(
        &results,
        &decision,
        &[("vulkan".into(), "not found".into())],
    );
    println!("{table}");
    assert!(table.contains("| gpu | large-v3 | 3.33 |"));
    assert!(table.contains("| Glossary terms wrong | Teacher names wrong |"));
    assert!(table.contains("33.33% (1/3)"));
    assert!(table.contains("failed: whisper-cli failed: exit code -1"));
    assert!(table.contains("| vulkan | | skipped: not found"));
    assert!(table.contains(
        "Left out: large-v3 on garbage, unusable output (word error rate 100.0% is over 50%)."
    ));
    assert_eq!(decision.unusable.len(), 2);
    assert!(table.contains(
        "Chosen setup: gpu, at 3.33 times real time, so run the whole corpus on this PC."
    ));
    assert!(table.contains(
        "rejected: distil-large-v3 got 1 glossary terms wrong on gpu where large-v3 got 0"
    ));
    scratch.finish().unwrap();
}

fn uuid_nil() -> uuid::Uuid {
    uuid::Uuid::nil()
}

fn sqlx_free_id() -> String {
    format!("{}", std::process::id())
}
