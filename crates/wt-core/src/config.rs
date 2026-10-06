//! The configuration loader.
//!
//! Every key from the Spec 01 configuration reference is read here, validated once at startup,
//! and exposed as a typed [`Config`]. A missing required key fails fast. Secret values are held
//! in [`Secret`], which never prints its contents, and no error message ever echoes a secret.
//! An empty value counts as unset, so a copied `.env.example` behaves like a missing key.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Deployment environment, from `WT_ENV`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    /// Local development. Human-readable logs, relaxed requirements.
    Dev,
    /// Shared pre-production environment.
    Staging,
    /// Production. Debug and trace logging are refused.
    Prod,
}

impl Env {
    /// Whether this is the local development environment.
    pub fn is_dev(self) -> bool {
        self == Env::Dev
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "dev" => Some(Env::Dev),
            "staging" => Some(Env::Staging),
            "prod" => Some(Env::Prod),
            _ => None,
        }
    }
}

/// A secret value. `Debug` prints a placeholder and there is no `Display`, so the value can only
/// leave through an explicit call to [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value. Call this only where the value is handed to the system that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Why the configuration could not be loaded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required key is unset or empty.
    #[error("required configuration key {key} is not set")]
    Missing {
        /// The environment variable name.
        key: &'static str,
    },
    /// A key is set to a value that does not parse or is not allowed.
    #[error("configuration key {key} is invalid: {reason}")]
    Invalid {
        /// The environment variable name.
        key: &'static str,
        /// What is wrong. Never contains a secret value.
        reason: String,
    },
}

/// The validated runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// `WT_ENV`.
    pub env: Env,
    /// `DATABASE_URL`, the connection for the application role.
    pub database_url: Secret,
    /// `DATABASE_URL_ANALYST`, used only by `wt report`.
    pub database_url_analyst: Option<Secret>,
    /// `WT_BIND`, the web listen address.
    pub bind: SocketAddr,
    /// `WT_SESSION_SECRET`, the key that signs session cookies. Always set outside dev.
    pub session_secret: Option<Secret>,
    /// `WT_SCRATCH_DIR`, the only place audio or video may exist.
    pub scratch_dir: PathBuf,
    /// `WT_TRANSCRIPT_BUCKET`. `None` means a local directory under `./data`.
    pub transcript_bucket: Option<String>,
    /// `WT_MODEL_DIR`, holding the exported ONNX models and tokenizers.
    pub model_dir: PathBuf,
    /// `WT_EMBED_MODEL`, the embedding model directory name inside `model_dir`.
    pub embed_model: String,
    /// `WT_RERANK_MODEL`, the reranker model directory name inside `model_dir`.
    pub rerank_model: String,
    /// `WT_LLM_MODEL`. Required only by the LLM stages: see [`Config::require_llm_model`].
    pub llm_model: Option<String>,
    /// `ANTHROPIC_API_KEY`.
    pub anthropic_api_key: Option<Secret>,
    /// `WT_LLM_REAL_DATA_OK`. Must be true before any call carries real transcript text.
    pub llm_real_data_ok: bool,
    /// `WT_LLM_MODEL_EXTRACT`, `WT_LLM_MODEL_SYNTH`, `WT_LLM_MODEL_VERIFY` and
    /// `WT_LLM_MODEL_VISION`: per-stage models, each falling back to `WT_LLM_MODEL`.
    pub llm_stage_models: LlmStageModels,
    /// `WT_LLM_MONTHLY_CAP_USD`, default 25. `wt llm cap set` overrides it in the database.
    pub llm_monthly_cap_usd: f64,
    /// `WT_ALERT_TOPIC_ARN`. Empty turns alerts off.
    pub alert_topic_arn: Option<String>,
    /// `YOUTUBE_API_KEY`.
    pub youtube_api_key: Option<Secret>,
    /// `WT_VIMEO_ACCESS_TOKEN`. With it, Vimeo discovery uses the API; without it, the talks page.
    pub vimeo_access_token: Option<Secret>,
    /// `WT_ALLOW_THIRD_PARTY_FETCH`. Enables the yt-dlp fetch mode.
    pub allow_third_party_fetch: bool,
    /// `WT_DIARIZE_URL`, the base URL of the diarization sidecar.
    pub diarize_url: String,
    /// `WT_WHISPER_MODEL`, the path to the whisper large-v3 model file.
    pub whisper_model: Option<PathBuf>,
    /// `WT_WHISPER_CLI`, the whisper.cpp `whisper-cli` executable built for this machine.
    pub whisper_cli: Option<PathBuf>,
    /// `WT_SITE_DANA_URL`, the site's own donation page.
    pub site_dana_url: Option<String>,
    /// `WT_LOG`, the log filter.
    pub log: String,
}

