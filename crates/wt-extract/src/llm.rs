//! `LlmClient`, the only code that calls the Claude API (Spec 04, Extraction and citation
//! resolution; Language-model spend cap).
//!
//! Rules: JSON comes back through structured outputs (`output_config.format`) with the schema
//! of the requested type, on every model, because Claude Sonnet 5.5, Opus 5.5 and Fable 5.1
//! reject a forced tool choice; temperature 0 is sent only to models the price table marks as
//! accepting sampling settings (decisions of October 5, 2026). A `Real` request is refused unless
//! `WT_LLM_REAL_DATA_OK=true`. Logs carry the stage, model, token counts and a hash of the
//! request, never its text. 429, 529 and 5xx responses retry with backoff up to three times. A
//! reply that does not match the type gets one repair attempt.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use wt_core::Secret;
use wt_core::config::LlmStage;
use wt_core::records::sha256_hex;

/// Whether a request carries real talk text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClass {
    /// Fixtures only.
    Synthetic,
    /// Real transcript text: refused unless `WT_LLM_REAL_DATA_OK=true`.
    Real,
}

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequest {
    /// The stage, which picks the model and labels the usage row.
    pub stage: LlmStage,
    /// The system prompt.
    pub system: String,
    /// The user message.
    pub user: String,
    /// The output limit.
    pub max_tokens: u32,
}

/// Language-model errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Real text without `WT_LLM_REAL_DATA_OK=true`.
    #[error("real transcript text may not be sent until WT_LLM_REAL_DATA_OK is true")]
    RealDataRefused,
    /// The month's cap is reached.
    #[error("the monthly language-model cap is reached ({month_to_date:.2} of {cap:.2} USD)")]
    CapReached {
        /// Spend so far.
        month_to_date: f64,
        /// The cap.
        cap: f64,
    },
    /// No model is configured for the stage.
    #[error("no model configured for stage {0}")]
    NoModel(&'static str),
    /// The model is not in the price table.
    #[error("model {0} is not in config/llm_prices.toml, so its cost cannot be estimated")]
    UnknownModel(String),
    /// The request could not be sent.
    #[error("calling the Claude API: {0}")]
    Http(String),
    /// The API answered with an error.
    #[error("the Claude API answered {status}: {kind}: {message}")]
    Status {
        /// HTTP status.
        status: u16,
        /// The error type the API names.
        kind: String,
        /// The API's explanation.
        message: String,
    },
    /// The model declined.
    #[error("the model declined the request")]
    Refusal,
    /// The reply hit `max_tokens`.
    #[error("the reply was cut off at max_tokens")]
    Truncated,
    /// The reply did not match the schema, even after the repair attempt.
    #[error("the reply did not match the schema: {0}")]
    Parse(String),
    /// The spend ledger failed.
    #[error("the spend ledger: {0}")]
    Ledger(String),
    /// The price table could not be read.
    #[error("{0}")]
    Prices(String),
}

/// The client every stage uses. `complete_value` is object safe; `complete_json` adds the type.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Sends `req` constrained to `schema` and returns the parsed JSON reply.
    async fn complete_value(
        &self,
        req: &LlmRequest,
        schema: &Value,
        class: DataClass,
    ) -> Result<Value, LlmError>;
}

/// Sends `req` and returns the reply as `T`, with one repair attempt if the reply does not
/// deserialize.
pub async fn complete_json<T: DeserializeOwned + JsonSchema>(
    client: &dyn LlmClient,
    req: &LlmRequest,
    class: DataClass,
) -> Result<T, LlmError> {
    let schema = api_schema::<T>();
    let first = client.complete_value(req, &schema, class).await;
    let error = match first {
        Ok(value) => match serde_json::from_value::<T>(value) {
            Ok(t) => return Ok(t),
            Err(e) => e.to_string(),
        },
        Err(LlmError::Parse(e)) => e,
        Err(other) => return Err(other),
    };
    let repair = LlmRequest {
        user: format!(
            "{}\n\nYour previous reply did not match the required JSON schema ({error}). \
             Reply again with JSON that matches it exactly.",
            req.user
        ),
        ..req.clone()
    };
    let value = client.complete_value(&repair, &schema, class).await?;
    serde_json::from_value::<T>(value).map_err(|e| LlmError::Parse(e.to_string()))
}

