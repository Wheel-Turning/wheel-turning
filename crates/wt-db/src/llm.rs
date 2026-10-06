//! The language-model spend ledger (Spec 04, Language-model spend cap): usage rows, the month
//! so far, the cap and its one alert per month.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use sqlx::PgConnection;

use crate::DbError;

/// One call's usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage<'a> {
    /// `extract`, `classify`, `synth`, `verify` or `vision`.
    pub stage: &'a str,
    /// The model id.
    pub model: &'a str,
    /// Input tokens from the API response.
    pub input_tokens: i32,
    /// Output tokens from the API response.
    pub output_tokens: i32,
    /// Estimated cost from the price table.
    pub est_cost_usd: f64,
}

/// The first day of `at`'s month.
pub fn month_of(at: DateTime<Utc>) -> NaiveDate {
    at.date_naive()
        .with_day(1)
        .unwrap_or_else(|| at.date_naive())
}

/// The first moment of the month after `at`'s, when a refused ingestion job runs again.
pub fn next_month(at: DateTime<Utc>) -> DateTime<Utc> {
    let first = month_of(at);
    let (y, m) = if first.month() == 12 {
        (first.year() + 1, 1)
    } else {
        (first.year(), first.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map_or(at, |d| d.and_utc())
}

/// Records one call.
pub async fn record(conn: &mut PgConnection, usage: &Usage<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "insert into llm_usage (stage, model, input_tokens, output_tokens, est_cost_usd)
         values ($1, $2, $3, $4, round($5::float8::numeric, 4))",
        usage.stage,
        usage.model,
        usage.input_tokens,
        usage.output_tokens,
        usage.est_cost_usd
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Estimated spend so far in `at`'s month.
pub async fn month_to_date(conn: &mut PgConnection, at: DateTime<Utc>) -> Result<f64, DbError> {
    let first = month_of(at);
    let sum = sqlx::query_scalar!(
        r#"select coalesce(sum(est_cost_usd), 0)::float8 as "sum!" from llm_usage
         where created_at >= $1::date and created_at < ($1::date + interval '1 month')"#,
        first
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(sum)
}

/// The cap: the owner's override if set, otherwise `default_usd` (`WT_LLM_MONTHLY_CAP_USD`).
pub async fn cap(conn: &mut PgConnection, default_usd: f64) -> Result<f64, DbError> {
    let value = sqlx::query_scalar!("select value from llm_setting where key = 'monthly_cap_usd'")
        .fetch_optional(&mut *conn)
        .await?;
    Ok(value.and_then(|v| v.parse().ok()).unwrap_or(default_usd))
}

/// `wt llm cap set`: sets the cap and releases ingestion jobs the cap had put off to next month.
/// Returns how many jobs were released.
pub async fn set_cap(conn: &mut PgConnection, usd: f64, by: &str) -> Result<u64, DbError> {
    sqlx::query!(
        "insert into llm_setting (key, value, updated_by) values ('monthly_cap_usd', $1, $2)
         on conflict (key) do update set value = excluded.value, updated_by = excluded.updated_by",
        usd.to_string(),
        by
    )
    .execute(&mut *conn)
    .await?;
    let released = sqlx::query!(
        "update job set run_at = now()
         where state = 'queued' and run_at > now() and last_error = 'cap_reached'"
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(released)
}

/// Records that `at`'s month crossed 80 percent. True only the first time, so one alert is
/// sent per month however many workers notice.
pub async fn claim_alert(conn: &mut PgConnection, at: DateTime<Utc>) -> Result<bool, DbError> {
    let inserted = sqlx::query!(
        "insert into llm_alert (month) values ($1) on conflict (month) do nothing",
        month_of(at)
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_roll_over() {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap_or_default();
        assert_eq!(
            month_of(at("2026-10-05T23:00:00Z")).to_string(),
            "2026-10-01"
        );
        assert_eq!(
            next_month(at("2026-10-05T23:00:00Z")),
            at("2026-11-01T00:00:00Z")
        );
        assert_eq!(
            next_month(at("2026-12-31T12:00:00Z")),
            at("2027-01-01T00:00:00Z")
        );
    }
}
