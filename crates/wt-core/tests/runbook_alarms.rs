//! Spec 11, PB4.4: the runbook has an entry for every CloudWatch alarm in Terraform, and names no
//! alarm Terraform lacks. Alarm names are read from `alarm_name = "..."` in every `.tf` file under
//! `infra/terraform`, with the environment's name prefix filled in as `wt-v1`; runbook entries
//! are the level-four headings of `docs/runbook.md` that hold only an alarm name in backticks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The repository root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.tf` file under `dir`, skipping Terraform's own `.terraform` caches.
fn tf_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != ".terraform") {
                tf_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "tf") {
            out.push(path);
        }
    }
}

/// The alarm names Terraform creates in the v1 environment.
fn terraform_alarms() -> BTreeSet<String> {
    let mut files = Vec::new();
    tf_files(&root().join("infra/terraform"), &mut files);
    let mut names = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("alarm_name") else {
                continue;
            };
            let Some(value) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            let name = value
                .trim()
                .trim_matches('"')
                .replace("${var.name}", "wt-v1")
                .replace("${local.name}", "wt-v1");
            assert!(
                !name.contains("${"),
                "{}: alarm name {name} has an interpolation this test cannot expand; \
                 write one alarm_name per alarm",
                file.display()
            );
            names.insert(name);
        }
    }
    names
}

/// The alarms the runbook has an entry for.
fn runbook_alarms() -> BTreeSet<String> {
    let text = std::fs::read_to_string(root().join("docs/runbook.md")).unwrap();
    text.lines()
        .filter_map(|l| l.strip_prefix("#### `"))
        .filter_map(|l| l.strip_suffix('`'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_runbook_names_every_alarm_in_terraform_and_no_other() {
    let terraform = terraform_alarms();
    let runbook = runbook_alarms();
    assert!(
        terraform.len() >= 9,
        "found only {terraform:?} in Terraform"
    );
    let missing: Vec<_> = terraform.difference(&runbook).collect();
    let extra: Vec<_> = runbook.difference(&terraform).collect();
    assert!(
        missing.is_empty(),
        "alarms without a runbook entry (docs/runbook.md, procedure 13): {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "runbook entries for alarms Terraform does not create: {extra:?}"
    );
}
