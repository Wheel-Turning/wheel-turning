//! The committed `config/` files load, and the five named series record part numbers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use wt_ingest::SourcePlatform;
use wt_ingest::classify::{self, normalize_title};
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_registry::RegistryFile;

fn config(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../config")
        .join(name)
}

fn cfg() -> ClassifyConfig {
    ClassifyConfig::load(&config("pipeline.toml"), &config("series.toml")).unwrap()
}

#[test]
fn the_five_named_series_record_their_parts() {
    let c = cfg();
    for (title, series, part) in [
        ("Genjokoan, Part 3", "Genjokoan", Some(3)),
        ("Genjōkōan pt. 12", "Genjokoan", Some(12)),
        ("Uji (Being-Time) #4", "Uji", Some(4)),
        ("Bendowa – Talk 2", "Bendowa", Some(2)),
        ("Blue Cliff Record, Case 46", "Blue Cliff Record", Some(46)),
        ("Song of the Grass Hut 1", "Song of the Grass Hut", Some(1)),
        (
            "Introducing the Song of the Grass Hut",
            "Song of the Grass Hut",
            None,
        ),
    ] {
        assert_eq!(
            classify::series(&c, &normalize_title(title)),
            Some((series.to_owned(), part)),
            "{title}"
        );
    }
    assert_eq!(
        classify::series(&c, &normalize_title("Facing the Jewel Mirror")),
        None
    );
}

#[test]
fn the_ssz_vimeo_source_credits_a_sangha_in_the_registry() {
    let sources = SourcesFile::load(&config("sources.toml")).unwrap();
    let vimeo = sources.get("ssz-vimeo").unwrap();
    assert_eq!(vimeo.platform, SourcePlatform::Vimeo);
    assert!(vimeo.talks_page.is_some());
    assert_eq!(
        sources.get("ssz-podcast").unwrap().role.as_deref(),
        Some("audio_only")
    );

    let registry = RegistryFile::load(&config("registry.toml")).unwrap();
    for s in &sources.sources {
        assert!(
            registry.sanghas.iter().any(|r| r.name == s.default_sangha),
            "{} credits {:?}, which the registry lacks",
            s.name,
            s.default_sangha
        );
    }
}

#[test]
fn the_committed_patterns_classify_the_ssz_inventory_cases() {
    let c = cfg();
    let excluded =
        |t: &str, d| classify::exclusion(&c, &normalize_title(t), Some(d)).map(|e| e.reason());
    assert_eq!(excluded("Buddhist Quote", 11), Some("non_talk"));
    assert_eq!(excluded("Zen Protocols", 120), Some("too_short"));
    assert_eq!(
        excluded("Antiracist 12 Steps", 5400),
        Some("excluded_session")
    );
    assert_eq!(excluded("Uji Part 4", 2400), None);

    let review = |t: &str| classify::talk_type(&c, &normalize_title(t), Some(2400)).review;
    assert_eq!(review("zoom_0").as_deref(), Some("untitled"));
    assert_eq!(
        review("Q&A with Kanshin").as_deref(),
        Some("student_voices")
    );
    assert_eq!(review("Facing the Jewel Mirror"), None);
}
