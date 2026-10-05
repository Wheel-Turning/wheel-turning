//! Spec 03 M2 acceptance: the alias seed in `config/registry.toml` loads the PRD's name variants,
//! and titles that name two teachers, or a bare shared name, do not resolve.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use wt_registry::{AliasMatcher, RegistryFile, SpeakerMatch};

fn seed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/registry.toml")
}

fn matcher() -> AliasMatcher<String> {
    let file = RegistryFile::load(&seed_path()).unwrap();
    AliasMatcher::new(file.teachers.iter().flat_map(|t| {
        t.all_aliases()
            .into_iter()
            .map(|a| (t.display_name.clone(), a))
    }))
    .unwrap()
}

fn who(title: &str) -> SpeakerMatch<String> {
    matcher().resolve(title, None)
}

#[test]
fn the_seed_loads_and_holds_no_email_address() {
    let file = RegistryFile::load(&seed_path()).unwrap();
    assert_eq!(file.sanghas.len(), 5);
    assert_eq!(file.teachers.len(), 11);
    let text = std::fs::read_to_string(seed_path()).unwrap();
    assert!(
        !text.contains('@'),
        "Spec 02: no email address in a registry seed"
    );
}

#[test]
fn each_name_variant_resolves_to_its_teacher() {
    for (title, teacher) in [
        ("Kanshin: anger and the precepts", "Kanshin Allison Tait"),
        (
            "Dharma talk by Kanshin Allison Tait",
            "Kanshin Allison Tait",
        ),
        ("Allison Tait on zazen", "Kanshin Allison Tait"),
        ("Daiho Shindo: Genjokoan 3", "Steve Given"),
        ("Daiho Shindo Steve Given - Uji", "Steve Given"),
        ("Shindo Steve Given on zazen", "Steve Given"),
        ("Jundo Steve Given - Uji", "Steve Given"),
        ("Daiho Chris Middleman on Dogen", "Chris Middleman"),
        ("Genzen Ed Cadman: the precepts", "Ed Cadman"),
        ("Mike Gillespie on practice", "Mike Gillespie"),
        ("Eko Jeff Kelley on zazen", "Jeff Kelley"),
        ("Kanho Chris Burkhart at Red Cedar", "Chris Burkhart"),
        ("Nomon: Song of the Grass Hut", "Tim Burnett"),
        ("Nomon Tim Burnett on Uji", "Tim Burnett"),
        ("Chikyo Ryunin Ewan Magie on zazen", "Ewan Magie"),
        ("Myoshin Kate McCandless on the precepts", "Kate McCandless"),
        ("Zoketsu Norman Fischer: Genjokoan", "Norman Fischer"),
    ] {
        match who(title) {
            SpeakerMatch::Resolved { teacher: found, .. } => assert_eq!(found, teacher, "{title}"),
            other => panic!("{title} should resolve to {teacher}, got {other:?}"),
        }
    }
}

#[test]
fn a_bare_shared_or_common_name_does_not_resolve() {
    // Daiho is both Steve Given's and Chris Middleman's name, and Jundo and Shindo alone are too
    // common to credit anyone.
    for title in [
        "Daiho on zazen",
        "Jundo on just sitting",
        "Shindo talk",
        "Tait on Uji",
    ] {
        assert_eq!(who(title), SpeakerMatch::Unresolved, "{title}");
    }
}

#[test]
fn two_teachers_in_one_title_are_ambiguous() {
    for title in [
        "Daiho Shindo and Daiho Chris Middleman in dialogue",
        "Kanho Chris Burkhart and Nomon Tim Burnett at Red Cedar",
        "Kanshin and Mike Gillespie: a conversation",
    ] {
        assert!(
            matches!(who(title), SpeakerMatch::Ambiguous { .. }),
            "{title}"
        );
    }
}