impl Config {
    /// Loads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Loads the configuration from any key lookup. Tests use this instead of mutating the
    /// process environment.
    pub fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let get = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());

        let env = match get("WT_ENV") {
            None => Env::Dev,
            Some(value) => Env::parse(value.trim()).ok_or_else(|| ConfigError::Invalid {
                key: "WT_ENV",
                reason: format!("expected dev, staging or prod, got {value:?}"),
            })?,
        };

        let database_url = match get("DATABASE_URL") {
            Some(url) => Secret::new(url),
            None if get("WT_DB_HOST").is_some() => database_url_from_parts(&get, env)?,
            None => {
                return Err(ConfigError::Missing {
                    key: "DATABASE_URL",
                });
            }
        };
        check_database_url("DATABASE_URL", &database_url, env)?;

        let database_url_analyst = get("DATABASE_URL_ANALYST").map(Secret::new);
        if let Some(url) = &database_url_analyst {
            check_database_url("DATABASE_URL_ANALYST", url, env)?;
        }

        let bind_text = get("WT_BIND").unwrap_or_else(|| "127.0.0.1:8080".to_owned());
        let bind = bind_text.parse().map_err(|_| ConfigError::Invalid {
            key: "WT_BIND",
            reason: format!("expected host:port, got {bind_text:?}"),
        })?;

        let session_secret = get("WT_SESSION_SECRET").map(Secret::new);
        if session_secret.is_none() && !env.is_dev() {
            return Err(ConfigError::Missing {
                key: "WT_SESSION_SECRET",
            });
        }

        let log = get("WT_LOG").unwrap_or_else(|| "info".to_owned());
        if env == Env::Prod && enables_debug_logging(&log) {
            return Err(ConfigError::Invalid {
                key: "WT_LOG",
                reason: "debug and trace logging are not allowed in prod".to_owned(),
            });
        }

        Ok(Self {
            env,
            database_url,
            database_url_analyst,
            bind,
            session_secret,
            scratch_dir: get("WT_SCRATCH_DIR")
                .map_or_else(|| PathBuf::from("./data/scratch"), PathBuf::from),
            transcript_bucket: get("WT_TRANSCRIPT_BUCKET"),
            model_dir: get("WT_MODEL_DIR").map_or_else(|| PathBuf::from("./models"), PathBuf::from),
            embed_model: get("WT_EMBED_MODEL").unwrap_or_else(|| "bge-m3-int8".to_owned()),
            rerank_model: get("WT_RERANK_MODEL")
                .unwrap_or_else(|| "bge-reranker-v2-m3-int8".to_owned()),
            llm_model: get("WT_LLM_MODEL"),
            anthropic_api_key: get("ANTHROPIC_API_KEY").map(Secret::new),
            llm_real_data_ok: parse_bool("WT_LLM_REAL_DATA_OK", get("WT_LLM_REAL_DATA_OK"))?,
            llm_stage_models: LlmStageModels {
                extract: get("WT_LLM_MODEL_EXTRACT"),
                synth: get("WT_LLM_MODEL_SYNTH"),
                verify: get("WT_LLM_MODEL_VERIFY"),
                vision: get("WT_LLM_MODEL_VISION"),
            },
            llm_monthly_cap_usd: match get("WT_LLM_MONTHLY_CAP_USD") {
                None => 25.0,
                Some(v) => v
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|usd| usd.is_finite() && *usd >= 0.0)
                    .ok_or_else(|| ConfigError::Invalid {
                        key: "WT_LLM_MONTHLY_CAP_USD",
                        reason: format!("expected a dollar amount, got {v:?}"),
                    })?,
            },
            alert_topic_arn: get("WT_ALERT_TOPIC_ARN"),
            youtube_api_key: get("YOUTUBE_API_KEY").map(Secret::new),
            vimeo_access_token: get("WT_VIMEO_ACCESS_TOKEN").map(Secret::new),
            allow_third_party_fetch: parse_bool(
                "WT_ALLOW_THIRD_PARTY_FETCH",
                get("WT_ALLOW_THIRD_PARTY_FETCH"),
            )?,
            diarize_url: get("WT_DIARIZE_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8090".to_owned()),
            whisper_model: get("WT_WHISPER_MODEL").map(PathBuf::from),
            whisper_cli: get("WT_WHISPER_CLI").map(PathBuf::from),
            site_dana_url: get("WT_SITE_DANA_URL"),
            log,
        })
    }

    /// The model for a language-model stage: its own setting, or else `WT_LLM_MODEL`. Fails when
    /// neither is set, so a worker that runs the stage refuses to start (Spec 04).
    pub fn llm_model_for(&self, stage: LlmStage) -> Result<&str, ConfigError> {
        let own = match stage {
            LlmStage::Extract | LlmStage::Classify => &self.llm_stage_models.extract,
            LlmStage::Synth => &self.llm_stage_models.synth,
            LlmStage::Verify => &self.llm_stage_models.verify,
            LlmStage::Vision => &self.llm_stage_models.vision,
        };
        own.as_deref()
            .or(self.llm_model.as_deref())
            .ok_or(ConfigError::Missing { key: stage.key() })
    }

    /// The model identifier for the Claude API, which the LLM stages require.
    pub fn require_llm_model(&self) -> Result<&str, ConfigError> {
        self.llm_model.as_deref().ok_or(ConfigError::Missing {
            key: "WT_LLM_MODEL",
        })
    }
}

