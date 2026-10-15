//! Large documents (Spec 08): chunked OCR jobs, resuming after a killed worker, the estimate,
//! parts, and the per-range check with `reocr`, all on fake tools that make pages up.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::LocalObjects;
use wt_db::job::JobKind;
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::large::{self, AssembleJob, ChunkJob};
use wt_ingest::texts::pdf::layout::{Block, Line, Origin, Page, Word};
use wt_ingest::texts::pdf::tools::{Crop, PdfTools};
use wt_ingest::texts::{self, PdfEnv, TextError};
use wt_ml::FakeEmbedder;

/// Words the fake book is made of, so the word list knows them all.
const WORDS: &str = "leaf tells how the student sat quietly in hall and listened to rain falling";

fn sentence(n: u32) -> String {
    format!(
        "Leaf {n} tells how the student sat quietly in the hall and listened to the rain falling."
    )
}

/// A book of scanned pages, each one paragraph. Pages in `weak` read badly below `good_dpi`.
struct Book {
    pages: u32,
    weak: BTreeSet<u32>,
    good_dpi: u32,
    /// Pages that fail once, as a worker killed mid-chunk would.
    fail_once: Mutex<BTreeSet<u32>>,
    ocr_calls: AtomicUsize,
    ocred: Mutex<Vec<u32>>,
    rendered_at: Mutex<BTreeMap<PathBuf, u32>>,
    most_images_in_scratch: AtomicUsize,
}

impl Book {
    fn new(pages: u32) -> Self {
        Self {
            pages,
            weak: BTreeSet::new(),
            good_dpi: 300,
            fail_once: Mutex::new(BTreeSet::new()),
            ocr_calls: AtomicUsize::new(0),
            ocred: Mutex::new(Vec::new()),
            rendered_at: Mutex::new(BTreeMap::new()),
            most_images_in_scratch: AtomicUsize::new(0),
        }
    }

    fn page(&self, n: u32, dpi: u32) -> Page {
        let conf = if self.weak.contains(&n) && dpi < self.good_dpi {
            0.55
        } else {
            0.95
        };
        let words = sentence(n)
            .split(' ')
            .enumerate()
            .map(|(i, w)| Word {
                text: w.into(),
                conf,
                x0: 300.0 + 120.0 * (i % 10) as f32,
                y0: 600.0 + 60.0 * (i / 10) as f32,
                x1: 400.0 + 120.0 * (i % 10) as f32,
                y1: 640.0 + 60.0 * (i / 10) as f32,
            })
            .collect::<Vec<_>>();
        Page {
            number: n,
            width: 2550.0,
            height: 3300.0,
            blocks: vec![Block {
                lines: words
                    .chunks(10)
                    .map(|c| Line { words: c.to_vec() })
                    .collect(),
            }],
            origin: Origin::Ocr,
        }
    }
}

impl PdfTools for Book {
    fn page_count(&self, _: &Path) -> Result<u32, TextError> {
        Ok(self.pages)
    }

    fn text_layer(&self, _: &Path, page: u32) -> Result<Page, TextError> {
        Ok(Page {
            number: page,
            width: 612.0,
            height: 792.0,
            blocks: Vec::new(),
            origin: Origin::TextLayer,
        })
    }

    fn render(
        &self,
        _: &Path,
        _: u32,
        dpi: u32,
        _: Option<Crop>,
        out: &Path,
    ) -> Result<(), TextError> {
        let images = std::fs::read_dir(out.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .count();
        self.most_images_in_scratch
            .fetch_max(images + 1, Ordering::SeqCst);
        std::fs::write(out, b"PNG").unwrap();
        self.rendered_at
            .lock()
            .unwrap()
            .insert(out.to_path_buf(), dpi);
        Ok(())
    }

    fn ocr(&self, png: &Path, _: &[String], page: u32) -> Result<Page, TextError> {
        if self.fail_once.lock().unwrap().remove(&page) {
            return Err(TextError::Tool(format!("killed while reading page {page}")));
        }
        self.ocr_calls.fetch_add(1, Ordering::SeqCst);
        self.ocred.lock().unwrap().push(page);
        let dpi = self
            .rendered_at
            .lock()
            .unwrap()
            .get(png)
            .copied()
            .unwrap_or(300);
        Ok(self.page(page, dpi))
    }

    fn engine(&self) -> Result<String, TextError> {
        Ok("fake tesseract 5".into())
    }
}

struct Fx {
    _db: TestDb,
    pool: PgPool,
    dir: PathBuf,
    objects: LocalObjects,
    evidence: Uuid,
}

async fn start() -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let dir = std::env::temp_dir().join(format!("wt-large-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("words.txt"), WORDS.replace(' ', "\n")).unwrap();
    let evidence = sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(), true)
         returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
        evidence,
    }
}

