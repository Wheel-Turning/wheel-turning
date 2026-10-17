//! PDFs and OCR (Spec 08): the import with fake tools end to end through the proofread gate,
//! and the real poppler and Tesseract on synthetic PDFs. The real-tool tests skip when the
//! tools are missing, unless `WT_REQUIRE_OCR_TOOLS` is set, as CI sets it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "pdf/fixture.rs"]
mod fixture;

use std::path::{Path, PathBuf};

use regex::Regex;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::LocalObjects;
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::pdf::layout::{Block, Line, Origin, Page, Word};
use wt_ingest::texts::pdf::proof::{Answer, Group, ProofFile};
use wt_ingest::texts::pdf::tools::{Crop, PdfTools, Poppler};
use wt_ingest::texts::pdf::{self, OcrMode, PdfSettings};
use wt_ingest::texts::proofread::{self, Thresholds};
use wt_ingest::texts::{self, PdfEnv, TextError};
use wt_ml::FakeEmbedder;

use fixture::{PageSpec, line};

// ---------------------------------------------------------------------------------------------
// Fake tools

struct FakeTools {
    pages: Vec<Page>,
}

impl PdfTools for FakeTools {
    fn page_count(&self, _: &Path) -> Result<u32, TextError> {
        Ok(u32::try_from(self.pages.len()).unwrap())
    }

    fn text_layer(&self, _: &Path, page: u32) -> Result<Page, TextError> {
        let p = &self.pages[page as usize - 1];
        Ok(if p.origin == Origin::TextLayer {
            p.clone()
        } else {
            Page {
                blocks: Vec::new(),
                origin: Origin::TextLayer,
                ..p.clone()
            }
        })
    }

    fn render(
        &self,
        _: &Path,
        _: u32,
        _: u32,
        _: Option<Crop>,
        out: &Path,
    ) -> Result<(), TextError> {
        std::fs::write(out, b"PNGDATA").unwrap();
        Ok(())
    }

    fn ocr(&self, png: &Path, _: &[String], page: u32) -> Result<Page, TextError> {
        assert!(
            png.exists(),
            "the page image is in scratch while it is read"
        );
        Ok(self.pages[page as usize - 1].clone())
    }

    fn engine(&self) -> Result<String, TextError> {
        Ok("fake tesseract 5".into())
    }
}

