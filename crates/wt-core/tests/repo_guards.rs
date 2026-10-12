//! Spec 07 repository guards that scan the committed tree. CLAUDE.md rule 10: real audio is never
//! committed, and fixtures that need audio generate it at test time.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

const AUDIO_AND_VIDEO: &[&str] = wt_core::scratch::AUDIO_AND_VIDEO_EXTENSIONS;

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

/// Where Rust source may read the `passage` table directly (CLAUDE.md and Spec 00, rule 3):
/// `wt-index` writes it, the takedown routine deletes from it, and `wt audit` looks in it for
/// passages that exist but must not be served, which the view by design cannot show (decision of
/// October 7, 2026). Everything else goes through `servable_passage`.
const PASSAGE_READERS: &[&str] = &[
    "crates/wt-index/",
    "crates/wt-db/src/takedown.rs",
    "crates/wt-db/src/audit.rs",
];

/// The SQL in `text` that names the `passage` table itself, not a view or table whose name only
/// ends in it, such as `servable_passage` or `answer_passage`.
fn direct_passage_sql(text: &str) -> Vec<String> {
    direct_sql(text, "passage")
}

/// The SQL in `text` that names `table` itself.
fn direct_sql(text: &str, table: &str) -> Vec<String> {
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut found = Vec::new();
    for keyword in ["from", "join", "update", "into", "table"] {
        let needle = format!("{keyword} {table}");
        let mut at = 0;
        while let Some(i) = flat[at..].find(&needle) {
            let start = at + i;
            let end = start + needle.len();
            let before = flat[..start].chars().next_back();
            let after = flat[end..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !word(before) && !word(after) {
                found.push(flat[start.saturating_sub(20)..(end + 30).min(flat.len())].to_owned());
            }
            at = end;
        }
    }
    found
}

#[test]
fn the_passage_guard_catches_direct_reads_and_spares_the_views() {
    assert_eq!(
        direct_passage_sql("select text from passage where id = $1").len(),
        1
    );
    assert_eq!(
        direct_passage_sql("SELECT p.id FROM\n    Passage p").len(),
        1
    );
    assert_eq!(
        direct_passage_sql("select * from t join passage p on true").len(),
        1
    );
    assert!(direct_passage_sql("select text from servable_passage").is_empty());
    assert!(direct_passage_sql("select * from answer_passage").is_empty());
    assert!(direct_passage_sql("select * from passages").is_empty());
    assert!(direct_passage_sql("a passage from passage_text").is_empty());
}

/// CLAUDE.md rule 3: serving code reads passages only through `servable_passage`. Library and
/// binary source outside [`PASSAGE_READERS`] may not name the `passage` table in SQL. Tests and
/// migrations are not serving code and are not scanned; a file's `#[cfg(test)]` module is
/// skipped too.
#[test]
fn only_the_named_modules_read_the_passage_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut offenders = Vec::new();
    for path in tracked_files() {
        let is_source =
            path.starts_with("crates/") && path.contains("/src/") && path.ends_with(".rs");
        if !is_source || PASSAGE_READERS.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&path)).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        for hit in direct_passage_sql(code) {
            offenders.push(format!("{path}: …{hit}…"));
        }
    }
    assert!(
        offenders.is_empty(),
        "direct reads of the passage table outside {PASSAGE_READERS:?}; use servable_passage:\n{}",
        offenders.join("\n")
    );
}

/// Where Rust source may read the `student_question` table directly (Spec 04, Student
/// questions, and rule 3): `wt-index` writes it, the takedown routine deletes from it, `wt audit`
/// checks it, and the owner's exports read a talk's questions as they read its stored
/// transcript. Serving reads `servable_student_question`.
const QUESTION_READERS: &[&str] = &[
    "crates/wt-index/",
    "crates/wt-db/src/takedown.rs",
    "crates/wt-db/src/audit.rs",
    "crates/wt-db/src/questions.rs",
];

#[test]
fn only_the_named_modules_read_the_student_question_table() {
    assert_eq!(
        direct_sql(
            "select text from student_question where",
            "student_question"
        )
        .len(),
        1
    );
    assert!(direct_sql("from servable_student_question q", "student_question").is_empty());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut offenders = Vec::new();
    for path in tracked_files() {
        let is_source =
            path.starts_with("crates/") && path.contains("/src/") && path.ends_with(".rs");
        if !is_source || QUESTION_READERS.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&path)).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        for hit in direct_sql(code, "student_question") {
            offenders.push(format!("{path}: …{hit}…"));
        }
    }
    assert!(
        offenders.is_empty(),
        "direct reads of student_question outside {QUESTION_READERS:?}; use \
         servable_student_question:\n{}",
        offenders.join("\n")
    );
}

/// Spec 08: nothing under `data-inbox/`, where source documents wait to be imported, is ever
/// committed.
#[test]
fn nothing_under_data_inbox_is_committed() {
    let tracked: Vec<String> = tracked_files()
        .into_iter()
        .filter(|path| path.starts_with("data-inbox/"))
        .collect();
    assert!(
        tracked.is_empty(),
        "files under data-inbox/ are committed: {tracked:?}"
    );
}