impl Fx {
    fn env<'a>(&self, tools: &'a dyn PdfTools) -> PdfEnv<'a> {
        PdfEnv {
            tools,
            scratch: self.dir.join("scratch"),
            word_list: self.dir.join("words.txt"),
        }
    }

    /// A manifest in its own folder, with `extra` appended to `[pdf]`.
    fn manifest(&self, folder: &str, title: &str, pdf: &str, rest: &str) -> PathBuf {
        let dir = self.dir.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("book.pdf"), format!("%PDF fixture {folder}")).unwrap();
        let text = format!(
            r#"role = "corpus"

[work]
title = "{title}"

[edition]
label = "Fixture 1900"
rights_holder = "public domain"
license_evidence_id = "{}"

[source]
files = ["book.pdf"]
format = "pdf"

[pdf]
{pdf}

[units]
scheme = "paragraph"
locator_prefix = "longbook"
label_template = "{{work}}, paragraph {{n}}"
{rest}
"#,
            self.evidence
        );
        let path = dir.join("manifest.toml");
        std::fs::write(&path, text).unwrap();
        path
    }
}

fn allowed() -> Vec<String> {
    LicenseSettings::default().allowed_license_codes
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One worker: claims and runs text jobs until none is due.
async fn work(pool: PgPool, objects: LocalObjects, tools: &Book, dir: PathBuf, name: String) {
    let mut conn = pool.acquire().await.unwrap();
    let embedder = FakeEmbedder::new("fake");
    let env = PdfEnv {
        tools,
        scratch: dir.join(format!("scratch-{name}")),
        word_list: dir.join("words.txt"),
    };
    while let Some(job) = wt_db::job::claim(
        &mut conn,
        &[JobKind::OcrChunk, JobKind::TextAssemble],
        &name,
    )
    .await
    .unwrap()
    {
        let result = match job.kind {
            JobKind::OcrChunk => {
                let input: ChunkJob = serde_json::from_value(job.payload.clone()).unwrap();
                large::run_chunk(&mut conn, &objects, tools, &env.scratch, &input)
                    .await
                    .map(|_| ())
            }
            _ => {
                let input: AssembleJob = serde_json::from_value(job.payload.clone()).unwrap();
                large::run_assemble(&mut conn, &objects, &embedder, &env, &allowed(), &input)
                    .await
                    .map(|_| ())
            }
        };
        match result {
            Ok(()) => wt_db::job::succeed(&mut conn, &job).await.unwrap(),
            Err(e) => {
                wt_db::job::fail(&mut conn, &job, &e.to_string())
                    .await
                    .unwrap();
            }
        }
    }
}

async fn unit_texts(pool: &PgPool, title: &str) -> Vec<String> {
    sqlx::query_scalar(
        "select u.text from text_unit u join text_edition e on e.id = u.edition_id
         join work w on w.id = e.work_id where w.title = $1 order by u.ordinal",
    )
    .bind(title)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_300_page_pdf_runs_as_6_chunks_on_3_workers_and_matches_a_single_pass() {
    let fx = start().await;
    let book = Book::new(300);
    let chunked = fx.manifest("chunked", "Long Fixture Book", "chunk_pages = 50", "");
    let mut conn = fx.pool.acquire().await.unwrap();
    let embedder = FakeEmbedder::new("fake");

    // A dry run reads nothing and says what --apply would queue.
    let dry = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &chunked,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: false,
            force: false,
            pdf_env: Some(&fx.env(&book)),
        },
    )
    .await
    .unwrap();
    assert_eq!((dry.units, book.ocr_calls.load(Ordering::SeqCst)), (0, 0));
    assert!(
        dry.warnings[0].contains("300 pages in 6 chunks"),
        "{:?}",
        dry.warnings
    );

    let queued = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &chunked,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&fx.env(&book)),
        },
    )
    .await
    .unwrap()
    .queued
    .unwrap();
    assert_eq!(
        (queued.chunks, queued.newly_queued, queued.pages),
        (6, 6, 300)
    );
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from job where kind = 'ocr_chunk'"
        )
        .await,
        6
    );
    assert_eq!(
        count(&fx.pool, "select count(*) from text_edition").await,
        0
    );

    let worker = |i: u32| {
        work(
            fx.pool.clone(),
            LocalObjects::new(fx.dir.join("objects")),
            &book,
            fx.dir.clone(),
            format!("w{i}"),
        )
    };
    tokio::join!(worker(0), worker(1), worker(2));
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from job where state = 'succeeded'"
        )
        .await,
        7
    );
    assert_eq!(
        book.ocr_calls.load(Ordering::SeqCst),
        300,
        "each page read once"
    );
    assert_eq!(
        book.most_images_in_scratch.load(Ordering::SeqCst),
        1,
        "one page image at a time"
    );
    let assembled = unit_texts(&fx.pool, "Long Fixture Book").await;
    assert_eq!(assembled.len(), 300);

    // The same book in one pass.
    let single_book = Book::new(300);
    let single = fx.manifest(
        "single",
        "Long Fixture Book, one pass",
        "chunk_pages = 1000",
        "",
    );
    let report = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &single,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&fx.env(&single_book)),
        },
    )
    .await
    .unwrap();
    assert!(report.applied && report.queued.is_none());
    assert_eq!(
        unit_texts(&fx.pool, "Long Fixture Book, one pass").await,
        assembled
    );
}