/// The JSON Schema of `T` in the form structured outputs accept: every object closed with
/// `additionalProperties: false`, numeric and length bounds removed, `oneOf` as `anyOf`.
pub fn api_schema<T: JsonSchema>() -> Value {
    let mut value = serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");
    }
    clean(&mut value);
    value
}

/// Rewrites a generated schema, recursively, into what structured outputs accept: see
/// [`api_schema`].
fn clean(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in [
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                "minLength",
                "maxLength",
                "minItems",
                "maxItems",
                "uniqueItems",
            ] {
                map.remove(key);
            }
            if let Some(format) = map.get("format").and_then(Value::as_str) {
                let allowed = [
                    "date-time",
                    "time",
                    "date",
                    "duration",
                    "email",
                    "hostname",
                    "uri",
                    "ipv4",
                    "ipv6",
                    "uuid",
                ];
                if !allowed.contains(&format) {
                    map.remove("format");
                }
            }
            if let Some(one_of) = map.remove("oneOf") {
                map.insert("anyOf".into(), one_of);
            }
            if map.contains_key("properties") {
                map.insert("additionalProperties".into(), Value::Bool(false));
            }
            for v in map.values_mut() {
                clean(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(clean),
        _ => {}
    }
}

/// One model's prices and sampling support, from `config/llm_prices.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    /// The model id.
    pub id: String,
    /// Dollars per million input tokens.
    pub input_per_mtok: f64,
    /// Dollars per million output tokens.
    pub output_per_mtok: f64,
    /// Whether the model accepts sampling settings such as temperature.
    pub sampling: bool,
}

/// `config/llm_prices.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceTable {
    /// When a person last checked the prices.
    pub checked_on: String,
    /// The models.
    #[serde(rename = "model")]
    pub models: Vec<ModelPrice>,
}

impl PriceTable {
    /// Reads the table.
    pub fn load(path: &Path) -> Result<Self, LlmError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| LlmError::Prices(format!("{}: {e}", path.display())))?;
        toml::from_str(&text).map_err(|e| LlmError::Prices(format!("{}: {e}", path.display())))
    }

    /// The entry for `model`.
    pub fn get(&self, model: &str) -> Option<&ModelPrice> {
        self.models.iter().find(|m| m.id == model)
    }
}

