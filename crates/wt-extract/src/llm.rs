//! `LlmClient`, the only code that calls the Claude API (Spec 04, Extraction and citation
//! resolution; Language-model spend cap).
//!
//! Rules: JSON comes back through structured outputs (`output_config.format`) with the schema
//! of the requested type, on every model, because Claude Sonnet 5.5, Opus 5.5 and Fable 5.1
//! reject a forced tool choice; temperature 0 is sent only to models the price table marks as
//! accepting sampling settings (decisions of October 5, 2026). A `Real` request is refused unless
//! `WT_LLM_REAL_DATA_OK=true`. Logs carry the stage, model, token counts and a hash of the
//! request, never its text. 429, 529 and 5xx responses retry with backoff up to three times. A
//! reply that does not match the type gets one repair attempt. A model whose price holds only up
//! to a prompt size (`max_prompt_tokens`) is never sent a longer prompt (Spec 11, PB2.0).

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
    /// The prompt is longer than the model's price holds for.
    #[error("the prompt is {tokens} tokens, over the {max} that {model}'s price covers")]
    PromptTooLong {
        /// The model.
        model: String,
        /// The prompt's tokens, as the API counted them.
        tokens: u64,
        /// The model's `max_prompt_tokens`.
        max: u64,
    },
}

impl LlmError {
    /// A short fixed name for the error, for logs and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            LlmError::RealDataRefused => "real_data_refused",
            LlmError::CapReached { .. } => "cap_reached",
            LlmError::NoModel(_) => "no_model",
            LlmError::UnknownModel(_) => "unknown_model",
            LlmError::Http(_) => "http",
            LlmError::Status { .. } => "status",
            LlmError::Refusal => "refusal",
            LlmError::Truncated => "truncated",
            LlmError::Parse(_) => "parse",
            LlmError::Ledger(_) => "ledger",
            LlmError::Prices(_) => "prices",
            LlmError::PromptTooLong { .. } => "prompt_too_long",
        }
    }
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
    /// The longest prompt, in tokens, the price holds for: a longer one is priced higher, and
    /// the table has no price for it, so the client refuses it. `None` is no limit.
    #[serde(default)]
    pub max_prompt_tokens: Option<u64>,
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

/// Alerts to the owner: [`SnsAlerter`] when `WT_ALERT_TOPIC_ARN` is set, [`LogAlerter`] otherwise.
#[async_trait]
pub trait Alerter: Send + Sync {
    /// Sends one alert. A failure to deliver is logged, never returned: an alert must not stop
    /// the work that raised it.
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

/// Why an alert could not be published.
#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    /// SNS refused or could not be reached.
    #[error("publishing to SNS: {0}")]
    Publish(String),
}

/// Publishes to one SNS topic. [`AwsSns`] in service, [`FakeSns`] in tests.
#[async_trait]
pub trait SnsPublisher: Send + Sync {
    /// Publishes one message and returns its message ID.
    async fn publish(
        &self,
        topic_arn: &str,
        subject: &str,
        body: &str,
    ) -> Result<String, AlertError>;
}

/// Amazon SNS, with credentials and region from the environment (the task or instance role and
/// `AWS_REGION`, or the owner's profile for `wt alerts test`).
pub struct AwsSns {
    /// The SNS client.
    client: aws_sdk_sns::Client,
}

impl AwsSns {
    /// A client from the environment.
    pub async fn from_env() -> Self {
        use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .http_client(http)
            .load()
            .await;
        Self {
            client: aws_sdk_sns::Client::new(&config),
        }
    }
}

#[async_trait]
impl SnsPublisher for AwsSns {
    async fn publish(
        &self,
        topic_arn: &str,
        subject: &str,
        body: &str,
    ) -> Result<String, AlertError> {
        let out = self
            .client
            .publish()
            .topic_arn(topic_arn)
            .subject(subject)
            .message(body)
            .send()
            .await
            .map_err(|e| {
                AlertError::Publish(aws_sdk_sns::error::DisplayErrorContext(e).to_string())
            })?;
        Ok(out.message_id().unwrap_or_default().to_owned())
    }
}

/// An SNS stand-in for tests: keeps every message, or fails every publish.
#[derive(Debug, Default)]
pub struct FakeSns {
    /// Each message as (topic ARN, subject, body).
    pub published: Mutex<Vec<(String, String, String)>>,
    /// Fail every publish, as when SNS is unreachable.
    fail: bool,
}