#[tokio::test]
async fn a_worker_killed_mid_chunk_leaves_only_the_unfinished_pages_to_read() {
    let fx = start().await;
    let book = Book::new(150);
    book.fail_once.lock().unwrap().insert(120);
    let manifest = fx.manifest("killed", "Interrupted Fixture Book", "chunk_pages = 50", "");
    let mut conn = fx.pool.acquire().await.unwrap();
    texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&fx.env(&book)),
        },
    )
    .await
    .unwrap();
    work(
        fx.pool.clone(),
        LocalObjects::new(fx.dir.join("objects")),
        &book,
        fx.dir.clone(),
        "w".into(),
    )
    .await;
    // The chunk with page 120 failed after reading 101 to 119; the others finished.
    assert_eq!(book.ocr_calls.load(Ordering::SeqCst), 119);
    assert_eq!(
        count(&fx.pool, "select count(*) from text_edition").await,
        0
    );

    // The restart: the retry is due now, and reads only 120 to 150.
    sqlx::query("update job set run_at = now() where state = 'queued'")
        .execute(&fx.pool)
        .await
        .unwrap();
    book.ocred.lock().unwrap().clear();
    work(
        fx.pool.clone(),
        LocalObjects::new(fx.dir.join("objects")),
        &book,
        fx.dir.clone(),
        "w".into(),
    )
    .await;
    let reread = book.ocred.lock().unwrap().clone();
    assert_eq!(reread, (120..=150).collect::<Vec<_>>());
    assert_eq!(
        unit_texts(&fx.pool, "Interrupted Fixture Book").await.len(),
        150
    );
}

#[tokio::test]
async fn the_estimate_queues_nothing_and_prints_every_figure() {
    let fx = start().await;
    let book = Book::new(300);
    let manifest = fx.manifest(
        "estimate",
        "Measured Fixture Book",
        "chunk_pages = 50\nskip_pages = [\"1-10\"]",
        "",
    );
    let e = large::estimate(&book, &manifest, &fx.dir.join("scratch"), 4).unwrap();
    assert_eq!(
        (e.total_pages, e.ocr_pages, e.sampled_pages, e.chunks),
        (290, 290, 3, 6)
    );
    assert!(e.pages_per_minute > 0.0 && e.projected_minutes > 0.0);
    assert_eq!((e.concurrency, e.vision_calls_worst_case), (4, 0));
    assert_eq!(count(&fx.pool, "select count(*) from job").await, 0);
    let json = serde_json::to_value(&e).unwrap();
    for key in [
        "total_pages",
        "ocr_pages",
        "pages_per_minute",
        "projected_minutes",
        "vision_calls_worst_case",
    ] {
        assert!(json.get(key).is_some(), "{key}");
    }
}

