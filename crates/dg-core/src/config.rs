//! The configuration loader.
//!
//! Every key from the Spec 01 configuration reference is read here, validated once at startup,
//! and exposed as a typed [`Config`]. A missing required key fails fast. Secret values are held
//! in [`Secret`], which never prints its contents, and no error message ever echoes a secret.
//! An empty value counts as unset, so a copied `.env.example` behaves like a missing key.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Deployment environment, from `DG_ENV`.
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
    /// `DG_ENV`.
    pub env: Env,
    /// `DATABASE_URL`, the connection for the application role.
    pub database_url: Secret,
    /// `DATABASE_URL_ANALYST`, used only by `dg report`.
    pub database_url_analyst: Option<Secret>,
    /// `DG_BIND`, the web listen address.
    pub bind: SocketAddr,
    /// `DG_SESSION_SECRET`, the key that signs session cookies. Always set outside dev.
    pub session_secret: Option<Secret>,
    /// `DG_SCRATCH_DIR`, the only place audio or video may exist.
    pub scratch_dir: PathBuf,
    /// `DG_TRANSCRIPT_BUCKET`. `None` means a local directory under `./data`.
    pub transcript_bucket: Option<String>,
    /// `DG_MODEL_DIR`, holding the exported ONNX models and tokenizers.
    pub model_dir: PathBuf,
    /// `DG_EMBED_MODEL`, the embedding model directory name inside `model_dir`.
    pub embed_model: String,
    /// `DG_RERANK_MODEL`, the reranker model directory name inside `model_dir`.
    pub rerank_model: String,
    /// `DG_LLM_MODEL`. Required only by the LLM stages: see [`Config::require_llm_model`].
    pub llm_model: Option<String>,
    /// `ANTHROPIC_API_KEY`.
    pub anthropic_api_key: Option<Secret>,
    /// `DG_LLM_REAL_DATA_OK`. Must be true before any call carries real transcript text.
    pub llm_real_data_ok: bool,
    /// `YOUTUBE_API_KEY`.
    pub youtube_api_key: Option<Secret>,
    /// `DG_ALLOW_THIRD_PARTY_FETCH`. Enables the yt-dlp fetch mode.
    pub allow_third_party_fetch: bool,
    /// `DG_DIARIZE_URL`, the base URL of the diarization sidecar.
    pub diarize_url: String,
    /// `DG_WHISPER_MODEL`, the path to the whisper large-v3 model file.
    pub whisper_model: Option<PathBuf>,
    /// `DG_SITE_DANA_URL`, the site's own donation page.
    pub site_dana_url: Option<String>,
    /// `DG_LOG`, the log filter.
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

        let env = match get("DG_ENV") {
            None => Env::Dev,
            Some(value) => Env::parse(value.trim()).ok_or_else(|| ConfigError::Invalid {
                key: "DG_ENV",
                reason: format!("expected dev, staging or prod, got {value:?}"),
            })?,
        };

        let database_url = Secret::new(get("DATABASE_URL").ok_or(ConfigError::Missing {
            key: "DATABASE_URL",
        })?);
        check_database_url("DATABASE_URL", &database_url, env)?;

        let database_url_analyst = get("DATABASE_URL_ANALYST").map(Secret::new);
        if let Some(url) = &database_url_analyst {
            check_database_url("DATABASE_URL_ANALYST", url, env)?;
        }

        let bind_text = get("DG_BIND").unwrap_or_else(|| "127.0.0.1:8080".to_owned());
        let bind = bind_text.parse().map_err(|_| ConfigError::Invalid {
            key: "DG_BIND",
            reason: format!("expected host:port, got {bind_text:?}"),
        })?;

        let session_secret = get("DG_SESSION_SECRET").map(Secret::new);
        if session_secret.is_none() && !env.is_dev() {
            return Err(ConfigError::Missing {
                key: "DG_SESSION_SECRET",
            });
        }

        let log = get("DG_LOG").unwrap_or_else(|| "info".to_owned());
        if env == Env::Prod && enables_debug_logging(&log) {
            return Err(ConfigError::Invalid {
                key: "DG_LOG",
                reason: "debug and trace logging are not allowed in prod".to_owned(),
            });
        }

        Ok(Self {
            env,
            database_url,
            database_url_analyst,
            bind,
            session_secret,
            scratch_dir: get("DG_SCRATCH_DIR")
                .map_or_else(|| PathBuf::from("/var/dg/scratch"), PathBuf::from),
            transcript_bucket: get("DG_TRANSCRIPT_BUCKET"),
            model_dir: get("DG_MODEL_DIR").map_or_else(|| PathBuf::from("./models"), PathBuf::from),
            embed_model: get("DG_EMBED_MODEL").unwrap_or_else(|| "bge-m3-int8".to_owned()),
            rerank_model: get("DG_RERANK_MODEL")
                .unwrap_or_else(|| "bge-reranker-v2-m3-int8".to_owned()),
            llm_model: get("DG_LLM_MODEL"),
            anthropic_api_key: get("ANTHROPIC_API_KEY").map(Secret::new),
            llm_real_data_ok: parse_bool("DG_LLM_REAL_DATA_OK", get("DG_LLM_REAL_DATA_OK"))?,
            youtube_api_key: get("YOUTUBE_API_KEY").map(Secret::new),
            allow_third_party_fetch: parse_bool(
                "DG_ALLOW_THIRD_PARTY_FETCH",
                get("DG_ALLOW_THIRD_PARTY_FETCH"),
            )?,
            diarize_url: get("DG_DIARIZE_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8090".to_owned()),
            whisper_model: get("DG_WHISPER_MODEL").map(PathBuf::from),
            site_dana_url: get("DG_SITE_DANA_URL"),
            log,
        })
    }

    /// The model identifier for the Claude API, which the LLM stages require.
    pub fn require_llm_model(&self) -> Result<&str, ConfigError> {
        self.llm_model.as_deref().ok_or(ConfigError::Missing {
            key: "DG_LLM_MODEL",
        })
    }
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

