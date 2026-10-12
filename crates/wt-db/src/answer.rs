//! Answers and what the answer, talk and transcript pages read (Specs 05 and 06). Passages are
//! read through `servable_passage` only, so a passage that stops serving disappears from every
//! page at once. An answer stores passage ids, never passage text.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::answer::StoredTheme;
use wt_core::ids::{AnswerId, PassageId, SanghaId, TalkId, TeacherId, TesterId};

use crate::DbError;

/// What `answer.payload` holds: mode, notice, counts and ordered passage ids. Takedown reads
/// `passage_ids` and `themes` too, so their shape is shared with it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Payload {
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// The named teacher in named mode.
    #[serde(default)]
    pub named_teacher_id: Option<TeacherId>,
    /// The notice key, such as `thin_coverage`, when one applies.
    #[serde(default)]
    pub notice: Option<String>,
    /// For `thin_coverage` and `named_few`: the count the notice states.
    #[serde(default)]
    pub notice_count: Option<u8>,
    /// Verified synthesis themes with the passage ids that support them.
    #[serde(default)]
    pub themes: Vec<StoredTheme>,
    /// Chosen passages, best first.
    pub passage_ids: Vec<PassageId>,
    /// The reranker was unavailable, so no relevance threshold applied.
    #[serde(default)]
    pub rerank_degraded: bool,
}

/// One ask to persist.
#[derive(Debug, Clone)]
pub struct NewAnswer<'a> {
    /// Who asked.
    pub tester: TesterId,
    /// The question as asked.
    pub query_text: &'a str,
    /// What the page renders.
    pub payload: Payload,
    /// Distinct teachers.
    pub teacher_count: i32,
    /// Distinct sanghas.
    pub sangha_count: i32,
    /// Fewer teachers than the target.
    pub thin_coverage: bool,
    /// End-to-end time.
    pub latency_ms: i32,
    /// Themes dropped by the filters or the verifier.
    pub themes_dropped: i32,
    /// `share` or `count_only` (Spec 06, Privacy modes).
    pub privacy_mode: &'a str,
    /// When the purge removes the text and the answer.
    pub text_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Spec 06, Logging, rule 1: the query log, the answer and its passages in one transaction, so
/// no answer is shown without its log row.
pub async fn persist(pool: &PgPool, a: &NewAnswer<'_>) -> Result<AnswerId, DbError> {
    let mut tx = pool.begin().await?;
    let passages = i32::try_from(a.payload.passage_ids.len()).unwrap_or(i32::MAX);
    let themes = i32::try_from(a.payload.themes.len()).unwrap_or(i32::MAX);
    let log = sqlx::query_scalar!(
        "insert into query_log (tester_id, query_text, mode, named_teacher_id, teacher_count,
                                sangha_count, passage_count, thin_coverage, latency_ms,
                                theme_count, themes_dropped, privacy_mode, text_expires_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) returning id",
        a.tester.as_uuid(),
        a.query_text,
        a.payload.mode,
        a.payload.named_teacher_id.map(|t| t.as_uuid()),
        a.teacher_count,
        a.sangha_count,
        passages,
        a.thin_coverage,
        a.latency_ms,
        themes,
        a.themes_dropped,
        a.privacy_mode,
        a.text_expires_at
    )
    .fetch_one(&mut *tx)
    .await?;
    let payload = serde_json::to_value(&a.payload).unwrap_or(serde_json::Value::Null);
    let answer = sqlx::query_scalar!(
        "insert into answer (query_log_id, payload) values ($1, $2) returning id",
        log,
        payload
    )
    .fetch_one(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = a.payload.passage_ids.iter().map(|p| p.as_uuid()).collect();
    sqlx::query!(
        "insert into answer_passage (answer_id, passage_id, rank)
         select $1, p, (r - 1)::int from unnest($2::uuid[]) with ordinality as u(p, r)",
        answer,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(AnswerId::from(answer))
}

/// A stored answer, as its owner sees it.
#[derive(Debug, Clone)]
pub struct Stored {
    /// The question as asked.
    pub query_text: String,
    /// The payload.
    pub payload: Payload,
    /// When it was asked.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The answer, if it exists and `tester` asked it. Anyone else gets `None`, which the page
/// turns into the same 404 as a missing answer.
pub async fn load(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
) -> Result<Option<Stored>, DbError> {
    let row = sqlx::query!(
        "select q.query_text, a.payload, a.created_at from answer a
         join query_log q on q.id = a.query_log_id
         where a.id = $1 and q.tester_id = $2",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        Some(Stored {
            query_text: r.query_text.unwrap_or_default(),
            payload: serde_json::from_value(r.payload).ok()?,
            created_at: r.created_at,
        })
    }))
}

