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
    // Shaped like the archive's titles, with invented subjects and dates.
    for (title, series, part) in [
        ("Genjokoan - Paragraph 3 (4-25-2021)", "Genjokoan", Some(3)),
        ("Genjokoan, Part 7", "Genjokoan", Some(7)),
        ("Genjōkōan pt. 12", "Genjokoan", Some(12)),
        ("Genjokoan Introduction (4/9/2021)", "Genjokoan", None),
        (
            "2021-11-03_Fixture_Sunday_Shobogenzo-Uji-Being-Time_Part-3.mp4",
            "Uji",
            Some(3),
        ),
        (
            "2021-10-24 Fixture - Sunday - Shobogenzo Uji Being Time - Part2.mp4",
            "Uji",
            Some(2),
        ),
        ("Uji on January 5, 2025", "Uji", None),
        (
            "Sunday Dharma Talk: Fixture - Bendowa Series",
            "Bendowa",
            None,
        ),
        (
            "Dharma Talk on Blue Cliff Record #11: \"A Made-Up Case\" on May 1, 2023",
            "Blue Cliff Record",
            Some(11),
        ),
        (
            "Dharma Talk on Blue Cliff Record Case #12: \"Another\", on June 4, 2023",
            "Blue Cliff Record",
            Some(12),
        ),
        (
            "Dharma Talk on Verse #2 from The Song of the Grass Hut, on October 13, 2024",
            "Song of the Grass Hut",
            Some(2),
        ),
        (
            "Dharma Talk on the Final Verse from The Song of the Grass Hut",
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

    // A title that says it is a dharma talk is one even under long_talk_seconds.
    let short_talk = classify::talk_type(
        &c,
        &normalize_title("Dharma Talk on a Fixture Case"),
        Some(860),
    );
    assert_eq!(
        (short_talk.talk_type.as_str(), short_talk.review),
        ("dharma_talk", None)
    );
    let short_other = classify::talk_type(&c, &normalize_title("A brief word"), Some(860));
    assert_eq!(short_other.review.as_deref(), Some("talk_type_unclear"));
}