impl FakeSns {
    /// A fake whose every publish fails.
    pub fn failing() -> Self {
        Self {
            published: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    /// The messages published so far.
    pub fn messages(&self) -> Vec<(String, String, String)> {
        self.published.lock().map(|m| m.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl SnsPublisher for FakeSns {
    async fn publish(
        &self,
        topic_arn: &str,
        subject: &str,
        body: &str,
    ) -> Result<String, AlertError> {
        if self.fail {
            return Err(AlertError::Publish("the fake fails every publish".into()));
        }
        let mut published = self
            .published
            .lock()
            .map_err(|e| AlertError::Publish(e.to_string()))?;
        published.push((topic_arn.to_owned(), subject.to_owned(), body.to_owned()));
        Ok(format!("fake-{}", published.len()))
    }
}

/// Alerts through the SNS topic `WT_ALERT_TOPIC_ARN`, which emails its subscribers (PB4.2).
pub struct SnsAlerter {
    /// Who publishes.
    publisher: Arc<dyn SnsPublisher>,
    /// `WT_ALERT_TOPIC_ARN`.
    topic_arn: String,
}

/// SNS rejects a subject over 100 characters or with a line break.
const SUBJECT_MAX: usize = 100;

impl SnsAlerter {
    /// An alerter publishing to `topic_arn` through `publisher`.
    pub fn new(publisher: Arc<dyn SnsPublisher>, topic_arn: impl Into<String>) -> Self {
        Self {
            publisher,
            topic_arn: topic_arn.into(),
        }
    }

    /// Publishes one alert and returns the SNS message ID. `wt alerts test` reports the error;
    /// [`Alerter::send`] logs it.
    pub async fn publish(&self, subject: &str, body: &str) -> Result<String, AlertError> {
        let subject: String = format!("Wheel Turning: {subject}")
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(SUBJECT_MAX)
            .collect();
        self.publisher
            .publish(&self.topic_arn, &subject, body)
            .await
    }
}

#[async_trait]
impl Alerter for SnsAlerter {
    async fn send(&self, subject: &str, body: &str) {
        tracing::warn!(alert = subject, detail = body, "owner alert");
        if let Err(e) = self.publish(subject, body).await {
            tracing::error!(event = "alert_failed", error = %e, "owner alert not delivered");
        }
    }
}

/// The alerter for a service: SNS when `topic_arn` is set, logs otherwise.
pub async fn alerter(topic_arn: Option<&str>) -> Arc<dyn Alerter> {
    match topic_arn {
        Some(arn) if !arn.trim().is_empty() => Arc::new(SnsAlerter::new(
            Arc::new(AwsSns::from_env().await),
            arn.trim(),
        )),
        _ => Arc::new(LogAlerter),
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

    /// Posts one Messages API request to `endpoint` (`messages` or `messages/count_tokens`),
    /// retrying a 429 or a server error up to three times with exponential backoff from
    /// `retry_base`.
    async fn send(&self, endpoint: &str, body: &Value) -> Result<Value, LlmError> {
        let mut attempt = 0u32;
        loop {
            let mut request = self
                .http
                .post(format!(
                    "{}/v1/{endpoint}",
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

    /// Refuses a prompt over `max` tokens. A token never covers less than one byte, so a request
    /// body well under `max` bytes cannot be over; only a larger one is counted by the API's free
    /// token-counting endpoint, which leaves the usual prompt without an extra call.
    async fn check_prompt_size(
        &self,
        body: &Value,
        bytes: usize,
        model: &str,
        max: u64,
    ) -> Result<(), LlmError> {
        // The API adds its own instructions for structured outputs, which the body does not hold.
        const OVERHEAD: u64 = 2_000;
        if (bytes as u64).saturating_add(OVERHEAD) <= max {
            return Ok(());
        }
        let count = json!({
            "model": body["model"],
            "system": body["system"],
            "messages": body["messages"],
        });
        let counted = self.send("messages/count_tokens", &count).await?;
        let tokens = counted["input_tokens"]
            .as_u64()
            .ok_or_else(|| LlmError::Http("count_tokens returned no input_tokens".into()))?;
        if tokens.saturating_add(OVERHEAD) > max {
            return Err(LlmError::PromptTooLong {
                model: model.to_owned(),
                tokens,
                max,
            });
        }
        Ok(())
    }

    /// One call: the cap check, the request, the ledger row and the parsed reply.
    async fn call(
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
        let text = body.to_string();
        if let Some(max) = price.max_prompt_tokens {
            self.check_prompt_size(&body, text.len(), model, max)
                .await?;
        }
        let request_hash = sha256_hex(text.as_bytes());
        let reply = self.send("messages", &body).await?;

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

#[async_trait]
impl LlmClient for ClaudeClient {
    /// Logs one event per call for the PB4.3 alarms, carrying the stage and never the text:
    /// `llm_call_succeeded`, `llm_call_failed` with the error's kind, or `llm_cap_reached`.
    async fn complete_value(
        &self,
        req: &LlmRequest,
        schema: &Value,
        class: DataClass,
    ) -> Result<Value, LlmError> {
        let stage = req.stage.as_str();
        let result = self.call(req, schema, class).await;
        match &result {
            Ok(_) => tracing::info!(
                event = "llm_call_succeeded",
                stage,
                "language-model call ok"
            ),
            Err(LlmError::CapReached { .. }) => {
                tracing::warn!(
                    event = "llm_cap_reached",
                    stage,
                    "language-model cap reached"
                )
            }
            Err(e) => tracing::warn!(
                event = "llm_call_failed",
                stage,
                error = e.kind(),
                "language-model call failed"
            ),
        }
        result
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