/// A serving passage with what the page shows around it.
#[derive(Debug, Clone)]
pub struct Served {
    /// The passage.
    pub id: PassageId,
    /// Verbatim text.
    pub text: String,
    /// Its position in the talk, for the transcript anchor.
    pub ordinal: i32,
    /// Start and end in the talk.
    pub start_ms: i32,
    /// End in the talk.
    pub end_ms: i32,
    /// The talk.
    pub talk: TalkCard,
}

/// What every page shows about a talk.
#[derive(Debug, Clone)]
pub struct TalkCard {
    /// The talk.
    pub id: TalkId,
    /// Its title.
    pub title: String,
    /// The recording date.
    pub date: Option<NaiveDate>,
    /// Series and part, when it belongs to one.
    pub series: Option<String>,
    /// See `series`.
    pub part: Option<i32>,
    /// `youtube`, `vimeo`, `engagewisdom` and so on.
    pub platform: String,
    /// The platform's id for the video.
    pub external_id: String,
    /// The source page.
    pub source_url: String,
    /// The teacher.
    pub teacher_id: TeacherId,
    /// Their name.
    pub teacher_name: String,
    /// Where the talk was given.
    pub sangha_id: SanghaId,
    /// Its name.
    pub sangha_name: String,
    /// The sangha shows give links only when listed and its link is verified.
    pub sangha_listed: bool,
    /// Its donation page.
    pub donation_url: Option<String>,
    /// Whether a person checked it.
    pub donation_verified: bool,
    /// `speaker_consent`, `license` or `mandate`.
    pub rights_basis: Option<String>,
    /// The license or mandate's attribution line.
    pub display_notice: Option<String>,
    /// `full` or `excerpts`: the transcript page exists only at `full`.
    pub display_level: String,
}

macro_rules! card {
    ($r:expr) => {
        TalkCard {
            id: TalkId::from($r.talk_id),
            title: $r.talk_title,
            date: $r.talk_date,
            series: $r.series,
            part: $r.part,
            platform: $r.platform,
            external_id: $r.external_id,
            source_url: $r.source_url,
            teacher_id: TeacherId::from($r.teacher_id),
            teacher_name: $r.teacher_name,
            sangha_id: SanghaId::from($r.sangha_id),
            sangha_name: $r.sangha_name,
            sangha_listed: $r.listed,
            donation_url: $r.donation_url,
            donation_verified: $r.donation_verified,
            rights_basis: $r.rights_basis,
            display_notice: $r.display_notice,
            display_level: $r.display_level,
        }
    };
}

