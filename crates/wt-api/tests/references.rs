//! Spec 05, Answer assembly, rules 2 and 5 as amended on October 12, 2026, on synthetic talks in
//! a real Postgres: a machine-transcribed YouTube talk, an EngageWisdom talk whose transcript is
//! the source's own text, a cited work, and the References list that closes the page.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use sqlx::types::Uuid;
use tower::ServiceExt;
use wt_api::{AppState, Site, router, session};
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TalkId};
use wt_db::answer::{NewAnswer, Payload};
use wt_db::test_support::TestDb;
use wt_db::transcript::{NewSegment, NewTranscript};
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"

[[sangha]]
name = "South Fixture Zen Center"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"

[[teacher]]
display_name = "Ben Fixture"
sangha = "South Fixture Zen Center"
"#;

const ACTOR: &str = "test";
const ADA: &str = "Today we read the Fixture Sutra together and sit with its words in the quiet \
                   hall of practice.";
const BEN: &str = "The moon in the water is not wet, and the water is not broken by the moon at \
                   all tonight.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    ada_talk: TalkId,
    ben_talk: TalkId,
    ada: PassageId,
    ben: PassageId,
}

/// A ready talk of `teacher` with one segment and one passage, its transcript stored the way
/// the `audio` job stores it.
async fn talk(
    pool: &PgPool,
    teacher: &str,
    (platform, external_id, url): (&str, &str, &str),
    origin: &str,
    text: &str,
) -> (TalkId, PassageId) {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ($1::text::source_platform, $2, $3, 'Fixture') returning id",
    )
    .bind(platform)
    .bind(external_id)
    .bind(url)
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, talk_date, teacher_id, sangha_id,
                           rights_holder, state)
         select $1, 'On ' || $2, date '1970-03-03', te.id, te.sangha_id, te.display_name,
                'awaiting_consent'
         from teacher te where te.display_name = $2 returning id",
    )
    .bind(video)
    .bind(teacher)
    .fetch_one(pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        pool,
        teacher,
        ConsentStatus::Granted,
        Some("f"),
        None,
        ACTOR,
    )
    .await
    .unwrap();
    sqlx::query("update talk set state = 'ready', content_hash = 'h' where id = $1")
        .bind(talk)
        .execute(pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let transcript = NewTranscript {
        version: 1,
        origin,
        key: "k",
        content_hash: "h",
        word_count: 20,
    };
    let segment = NewSegment {
        kind: "talk",
        start_ms: 120_000,
        end_ms: 140_000,
        speaker_label: "S0",
        text,
    };
    wt_db::transcript::store(&mut conn, TalkId::from(talk), &transcript, &[segment])
        .await
        .unwrap();
    let passage: Uuid = sqlx::query_scalar(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                              text, word_count, content_hash, embedding, embedding_model)
         select $1, s.id, 'talk', 0, 125000, 140000, $2, 20, 'h', $3::text::vector, 'fake-embed'
         from segment s where s.talk_id = $1 returning id",
    )
    .bind(talk)
    .bind(text)
    .bind(wt_retrieve::search::vector_literal(
        &wt_ml::FakeEmbedder::vector(text),
    ))
    .fetch_one(pool)
    .await
    .unwrap();
    (TalkId::from(talk), PassageId::from(passage))
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    let (ada_talk, ada) = talk(
        &pool,
        "Ada Fixture",
        ("youtube", "vid1", "https://example.invalid/vid1"),
        "asr",
        ADA,
    )
    .await;
    let (ben_talk, ben) = talk(
        &pool,
        "Ben Fixture",
        (
            "engagewisdom",
            "BZ-00001",
            "https://ew.example.invalid/bz/1",
        ),
        "aligned",
        BEN,
    )
    .await;
    // Ada's passage cites a work at a locator, with the citing words' character span.
    let at = i32::try_from(ADA.find("Fixture Sutra").unwrap()).unwrap();
    sqlx::query(
        "with w as (insert into work (title, author, translator, link_out_url)
                    values ('Fixture Sutra', 'Zed Old Fixture', 'A. Translator',
                            'https://texts.example.invalid/sutra') returning id),
              l as (insert into work_locator (work_id, locator, label)
                    select id, 'fixture:5', 'chapter 5' from w returning id)
         insert into citation (passage_id, talk_id, span_start, span_end, raw_text,
                               work_locator_id, confidence, method)
         select $1, $2, $3, $3 + 13, 'Fixture Sutra', l.id, 'probable', 'hand_list' from l",
    )
    .bind(ada.as_uuid())
    .bind(ada_talk.as_uuid())
    .bind(at)
    .execute(&pool)
    .await
    .unwrap();
    Fx {
        _db: db,
        pool,
        ada_talk,
        ben_talk,
        ada,
        ben,
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// An answer showing Ada's passage, then Ben's, rendered for the tester who asked it.
async fn page(fx: &Fx) -> String {
    let app = router(AppState {
        pool: fx.pool.clone(),
        site: Arc::new(Site::with_fakes("fake-embed")),
    });
    let token = session::new_token().unwrap();
    let tester = wt_db::tester::invite(&fx.pool, "owner", &session::token_hash(&token))
        .await
        .unwrap();
    let (_, headers, _) = send(
        &app,
        Request::get(format!("/join/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let answer = wt_db::answer::persist(
        &fx.pool,
        &NewAnswer {
            tester,
            query_text: "What is the moon in the water?",
            payload: Payload {
                mode: "standard".into(),
                named_teacher_id: None,
                notice: None,
                notice_count: None,
                themes: Vec::new(),
                passage_ids: vec![fx.ada, fx.ben],
                rerank_degraded: false,
                curated: false,
                spans: Vec::new(),
            },
            teacher_count: 2,
            sangha_count: 2,
            thin_coverage: false,
            latency_ms: 1,
            themes_dropped: 0,
            sensitive_category: "none",
            relevance: None,
            triage_degraded: false,
            privacy_mode: "share",
            text_expires_at: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("update answer set created_at = timestamptz '2026-10-12 18:00:00+00'")
        .execute(&fx.pool)
        .await
        .unwrap();
    let (status, _, body) = send(
        &app,
        Request::get(format!("/a/{answer}"))
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The References list's markup, from its opening tag to its close.
fn references(page: &str) -> &str {
    let from = page.find("<details class=\"references\"").unwrap();
    let to = from + page[from..].find("</details>").unwrap();
    &page[from..to]
}

#[tokio::test]
async fn the_audio_job_records_whether_the_transcript_is_the_sources_own() {
    let fx = start().await;
    let flag = |t: TalkId| {
        sqlx::query_scalar::<_, bool>("select transcript_from_source from talk where id = $1")
            .bind(t.as_uuid())
            .fetch_one(&fx.pool)
    };
    assert!(!flag(fx.ada_talk).await.unwrap(), "asr");
    assert!(flag(fx.ben_talk).await.unwrap(), "aligned");
    // Both teachers are at `full`; the source-made talk serves at `excerpts` all the same.
    let level = |p: PassageId| {
        sqlx::query_scalar::<_, String>(
            "select display_level from servable_passage where passage_id = $1",
        )
        .bind(p.as_uuid())
        .fetch_one(&fx.pool)
    };
    assert_eq!(level(fx.ada).await.unwrap(), "full");
    assert_eq!(level(fx.ben).await.unwrap(), "excerpts");
    // A later machine transcript of the same talk clears the flag.
    let mut conn = fx.pool.acquire().await.unwrap();
    let transcript = NewTranscript {
        version: 2,
        origin: "asr",
        key: "k2",
        content_hash: "h2",
        word_count: 20,
    };
    wt_db::transcript::store(&mut conn, fx.ben_talk, &transcript, &[])
        .await
        .unwrap();
    assert!(!flag(fx.ben_talk).await.unwrap());
}

#[tokio::test]
async fn source_links_follow_who_made_the_transcript() {
    let fx = start().await;
    let page = page(&fx).await;
    // Ada's machine transcript: the recording at the second, less the lead-in, and this site's
    // transcript with the machine label.
    assert!(page.contains("watch?v=vid1&#38;t=123s"), "{page}");
    assert!(page.contains("Listen at 2:05 (opens the source in a new tab)"));
    assert!(page.contains(&format!(
        "href=\"/talk/{}/transcript#p0\" aria-label=\"Read our transcript (made by machine, may \
         contain errors)\">Read</a>",
        fx.ada_talk
    )));
    // Ben's source transcript: the talk page with a text fragment, no player time, and no link
    // to a transcript on this site.
    assert!(page.contains(
        "href=\"https://ew.example.invalid/bz/1#:~:text=The%20moon%20in%20the,moon%20at%20all%20tonight.\""
    ));
    assert!(page.contains(
        "Read the talk&#39;s transcript at South Fixture Zen Center (opens their page in a new \
         tab)"
    ));
    assert!(page.contains("<li class=\"muted small\">starts at 2:05</li>"));
    assert!(!page.contains(&format!("/talk/{}/transcript", fx.ben_talk)));
}

#[tokio::test]
async fn references_close_the_page_in_mla_order_with_links_to_each_entry() {
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let change = wt_db::registry::set_citation(
        &mut conn,
        "Ben Fixture",
        Some("Fixture, Ben"),
        Some("Abe Fixture"),
        ACTOR,
        "test",
    )
    .await
    .unwrap();
    assert!(change.changed);
    let again = wt_db::registry::set_citation(
        &mut conn,
        "ben fixture",
        Some("Fixture, Ben"),
        Some("Abe Fixture"),
        ACTOR,
        "test",
    )
    .await
    .unwrap();
    assert!(!again.changed, "rerunning changes nothing");
    let logged: i64 = sqlx::query_scalar(
        "select count(*) from correction_log
         where entity = 'teacher' and field in ('citation_name', 'citation_sort')",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, 2);
    drop(conn);

    let page = page(&fx).await;
    let refs = references(&page);
    assert!(
        refs.starts_with("<details class=\"references\" id=\"references\">"),
        "closed"
    );
    assert!(refs.contains("<summary><h2>References (3)</h2></summary>"));
    let expected = [
        // Ben sorts by his citation sort key, ahead of Ada, and is entered by citation name.
        "<li id=\"ref-1\">Fixture, Ben. &#34;On Ben Fixture.&#34; Dharma talk. <i>South Fixture \
         Zen Center</i>, 3 Mar. 1970, ew.example.invalid/bz/1. Accessed 12 Oct. 2026.</li>",
        // Ada has no citation name: her display name, uninverted, on YouTube.
        "<li id=\"ref-2\">Ada Fixture. &#34;On Ada Fixture.&#34; Dharma talk. <i>YouTube</i>, \
         North Fixture Zendo, 3 Mar. 1970, example.invalid/vid1. Accessed 12 Oct. 2026.</li>",
        "<li id=\"ref-3\">Zed Old Fixture. <i>Fixture Sutra</i>. Translated by A. Translator, \
         chapter 5. texts.example.invalid/sutra. <span class=\"muted\">(citation \
         probable)</span></li>",
    ];
    let mut last = 0;
    for entry in expected {
        let at = refs
            .find(entry)
            .unwrap_or_else(|| panic!("{entry}\n\n{refs}"));
        assert!(at >= last, "alphabetical by sort key");
        last = at;
    }
    // Each passage's metadata line links to its talk's entry, and the footnote marker after
    // the citing words to the work's.
    assert!(page.contains("<p class=\"meta\"><a href=\"#ref-2\">On Ada Fixture"));
    assert!(page.contains("<p class=\"meta\"><a href=\"#ref-1\">On Ben Fixture"));
    assert!(page.contains(
        "Today we read the Fixture Sutra<sup><a class=\"fn\" href=\"#ref-3\" \
         aria-label=\"Footnote 1, reference 3\">1</a></sup> together"
    ));
    assert!(page.contains("<script src=\"/static/references.js\" defer></script>"));
}