impl ModelPrice {
    /// The estimated cost of a call.
    pub fn cost(&self, input_tokens: i64, output_tokens: i64) -> f64 {
        (input_tokens as f64 * self.input_per_mtok + output_tokens as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// Where usage is recorded and the cap is read.
#[async_trait]
pub trait Ledger: Send + Sync {
    /// Spend so far in `at`'s month.
    async fn month_to_date(&self, at: DateTime<Utc>) -> Result<f64, LlmError>;
    /// The cap.
    async fn cap(&self) -> Result<f64, LlmError>;
    /// Records a call.
    async fn record(
        &self,
        stage: &str,
        model: &str,
        input: i32,
        output: i32,
        usd: f64,
    ) -> Result<(), LlmError>;
    /// True the first time `at`'s month crosses 80 percent.
    async fn claim_alert(&self, at: DateTime<Utc>) -> Result<bool, LlmError>;
}

/// The ledger in Postgres.
#[derive(Debug, Clone)]
pub struct PgLedger {
    /// The database holding the spend ledger.
    pool: sqlx::PgPool,
    /// The cap when none is set in the database.
    default_cap: f64,
}

impl PgLedger {
    /// A ledger with `WT_LLM_MONTHLY_CAP_USD` as the cap unless the owner set one.
    pub fn new(pool: sqlx::PgPool, default_cap: f64) -> Self {
        Self { pool, default_cap }
    }
}

/// A spend-ledger failure as a model-call error.
fn ledger_error(e: impl std::fmt::Display) -> LlmError {
    LlmError::Ledger(e.to_string())
}

#[async_trait]
impl Ledger for PgLedger {
    async fn month_to_date(&self, at: DateTime<Utc>) -> Result<f64, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(ledger_error)?;
        wt_db::llm::month_to_date(&mut conn, at)
            .await
            .map_err(ledger_error)
    }

    async fn cap(&self) -> Result<f64, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(ledger_error)?;
        wt_db::llm::cap(&mut conn, self.default_cap)
            .await
            .map_err(ledger_error)
    }

    async fn record(
        &self,
        stage: &str,
        model: &str,
        input: i32,
        output: i32,
        usd: f64,
    ) -> Result<(), LlmError> {
        let mut conn = self.pool.acquire().await.map_err(ledger_error)?;
        wt_db::llm::record(
            &mut conn,
            &wt_db::llm::Usage {
                stage,
                model,
                input_tokens: input,
                output_tokens: output,
                est_cost_usd: usd,
            },
        )
        .await
        .map_err(ledger_error)
    }

    async fn claim_alert(&self, at: DateTime<Utc>) -> Result<bool, LlmError> {
        let mut conn = self.pool.acquire().await.map_err(ledger_error)?;
        wt_db::llm::claim_alert(&mut conn, at)
            .await
            .map_err(ledger_error)
    }
}

/// Alerts to the owner. SNS (`WT_ALERT_TOPIC_ARN`) arrives with the project's AWS client; until
/// then [`LogAlerter`] writes a warning.
#[async_trait]
pub trait Alerter: Send + Sync {
    /// Sends one alert.
    async fn send(&self, subject: &str, body: &str);
}

/// Logs alerts as warnings.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogAlerter;

#[async_trait]
impl Alerter for LogAlerter {
    async fn send(&self, subject: &str, body: &str) {
        tracing::warn!(alert = subject, detail = body, "owner alert");
    }
}

/// How the client reaches the API.
#[derive(Debug, Clone)]
pub struct ClaudeSettings {
    /// `https://api.anthropic.com`; tests point it at a local server.
    pub base_url: String,
    /// `ANTHROPIC_API_KEY`.
    pub api_key: Secret,
    /// `ANTHROPIC_WORKSPACE_ID`, for a key not scoped to a workspace.
    pub workspace_id: Option<String>,
    /// The model per stage.
    pub models: BTreeMap<&'static str, String>,
    /// `WT_LLM_REAL_DATA_OK`.
    pub real_data_ok: bool,
    /// The first retry's wait; it doubles each time.
    pub retry_base: Duration,
}

/// The Claude API client.
pub struct ClaudeClient {
    /// The HTTP client.
    http: reqwest::Client,
    /// Endpoint, key, models and retry settings.
    settings: ClaudeSettings,
    /// Prices per model, for the ledger.
    prices: PriceTable,
    /// Where spend is recorded and the cap checked.
    ledger: Arc<dyn Ledger>,
    /// Who hears when the cap is reached.
    alerter: Arc<dyn Alerter>,
    /// The time, replaceable in tests.
    clock: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl ClaudeClient {
    /// A client with the real clock.
    pub fn new(
        settings: ClaudeSettings,
        prices: PriceTable,
        ledger: Arc<dyn Ledger>,
        alerter: Arc<dyn Alerter>,
    ) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::Http(e.to_string()))?;
        Ok(Self {
            http,
            settings,
            prices,
            ledger,
            alerter,
            clock: Box::new(Utc::now),
        })
    }