/// The passages that still serve, in the order given. A passage that stopped serving is
/// absent, and the page shows a withdrawn marker in its place.
pub async fn passages(pool: &PgPool, ids: &[PassageId]) -> Result<Vec<Served>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", ordinal as "ordinal!",
                  start_ms as "start_ms!", end_ms as "end_ms!", talk_id as "talk_id!",
                  talk_title as "talk_title!", talk_date, series, part,
                  platform::text as "platform!", external_id as "external_id!",
                  source_url as "source_url!", teacher_id as "teacher_id!",
                  teacher_name as "teacher_name!", sangha_id as "sangha_id!",
                  sangha_name as "sangha_name!", listed as "listed!", donation_url,
                  donation_verified as "donation_verified!", rights_basis::text as rights_basis,
                  display_notice, display_level as "display_level!"
           from servable_passage where passage_id = any($1)"#,
        &uuids
    )
    .fetch_all(pool)
    .await?;
    let mut out: Vec<Served> = rows
        .into_iter()
        .map(|r| Served {
            id: PassageId::from(r.passage_id),
            text: r.text,
            ordinal: r.ordinal,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            talk: card!(r),
        })
        .collect();
    out.sort_by_key(|s| ids.iter().position(|id| *id == s.id));
    Ok(out)
}

/// A serving talk's card and its passages in order. `None` when the talk does not serve.
pub async fn talk(pool: &PgPool, talk: TalkId) -> Result<Option<(TalkCard, Vec<Served>)>, DbError> {
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", ordinal as "ordinal!",
                  start_ms as "start_ms!", end_ms as "end_ms!", talk_id as "talk_id!",
                  talk_title as "talk_title!", talk_date, series, part,
                  platform::text as "platform!", external_id as "external_id!",
                  source_url as "source_url!", teacher_id as "teacher_id!",
                  teacher_name as "teacher_name!", sangha_id as "sangha_id!",
                  sangha_name as "sangha_name!", listed as "listed!", donation_url,
                  donation_verified as "donation_verified!", rights_basis::text as rights_basis,
                  display_notice, display_level as "display_level!"
           from servable_passage where talk_id = $1 order by ordinal"#,
        talk.as_uuid()
    )
    .fetch_all(pool)
    .await?;
    let passages: Vec<Served> = rows
        .into_iter()
        .map(|r| Served {
            id: PassageId::from(r.passage_id),
            text: r.text,
            ordinal: r.ordinal,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            talk: card!(r),
        })
        .collect();
    Ok(passages
        .first()
        .map(|p| p.talk.clone())
        .map(|card| (card, passages)))
}

/// Feedback kinds a tester can leave (Spec 06, Logging, rule 4).
pub const FEEDBACK_KINDS: [&str; 4] = ["good", "not_faithful", "not_relevant", "flag_query"];

/// Records feedback on an answer the tester asked; `flag_query` also flags the query through
/// `flag_query()`. `note` holds the passage id for per-passage feedback. Returns false when the
/// answer is not theirs or the kind is unknown.
pub async fn feedback(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    kind: &str,
    note: Option<&str>,
) -> Result<bool, DbError> {
    if !FEEDBACK_KINDS.contains(&kind) {
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    let log = sqlx::query_scalar!(
        "select q.id from answer a join query_log q on q.id = a.query_log_id
         where a.id = $1 and q.tester_id = $2",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(log) = log else {
        return Ok(false);
    };
    sqlx::query!(
        "insert into feedback (answer_id, tester_id, kind, note) values ($1, $2, $3, $4)",
        answer.as_uuid(),
        tester.as_uuid(),
        kind,
        note
    )
    .execute(&mut *tx)
    .await?;
    if kind == "flag_query" {
        // The function's result is not needed, only its effect.
        sqlx::query("select flag_query($1)")
            .bind(log)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Every alias and display name with its teacher, for named-teacher detection.
pub async fn aliases(pool: &PgPool) -> Result<Vec<(TeacherId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"select teacher_id as "teacher_id!", alias as "alias!" from teacher_alias
           union select id, display_name from teacher"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (TeacherId::from(r.teacher_id), r.alias))
        .collect())
}

/// Where a give click goes: the registry's page and its amount template, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveTarget {
    /// The giving page.
    pub url: String,
    /// The page with an amount placeholder.
    pub template: Option<String>,
}

/// Spec 06, Logging, rule 3: the donation link for a give click, from the registry row and
/// never from the request, logged as `link_click`. `None` when the answer is not the tester's
/// or the sangha is unlisted, unverified or has no link.
pub async fn give(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    sangha: SanghaId,
) -> Result<Option<GiveTarget>, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        r#"select s.donation_url as "url!", s.donation_url_template from sangha s
           where s.id = $3 and s.listed and s.donation_verified and s.donation_url is not null
             and exists (select 1 from answer a join query_log q on q.id = a.query_log_id
                         where a.id = $1 and q.tester_id = $2)"#,
        answer.as_uuid(),
        tester.as_uuid(),
        sangha.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    if row.is_some() {
        sqlx::query!(
            "insert into dana_event (answer_id, sangha_id, kind) values ($1, $2, 'link_click')",
            answer.as_uuid(),
            sangha.as_uuid()
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(row.map(|r| GiveTarget {
        url: r.url,
        template: r.donation_url_template,
    }))
}

/// A teacher's display name, for the named-teacher notices.
pub async fn teacher_name(pool: &PgPool, teacher: TeacherId) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "select display_name from teacher where id = $1",
        teacher.as_uuid()
    )
    .fetch_optional(pool)
    .await?)
}

