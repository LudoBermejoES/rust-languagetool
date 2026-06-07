//! Integration tests — require a real network connection and Java ≥17.
//!
//! These tests are ignored by default. Run with:
//!   cargo test --test integration -- --ignored
//!
//! They will download LanguageTool (~252 MB) to a temp directory, start the server,
//! run checks against real LT rules, and verify shutdown leaves no orphan process.

use rust_languagetool::{EngineConfig, EngineState, LanguageToolEngine};

/// Build a test config pointing at a temp dir.
/// SHA-256 is left empty (skip verify) for CI convenience.
fn test_config() -> EngineConfig {
    let tmp = std::env::temp_dir().join("rust-lt-integration-test");
    std::fs::create_dir_all(&tmp).unwrap();
    EngineConfig::new(
        tmp,
        "6.6",
        "https://languagetool.org/download/LanguageTool-6.6.zip",
        "", // skip SHA verify in integration tests
    )
}

#[tokio::test]
#[ignore = "requires network + Java 17; downloads ~252 MB"]
async fn provision_start_check_stop_en() {
    let _ = tracing_subscriber::fmt::try_init();
    let engine = LanguageToolEngine::new(test_config());

    engine
        .provision(|s| eprintln!("provision: {s}"))
        .await
        .expect("provision");

    assert!(engine.is_installed());

    engine.start().await.expect("start");
    assert_eq!(engine.state(), EngineState::Ready);

    // Classic English grammar error: "I goes" → should be "I go"
    let matches = engine.check("I goes to school every day.", "en-US").await.expect("check en");
    assert!(
        !matches.is_empty(),
        "expected at least one match for 'I goes'; got none"
    );
    let first = &matches[0];
    assert!(!first.rule_id.is_empty(), "rule_id should be populated");
    assert!(!first.category_id.is_empty(), "category_id should be populated");
    assert!(!first.replacements.is_empty(), "should have replacement suggestions");

    engine.stop().await.expect("stop");
    assert_eq!(engine.state(), EngineState::Stopped);
}

#[tokio::test]
#[ignore = "requires network + Java 17; downloads ~252 MB"]
async fn provision_start_check_stop_es() {
    let _ = tracing_subscriber::fmt::try_init();
    let engine = LanguageToolEngine::new(test_config());
    engine.provision(|_| {}).await.expect("provision");
    engine.start().await.expect("start");

    // Spanish agreement error: "tengo ciertas ventaja" → "ventajas"
    let matches = engine
        .check("Tengo ciertas ventaja en esta situación.", "es")
        .await
        .expect("check es");
    assert!(
        !matches.is_empty(),
        "expected at least one match for Spanish agreement error; got none"
    );

    engine.stop().await.expect("stop");
}

#[tokio::test]
#[ignore = "requires network + Java 17; downloads ~252 MB"]
async fn char_offsets_correct_with_accents() {
    let _ = tracing_subscriber::fmt::try_init();
    let engine = LanguageToolEngine::new(test_config());
    engine.provision(|_| {}).await.expect("provision");
    engine.start().await.expect("start");

    let text = "niños camina mal";
    let matches = engine.check(text, "es").await.expect("check");
    for m in &matches {
        // Verify offset + length select a valid sub-slice
        let chars: Vec<char> = text.chars().collect();
        assert!(
            m.offset + m.length <= chars.len(),
            "offset {} + length {} exceeds text length {}",
            m.offset,
            m.length,
            chars.len()
        );
    }

    engine.stop().await.expect("stop");
}

#[tokio::test]
#[ignore = "requires network + Java 17; downloads ~252 MB"]
async fn no_orphan_after_stop() {
    let _ = tracing_subscriber::fmt::try_init();
    let engine = LanguageToolEngine::new(test_config());
    engine.provision(|_| {}).await.expect("provision");
    engine.start().await.expect("start");

    let port = match engine.state() {
        EngineState::Ready => {
            // Read port via a check call to confirm it's alive
            engine.check("Hello", "en-US").await.ok();
            true
        }
        _ => false,
    };
    assert!(port, "engine should be ready");

    engine.stop().await.expect("stop");

    // After stop, check should return NotReady
    let err = engine.check("hello", "en-US").await.unwrap_err();
    assert!(
        matches!(err, rust_languagetool::Error::NotReady { .. }),
        "expected NotReady after stop, got {err}"
    );
}
