//! Spec 04, the language-model client and spend cap, against a local `wiremock` server standing
//! in for the Claude API. No request leaves the machine; every prompt is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use wt_core::Secret;
use wt_core::config::LlmStage;
use wt_extract::llm::{ModelPrice, PriceTable};
use wt_extract::{
    Alerter, ClaudeClient, ClaudeSettings, DataClass, FakeSns, Ledger, LlmError, LlmRequest,
    PgLedger, SnsAlerter, complete_json,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Label {
    label: String,
    reason: String,
}

/// One recorded call: stage, model, input tokens, output tokens, cost.
type Row = (String, String, i32, i32, f64);

#[derive(Default)]
struct MemoryLedger {
    spent: Mutex<f64>,
    cap: f64,
    alerted: Mutex<bool>,
    rows: Mutex<Vec<Row>>,
}

#[async_trait]
impl Ledger for MemoryLedger {
    async fn month_to_date(&self, _at: DateTime<Utc>) -> Result<f64, LlmError> {
        Ok(*self.spent.lock().unwrap())
    }
    async fn cap(&self) -> Result<f64, LlmError> {
        Ok(self.cap)
    }
    async fn record(
        &self,
        stage: &str,
        model: &str,
        i: i32,
        o: i32,
        usd: f64,
    ) -> Result<(), LlmError> {
        *self.spent.lock().unwrap() += usd;
        self.rows
            .lock()
            .unwrap()
            .push((stage.into(), model.into(), i, o, usd));
        Ok(())
    }
    async fn claim_alert(&self, _at: DateTime<Utc>) -> Result<bool, LlmError> {
        let mut a = self.alerted.lock().unwrap();
        Ok(!std::mem::replace(&mut *a, true))
    }
}

#[derive(Default)]
struct Alerts(Mutex<Vec<String>>);

#[async_trait]
impl Alerter for Alerts {
    async fn send(&self, subject: &str, _body: &str) {
        self.0.lock().unwrap().push(subject.into());
    }
}

fn prices() -> PriceTable {
    PriceTable {
        checked_on: "2026-10-05".into(),
        models: vec![
            ModelPrice {
                id: "claude-haiku-4-5".into(),
                input_per_mtok: 1.0,
                output_per_mtok: 5.0,
                sampling: true,
            },
            ModelPrice {
                id: "claude-sonnet-5-5".into(),
                input_per_mtok: 2.0,
                output_per_mtok: 10.0,
                sampling: false,
            },
        ],
    }
}

fn client(
    server: &MockServer,
    ledger: Arc<dyn Ledger>,
    alerts: Arc<dyn Alerter>,
    real_ok: bool,
) -> ClaudeClient {
    let models: BTreeMap<&'static str, String> = [
        ("classify", "claude-haiku-4-5".to_owned()),
        ("synth", "claude-sonnet-5-5".to_owned()),
    ]
    .into_iter()
    .collect();
    ClaudeClient::new(
        ClaudeSettings {
            base_url: server.uri(),
            api_key: Secret::new("fixture-key"),
            workspace_id: Some("wrkspc_fixture".into()),
            models,
            real_data_ok: real_ok,
            retry_base: Duration::from_millis(1),
        },
        prices(),
        ledger,
        alerts,
    )
    .unwrap()
}

fn reply(text: &str, input: i64, output: i64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_fixture", "type": "message", "role": "assistant", "model": "fixture",
        "content": [{ "type": "text", "text": text }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": input, "output_tokens": output }
    }))
}

fn req(stage: LlmStage) -> LlmRequest {
    LlmRequest {
        stage,
        system: "Label the window.".into(),
        user: "fixture words".into(),
        max_tokens: 256,
    }
}