/// Builds `DATABASE_URL` from separate parts, for deployments where the password is injected on
/// its own from Secrets Manager and must never sit in a stored connection string:
/// `WT_DB_HOST`, `WT_DB_PORT` (default 5432), `WT_DB_NAME` (default `wt`), `WT_DB_USER` and
/// `WT_DB_PASSWORD`. Outside dev the URL requires TLS.
fn database_url_from_parts<G>(get: &G, env: Env) -> Result<Secret, ConfigError>
where
    G: Fn(&str) -> Option<String>,
{
    let host = get("WT_DB_HOST").ok_or(ConfigError::Missing { key: "WT_DB_HOST" })?;
    let port_text = get("WT_DB_PORT").unwrap_or_else(|| "5432".to_owned());
    let port: u16 = port_text.trim().parse().map_err(|_| ConfigError::Invalid {
        key: "WT_DB_PORT",
        reason: format!("expected a port number, got {port_text:?}"),
    })?;
    let name = get("WT_DB_NAME").unwrap_or_else(|| "wt".to_owned());
    let user = get("WT_DB_USER").ok_or(ConfigError::Missing { key: "WT_DB_USER" })?;
    let password = Secret::new(get("WT_DB_PASSWORD").ok_or(ConfigError::Missing {
        key: "WT_DB_PASSWORD",
    })?);
    let ssl = if env.is_dev() { "" } else { "?sslmode=require" };
    Ok(Secret::new(format!(
        "postgres://{}:{}@{}:{}/{}{}",
        percent_encode(&user),
        percent_encode(password.expose()),
        host.trim(),
        port,
        percent_encode(&name),
        ssl
    )))
}

