//! Teachers, aliases, sanghas, the glossary and consent records.
//!
//! This crate holds the parts that need no database: the registry file format read by
//! `dg registry import`, and speaker resolution, which matches teacher aliases in a talk's title
//! and description. `wt-db` stores what these produce.

pub mod glossary;

use std::collections::BTreeSet;
use std::path::Path;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::Deserialize;

/// Registry file errors.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The file could not be read.
    #[error("could not read {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        source: std::io::Error,
    },
    /// The file is not valid registry TOML.
    #[error("{path} is not a valid registry file: {source}")]
    Parse {
        /// The file.
        path: String,
        /// The cause.
        source: Box<toml::de::Error>,
    },
    /// The file parsed but breaks a rule.
    #[error("{0}")]
    Invalid(String),
    /// The alias matcher could not be built.
    #[error("could not build the alias matcher: {0}")]
    Matcher(String),
}

/// The registry file: `dg registry import --file config/registry.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryFile {
    /// Sanghas, by name.
    #[serde(default, rename = "sangha")]
    pub sanghas: Vec<SanghaEntry>,
    /// Teachers, by display name.
    #[serde(default, rename = "teacher")]
    pub teachers: Vec<TeacherEntry>,
}

/// One sangha.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SanghaEntry {
    /// Unique name, as shown to testers.
    pub name: String,
    /// Lineage tag. Defaults to `soto`.
    #[serde(default = "soto")]
    pub lineage: String,
    /// Wider affiliation, if any.
    pub affiliation: Option<String>,
    /// The sangha's own donation page.
    pub donation_url: Option<String>,
    /// Optional template with `{amount_cents}`.
    pub donation_url_template: Option<String>,
    /// False means attribution only, never a give button.
    #[serde(default = "yes")]
    pub listed: bool,
}

/// One teacher.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeacherEntry {
    /// Unique display name.
    pub display_name: String,
    /// The teacher's home sangha. Talks credit the sangha where they were given instead.
    pub sangha: String,
    /// Names the teacher appears under in titles and descriptions. The display name is always
    /// an alias too.
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn soto() -> String {
    "soto".to_owned()
}

fn yes() -> bool {
    true
}