fn body(r: &Request) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn requests_use_structured_outputs_and_temperature_only_where_accepted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("x-api-key", "fixture-key"))
        .and(header("anthropic-workspace-id", "wrkspc_fixture"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 1000, 100))
        .mount(&server)
        .await;
    let ledger = Arc::new(MemoryLedger {
        cap: 25.0,
        ..Default::default()
    });
    let c = client(&server, ledger.clone(), Arc::new(Alerts::default()), false);

    let label: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap();
    assert_eq!(label.label, "talk");
    let _: Label = complete_json(&c, &req(LlmStage::Synth), DataClass::Synthetic)
        .await
        .unwrap();

    let seen = server.received_requests().await.unwrap();
    let (haiku, sonnet) = (body(&seen[0]), body(&seen[1]));
    assert_eq!(haiku["model"], "claude-haiku-4-5");
    assert_eq!(haiku["temperature"], 0);
    assert!(
        sonnet.get("temperature").is_none(),
        "Sonnet 5.5 rejects sampling settings"
    );
    for b in [&haiku, &sonnet] {
        assert!(b.get("tool_choice").is_none() && b.get("tools").is_none());
        let schema = &b["output_config"]["format"];
        assert_eq!(schema["type"], "json_schema");
        assert_eq!(schema["schema"]["additionalProperties"], false);
        assert!(schema["schema"].get("$schema").is_none());
    }
    // Tokens and cost are recorded per call: 1000 in and 100 out on Haiku is 0.0015 USD.
    let rows = ledger.rows.lock().unwrap().clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].0.as_str(), rows[0].2, rows[0].3),
        ("classify", 1000, 100)
    );
    assert!((rows[0].4 - 0.0015).abs() < 1e-9);
}

#[tokio::test]
async fn real_text_is_refused_until_allowed() {
    let server = MockServer::start().await;
    let c = client(
        &server,
        Arc::new(MemoryLedger {
            cap: 25.0,
            ..Default::default()
        }),
        Arc::new(Alerts::default()),
        false,
    );
    let err = complete_json::<Label>(&c, &req(LlmStage::Classify), DataClass::Real)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::RealDataRefused), "{err}");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing was sent"
    );
}

#[tokio::test]
async fn eighty_percent_alerts_once_and_the_cap_refuses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 10, 1))
        .mount(&server)
        .await;
    let ledger = Arc::new(MemoryLedger {
        cap: 10.0,
        spent: Mutex::new(8.0),
        ..Default::default()
    });
    let alerts = Arc::new(Alerts::default());
    let c = client(&server, ledger.clone(), alerts.clone(), false);
    for _ in 0..3 {
        let _: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
            .await
            .unwrap();
    }
    assert_eq!(alerts.0.lock().unwrap().len(), 1, "one alert per month");

    *ledger.spent.lock().unwrap() = 10.0;
    let before = server.received_requests().await.unwrap().len();
    let err = complete_json::<Label>(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::CapReached { .. }), "{err}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before,
        "refused before sending"
    );
}

/// Log lines written while the returned guard lives, as flattened JSON like production's.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn install(&self) -> tracing::subscriber::DefaultGuard {
        let writer = self.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
            .with_writer(move || writer.clone())
            .json()
            .flatten_event(true)
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    /// The parsed lines whose `event` field is `name`.
    fn events(&self, name: &str) -> Vec<Value> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["event"] == name)
            .collect()
    }
}

/// PB4.2: the 80 percent alert goes to the SNS topic, through the fake.
#[tokio::test]
async fn the_eighty_percent_alert_reaches_sns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 10, 1))
        .mount(&server)
        .await;
    let ledger = Arc::new(MemoryLedger {
        cap: 10.0,
        spent: Mutex::new(8.5),
        ..Default::default()
    });
    let sns = Arc::new(FakeSns::default());
    let topic = "arn:aws:sns:us-east-2:000000000000:wt-v1-alarms";
    let c = client(
        &server,
        ledger,
        Arc::new(SnsAlerter::new(sns.clone(), topic)),
        false,
    );
    let _: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap();
    let messages = sns.messages();
    assert_eq!(messages.len(), 1);
    let (to, subject, body) = &messages[0];
    assert_eq!(to, topic);
    assert_eq!(subject, "Wheel Turning: Language-model spend at 80 percent");
    assert!(body.contains("8.50 of 10.00 USD"), "{body}");
}

/// An alert that cannot be delivered is logged and does not fail the call.
#[tokio::test]
async fn an_undelivered_alert_does_not_fail_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 10, 1))
        .mount(&server)
        .await;
    let ledger = Arc::new(MemoryLedger {
        cap: 10.0,
        spent: Mutex::new(9.0),
        ..Default::default()
    });
    let logs = Captured::default();
    let _guard = logs.install();
    let c = client(
        &server,
        ledger,
        Arc::new(SnsAlerter::new(Arc::new(FakeSns::failing()), "arn:fixture")),
        false,
    );
    let _: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap();
    assert_eq!(logs.events("alert_failed").len(), 1);
}

