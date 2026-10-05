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