/// Whether a `DG_LOG` filter turns on debug or trace output for any target.
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
            ("DG_ENV", "prod".to_owned()),
            (
                "DATABASE_URL",
                format!("postgres://dg_web_login:{DB_SECRET}@db.internal:5432/dg?sslmode=require"),
            ),
            ("DG_SESSION_SECRET", SESSION_SECRET.to_owned()),
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
        pairs.retain(|(k, _)| *k != "DG_SESSION_SECRET");
        assert_eq!(
            load_owned(&pairs).unwrap_err(),
            ConfigError::Missing {
                key: "DG_SESSION_SECRET"
            }
        );
    }

    #[test]
    fn dev_defaults_match_the_spec() {
        let config = load(&[("DATABASE_URL", "postgres://dg:dg@localhost:5432/dg")]).unwrap();
        assert_eq!(config.env, Env::Dev);
        assert_eq!(config.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.scratch_dir, PathBuf::from("/var/dg/scratch"));
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
                key: "DG_LLM_MODEL"
            }
        );
    }

    #[test]
    fn non_dev_requires_tls_to_postgres() {
        let mut pairs = full_prod();
        pairs[1].1 = format!("postgres://u:{DB_SECRET}@db.internal:5432/dg");
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
        for filter in ["debug", "info,dg_api=trace", "warn,sqlx=DEBUG"] {
            let mut pairs = full_prod();
            pairs.push(("DG_LOG", filter.to_owned()));
            assert!(
                matches!(
                    load_owned(&pairs),
                    Err(ConfigError::Invalid { key: "DG_LOG", .. })
                ),
                "{filter} should be refused in prod"
            );
        }
        let mut pairs = full_prod();
        pairs.push(("DG_LOG", "info,dg_api=warn".to_owned()));
        assert!(load_owned(&pairs).is_ok());
    }

    #[test]
    fn invalid_values_are_reported_by_key() {
        let base = ("DATABASE_URL", "postgres://dg:dg@localhost:5432/dg");
        assert!(matches!(
            load(&[base, ("DG_ENV", "production")]),
            Err(ConfigError::Invalid { key: "DG_ENV", .. })
        ));
        assert!(matches!(
            load(&[base, ("DG_BIND", "localhost")]),
            Err(ConfigError::Invalid { key: "DG_BIND", .. })
        ));
        assert!(matches!(
            load(&[base, ("DG_LLM_REAL_DATA_OK", "yes")]),
            Err(ConfigError::Invalid {
                key: "DG_LLM_REAL_DATA_OK",
                ..
            })
        ));
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
}
