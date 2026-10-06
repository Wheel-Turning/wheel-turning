//! The sidecar client against a local `wiremock` server: it uploads the WAV and reads the turns.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_segment::{Diarizer, SidecarDiarizer, Turn};

#[tokio::test]
async fn the_client_posts_the_audio_and_reads_turns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/diarize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "turns": [
                {"speaker": "S1", "start_ms": 0, "end_ms": 5400},
                {"speaker": "S2", "start_ms": 5400, "end_ms": 7000}
            ],
            "speaker_count": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    let dir = std::env::temp_dir().join(format!("wt-sidecar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let audio = dir.join("fixture.bin");
    std::fs::write(&audio, b"RIFF fixture").unwrap();

    let turns = SidecarDiarizer::new(&server.uri())
        .unwrap()
        .diarize(&audio)
        .await
        .unwrap();
    assert_eq!(
        turns,
        [
            Turn {
                speaker: "S1".into(),
                start_ms: 0,
                end_ms: 5400
            },
            Turn {
                speaker: "S2".into(),
                start_ms: 5400,
                end_ms: 7000
            },
        ]
    );
    let body = &server.received_requests().await.unwrap()[0].body;
    assert!(
        body.windows(12).any(|w| w == b"RIFF fixture"),
        "the audio is uploaded"
    );

    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&failing)
        .await;
    let err = SidecarDiarizer::new(&failing.uri())
        .unwrap()
        .diarize(&audio)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