/// PB4.3: each call logs one event with its stage and no request text.
#[tokio::test]
async fn calls_log_succeeded_failed_and_cap_reached_events() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 10, 1))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"type":"error","error":{"type":"invalid_request_error","message":"bad"}}),
        ))
        .mount(&server)
        .await;
    let ledger = Arc::new(MemoryLedger {
        cap: 10.0,
        ..Default::default()
    });
    let logs = Captured::default();
    let _guard = logs.install();
    let c = client(&server, ledger.clone(), Arc::new(Alerts::default()), false);
    let _: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap();
    complete_json::<Label>(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap_err();
    *ledger.spent.lock().unwrap() = 10.0;
    complete_json::<Label>(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap_err();

    let ok = logs.events("llm_call_succeeded");
    let failed = logs.events("llm_call_failed");
    let capped = logs.events("llm_cap_reached");
    assert_eq!(ok.len(), 1);
    assert_eq!(failed.len(), 1);
    assert_eq!(capped.len(), 1);
    assert_eq!(failed[0]["stage"], "classify");
    assert_eq!(failed[0]["error"], "status");
    assert_eq!(capped[0]["stage"], "classify");
    let all = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(
        !all.contains("Label the window") && !all.contains("fixture words"),
        "no request text in logs"
    );
}

#[tokio::test]
async fn overload_retries_and_a_bad_reply_gets_one_repair() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(529)
                .set_body_json(json!({"type":"error","error":{"type":"overloaded_error"}})),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk"}"#, 10, 1))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"chant","reason":"fixture"}"#, 10, 1))
        .mount(&server)
        .await;
    let c = client(
        &server,
        Arc::new(MemoryLedger {
            cap: 25.0,
            ..Default::default()
        }),
        Arc::new(Alerts::default()),
        false,
    );
    let label: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap();
    assert_eq!(label.label, "chant", "the repaired reply");
    let seen = server.received_requests().await.unwrap();
    assert_eq!(
        seen.len(),
        4,
        "two overloads, a reply missing a field, the repair"
    );
    assert!(
        body(&seen[3])["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("did not match")
    );

    let rejecting = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"type":"error","error":{"type":"invalid_request_error",
                                         "message":"add the anthropic-workspace-id header"}}),
        ))
        .mount(&rejecting)
        .await;
    let c = client(
        &rejecting,
        Arc::new(MemoryLedger {
            cap: 25.0,
            ..Default::default()
        }),
        Arc::new(Alerts::default()),
        false,
    );
    let err = complete_json::<Label>(&c, &req(LlmStage::Classify), DataClass::Synthetic)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Status { status: 400, .. }), "{err}");
    assert!(
        err.to_string()
            .contains("add the anthropic-workspace-id header"),
        "{err}"
    );
    assert_eq!(
        rejecting.received_requests().await.unwrap().len(),
        1,
        "a 400 is not retried"
    );
}

#[tokio::test]
async fn usage_rows_in_postgres_sum_to_the_month_so_far() {
    let db = wt_db::test_support::TestDb::start()
        .await
        .expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(r#"{"label":"talk","reason":"fixture"}"#, 1234, 56))
        .mount(&server)
        .await;
    let ledger = Arc::new(PgLedger::new(pool.clone(), 25.0));
    let c = client(&server, ledger.clone(), Arc::new(Alerts::default()), false);
    for _ in 0..3 {
        let _: Label = complete_json(&c, &req(LlmStage::Classify), DataClass::Synthetic)
            .await
            .unwrap();
    }
    let (rows, sum): (i64, f64) = sqlx::query_as(
        "select count(*), sum(est_cost_usd)::float8 from llm_usage where stage = 'classify'
           and input_tokens = 1234 and output_tokens = 56",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 3);
    let month = ledger.month_to_date(Utc::now()).await.unwrap();
    assert!((month - sum).abs() < 1e-9, "{month} vs {sum}");
    assert!((sum - 3.0 * 0.001514).abs() < 1e-4);

    // The owner's cap overrides the default and releases capped jobs.
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(wt_db::llm::cap(&mut conn, 25.0).await.unwrap(), 25.0);
    sqlx::query(
        "insert into job (kind, idempotency_key, run_at, last_error)
         values ('extract', 'capped', now() + interval '20 days', 'cap_reached')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        wt_db::llm::set_cap(&mut conn, 60.0, "owner").await.unwrap(),
        1
    );
    assert_eq!(wt_db::llm::cap(&mut conn, 25.0).await.unwrap(), 60.0);
    assert!(
        wt_db::llm::claim_alert(&mut conn, Utc::now())
            .await
            .unwrap()
    );
    assert!(
        !wt_db::llm::claim_alert(&mut conn, Utc::now())
            .await
            .unwrap(),
        "once a month"
    );
}
