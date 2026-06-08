//! End-to-end example: provision → start → check → stop.
//!
//! Run with:
//!   cargo run --example check
//!
//! On first run this downloads LanguageTool (~250 MB) and, if no system Java ≥17
//! is found, a Temurin 17 JRE. Subsequent runs skip the download (version.json match).
//!
//! The data directory defaults to `~/.local/share/lt-example/languagetool` (or OS equivalent).
//! Override with the `LT_DATA_DIR` env var.

use rust_languagetool::{EngineConfig, EngineState, LanguageToolEngine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rust_languagetool=info".parse()?),
        )
        .init();

    // ── Config ────────────────────────────────────────────────────────────────
    let cfg = if let Ok(dir) = std::env::var("LT_DATA_DIR") {
        EngineConfig::new(
            std::path::PathBuf::from(dir),
            "6.6",
            "https://languagetool.org/download/LanguageTool-6.6.zip",
            "", // skip SHA-256 in this example
        )
    } else {
        EngineConfig::default_for_app("lt-example", "")
            .expect("could not resolve OS app-data directory")
    };

    println!("Data dir: {}", cfg.data_dir.display());

    let engine = LanguageToolEngine::new(cfg);

    // ── Provision ─────────────────────────────────────────────────────────────
    if engine.is_installed() {
        println!("Already installed — skipping download.");
    } else {
        println!("Provisioning LanguageTool (this may take a while on first run)…");
    }

    engine
        .provision(|state| match &state {
            EngineState::Downloading { downloaded, total } => {
                let mb = *downloaded as f64 / 1_048_576.0;
                if let Some(t) = total {
                    let pct = (*downloaded as f64 / *t as f64 * 100.0) as u32;
                    eprint!("\r  Downloading… {mb:.1} MB / {:.1} MB ({pct}%)   ",
                        *t as f64 / 1_048_576.0);
                } else {
                    eprint!("\r  Downloading… {mb:.1} MB   ");
                }
            }
            EngineState::Installing => eprintln!("\n  Installing (unzipping)…"),
            other => eprintln!("  State: {other}"),
        })
        .await?;

    eprintln!();

    if engine.using_system_java() {
        println!("Using system Java.");
    } else {
        println!("Using downloaded JRE.");
    }

    // ── Start ─────────────────────────────────────────────────────────────────
    println!("Starting LanguageTool server (warm-up may take up to 30 s)…");
    engine.start().await?;
    println!("Server ready.");

    // ── Check English ─────────────────────────────────────────────────────────
    let en_text = "She go to school every day and have a good time.";
    println!("\n[en-US] Checking: {en_text:?}");
    let en_matches = engine.check(en_text, "en-US").await?;
    if en_matches.is_empty() {
        println!("  No issues found.");
    }
    for m in &en_matches {
        let snippet: String = en_text.chars().skip(m.offset).take(m.length).collect();
        println!(
            "  [{offset}+{len}] {snippet:?} → {:?}  ({rule})",
            m.replacements,
            offset = m.offset,
            len = m.length,
            rule = m.rule_id,
        );
        println!("    {}", m.message);
    }

    // Helper: print matches with byte vs char offset diagnostics
    fn print_matches(label: &str, text: &str, matches: &[rust_languagetool::GrammarMatch]) {
        println!("\n[{label}] Checking: {text:?}");
        if matches.is_empty() {
            println!("  No issues found.");
            return;
        }
        for m in matches {
            let by_char: String = text.chars().skip(m.offset).take(m.length).collect();
            // Also show what the same range looks like if treated as byte offset
            let byte_snippet = text.get(m.offset..m.offset + m.length).unwrap_or("<oob>");
            let correct = by_char == byte_snippet;
            println!(
                "  [{offset}+{len}] char={by_char:?}  byte={byte_snippet:?}  match={correct}  → {:?}  ({rule})",
                m.replacements.iter().take(3).collect::<Vec<_>>(),
                offset = m.offset,
                len = m.length,
                rule = m.rule_id,
            );
            println!("    {}", m.message);
        }
    }

    // ── Spanish: año (multi-byte ñ) ───────────────────────────────────────────
    // "año" = a(1)+ñ(2 bytes)+o(1) = 4 bytes, 3 chars
    // "Ella tiene veinte año y..."
    //  char offsets:  0123456789...
    //  "año" starts at char 18
    let es_text = "Ella tiene veinte año y estudia en la universidad.";
    let es_matches = engine.check(es_text, "es").await?;
    print_matches("es año", es_text, &es_matches);

    // ── Spanish: plain ASCII, no multi-byte before error ─────────────────────
    let es2_text = "Tengo veinte ano y soy estudiante.";
    let es2_matches = engine.check(es2_text, "es").await?;
    print_matches("es ano (ascii)", es2_text, &es2_matches);

    // ── Spanish: error after multi-byte chars ─────────────────────────────────
    // "niños" contains ñ (2 bytes). Error word "malo" starts after it.
    let es3_text = "Los niños son malo y feliz.";
    let es3_matches = engine.check(es3_text, "es").await?;
    print_matches("es niños…malo", es3_text, &es3_matches);

    // ── Spanish: multiple accented words before error ─────────────────────────
    let es4_text = "El árbol está en el jardín y tiene año.";
    let es4_matches = engine.check(es4_text, "es").await?;
    print_matches("es árbol/está/jardín/año", es4_text, &es4_matches);

    // ── Spanish: error is the multi-byte word itself ──────────────────────────
    let es5_text = "Ella tiene un nino pequeño en casa.";
    let es5_matches = engine.check(es5_text, "es").await?;
    print_matches("es nino (before ñ)", es5_text, &es5_matches);

    // ── Check multi-byte (accents) ────────────────────────────────────────────
    let accent_text = "Los nino juegan en el parque todos los dias.";
    let accent_matches = engine.check(accent_text, "es").await?;
    print_matches("es nino/dias", accent_text, &accent_matches);

    // ── Determiner↔noun gender agreement (correctly-spelled noun) ─────────────
    let agr_text = "La balón es buena.";
    let agr_matches = engine.check(agr_text, "es").await?;
    print_matches("es La balón", agr_text, &agr_matches);

    // ── Stop ──────────────────────────────────────────────────────────────────
    println!("\nStopping server…");
    engine.stop().await?;
    println!("Done.");

    Ok(())
}