#[tokio::test]
async fn two_parts_make_two_editions_with_their_own_locators_and_proofreads() {
    let fx = start().await;
    let book = Book::new(20);
    let manifest = fx.manifest(
        "parts",
        "Bound Volume",
        "",
        r#"
[[parts]]
title = "First Fixture Text"
pages = "1-8"
locator_prefix = "first"

[[parts]]
title = "Second Fixture Text"
label = "Fixture 1901"
pages = "9-20"
locator_prefix = "second"
"#,
    );
    let mut conn = fx.pool.acquire().await.unwrap();
    let report = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&fx.env(&book)),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.parts.len(), 2);
    assert_eq!((report.parts[0].units, report.parts[1].units), (8, 12));
    let editions: Vec<(String, String, String)> = sqlx::query_as(
        "select w.title, e.edition_label, min(u.locator) from text_edition e
         join work w on w.id = e.work_id join text_unit u on u.edition_id = e.id
         group by 1, 2 order by 1",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        editions,
        vec![
            (
                "First Fixture Text".into(),
                "Fixture 1900".into(),
                "first:1".into()
            ),
            (
                "Second Fixture Text".into(),
                "Fixture 1901".into(),
                "second:1".into()
            ),
        ]
    );
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from work_locator where locator like 'first:%'"
        )
        .await,
        8
    );
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from work_locator where locator like 'second:%'"
        )
        .await,
        12
    );
    assert_eq!(
        book.ocr_calls.load(Ordering::SeqCst),
        20,
        "the shared file is read once"
    );

    // Each edition needs its own proofread.
    let first: Uuid = sqlx::query_scalar(
        "select e.id from text_edition e join work w on w.id = e.work_id where w.title = 'First Fixture Text'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into proof_check (edition_id, sample_size, errors, reviewer, passed, units_hash)
         select id, 8, 0, 'Reviewer', true, units_hash from text_edition where id = $1",
    )
    .bind(first)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update text_edition set state = 'ready'")
        .execute(&fx.pool)
        .await
        .unwrap();
    let served: Vec<String> =
        sqlx::query_scalar("select distinct work_title from servable_text_unit")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(served, vec!["First Fixture Text"]);
}

#[tokio::test]
async fn a_degraded_range_is_read_again_named_and_passes_after_reocr() {
    let fx = start().await;
    let mut book = Book::new(40);
    // Pages 13 to 16, a tenth of the book, read well only at 600 dpi.
    book.weak = (13..=16).collect();
    book.good_dpi = 600;
    let manifest = fx.manifest(
        "degraded",
        "Worn Fixture Book",
        "max_suspect_share = 0.5",
        "",
    );
    let mut conn = fx.pool.acquire().await.unwrap();
    let embedder = FakeEmbedder::new("fake");
    let report = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&fx.env(&book)),
        },
    )
    .await
    .unwrap();
    let pdf = report.pdf.unwrap();
    assert_eq!(pdf.low_ranges.len(), 1, "{:?}", pdf.low_ranges);
    assert_eq!((pdf.low_ranges[0].first, pdf.low_ranges[0].last), (13, 16));
    // The range was read a second time at 400 dpi before it was named.
    assert_eq!(book.ocr_calls.load(Ordering::SeqCst), 44);

    // The proof sheet spot-checks every unit of it, in whichever group drew it.
    let edition: Uuid = sqlx::query_scalar("select id from text_edition")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let (_, html) = texts::proofread::sheet(
        &mut conn,
        &fx.objects,
        &book,
        edition,
        &fx.dir.join("scratch"),
    )
    .await
    .unwrap();
    for n in 13..=16 {
        assert!(
            html.contains(&format!("paragraph {n}, page {n}")),
            "paragraph {n}"
        );
    }

    // reocr at 600 dpi reads it well, and the range is no longer named.
    let again = large::reocr(
        &mut conn,
        &fx.objects,
        &embedder,
        &fx.env(&book),
        &allowed(),
        &large::ReocrRequest {
            edition,
            pages: (13, 16),
            dpi: 600,
            by: "owner",
        },
    )
    .await
    .unwrap();
    assert!(again.applied);
    assert!(again.pdf.unwrap().low_ranges.is_empty());
    let (_, html) = texts::proofread::sheet(
        &mut conn,
        &fx.objects,
        &book,
        edition,
        &fx.dir.join("scratch"),
    )
    .await
    .unwrap();
    assert_eq!(html.matches("data-group=\"range\"").count(), 0);
}