    /// Replaces the clock, for tests of month boundaries.
    pub fn with_clock(mut self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Posts one Messages API request, retrying a 429 or a server error up to three times with
    /// exponential backoff from `retry_base`.
    async fn send(&self, body: &Value) -> Result<Value, LlmError> {
        let mut attempt = 0u32;
        loop {
            let mut request = self
                .http
                .post(format!(
                    "{}/v1/messages",
                    self.settings.base_url.trim_end_matches('/')
                ))
                .header("x-api-key", self.settings.api_key.expose())
                .header("anthropic-version", "2023-06-01");
            if let Some(workspace) = &self.settings.workspace_id {
                request = request.header("anthropic-workspace-id", workspace);
            }
            let response = request.json(body).send().await;
            let retryable = match &response {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_timeout() || e.is_connect(),
            };
            if retryable && attempt < 3 {
                tokio::time::sleep(self.settings.retry_base * 2u32.pow(attempt)).await;
                attempt += 1;
                continue;
            }
            let response = response.map_err(|e| LlmError::Http(e.without_url().to_string()))?;
            let status = response.status().as_u16();
            let value: Value = response
                .json()
                .await
                .map_err(|e| LlmError::Http(e.without_url().to_string()))?;
            if !(200..300).contains(&status) {
                let kind = value["error"]["type"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_owned();
                // The API's own message says what to fix; it never repeats the request text.
                let message = value["error"]["message"].as_str().unwrap_or("").to_owned();
                return Err(LlmError::Status {
                    status,
                    kind,
                    message,
                });
            }
            return Ok(value);
        }
    }
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn complete_value(
        &self,
        req: &LlmRequest,
        schema: &Value,
        class: DataClass,
    ) -> Result<Value, LlmError> {
        if class == DataClass::Real && !self.settings.real_data_ok {
            return Err(LlmError::RealDataRefused);
        }
        let stage = req.stage.as_str();
        let model = self
            .settings
            .models
            .get(stage)
            .ok_or(LlmError::NoModel(stage))?;
        let price = self
            .prices
            .get(model)
            .ok_or_else(|| LlmError::UnknownModel(model.clone()))?;

        let now = (self.clock)();
        let (spent, cap) = (
            self.ledger.month_to_date(now).await?,
            self.ledger.cap().await?,
        );
        if spent >= cap {
            return Err(LlmError::CapReached {
                month_to_date: spent,
                cap,
            });
        }
        if spent >= cap * 0.8 && self.ledger.claim_alert(now).await? {
            self.alerter
                .send(
                    "Language-model spend at 80 percent",
                    &format!("{spent:.2} of {cap:.2} USD this month"),
                )
                .await;
        }

        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": [{ "role": "user", "content": req.user }],
            "output_config": { "format": { "type": "json_schema", "schema": schema } },
        });
        if price.sampling {
            body["temperature"] = json!(0);
        }
        let request_hash = sha256_hex(body.to_string().as_bytes());
        let reply = self.send(&body).await?;

        let input = reply["usage"]["input_tokens"].as_i64().unwrap_or(0);
        let output = reply["usage"]["output_tokens"].as_i64().unwrap_or(0);
        let clamp = |n: i64| i32::try_from(n).unwrap_or(i32::MAX);
        let cost = price.cost(input, output);
        self.ledger
            .record(stage, model, clamp(input), clamp(output), cost)
            .await?;
        tracing::info!(stage, model = %model, input_tokens = input, output_tokens = output,
                       est_cost_usd = cost, request = %&request_hash[..16], "language-model call");

        match reply["stop_reason"].as_str() {
            Some("refusal") => return Err(LlmError::Refusal),
            Some("max_tokens") => return Err(LlmError::Truncated),
            _ => {}
        }
        let text: String = reply["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        serde_json::from_str(&text).map_err(|e| LlmError::Parse(e.to_string()))
    }
}

/// A client that returns scripted replies and keeps the requests, for tests.
#[derive(Debug, Default)]
pub struct FakeLlm {
    /// The replies to give, in order.
    replies: Mutex<Vec<Result<Value, LlmError>>>,
    /// Every request seen, with its data class.
    pub seen: Mutex<Vec<(LlmRequest, DataClass)>>,
}

impl FakeLlm {
    /// Replies in order; an `Err` is a reply that fails to parse.
    pub fn new(replies: Vec<Result<Value, String>>) -> Self {
        Self::scripted(
            replies
                .into_iter()
                .map(|r| r.map_err(LlmError::Parse))
                .collect(),
        )
    }

    /// Replies in order, each a value or the error the call returns, such as
    /// [`LlmError::Truncated`].
    pub fn scripted(replies: Vec<Result<Value, LlmError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmClient for FakeLlm {
    async fn complete_value(
        &self,
        req: &LlmRequest,
        _schema: &Value,
        class: DataClass,
    ) -> Result<Value, LlmError> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((req.clone(), class));
        }
        let next = self.replies.lock().ok().and_then(|mut r| r.pop());
        match next {
            Some(reply) => reply,
            None => Err(LlmError::Http("no scripted reply left".into())),
        }
    }
}

/// The client a worker uses when no model may be called: no API key, so every request is
/// refused as having no model. Extraction then stages its citations as skipped.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableLlm;

#[async_trait]
impl LlmClient for UnavailableLlm {
    async fn complete_value(
        &self,
        req: &LlmRequest,
        _schema: &Value,
        _class: DataClass,
    ) -> Result<Value, LlmError> {
        Err(LlmError::NoModel(req.stage.as_str()))
    }
}
