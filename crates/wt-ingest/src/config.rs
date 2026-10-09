//! The ingest configuration files: `config/sources.toml`, `config/pipeline.toml` (its
//! `[ingest]` table) and `config/series.toml` (Spec 03).

use std::path::{Path, PathBuf};

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
    /// The talks source in the rights ledger (`wt source add --kind talks`) whose mandate the
    /// source's talks clear on when their speaker has not answered. None means no mandate
    /// (Spec 03, Mandated talks; decision of October 5, 2026).
    #[serde(default)]
    pub rights_source: Option<String>,
    /// An EngageWisdom collection's listing page, such as `https://sfzc.engagewisdom.com/sf`.
    #[serde(default)]
    pub collection_url: Option<String>,
    /// The most talks discovery reads, newest first: the sample's ten (Spec 03, EngageWisdom
    /// sources). Unset reads every talk.
    #[serde(default)]
    pub max_talks: Option<usize>,
    /// The type a talk takes when no title pattern or length decides, for a source that lists
    /// only talks, such as `dharma_talk`.
    #[serde(default)]
    pub default_talk_type: Option<String>,
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
    /// An episode whose exact title, less the speaker in brackets, belongs to one Vimeo talk
    /// matches it when the upload is within this many days of the episode. Re-uploads under a
    /// new title came weeks after the talk, so this is wider than `feed_match_days`.
    pub feed_exact_title_days: i64,
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
            feed_exact_title_days: 60,
        }
    }
}

/// `[rights]` in `config/pipeline.toml` (Spec 08, Rights lifecycle).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RightsSettings {
    /// How `wt source add` uses a new source when `--basis` is not given: `mandate` by
    /// default, or `license` or `permission` with evidence in hand.
    pub default_basis_for_new_sources: wt_db::rights::Basis,
}

impl Default for RightsSettings {
    fn default() -> Self {
        Self {
            default_basis_for_new_sources: wt_db::rights::Basis::Mandate,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RightsFile {
    #[serde(default)]
    rights: RightsSettings,
}

impl RightsSettings {
    /// Reads `[rights]` from `config/pipeline.toml`. A file without the section gets the
    /// defaults.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        Self::parse(&read(pipeline)?, &pipeline.display().to_string())
    }

    /// Parses `[rights]` from the text of `config/pipeline.toml`.
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        Ok(parse::<RightsFile>(text, origin)?.rights)
    }
}

/// `[licenses]` in `config/pipeline.toml` (Spec 03, Licensed sources; Spec 08): the license
/// codes evidence may carry.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LicenseSettings {
    /// Codes a person may approve and an import may rely on.
    pub allowed_license_codes: Vec<String>,
}

impl Default for LicenseSettings {
    fn default() -> Self {
        Self {
            allowed_license_codes: [
                "public-domain",
                "permission-excerpt",
                "operator-mandate",
                "cc0",
                "cc-by-4.0",
                "cc-by-3.0",
                "cc-by-sa-4.0",
                "cc-by-sa-3.0",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LicensesFile {
    #[serde(default)]
    licenses: LicenseSettings,
}

impl LicenseSettings {
    /// Reads `[licenses]` from `config/pipeline.toml`; without the section, the defaults.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        Ok(parse::<LicensesFile>(&read(pipeline)?, &pipeline.display().to_string())?.licenses)
    }
}

/// `[citations]` in `config/pipeline.toml` (Spec 08, Work-level citations): phrases too common
/// in talks to identify a work, which never match even as a title or alias.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CitationSettings {
    /// Compared after normalization.
    pub work_alias_stoplist: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CitationsFile {
    #[serde(default)]
    citations: CitationSettings,
}

impl CitationSettings {
    /// Reads `[citations]` from `config/pipeline.toml`; without the section, an empty stoplist.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        Ok(parse::<CitationsFile>(&read(pipeline)?, &pipeline.display().to_string())?.citations)
    }
}

/// `[texts]` in `config/pipeline.toml` (Spec 08, PDFs and OCR): the English word list the
/// suspect check reads, and the errors a proofread allows.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TextSettings {
    /// One word to a line, such as `/usr/share/dict/words` from `wamerican`.
    pub word_list: PathBuf,
    /// Errors allowed in the random part of a proofread sample.
    pub proof_random_max_errors: u32,
    /// Errors allowed in its lowest-confidence part.
    pub proof_worst_max_errors: u32,
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            word_list: PathBuf::from("/usr/share/dict/words"),
            proof_random_max_errors: 1,
            proof_worst_max_errors: 3,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TextsFile {
    #[serde(default)]
    texts: TextSettings,
}

impl TextSettings {
    /// Reads `[texts]` from `config/pipeline.toml`; without the section, the defaults.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        Ok(parse::<TextsFile>(&read(pipeline)?, &pipeline.display().to_string())?.texts)
    }
}

/// `[deployment]` in `config/pipeline.toml` (Spec 08, Mandated sources).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Deployment {
    /// `private`, the default, or `public`.
    pub exposure: String,
}

