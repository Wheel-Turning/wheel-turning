//! Spec 07 repository guards that scan the committed tree. CLAUDE.md rule 10: real audio is never
//! committed, and fixtures that need audio generate it at test time.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

const AUDIO_AND_VIDEO: &[&str] = &[
    "aac", "aif", "aiff", "amr", "flac", "m4a", "m4b", "mka", "mkv", "mov", "mp3", "mp4", "mpeg",
    "mpg", "oga", "ogg", "opus", "wav", "weba", "webm", "wma",
];

fn tracked_files() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git must be on PATH for repository guard tests");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn no_audio_or_video_file_is_committed() {
    let found: Vec<_> = tracked_files()
        .into_iter()
        .filter(|path| {
            Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| AUDIO_AND_VIDEO.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .collect();
    assert!(
        found.is_empty(),
        "audio or video files are committed: {found:?}"
    );
}

/// Calls that put bytes on disk or name an output file.
const WRITES: &[&str] = &[
    "fs::write",
    "File::create",
    "OpenOptions",
    "fs::copy",
    "fs::rename",
    "create_new",
    "persist(",
    "\"-y\"",
];

/// Spec 04, Media handling, rule 6: audio is written only by `wt-core::scratch`. Any line of
/// Rust source that names an audio or video file extension within three lines of a write fails,
/// outside the scratch module and tests.
#[test]
fn audio_is_written_only_under_scratch() {
    let mut found = Vec::new();
    for path in tracked_files() {
        let is_source =
            path.starts_with("crates/") && path.contains("/src/") && path.ends_with(".rs");
        if !is_source || path == "crates/wt-core/src/scratch.rs" {
            continue;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join(&path)).unwrap();
        // Unit tests at the bottom of a file write fixtures; production code comes first.
        let production = text.split("#[cfg(test)]").next().unwrap_or_default();
        let lines: Vec<&str> = production.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let code = line
                .split("//")
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let names_audio = AUDIO_AND_VIDEO.iter().any(|ext| {
                code.contains(&format!(".{ext}\"")) || code.contains(&format!(".{ext}'"))
            });
            if !names_audio {
                continue;
            }
            let near = &lines[i.saturating_sub(3)..(i + 4).min(lines.len())];
            if near.iter().any(|l| WRITES.iter().any(|w| l.contains(w))) {
                found.push(format!("{path}:{}", i + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "audio written outside wt-core::scratch: {found:?}"
    );
}

/// Spec 04, Model license record: CI fails when `config/` names a model that
/// `docs/model-licenses.md` does not list with a revision. The models named are the priced
/// language models, the benchmark's model files and the default retrieval models.
#[test]
fn config_names_only_models_the_license_record_lists_with_a_revision() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let record = wt_core::licenses::LicenseRecord::load(&root.join("docs/model-licenses.md"))
        .expect("docs/model-licenses.md");
    let mut named: Vec<String> = Vec::new();
    let prices = std::fs::read_to_string(root.join("config/llm_prices.toml")).unwrap();
    named.extend(
        prices
            .lines()
            .filter_map(|l| l.trim().strip_prefix("id = "))
            .map(|v| v.trim_matches('"').to_owned()),
    );
    let bench = std::fs::read_to_string(root.join("config/asr_bench.toml")).unwrap();
    named.extend(
        bench
            .lines()
            .filter_map(|l| l.trim().strip_prefix("path = "))
            .filter_map(|v| v.trim_matches('"').rsplit('/').next().map(str::to_owned)),
    );
    let env = std::fs::read_to_string(root.join(".env.example")).unwrap();
    named.extend(env.lines().filter_map(|l| {
        ["WT_EMBED_MODEL=", "WT_RERANK_MODEL="]
            .iter()
            .find_map(|k| l.strip_prefix(k))
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    }));
    assert!(named.len() >= 6, "found {named:?}");
    let missing: Vec<String> = named
        .iter()
        .filter_map(|m| record.require(m).err().map(|e| e.to_string()))
        .collect();
    assert!(missing.is_empty(), "{missing:#?}");
}