/// A cited sangha's giving details for the dana panel, from the registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanaSangha {
    /// The sangha.
    pub id: SanghaId,
    /// Its name.
    pub name: String,
    /// Its giving page.
    pub donation_url: Option<String>,
    /// The page with an amount placeholder.
    pub donation_url_template: Option<String>,
    /// A person checked the page.
    pub donation_verified: bool,
    /// False when the sangha asked for attribution only.
    pub listed: bool,
    /// Why the link is a contact page, missing or unverified.
    pub donation_note: Option<String>,
}

/// The registry rows of the given sanghas, by name.
pub async fn dana_sanghas(pool: &PgPool, ids: &[SanghaId]) -> Result<Vec<DanaSangha>, DbError> {
    let uuids: Vec<Uuid> = ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query!(
        "select id, name, donation_url, donation_url_template, donation_verified, listed,
                donation_note
         from sangha where id = any($1) order by name",
        &uuids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DanaSangha {
            id: SanghaId::from(r.id),
            name: r.name,
            donation_url: r.donation_url,
            donation_url_template: r.donation_url_template,
            donation_verified: r.donation_verified,
            listed: r.listed,
            donation_note: r.donation_note,
        })
        .collect())
}

/// What the dana panel logs (Spec 06, Logging, rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanaEvent {
    /// The donor started using the panel.
    PanelOpen,
    /// A split was shown, with the gift total entered.
    SplitComputed {
        /// The gift total in cents.
        total_cents: i32,
    },
}

/// Logs a panel event on an answer the tester asked. Returns false, logging nothing, when the
/// answer is not theirs.
pub async fn dana_event(
    pool: &PgPool,
    answer: AnswerId,
    tester: TesterId,
    event: DanaEvent,
) -> Result<bool, DbError> {
    let (kind, total) = match event {
        DanaEvent::PanelOpen => ("panel_open", None),
        DanaEvent::SplitComputed { total_cents } => ("split_computed", Some(total_cents)),
    };
    let done = sqlx::query!(
        "insert into dana_event (answer_id, kind, total_cents)
         select a.id, $3, $4 from answer a join query_log q on q.id = a.query_log_id
         where a.id = $1 and q.tester_id = $2",
        answer.as_uuid(),
        tester.as_uuid(),
        kind,
        total
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The site line's give click: logged as `link_click` with no sangha when the answer is the
/// tester's. Returns whether it was logged.
pub async fn give_site(pool: &PgPool, answer: AnswerId, tester: TesterId) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "insert into dana_event (answer_id, sangha_id, kind)
         select a.id, null, 'link_click' from answer a join query_log q on q.id = a.query_log_id
         where a.id = $1 and q.tester_id = $2",
        answer.as_uuid(),
        tester.as_uuid()
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}