fn block(lines: &[&str], top: f32, size: f32, conf: f32) -> Block {
    Block {
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let y = top + i as f32 * size * 1.3;
                Line {
                    words: text
                        .split(' ')
                        .enumerate()
                        .map(|(j, w)| Word {
                            text: w.into(),
                            conf,
                            x0: 72.0 + 40.0 * j as f32,
                            y0: y,
                            x1: 108.0 + 40.0 * j as f32,
                            y1: y + size,
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

fn page(number: u32, origin: Origin, mut blocks: Vec<Block>) -> Page {
    blocks.insert(0, block(&["FIXTURE SCAN BOOK"], 30.0, 9.0, 0.97));
    blocks.push(block(&[&number.to_string()], 760.0, 10.0, 0.97));
    Page {
        number,
        width: 612.0,
        height: 792.0,
        blocks,
        origin,
    }
}

/// Three pages: a text layer with a heading, hyphenation and a footnote, then two scans, one
/// paragraph of which is read badly.
fn book() -> Vec<Page> {
    vec![
        page(
            1,
            Origin::TextLayer,
            vec![
                block(&["First Chapter"], 90.0, 18.0, 1.0),
                block(
                    &[
                        "The student came to the hall before dawn and sat facing the",
                        "white wall while the bell sounded three times in the quiet.",
                    ],
                    150.0,
                    12.0,
                    1.0,
                ),
                block(
                    &[
                        "Nothing was asked of her except to breathe and to con-",
                        "tinue noticing the breath as it came and went like a guest.",
                    ],
                    250.0,
                    12.0,
                    1.0,
                ),
                block(
                    &["1 This footnote is set in small type at the foot."],
                    700.0,
                    8.0,
                    1.0,
                ),
            ],
        ),
        page(
            2,
            Origin::Ocr,
            vec![
                block(
                    &[
                        "When the talk ended the students bowed and walked slowly back",
                        "to the garden where the morning light was falling on stones.",
                    ],
                    150.0,
                    12.0,
                    0.95,
                ),
                block(
                    &[
                        "Later the teacher spoke about the moon in a drop of dew and",
                        "how the whole sky rests in a small bead of water on grass.",
                    ],
                    300.0,
                    12.0,
                    0.93,
                ),
            ],
        ),
        page(
            3,
            Origin::Ocr,
            vec![
                block(
                    &[
                        "In the evening they sat again and the hall was very still,",
                        "with only the sound of rain moving across the high roof.",
                    ],
                    150.0,
                    12.0,
                    0.94,
                ),
                block(
                    &["Tlie bnll rnng aud tlie dnor clnsed ns tlie nigbt cnme dnwn."],
                    300.0,
                    12.0,
                    0.52,
                ),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// Database fixture

struct Fx {
    _db: TestDb,
    pool: PgPool,
    dir: PathBuf,
    objects: LocalObjects,
}

async fn start() -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let dir = std::env::temp_dir().join(format!("wt-pdf-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
    }
}

async fn evidence(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(), true)
         returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn write_manifest(dir: &Path, evidence: Uuid, max_suspect_share: f32) -> PathBuf {
    std::fs::write(dir.join("book.pdf"), b"%PDF-1.4 fixture").unwrap();
    let manifest = format!(
        r#"role = "corpus"

[work]
title = "Fixture Scan Book"
author = "Fixture Author"

[edition]
label = "Fixture 1888"
rights_holder = "public domain"
license_evidence_id = "{evidence}"

[source]
files = ["book.pdf"]
format = "pdf"

[pdf]
max_suspect_share = {max_suspect_share}

[units]
scheme = "paragraph"
locator_prefix = "scanbook"
label_template = "{{work}}, paragraph {{n}}"
"#
    );
    let path = dir.join("manifest.toml");
    std::fs::write(&path, manifest).unwrap();
    path
}

/// A word list of every word in the book except the misread ones.
fn word_list(dir: &Path) -> PathBuf {
    let mut words: Vec<String> = book()
        .iter()
        .flat_map(|p| p.words().map(|w| w.text.to_lowercase()).collect::<Vec<_>>())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
        .collect();
    words.extend(["continue".to_owned()]);
    words.retain(|w| {
        ![
            "tlie", "bnll", "rnng", "aud", "dnor", "clnsed", "ns", "nigbt", "cnme", "dnwn",
        ]
        .contains(&w.as_str())
    });
    let path = dir.join("words.txt");
    std::fs::write(&path, words.join("\n")).unwrap();
    path
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

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

/// Answers every unit on a sheet, with `errors` on the first random one.
fn answer_sheet(html: &str, edition: Uuid, errors: u32) -> ProofFile {
    let unit = Regex::new(r#"data-unit="([^"]+)" data-group="([^"]+)""#).unwrap();
    let hash = Regex::new(r#"units_hash: "([^"]*)""#).unwrap();
    let mut answers: Vec<Answer> = unit
        .captures_iter(html)
        .map(|c| Answer {
            unit_id: c[1].parse().unwrap(),
            group: if &c[2] == "worst" {
                Group::Worst
            } else {
                Group::Random
            },
            errors: 0,
            note: None,
        })
        .collect();
    answers[0].errors = errors;
    answers[0].note = Some("rn read as m".into());
    ProofFile {
        edition_id: edition,
        units_hash: hash.captures(html).unwrap()[1].to_owned(),
        answers,
    }
}

const THRESHOLDS: Thresholds = Thresholds {
    random_max: 1,
    worst_max: 3,
};

#[tokio::test]
async fn a_pdf_imports_page_by_page_and_serves_only_after_a_proofread() {
    let fx = start().await;
    let ev = evidence(&fx.pool).await;
    let manifest = write_manifest(&fx.dir, ev, 0.2);
    let tools = FakeTools { pages: book() };
    let env = PdfEnv {
        tools: &tools,
        scratch: fx.dir.join("scratch"),
        word_list: word_list(&fx.dir),
    };
    let embedder = FakeEmbedder::new("fake");
    let mut conn = fx.pool.acquire().await.unwrap();

    // The dry run lists every excluded block and the suspect unit, and writes nothing.
    let dry = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: false,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    let report = dry.pdf.as_ref().unwrap();
    assert_eq!((report.text_layer_pages, report.ocr_pages), (1, 2));
    assert_eq!(report.extraction, "ocr");
    let kinds = |k: &str| {
        report
            .excluded_blocks
            .iter()
            .filter(|e| e.kind == k)
            .count()
    };
    assert_eq!(
        (
            kinds("running_head"),
            kinds("page_number"),
            kinds("footnote")
        ),
        (3, 3, 1)
    );
    assert_eq!(report.suspect.len(), 1, "{:?}", report.suspect);
    assert!(report.suspect[0].confidence.unwrap() < 0.8);
    assert!(report.stopped.is_none());
    assert_eq!(dry.sections_kept, vec!["First Chapter"]);
    assert_eq!(
        count(&fx.pool, "select count(*) from text_edition").await,
        0
    );
    assert!(
        files_under(&fx.dir.join("scratch")).is_empty(),
        "page images are deleted once read"
    );

    // Apply: units carry their page and OCR confidence; the edition waits for a proofread.
    let applied = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    assert!(applied.applied);
    let (edition, extraction, engine, state): (Uuid, String, Option<String>, String) =
        sqlx::query_as("select id, extraction, ocr_engine, state::text from text_edition")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        (extraction.as_str(), engine.as_deref(), state.as_str()),
        ("ocr", Some("fake tesseract 5"), "imported")
    );
    let units: Vec<(String, Option<i32>, Option<f32>)> =
        sqlx::query_as("select text, source_page, confidence from text_unit order by ordinal")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(units.len(), 6);
    assert!(
        units[1].0.contains("to continue noticing"),
        "hyphenation removed: {}",
        units[1].0
    );
    assert_eq!(
        units.iter().map(|u| u.1).collect::<Vec<_>>(),
        vec![Some(1), Some(1), Some(2), Some(2), Some(3), Some(3)]
    );
    assert!(
        units[0].2.is_none() && units[2].2.is_some(),
        "only OCR units carry a confidence"
    );
    let anchors: Vec<Option<String>> =
        sqlx::query_scalar("select source_anchor from text_unit order by ordinal")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        anchors[0].as_deref(),
        Some("#page=1"),
        "a PDF unit links to its page"
    );
    assert_eq!(anchors[5].as_deref(), Some("#page=3"));
    assert_eq!(
        count(&fx.pool, "select count(*) from servable_text_unit").await,
        0
    );
    let stored = files_under(&fx.dir.join("objects"));
    assert!(stored.iter().any(|f| f.ends_with("placements.json")));
    assert!(
        stored.iter().all(|f| !f.ends_with(".png")),
        "no page image is stored: {stored:?}"
    );

    // The proof sheet shows each sampled unit beside its crop.
    let (_, html) = proofread::sheet(
        &mut conn,
        &fx.objects,
        &tools,
        edition,
        &fx.dir.join("scratch"),
    )
    .await
    .unwrap();
    assert_eq!(html.matches("data-unit=").count(), 6);
    assert_eq!(
        html.matches("data:image/png;base64,UE5HREFUQQ==").count(),
        6
    );
    assert!(html.contains("page 3"));
    assert!(
        files_under(&fx.dir.join("scratch")).is_empty(),
        "crops are deleted from scratch"
    );

    // A failing proofread records the errors and keeps the edition out.
    let failed = proofread::import_proof(
        &mut conn,
        &fx.objects,
        edition,
        &answer_sheet(&html, edition, 2),
        "Reviewer",
        THRESHOLDS,
    )
    .await
    .unwrap();
    assert!(!failed.verdict.passed && !failed.ready);
    assert!(failed.verdict.notes[0].contains("rn read as m"));
    assert_eq!(
        count(&fx.pool, "select count(*) from servable_text_unit").await,
        0
    );

    // A passing one serves it, with page numbers.
    let passed = proofread::import_proof(
        &mut conn,
        &fx.objects,
        edition,
        &answer_sheet(&html, edition, 1),
        "Reviewer",
        THRESHOLDS,
    )
    .await
    .unwrap();
    assert!(passed.verdict.passed && passed.ready);
    assert_eq!(passed.verdict.random_sampled, 6);
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from servable_text_unit where source_page is not null"
        )
        .await,
        6
    );
    assert_eq!(count(&fx.pool, "select count(*) from proof_check").await, 2);

    // A fix changes the unit, logs it, and needs a new proofread.
    let unit: Uuid = sqlx::query_scalar("select id from text_unit where confidence < 0.8")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let fixed = proofread::fix_unit(
        &mut conn,
        &embedder,
        unit,
        "The bell rang and the door closed as the night came down.",
        "owner",
        "proofread correction",
    )
    .await
    .unwrap();
    assert_eq!(fixed.edition, edition);
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from correction_log where entity = 'text_unit'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&fx.pool, "select count(*) from servable_text_unit").await,
        0
    );
    let stale = proofread::import_proof(
        &mut conn,
        &fx.objects,
        edition,
        &answer_sheet(&html, edition, 0),
        "Reviewer",
        THRESHOLDS,
    )
    .await;
    assert!(
        stale
            .unwrap_err()
            .to_string()
            .contains("changed since the sheet")
    );
    let (_, html) = proofread::sheet(
        &mut conn,
        &fx.objects,
        &tools,
        edition,
        &fx.dir.join("scratch"),
    )
    .await
    .unwrap();
    let again = proofread::import_proof(
        &mut conn,
        &fx.objects,
        edition,
        &answer_sheet(&html, edition, 0),
        "Reviewer",
        THRESHOLDS,
    )
    .await
    .unwrap();
    assert!(again.ready);
    assert_eq!(
        count(&fx.pool, "select count(*) from servable_text_unit").await,
        6
    );
}

#[tokio::test]
async fn too_many_suspect_units_stop_the_import_with_a_report() {
    let fx = start().await;
    let ev = evidence(&fx.pool).await;
    // One suspect unit in six is over the default 10 percent.
    let manifest = write_manifest(&fx.dir, ev, 0.1);
    let tools = FakeTools { pages: book() };
    let env = PdfEnv {
        tools: &tools,
        scratch: fx.dir.join("scratch"),
        word_list: word_list(&fx.dir),
    };
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
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    assert!(!report.applied);
    let stopped = report.pdf.unwrap().stopped.unwrap();
    assert!(stopped.contains("1 of 6 units are suspect"), "{stopped}");
    assert_eq!(
        count(&fx.pool, "select count(*) from text_edition").await,
        0
    );

    // A unit is suspect on its words alone too: an empty word list flags everything.
    std::fs::write(fx.dir.join("words.txt"), "the\n").unwrap();
    let env = PdfEnv {
        word_list: fx.dir.join("words.txt"),
        ..env
    };
    let report = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: false,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.pdf.unwrap().suspect.len(), 6);
}

#[tokio::test]
async fn a_pdf_whose_first_page_names_another_holder_is_blocked_until_evidence_names_it() {
    let fx = start().await;
    let ev = evidence(&fx.pool).await;
    let mut pages = book();
    pages[0].blocks.insert(
        1,
        block(
            &["Copyright 1994 by Fixture Holder Estate. All rights reserved."],
            60.0,
            12.0,
            1.0,
        ),
    );
    let tools = FakeTools { pages };
    let env = PdfEnv {
        tools: &tools,
        scratch: fx.dir.join("scratch"),
        word_list: word_list(&fx.dir),
    };
    let manifest = write_manifest(&fx.dir, ev, 0.2);
    let mut conn = fx.pool.acquire().await.unwrap();
    let blocked = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        blocked.notice_outcome.as_deref(),
        Some("blocked: third_party_notice")
    );
    assert!(!blocked.applied);
    let text = std::fs::read_to_string(&manifest).unwrap().replace(
        "rights_holder = \"public domain\"",
        "rights_holder = \"Fixture Holder Estate\"",
    );
    std::fs::write(&manifest, text).unwrap();
    let cleared = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: true,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap();
    assert!(cleared.applied && cleared.legal_blocks == 1, "{cleared:?}");
}

#[test]
fn the_vision_pass_is_refused() {
    let err = texts::manifest::CorpusManifest::parse(
        r#"role = "corpus"
[work]
title = "W"
[edition]
label = "E"
rights_holder = "public domain"
license_evidence_id = "00000000-0000-0000-0000-000000000000"
[source]
files = ["a.pdf"]
format = "pdf"
[pdf]
vision_pass = "flagged"
[units]
scheme = "paragraph"
locator_prefix = "w"
label_template = "{work} {n}"
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("deferred"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Real tools

fn real_tools() -> Option<Poppler> {
    let tools = Poppler::new(
        std::env::var_os("WT_POPPLER_BIN").map(std::path::PathBuf::from),
        std::env::var_os("WT_TESSERACT").map(std::path::PathBuf::from),
    );
    if tools.available() {
        return Some(tools);
    }
    assert!(
        std::env::var_os("WT_REQUIRE_OCR_TOOLS").is_none(),
        "WT_REQUIRE_OCR_TOOLS is set but poppler or Tesseract is missing"
    );
    eprintln!("skipped: poppler or Tesseract is not installed (set WT_POPPLER_BIN, WT_TESSERACT)");
    None
}

fn pdftoppm() -> PathBuf {
    std::env::var_os("WT_POPPLER_BIN").map_or_else(
        || PathBuf::from("pdftoppm"),
        |b| PathBuf::from(b).join("pdftoppm"),
    )
}

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wt-pdf-real-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const PROSE: [&str; 8] = [
    "The student came to the hall before dawn and sat facing the",
    "white wall while the bell sounded three times in the quiet.",
    "Nothing was asked of her except to breathe and to notice the",
    "breath as it came and went, like a guest who does not stay.",
    "When the talk ended the students bowed and walked slowly back",
    "to the garden where the morning light was falling on stones.",
    "Later the teacher spoke about the moon in a drop of dew and",
    "how the whole sky rests in a small bead of water on grass.",
];

fn prose_page() -> Vec<fixture::TextLine> {
    PROSE
        .iter()
        .enumerate()
        .map(|(i, t)| line(72.0, 120.0 + 22.0 * i as f32, 13.0, t))
        .collect()
}

/// A scan of `pages`: each rendered at 300 dpi, noise planted, wrapped as an image page.
fn scan_of(dir: &Path, pages: &[Vec<fixture::TextLine>]) -> Vec<PageSpec> {
    let text_pdf = dir.join("source.pdf");
    std::fs::write(
        &text_pdf,
        fixture::pdf(
            &pages
                .iter()
                .cloned()
                .map(PageSpec::Text)
                .collect::<Vec<_>>(),
        ),
    )
    .unwrap();
    (1..=pages.len())
        .map(|n| {
            let (width, height, mut gray) =
                fixture::render_gray(&pdftoppm(), &text_pdf, u32::try_from(n).unwrap(), 300);
            fixture::plant_noise(&mut gray, 2, 0x5eed + n as u64);
            PageSpec::Image {
                width,
                height,
                gray,
            }
        })
        .collect()
}

#[test]
fn a_mixed_pdf_reads_its_text_layer_and_ocrs_its_scanned_page() {
    let Some(tools) = real_tools() else { return };
    let dir = temp();
    let mut specs = vec![PageSpec::Text(prose_page())];
    specs.extend(scan_of(&dir, &[prose_page()]));
    let path = dir.join("mixed.pdf");
    std::fs::write(&path, fixture::pdf(&specs)).unwrap();
    let read = pdf::read(
        &tools,
        &path,
        &PdfSettings::default(),
        &dir.join("scratch"),
        pdf::ReadOptions::default(),
    )
    .unwrap();
    let origins: Vec<Origin> = read.pages.iter().map(|p| p.origin).collect();
    assert_eq!(origins, vec![Origin::TextLayer, Origin::Ocr]);
    assert!(files_under(&dir.join("scratch")).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ocr_of_a_synthetic_scan_is_within_two_percent_of_its_source() {
    let Some(tools) = real_tools() else { return };
    let dir = temp();
    let path = dir.join("scan.pdf");
    std::fs::write(
        &path,
        fixture::pdf(&scan_of(&dir, &[prose_page(), prose_page()])),
    )
    .unwrap();
    let settings = PdfSettings {
        ocr: OcrMode::Always,
        strip_running_heads: false,
        ..PdfSettings::default()
    };
    let read = pdf::read(
        &tools,
        &path,
        &settings,
        &dir.join("scratch"),
        pdf::ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(read.ocr_pages, 2);
    let got: Vec<String> = read
        .pages
        .iter()
        .flat_map(|p| p.words().map(|w| w.text.clone()).collect::<Vec<_>>())
        .collect();
    let want = format!("{} {}", PROSE.join(" "), PROSE.join(" "));
    let cer = fixture::cer(&want, &got.join(" "));
    assert!(cer < 0.02, "character error rate {cer:.4}");
    assert!(
        read.pages
            .iter()
            .all(|p| p.mean_confidence().unwrap() > 0.8)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_text_layer_loses_hyphens_heads_page_numbers_and_footnotes() {
    let Some(tools) = real_tools() else { return };
    let dir = temp();
    let pages: Vec<PageSpec> = (1..=3)
        .map(|n| {
            PageSpec::Text(vec![
                line(72.0, 36.0, 9.0, "FIXTURE SCAN BOOK"),
                line(
                    72.0,
                    120.0,
                    12.0,
                    "Nothing was asked of her except to breathe and to con-",
                ),
                line(
                    72.0,
                    136.0,
                    12.0,
                    "tinue noticing the breath as it came and went like a guest.",
                ),
                line(
                    72.0,
                    170.0,
                    12.0,
                    "The student came to the hall before dawn and sat facing the",
                ),
                line(
                    72.0,
                    186.0,
                    12.0,
                    "white wall while the bell sounded three times in the quiet.",
                ),
                line(
                    72.0,
                    690.0,
                    7.0,
                    "1 A footnote set in small type at the foot of the page.",
                ),
                line(300.0, 750.0, 10.0, &n.to_string()),
            ])
        })
        .collect();
    let path = dir.join("layer.pdf");
    std::fs::write(&path, fixture::pdf(&pages)).unwrap();
    let settings = PdfSettings {
        min_layer_chars: 50,
        ..PdfSettings::default()
    };
    let read = pdf::read(
        &tools,
        &path,
        &settings,
        &dir.join("scratch"),
        pdf::ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(read.text_layer_pages, 3);
    let rebuilt = pdf::rebuild::rebuild(&read.pages, true, false);
    let text = rebuilt
        .sections
        .iter()
        .flat_map(|s| s.blocks.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("to continue noticing"), "{text}");
    assert!(
        !text.contains("FIXTURE") && !text.contains("footnote"),
        "{text}"
    );
    let kinds = |k: &str| rebuilt.excluded.iter().filter(|e| e.kind == k).count();
    assert_eq!(
        (
            kinds("running_head"),
            kinds("page_number"),
            kinds("footnote")
        ),
        (3, 3, 3),
        "{:?}",
        rebuilt.excluded
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A journal page: wide margins, so the running head stands at 13 percent of the page, below
/// the fixed margin; the journal's name on even pages and the article's title on odd ones,
/// each with the page number on the outer side. A line given with a leading `>` is indented.
fn journal_page(n: u32, body: &[&str], foot: Option<&str>) -> Vec<fixture::TextLine> {
    let printed = 224 + n;
    let mut lines = vec![if n % 2 == 1 {
        line(380.0, 105.0, 9.0, &format!("THE LOTUS SUTRA {printed}"))
    } else {
        line(72.0, 105.0, 9.0, &format!("{printed} THE SYNTHETIC REVIEW"))
    }];
    for (i, text) in body.iter().enumerate() {
        let (x, text) = text.strip_prefix('>').map_or((72.0, *text), |t| (96.0, t));
        lines.push(line(x, 140.0 + 17.0 * i as f32, 12.0, text));
    }
    if let Some(text) = foot {
        lines.push(line(72.0, 660.0, 8.0, text));
    }
    lines
}

#[test]
fn a_journal_scan_loses_alternating_heads_and_footnotes_and_joins_across_pages() {
    let Some(tools) = real_tools() else { return };
    let dir = temp();
    let pages = [
        journal_page(
            1,
            &[
                ">Then the teacher said to the student that he should go",
                "to the hall and ask after the health of the old monk.",
                ">The student replied that the moon is one and is not",
                "divided in any way at all, nor is",
            ],
            None,
        ),
        journal_page(
            2,
            &[
                "Suchness divided into parts by the mind of the student.",
                "Then he sat down and the bell rang twice in the hall",
            ],
            Some("1 The moon here stands for the mind, as the old commentaries hold."),
        ),
        journal_page(
            3,
            &[
                ">Morning came and the student rose early to sweep.",
                "He swept the hall and lit the lamps before the bell.",
            ],
            None,
        ),
        journal_page(
            4,
            &[">The old monk smiled at him and nothing more was said."],
            None,
        ),
        journal_page(
            5,
            &[">Then the rain stopped and the student went home again."],
            None,
        ),
        journal_page(
            6,
            &[">The book ends here, and thus have I heard it told."],
            None,
        ),
    ];
    let path = dir.join("journal.pdf");
    std::fs::write(&path, fixture::pdf(&scan_of(&dir, &pages))).unwrap();
    let settings = PdfSettings {
        ocr: OcrMode::Always,
        ..PdfSettings::default()
    };
    let read = pdf::read(
        &tools,
        &path,
        &settings,
        &dir.join("scratch"),
        pdf::ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(read.ocr_pages, 6);
    let rebuilt = pdf::rebuild::rebuild(&read.pages, true, false);
    let blocks: Vec<String> = rebuilt
        .sections
        .iter()
        .flat_map(|s| s.blocks.clone())
        .collect();
    let text = blocks.join("\n");
    assert!(
        !text.contains("LOTUS") && !text.contains("SYNTHETIC") && !text.contains("commentaries"),
        "{text}"
    );
    let kinds = |k: &str| rebuilt.excluded.iter().filter(|e| e.kind == k).count();
    assert_eq!(
        (kinds("running_head"), kinds("footnote")),
        (6, 1),
        "{:?}",
        rebuilt.excluded
    );
    // The paragraph that runs over the page break is one block; planted noise may add a
    // stray mark between its halves.
    assert!(
        blocks
            .iter()
            .any(|b| b.contains("in any way at all, nor is")
                && b.contains("Suchness divided into parts")),
        "{blocks:#?}"
    );
    assert!(
        blocks.iter().any(|b| b.starts_with("Morning came")),
        "{blocks:#?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Tools that must not be reached.
struct Untouched;

impl PdfTools for Untouched {
    fn page_count(&self, _: &Path) -> Result<u32, TextError> {
        panic!("the PDF was read before the word list was checked")
    }

    fn text_layer(&self, _: &Path, _: u32) -> Result<Page, TextError> {
        panic!("the PDF was read before the word list was checked")
    }

    fn render(&self, _: &Path, _: u32, _: u32, _: Option<Crop>, _: &Path) -> Result<(), TextError> {
        panic!("the PDF was read before the word list was checked")
    }

    fn ocr(&self, _: &Path, _: &[String], _: u32) -> Result<Page, TextError> {
        panic!("the PDF was read before the word list was checked")
    }

    fn engine(&self) -> Result<String, TextError> {
        panic!("the PDF was read before the word list was checked")
    }
}

#[tokio::test]
async fn a_missing_word_list_refuses_the_import_before_any_page_is_read() {
    let fx = start().await;
    let ev = evidence(&fx.pool).await;
    let manifest = write_manifest(&fx.dir, ev, 0.1);
    let env = PdfEnv {
        tools: &Untouched,
        scratch: fx.dir.join("scratch"),
        word_list: fx.dir.join("no-such-words"),
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    let err = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &texts::ImportOptions {
            allowed_codes: &allowed(),
            actor: "owner",
            apply: false,
            force: false,
            pdf_env: Some(&env),
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("no-such-words"), "{err}");
    assert!(err.contains("[texts] word_list"), "{err}");
}