/// Percent-encodes everything outside the URL unreserved set.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn check_database_url(key: &'static str, url: &Secret, env: Env) -> Result<(), ConfigError> {
    let value = url.expose();
    if !(value.starts_with("postgres://") || value.starts_with("postgresql://")) {
        return Err(ConfigError::Invalid {
            key,
            reason: "expected a postgres:// connection string".to_owned(),
        });
    }
    let requires_tls = [
        "sslmode=require",
        "sslmode=verify-ca",
        "sslmode=verify-full",
    ]
    .iter()
    .any(|mode| value.contains(mode));
    if !env.is_dev() && !requires_tls {
        return Err(ConfigError::Invalid {
            key,
            reason: "outside dev the connection string must set sslmode=require".to_owned(),
        });
    }
    Ok(())
}

/// Per-stage language models, each optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmStageModels {
    /// `WT_LLM_MODEL_EXTRACT`, also used to label ambiguous segment windows.
    pub extract: Option<String>,
    /// `WT_LLM_MODEL_SYNTH`.
    pub synth: Option<String>,
    /// `WT_LLM_MODEL_VERIFY`.
    pub verify: Option<String>,
    /// `WT_LLM_MODEL_VISION`.
    pub vision: Option<String>,
}

/// A stage that calls the language model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmStage {
    /// Citation extraction.
    Extract,
    /// Labeling ambiguous segment windows. Uses the extraction model (decision of October 5,
    /// 2026): both are ingestion calls Spec 04 gives a Haiku-class model.
    Classify,
    /// Answer synthesis.
    Synth,
    /// Theme verification.
    Verify,
    /// The OCR rescue pass.
    Vision,
}

impl LlmStage {
    /// The value stored in `llm_usage.stage`.
    pub fn as_str(self) -> &'static str {
        match self {
            LlmStage::Extract => "extract",
            LlmStage::Classify => "classify",
            LlmStage::Synth => "synth",
            LlmStage::Verify => "verify",
            LlmStage::Vision => "vision",
        }
    }

    /// The setting a missing model is reported under.
    pub fn key(self) -> &'static str {
        match self {
            LlmStage::Extract | LlmStage::Classify => "WT_LLM_MODEL_EXTRACT or WT_LLM_MODEL",
            LlmStage::Synth => "WT_LLM_MODEL_SYNTH or WT_LLM_MODEL",
            LlmStage::Verify => "WT_LLM_MODEL_VERIFY or WT_LLM_MODEL",
            LlmStage::Vision => "WT_LLM_MODEL_VISION or WT_LLM_MODEL",
        }
    }

    /// Whether a refusal at the cap puts the job off to next month (ingestion) rather than
    /// rendering without the call (synthesis and verification).
    pub fn is_ingestion(self) -> bool {
        matches!(
            self,
            LlmStage::Extract | LlmStage::Classify | LlmStage::Vision
        )
    }
}

fn parse_bool(key: &'static str, value: Option<String>) -> Result<bool, ConfigError> {
    match value.as_deref().map(str::trim) {
        None => Ok(false),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => Err(ConfigError::Invalid {
            key,
            reason: format!("expected true or false, got {other:?}"),
        }),
    }
}

