//! Spec 04: the real bge-m3 export on CPU. Ignored by default: it needs the model from
//! `scripts/export_models.py` and an ONNX Runtime library, and CI has neither.
//!
//!     WT_MODEL_DIR=models WT_ORT_DYLIB=/path/to/onnxruntime.dll \
//!         cargo test -p wt-ml --test onnx -- --ignored
#![allow(clippy::unwrap_used, clippy::expect_used)]

use wt_ml::{EMBED_DIM, Embedder, OnnxEmbedder, OnnxSettings};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[tokio::test]
#[ignore = "needs the exported model and ONNX Runtime"]
async fn bge_m3_returns_repeatable_unit_vectors() {
    let dir = std::env::var("WT_MODEL_DIR").expect("set WT_MODEL_DIR");
    let mut settings = OnnxSettings::new(
        dir,
        std::env::var("WT_EMBED_MODEL").unwrap_or_else(|_| "bge-m3-int8".into()),
    );
    settings.runtime = std::env::var("WT_ORT_DYLIB").ok().map(Into::into);
    let e = OnnxEmbedder::load(&settings).unwrap();
    // Invented sentences; more than one batch.
    let mut texts: Vec<String> = vec![
        "How do I sit with grief when it keeps returning?".into(),
        "Sorrow comes back again and again; we let it sit with us on the cushion.".into(),
        "The parking lot is closed on Sunday mornings for repaving.".into(),
    ];
    texts.extend((0..20).map(|i| format!("Breath number {i}, counted and released.")));

    let started = std::time::Instant::now();
    let a = e.embed(&texts).await.unwrap();
    let elapsed = started.elapsed();
    let b = e.embed(&texts).await.unwrap();
    assert_eq!((a.len(), e.dim()), (texts.len(), EMBED_DIM));
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.len(), EMBED_DIM);
        let norm = cosine(x, x).sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm {norm}");
        let drift = x
            .iter()
            .zip(y)
            .map(|(p, q)| (p - q).abs())
            .fold(0.0f32, f32::max);
        assert!(drift <= 1e-5, "repeated runs differ by {drift}");
    }
    let related = cosine(&a[0], &a[1]);
    let unrelated = cosine(&a[0], &a[2]);
    println!(
        "{} texts in {elapsed:?}; related {related:.3}, unrelated {unrelated:.3}",
        texts.len()
    );
    assert!(related > unrelated + 0.1);
}

#[tokio::test]
#[ignore = "needs the exported model and ONNX Runtime"]
async fn a_vector_depends_on_its_text_alone() {
    let dir = std::env::var("WT_MODEL_DIR").expect("set WT_MODEL_DIR");
    let mut settings = OnnxSettings::new(
        dir,
        std::env::var("WT_EMBED_MODEL").unwrap_or_else(|_| "bge-m3-int8".into()),
    );
    settings.runtime = std::env::var("WT_ORT_DYLIB").ok().map(Into::into);
    let e = OnnxEmbedder::load(&settings).unwrap();
    // Invented sentences of very different lengths, so a shared batch would pad the short one.
    let short = "Grief returns.".to_owned();
    let long = "When the bell rings we stand, bow, and walk slowly around the room, \
                feeling each step meet the floor, and then we sit again and let the \
                breath come and go without managing it."
        .repeat(4);
    let alone = e.embed(std::slice::from_ref(&short)).await.unwrap();
    let mixed = e.embed(&[long, short]).await.unwrap();
    let drift = alone[0]
        .iter()
        .zip(&mixed[1])
        .map(|(p, q)| (p - q).abs())
        .fold(0.0f32, f32::max);
    assert!(
        drift <= 1e-5,
        "a text's vector moved by {drift} beside another text"
    );
}
