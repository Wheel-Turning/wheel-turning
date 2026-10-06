//! `[asr]` in config/pipeline.toml matches the code's defaults.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn the_shipped_settings_parse_and_equal_the_defaults() {
    #[derive(serde::Deserialize)]
    struct File {
        asr: wt_asr::AsrSettings,
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pipeline.toml");
    let file: File = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(file.asr, wt_asr::AsrSettings::default());
}