/// Whether a `WT_LOG` filter turns on debug or trace output for any target.
fn enables_debug_logging(filter: &str) -> bool {
    filter.split(',').any(|directive| {
        let level = directive
            .rsplit('=')
            .next()
            .unwrap_or(directive)
            .trim()
            .to_ascii_lowercase();
        level == "debug" || level == "trace"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DB_SECRET: &str = "pw-7f3a9c-db";
    const SESSION_SECRET: &str = "sess-5be21d";
    const ANTHROPIC_SECRET: &str = "sk-ant-test-91c0";
    const YOUTUBE_SECRET: &str = "yt-key-44e8";

    fn load(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|key| map.get(key).cloned())
    }

    fn full_prod() -> Vec<(&'static str, String)> {
        vec![
            ("WT_ENV", "prod".to_owned()),
            (
                "DATABASE_URL",
                format!("postgres://wt_web_login:{DB_SECRET}@db.internal:5432/wt?sslmode=require"),
            ),
            ("WT_SESSION_SECRET", SESSION_SECRET.to_owned()),
            ("ANTHROPIC_API_KEY", ANTHROPIC_SECRET.to_owned()),
            ("YOUTUBE_API_KEY", YOUTUBE_SECRET.to_owned()),
        ]
    }

    fn load_owned(pairs: &[(&'static str, String)]) -> Result<Config, ConfigError> {
        let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        load(&borrowed)
    }

    #[test]
    fn missing_database_url_fails_at_startup() {
        assert_eq!(
            load(&[]).unwrap_err(),
            ConfigError::Missing {
                key: "DATABASE_URL"
            }
        );
    }

    #[test]
    fn empty_value_counts_as_missing() {
        assert_eq!(
            load(&[("DATABASE_URL", "  ")]).unwrap_err(),
            ConfigError::Missing {
                key: "DATABASE_URL"
            }
        );
    }

    #[test]
    fn session_secret_is_required_outside_dev() {
        let mut pairs = full_prod();
        pairs.retain(|(k, _)| *k != "WT_SESSION_SECRET");
        assert_eq!(
            load_owned(&pairs).unwrap_err(),
            ConfigError::Missing {
                key: "WT_SESSION_SECRET"
            }
        );
    }

    #[test]
    fn dev_defaults_match_the_spec() {
        let config = load(&[("DATABASE_URL", "postgres://wt:wt@localhost:5432/wt")]).unwrap();
        assert_eq!(config.env, Env::Dev);
        assert_eq!(config.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.scratch_dir, PathBuf::from("./data/scratch"));
        assert_eq!(config.model_dir, PathBuf::from("./models"));
        assert_eq!(config.embed_model, "bge-m3-int8");
        assert_eq!(config.rerank_model, "bge-reranker-v2-m3-int8");
        assert_eq!(config.diarize_url, "http://127.0.0.1:8090");
        assert_eq!(config.log, "info");
        assert!(!config.llm_real_data_ok);
        assert!(!config.allow_third_party_fetch);
        assert!(config.session_secret.is_none());
        assert_eq!(
            config.require_llm_model().unwrap_err(),
            ConfigError::Missing {
                key: "WT_LLM_MODEL"
            }
        );
    }

    #[test]
    fn non_dev_requires_tls_to_postgres() {
        let mut pairs = full_prod();
        pairs[1].1 = format!("postgres://u:{DB_SECRET}@db.internal:5432/wt");
        let err = load_owned(&pairs).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                key: "DATABASE_URL",
                ..
            }
        ));
        assert!(!err.to_string().contains(DB_SECRET));
    }

    #[test]
    fn prod_refuses_debug_logging() {
        for filter in ["debug", "info,wt_api=trace", "warn,sqlx=DEBUG"] {
            let mut pairs = full_prod();
            pairs.push(("WT_LOG", filter.to_owned()));
            assert!(
                matches!(
                    load_owned(&pairs),
                    Err(ConfigError::Invalid { key: "WT_LOG", .. })
                ),
                "{filter} should be refused in prod"
            );
        }
        let mut pairs = full_prod();
        pairs.push(("WT_LOG", "info,wt_api=warn".to_owned()));
        assert!(load_owned(&pairs).is_ok());
    }

    #[test]
    fn invalid_values_are_reported_by_key() {
        let base = ("DATABASE_URL", "postgres://wt:wt@localhost:5432/wt");
        assert!(matches!(
            load(&[base, ("WT_ENV", "production")]),
            Err(ConfigError::Invalid { key: "WT_ENV", .. })
        ));
        assert!(matches!(
            load(&[base, ("WT_BIND", "localhost")]),
            Err(ConfigError::Invalid { key: "WT_BIND", .. })
        ));
        assert!(matches!(
            load(&[base, ("WT_LLM_REAL_DATA_OK", "yes")]),
            Err(ConfigError::Invalid {
                key: "WT_LLM_REAL_DATA_OK",
                ..
            })
        ));
    }

    #[test]
    fn database_url_is_built_from_parts_with_tls_outside_dev() {
        let config = load(&[
            ("WT_ENV", "prod"),
            ("WT_SESSION_SECRET", SESSION_SECRET),
            ("WT_DB_HOST", "wt-v1.abc.us-east-2.rds.amazonaws.com"),
            ("WT_DB_USER", "wt_web_login"),
            ("WT_DB_PASSWORD", "p@ss/w:rd"),
        ])
        .unwrap();
        assert_eq!(
            config.database_url.expose(),
            "postgres://wt_web_login:p%40ss%2Fw%3Ard@wt-v1.abc.us-east-2.rds.amazonaws.com:5432/wt?sslmode=require"
        );
    }

    #[test]
    fn database_url_parts_require_user_and_password() {
        assert_eq!(
            load(&[("WT_DB_HOST", "localhost"), ("WT_DB_USER", "wt")]).unwrap_err(),
            ConfigError::Missing {
                key: "WT_DB_PASSWORD"
            }
        );
        assert_eq!(
            load(&[("WT_DB_HOST", "localhost"), ("WT_DB_PASSWORD", "x")]).unwrap_err(),
            ConfigError::Missing { key: "WT_DB_USER" }
        );
    }

    #[test]
    fn explicit_database_url_wins_over_parts() {
        let config = load(&[
            ("DATABASE_URL", "postgres://wt:wt@localhost:5432/wt"),
            ("WT_DB_HOST", "elsewhere"),
        ])
        .unwrap();
        assert_eq!(
            config.database_url.expose(),
            "postgres://wt:wt@localhost:5432/wt"
        );
    }

    #[test]
    fn secrets_never_appear_in_debug_output() {
        let config = load_owned(&full_prod()).unwrap();
        let debug = format!("{config:?}");
        let pretty = format!("{config:#?}");
        for secret in [DB_SECRET, SESSION_SECRET, ANTHROPIC_SECRET, YOUTUBE_SECRET] {
            assert!(!debug.contains(secret), "{secret} leaked into Debug");
            assert!(
                !pretty.contains(secret),
                "{secret} leaked into pretty Debug"
            );
        }
        assert!(debug.contains("[redacted]"));
        assert_eq!(config.anthropic_api_key.unwrap().expose(), ANTHROPIC_SECRET);
    }

    #[test]
    fn secrets_never_appear_in_log_output() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();

        let config = load_owned(&full_prod()).unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(config = ?config, "configuration loaded");
            tracing::debug!(database_url = ?config.database_url, "connecting");
        });

        let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("configuration loaded"));
        for secret in [DB_SECRET, SESSION_SECRET, ANTHROPIC_SECRET, YOUTUBE_SECRET] {
            assert!(!output.contains(secret), "{secret} leaked into the log");
        }
    }

    #[test]
    fn stage_models_fall_back_to_the_default_and_fail_when_neither_is_set() {
        let base = [
            ("WT_ENV", "dev"),
            ("DATABASE_URL", "postgres://wt:wt@localhost/wt"),
        ];
        let none = load(&base).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            none.llm_model_for(LlmStage::Extract),
            Err(ConfigError::Missing { .. })
        ));
        let fallback = load(&[
            ("WT_ENV", "dev"),
            ("DATABASE_URL", "postgres://wt:wt@localhost/wt"),
            ("WT_LLM_MODEL", "claude-haiku-4-5"),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            fallback.llm_model_for(LlmStage::Synth).ok(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            fallback.llm_model_for(LlmStage::Classify).ok(),
            Some("claude-haiku-4-5")
        );
        let own = load(&[
            ("WT_ENV", "dev"),
            ("DATABASE_URL", "postgres://wt:wt@localhost/wt"),
            ("WT_LLM_MODEL", "claude-haiku-4-5"),
            ("WT_LLM_MODEL_SYNTH", "claude-sonnet-5-5"),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            own.llm_model_for(LlmStage::Synth).ok(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(own.llm_monthly_cap_usd, 25.0);
        assert!(
            load(&[
                ("WT_ENV", "dev"),
                ("DATABASE_URL", "postgres://wt:wt@localhost/wt"),
                ("WT_LLM_MONTHLY_CAP_USD", "lots")
            ])
            .is_err()
        );
    }
}