impl Default for Deployment {
    fn default() -> Self {
        Self {
            exposure: "private".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeploymentFile {
    #[serde(default)]
    deployment: Deployment,
}

impl Deployment {
    /// Reads `[deployment]` from `config/pipeline.toml`; without the section, private.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        let d =
            parse::<DeploymentFile>(&read(pipeline)?, &pipeline.display().to_string())?.deployment;
        if !matches!(d.exposure.as_str(), "private" | "public") {
            return Err(ConfigError::Parse {
                origin: pipeline.display().to_string(),
                message: format!(
                    "[deployment] exposure = {:?}; use private or public",
                    d.exposure
                ),
            });
        }
        Ok(d)
    }

    /// Whether the tool is public.
    pub fn public(&self) -> bool {
        self.exposure == "public"
    }
}

/// `[url_policy]` in `config/pipeline.toml` (Spec 08, Ingesting from URLs and Source policy):
/// politeness, the download cap, and aggregator hosts, which evidence may not cover host-wide.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct UrlPolicy {
    /// The least time between two requests to one host.
    pub fetch_delay_ms: u64,
    /// The largest download.
    pub max_download_mb: u64,
    /// Hosts that republish many rights holders' works.
    pub aggregator_hosts: Vec<String>,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self {
            fetch_delay_ms: 1000,
            max_download_mb: 200,
            aggregator_hosts: vec!["terebess.hu".into()],
        }
    }
}

#[derive(Debug, Deserialize)]
struct UrlPolicyFile {
    #[serde(default)]
    url_policy: UrlPolicy,
}

impl UrlPolicy {
    /// Reads `[url_policy]` from `config/pipeline.toml`; without the section, the defaults.
    pub fn load(pipeline: &Path) -> Result<Self, ConfigError> {
        Ok(parse::<UrlPolicyFile>(&read(pipeline)?, &pipeline.display().to_string())?.url_policy)
    }

    /// The fetcher's settings.
    pub fn web(&self) -> crate::texts::web::WebSettings {
        crate::texts::web::WebSettings {
            fetch_delay: std::time::Duration::from_millis(self.fetch_delay_ms),
            max_download_bytes: self.max_download_mb.saturating_mul(1024 * 1024),
            ..crate::texts::web::WebSettings::default()
        }
    }

    /// Refuses a prefix that covers an aggregator host as a whole: each of its documents needs
    /// its own evidence (Spec 08, Source policy, rule 1).
    pub fn check_prefix(&self, prefix: &str) -> Result<(), String> {
        let url = url::Url::parse(prefix).map_err(|e| format!("{prefix}: {e}"))?;
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let aggregator = self.aggregator_hosts.iter().any(|a| {
            let a = a.to_ascii_lowercase();
            host == a || host.ends_with(&format!(".{a}"))
        });
        let path = url.path().trim_matches('/');
        if aggregator && path.is_empty() {
            return Err(format!(
                "{host} is an aggregator: evidence names each document or a narrow path, never                  the whole host"
            ));
        }
        Ok(())
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
    /// The text locator a part refers to, with `{part}` for the number, such as
    /// `genjokoan:{part}`. Citation resolution uses it for generic citations in the series'
    /// talks (Spec 04, resolution rule 2).
    #[serde(default)]
    pub locator: Option<String>,
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
    /// The locator template for a part.
    pub locator: Option<String>,
    pub(crate) pattern: Regex,
}

impl Series {
    /// The locator a part of this series refers to, if the series maps parts to locators.
    pub fn locator_for(&self, part: i32) -> Option<String> {
        self.locator
            .as_deref()
            .map(|t| t.replace("{part}", &part.to_string()))
    }
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
                    locator: e.locator,
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