impl RegistryFile {
    /// Reads and validates a registry file.
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let text = std::fs::read_to_string(path).map_err(|source| RegistryError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parses and validates registry TOML.
    pub fn parse(text: &str, origin: &str) -> Result<Self, RegistryError> {
        let file: RegistryFile = toml::from_str(text).map_err(|source| RegistryError::Parse {
            path: origin.to_owned(),
            source: Box::new(source),
        })?;
        file.validate()?;
        Ok(file)
    }

    fn validate(&self) -> Result<(), RegistryError> {
        let sanghas: BTreeSet<&str> = self.sanghas.iter().map(|s| s.name.as_str()).collect();
        if sanghas.len() != self.sanghas.len() {
            return Err(RegistryError::Invalid(
                "a sangha name appears twice".to_owned(),
            ));
        }
        let mut aliases = BTreeSet::new();
        for teacher in &self.teachers {
            if !sanghas.contains(teacher.sangha.as_str()) {
                return Err(RegistryError::Invalid(format!(
                    "teacher {} names sangha {}, which the file does not define",
                    teacher.display_name, teacher.sangha
                )));
            }
            for alias in teacher.all_aliases() {
                if !aliases.insert(alias.to_lowercase()) {
                    return Err(RegistryError::Invalid(format!(
                        "alias {alias:?} is used by more than one teacher"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl TeacherEntry {
    /// The display name and every alias, trimmed, without repeats.
    pub fn all_aliases(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        std::iter::once(&self.display_name)
            .chain(self.aliases.iter())
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty() && seen.insert(a.to_lowercase()))
            .collect()
    }
}

/// The outcome of speaker resolution for one talk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakerMatch<T> {
    /// Exactly one teacher matched. `alias` is the text that matched, kept as `speaker_guess`.
    Resolved {
        /// The teacher.
        teacher: T,
        /// The alias that matched first.
        alias: String,
    },
    /// Several teachers matched; a person must decide.
    Ambiguous {
        /// Every alias that matched.
        aliases: Vec<String>,
    },
    /// No alias matched.
    Unresolved,
}

/// Matches teacher aliases on word boundaries, case-insensitively (Spec 03, rule 6).
pub struct AliasMatcher<T> {
    automaton: AhoCorasick,
    owners: Vec<(T, String)>,
}

impl<T: Clone + Ord> AliasMatcher<T> {
    /// Builds a matcher from `(teacher, alias)` pairs.
    pub fn new(aliases: impl IntoIterator<Item = (T, String)>) -> Result<Self, RegistryError> {
        let owners: Vec<(T, String)> = aliases
            .into_iter()
            .filter(|(_, alias)| !alias.trim().is_empty())
            .map(|(t, alias)| (t, alias.trim().to_owned()))
            .collect();
        let automaton = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(owners.iter().map(|(_, alias)| alias.as_str()))
            .map_err(|e| RegistryError::Matcher(e.to_string()))?;
        Ok(Self { automaton, owners })
    }

    /// Resolves the speaker from the title, and from the description only when the title names
    /// nobody.
    pub fn resolve(&self, title: &str, description: Option<&str>) -> SpeakerMatch<T> {
        let in_title = self.matches(title);
        let found = if in_title.is_empty() {
            description.map(|d| self.matches(d)).unwrap_or_default()
        } else {
            in_title
        };
        let teachers: BTreeSet<&T> = found.iter().map(|(t, _)| t).collect();
        match teachers.len() {
            0 => SpeakerMatch::Unresolved,
            1 => {
                let (teacher, alias) = &found[0];
                SpeakerMatch::Resolved {
                    teacher: teacher.clone(),
                    alias: alias.clone(),
                }
            }
            _ => SpeakerMatch::Ambiguous {
                aliases: found.into_iter().map(|(_, alias)| alias).collect(),
            },
        }
    }

    fn matches(&self, text: &str) -> Vec<(T, String)> {
        self.automaton
            .find_iter(text)
            .filter(|m| on_word_boundary(text, m.start(), m.end()))
            .map(|m| {
                let (teacher, alias) = &self.owners[m.pattern().as_usize()];
                (teacher.clone(), alias.clone())
            })
            .collect()
    }
}

fn on_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [[sangha]]
        name = "Seattle Soto Zen"

        [[sangha]]
        name = "Red Cedar"

        [[teacher]]
        display_name = "Kanshin"
        sangha = "Seattle Soto Zen"

        [[teacher]]
        display_name = "Steve Given"
        sangha = "Seattle Soto Zen"
        aliases = ["Shindo Steve Given", "Daiho Shindo", "Jundo Steve Given"]

        [[teacher]]
        display_name = "Tim Burnett"
        sangha = "Red Cedar"
        aliases = ["Nomon", "Nomon Tim Burnett"]
    "#;

    fn matcher() -> AliasMatcher<String> {
        let file = RegistryFile::parse(SAMPLE, "sample").unwrap();
        AliasMatcher::new(file.teachers.iter().flat_map(|t| {
            t.all_aliases()
                .into_iter()
                .map(|a| (t.display_name.clone(), a))
        }))
        .unwrap()
    }

    fn resolved(m: SpeakerMatch<String>) -> Option<String> {
        match m {
            SpeakerMatch::Resolved { teacher, .. } => Some(teacher),
            _ => None,
        }
    }

    #[test]
    fn matches_each_alias_case_insensitively() {
        let m = matcher();
        for (title, who) in [
            ("Dharma Talk by Kanshin, 2023-04-02", "Kanshin"),
            ("KANSHIN on the precepts", "Kanshin"),
            ("Daiho Shindo: Genjokoan 3", "Steve Given"),
            ("Jundo Steve Given - Uji", "Steve Given"),
            ("Shindo Steve Given on zazen", "Steve Given"),
            ("Nomon: Song of the Grass Hut", "Tim Burnett"),
        ] {
            assert_eq!(
                resolved(m.resolve(title, None)).as_deref(),
                Some(who),
                "{title}"
            );
        }
    }

    #[test]
    fn bare_jundo_or_shindo_do_not_resolve() {
        let m = matcher();
        assert_eq!(
            m.resolve("Jundo on just sitting", None),
            SpeakerMatch::Unresolved
        );
        assert_eq!(m.resolve("Shindo talk", None), SpeakerMatch::Unresolved);
    }

    #[test]
    fn requires_word_boundaries() {
        let m = matcher();
        assert_eq!(
            m.resolve("Kanshinjo is not a person", None),
            SpeakerMatch::Unresolved
        );
        assert_eq!(m.resolve("xNomon", None), SpeakerMatch::Unresolved);
    }

    #[test]
    fn two_teachers_are_ambiguous() {
        let m = matcher();
        assert!(matches!(
            m.resolve("Kanshin and Nomon in dialogue", None),
            SpeakerMatch::Ambiguous { .. }
        ));
    }

    #[test]
    fn description_is_used_only_when_the_title_names_nobody() {
        let m = matcher();
        assert_eq!(
            resolved(m.resolve("Sunday talk", Some("Given by Kanshin at SSZ"))).as_deref(),
            Some("Kanshin")
        );
        assert_eq!(
            resolved(m.resolve("Kanshin: anger", Some("with Nomon attending"))).as_deref(),
            Some("Kanshin")
        );
        assert_eq!(m.resolve("Sunday talk", None), SpeakerMatch::Unresolved);
    }

    #[test]
    fn rejects_shared_aliases_and_unknown_sanghas() {
        let shared = r#"
            [[sangha]]
            name = "A"
            [[teacher]]
            display_name = "One"
            sangha = "A"
            aliases = ["X"]
            [[teacher]]
            display_name = "Two"
            sangha = "A"
            aliases = ["x"]
        "#;
        assert!(RegistryFile::parse(shared, "t").is_err());
        let missing = r#"
            [[teacher]]
            display_name = "One"
            sangha = "Nowhere"
        "#;
        assert!(RegistryFile::parse(missing, "t").is_err());
    }
}
