//! The ingest configuration files: `config/sources.toml`, `config/pipeline.toml` (its
//! `[ingest]` table) and `config/series.toml` (Spec 03).

use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::source::SourcePlatform;

/// Configuration errors. Each names the file and what is wrong, and never a secret.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// Why.
        source: std::io::Error,
    },
    /// The file is not valid TOML for its shape.
    #[error("{origin} is not valid: {message}")]
    Parse {
        /// The file or fixture name.
        origin: String,
        /// The parser's message.
        message: String,
    },
    /// A pattern does not compile.
    #[error("{origin}: pattern {pattern:?} is not a valid regular expression: {message}")]
    Pattern {
        /// The file or fixture name.
        origin: String,
        /// The pattern.
        pattern: String,
        /// The compiler's message.
        message: String,
    },
    /// A series pattern lacks the named capture `part`.
    #[error("{origin}: series {series:?} needs a named capture `part` in its pattern")]
    SeriesWithoutPart {
        /// The file or fixture name.
        origin: String,
        /// The series.
        series: String,
    },
    /// Two sources share a name.
    #[error("{origin}: source {0:?} is listed twice", origin = .1)]
    DuplicateSource(String, String),
    /// No source has that name.
    #[error("no source named {0:?} in config/sources.toml")]
    UnknownSource(String),
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str, origin: &str) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse {
        origin: origin.to_owned(),
        message: e.to_string(),
    })
}

/// Patterns are matched case-insensitively against the normalized title.
fn compile(pattern: &str, origin: &str) -> Result<Regex, ConfigError> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| ConfigError::Pattern {
            origin: origin.to_owned(),
            pattern: pattern.to_owned(),
            message: e.to_string(),
        })
}

/// One configured source.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SourceConfig {
    /// Unique name, such as `ssz-vimeo`.
    pub name: String,
    /// Which adapter reads it.
    pub platform: SourcePlatform,
    /// The sangha where the source's talks were given, which receives their dana.
    pub default_sangha: String,
    /// Vimeo without a token: the public page whose embedded videos are the talk list.
    #[serde(default)]
    pub talks_page: Option<String>,
    /// How timestamped links render: `vimeo`, `youtube` or `none`.
    #[serde(default)]
    pub link_style: Option<String>,
    /// `audio_only` sources attach audio to existing talks and never create one.
    #[serde(default)]
    pub role: Option<String>,
    /// A podcast feed's address, set by `wt source resolve-feed` once a person confirms it.
    #[serde(default)]
    pub feed_url: Option<String>,
}

impl SourceConfig {
    /// Whether this source only attaches audio to talks another source lists.
    pub fn is_audio_only(&self) -> bool {
        self.role.as_deref() == Some("audio_only")
    }
}

/// `config/sources.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SourcesFile {
    /// Every source.
    #[serde(rename = "source", default)]
    pub sources: Vec<SourceConfig>,
}

impl SourcesFile {
    /// Loads and checks the file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::parse(&read(path)?, &path.display().to_string())
    }

    /// Parses and checks text.
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        let file: SourcesFile = parse(text, origin)?;
        let mut seen = std::collections::BTreeSet::new();
        for s in &file.sources {
            if !seen.insert(s.name.as_str()) {
                return Err(ConfigError::DuplicateSource(
                    s.name.clone(),
                    origin.to_owned(),
                ));
            }
        }
        Ok(file)
    }

    /// The source with this name.
    pub fn get(&self, name: &str) -> Result<&SourceConfig, ConfigError> {
        self.sources
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| ConfigError::UnknownSource(name.to_owned()))
    }
}

/// A title pattern that holds a video for review, with the reason it records.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReviewPatternEntry {
    /// The regular expression.
    pub pattern: String,
    /// The `state_reason`, such as `untitled` or `student_voices`.
    pub reason: String,
}

/// A title pattern for a talk type.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TypePatternEntry {
    /// The regular expression.
    pub pattern: String,
    /// `class`, `retreat_talk` or `service`.
    pub talk_type: String,
}

