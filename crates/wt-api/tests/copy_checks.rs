//! Spec 11 checks on public copy that need no database: no public promise of a time to act
//! (PB1.7), and one privacy line that the footer, the home page and the invitation all read
//! (PB3.1).
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::path::{Path, PathBuf};

use regex::Regex;
use wt_api::chrome::{invitation, privacy_line};
use wt_api::privacy::Settings;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir` with one of `extensions`, recursively.
fn files(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, extensions, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            out.push(path);
        }
    }
}

/// "business day", or "within" followed by a number and a unit of time.
fn time_promise() -> Regex {
    Regex::new(
        r"(?i)business\s+days?|\bwithin\s+(?:\d+|an?|one|two|three|four|five|six|seven|eight|nine|ten|twelve|fourteen|twenty-four|thirty)\s+(?:business\s+|working\s+|calendar\s+)?(?:seconds?|minutes?|hours?|days?|weeks?|months?|years?)\b",
    )
    .unwrap()
}

#[test]
fn the_time_promise_pattern_catches_what_it_must() {
    let re = time_promise();
    for promise in [
        "Within one business day your talks stop appearing.",
        "We act within 24 hours.",
        "It is removed within a week.",
        "removed in two business days",
    ] {
        assert!(re.is_match(promise), "{promise}");
    }
    for fine in [
        "Requests are handled in the order they arrive.",
        "It works once, for 24 hours.",
        "within reason",
    ] {
        assert!(!re.is_match(fine), "{fine}");
    }
}

/// PB1.7: no public template, page copy, static page or letter states a time to act.
#[test]
fn no_public_copy_promises_a_time_to_act() {
    let root = root();
    let mut checked = Vec::new();
    files(
        &root.join("crates/wt-api/templates"),
        &["html"],
        &mut checked,
    );
    files(&root.join("copy"), &["md"], &mut checked);
    files(&root.join("site"), &["html"], &mut checked);
    files(&root.join("templates"), &["md"], &mut checked);
    assert!(checked.len() > 30, "found only {} files", checked.len());

    let re = time_promise();
    let mut found = Vec::new();
    for path in &checked {
        let text = std::fs::read_to_string(path).unwrap();
        for m in re.find_iter(&text) {
            found.push(format!("{}: {}", path.display(), m.as_str()));
        }
    }
    let mail = wt_api::rights::confirmation("a@example.invalid", "https://x.invalid", "t");
    for m in re.find_iter(&mail.text) {
        found.push(format!("confirmation email: {}", m.as_str()));
    }
    assert!(
        found.is_empty(),
        "public time promises:\n{}",
        found.join("\n")
    );
}

/// PB3.1: one source, read by the footer and home page (rendered in `pages.rs`), the invitation
/// and Spec 09; it states the settings the code enforces.
#[test]
fn the_privacy_line_has_one_source_and_states_the_enforced_settings() {
    let root = root();
    let source = std::fs::read_to_string(root.join("copy/privacy_line.md")).unwrap();
    let line = privacy_line();
    assert_eq!(line, source.trim());
    assert!(!line.contains('\n'), "one paragraph");
    assert!(
        !line.contains(['<', '>', '&', '"', '\'']),
        "nothing the template would escape, so pages show it verbatim"
    );

    let templates = root.join("crates/wt-api/templates");
    for page in ["home.html", "base.html"] {
        let html = std::fs::read_to_string(templates.join(page)).unwrap();
        assert!(html.contains("{{ chrome.logging_line() }}"), "{page}");
        assert!(!html.contains("Only totals are read"), "{page}");
    }
    let link = "https://app.example.invalid/join/abc";
    let invite = invitation(link);
    assert!(invite.contains(link) && invite.ends_with(line), "{invite}");
    let spec = std::fs::read_to_string(root.join("docs/specs/09-copy-and-templates.md")).unwrap();
    assert!(spec.contains(line), "Spec 09 quotes the line as it is");

    // What it says matches what the code enforces.
    let enforced = Settings::default();
    assert_eq!(enforced.default_mode, wt_db::privacy::SHARE);
    assert!(line.starts_with("Unless you choose counts only, your questions are kept"));
    assert!(line.contains(&format!("kept for {} days", enforced.retention_days)));
    assert!(line.contains(&format!(
        "deleted after {} hours",
        enforced.count_only_ttl_hours
    )));
    for must in [
        "personal details",
        "removed",
        "the owner and approved team members",
        "a language model provider",
        "delete all your questions",
    ] {
        assert!(line.contains(must), "{must}");
    }
}