/// The `[ingest]` table of `config/pipeline.toml`, with Spec 03's defaults.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IngestSettings {
    /// Shorter videos are excluded with reason `too_short`.
    pub min_talk_seconds: i32,
    /// Minimum `pg_trgm` similarity of normalized titles for a duplicate.
    pub dedupe_title_similarity: f32,
    /// Duration tolerance for a duplicate, as a percentage of the longer video.
    pub dedupe_duration_tolerance_pct: f32,
    /// The duration tolerance never falls below this many seconds.
    pub dedupe_duration_tolerance_min_s: i32,
    /// Identical normalized titles match within this many seconds of duration.
    pub dedupe_title_identical_tolerance_s: i32,
    /// An unmatched video at least this long is a `dharma_talk`.
    pub long_talk_seconds: i32,
    /// Matching titles are `non_talk`, excluded with reason `non_talk`.
    pub exclude_title_patterns: Vec<String>,
    /// Matching titles are excluded by decision, with reason `excluded_session`.
    pub exclude_session_patterns: Vec<String>,
    /// Matching titles go to `needs_review` with the entry's reason.
    pub review_title_patterns: Vec<ReviewPatternEntry>,
    /// Title patterns for `class`, `retreat_talk` and `service`.
    pub type_patterns: Vec<TypePatternEntry>,
    /// The job lock timeout used by the reaper, in seconds.
    pub job_lock_timeout_s: i32,
    /// A feed episode and a Vimeo talk match only if their dates are this many days apart or
    /// fewer. Episodes are often published days or weeks after the talk.
    pub feed_match_days: i64,
    /// Their lengths may differ by the larger of this many seconds and the percentage below,
    /// since podcast audio is often trimmed.
    pub feed_match_duration_tolerance_s: i32,
    /// See `feed_match_duration_tolerance_s`.
    pub feed_match_duration_tolerance_pct: f32,
    /// Minimum `pg_trgm` similarity of normalized titles for a title match.
    pub feed_match_title_similarity: f32,
}

impl Default for IngestSettings {
    fn default() -> Self {
        Self {
            min_talk_seconds: 300,
            dedupe_title_similarity: 0.6,
            dedupe_duration_tolerance_pct: 2.0,
            dedupe_duration_tolerance_min_s: 30,
            dedupe_title_identical_tolerance_s: 120,
            long_talk_seconds: 900,
            exclude_title_patterns: Vec::new(),
            exclude_session_patterns: Vec::new(),
            review_title_patterns: Vec::new(),
            type_patterns: Vec::new(),
            job_lock_timeout_s: 7200,
            feed_match_days: 14,
            feed_match_duration_tolerance_s: 120,
            feed_match_duration_tolerance_pct: 5.0,
            feed_match_title_similarity: 0.5,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PipelineFile {
    #[serde(default)]
    ingest: IngestSettings,
}

/// One series from `config/series.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SeriesEntry {
    /// The name stored in `talk.series`.
    pub name: String,
    /// A regular expression with a named capture `part`.
    pub pattern: String,
    /// The number of parts, when known, so the report can list missing ones.
    #[serde(default)]
    pub parts: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct SeriesFile {
    #[serde(rename = "series", default)]
    series: Vec<SeriesEntry>,
}

/// A compiled series.
#[derive(Debug, Clone)]
pub struct Series {
    /// The name stored in `talk.series`.
    pub name: String,
    /// The number of parts, when known.
    pub parts: Option<i32>,
    pub(crate) pattern: Regex,
}

/// Everything classification reads, compiled once.
#[derive(Debug, Clone)]
pub struct ClassifyConfig {
    /// The raw settings.
    pub settings: IngestSettings,
    pub(crate) exclude_title: Vec<Regex>,
    pub(crate) exclude_session: Vec<Regex>,
    pub(crate) review: Vec<(Regex, String)>,
    pub(crate) types: Vec<(Regex, String)>,
    /// Known series.
    pub series: Vec<Series>,
}

impl ClassifyConfig {
    /// Loads `config/pipeline.toml` and `config/series.toml`.
    pub fn load(pipeline: &Path, series: &Path) -> Result<Self, ConfigError> {
        Self::parse(
            &read(pipeline)?,
            &pipeline.display().to_string(),
            &read(series)?,
            &series.display().to_string(),
        )
    }

    /// Parses and compiles both files.
    pub fn parse(
        pipeline: &str,
        pipeline_origin: &str,
        series: &str,
        series_origin: &str,
    ) -> Result<Self, ConfigError> {
        let settings = parse::<PipelineFile>(pipeline, pipeline_origin)?.ingest;
        let all = |patterns: &[String]| -> Result<Vec<Regex>, ConfigError> {
            patterns
                .iter()
                .map(|p| compile(p, pipeline_origin))
                .collect()
        };
        let exclude_title = all(&settings.exclude_title_patterns)?;
        let exclude_session = all(&settings.exclude_session_patterns)?;
        let review = settings
            .review_title_patterns
            .iter()
            .map(|e| Ok((compile(&e.pattern, pipeline_origin)?, e.reason.clone())))
            .collect::<Result<_, ConfigError>>()?;
        let types = settings
            .type_patterns
            .iter()
            .map(|e| Ok((compile(&e.pattern, pipeline_origin)?, e.talk_type.clone())))
            .collect::<Result<_, ConfigError>>()?;
        let series = parse::<SeriesFile>(series, series_origin)?
            .series
            .into_iter()
            .map(|e| {
                let pattern = compile(&e.pattern, series_origin)?;
                if !pattern.capture_names().flatten().any(|n| n == "part") {
                    return Err(ConfigError::SeriesWithoutPart {
                        origin: series_origin.to_owned(),
                        series: e.name,
                    });
                }
                Ok(Series {
                    name: e.name,
                    parts: e.parts,
                    pattern,
                })
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self {
            settings,
            exclude_title,
            exclude_session,
            review,
            types,
            series,
        })
    }
}
