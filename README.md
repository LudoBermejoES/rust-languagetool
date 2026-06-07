# rust-languagetool

Download-on-demand [LanguageTool](https://languagetool.org/) grammar checking for offline Rust desktop apps.

## Why

LanguageTool has first-class Spanish and English grammar rules (~3,500 ES + ~6,500 EN hand-tuned patterns). But it's Java — and there's no good pure-Rust alternative for Spanish grammar. This crate solves the distribution problem:

**LanguageTool is never bundled in your installer.** It's an opt-in download. When the user enables grammar checking, this crate downloads the official LanguageTool distribution (and a JRE if no suitable Java is on the system) into the OS app-data directory, runs it as a supervised HTTP sidecar, and exposes a clean Rust API for checking text.

## State machine

```
NotInstalled → Downloading{…} → Installing → Stopped
                                                │
                                             start()
                                                │
                                            Starting → Ready
                                                          │
                                                        stop()
                                                          │
                                                      Stopped
```

Any step can transition to `Error { message }`.

## Usage

```rust
use rust_languagetool::{LanguageToolEngine, EngineConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build config — supply the SHA-256 of the official download for integrity checking
    let cfg = EngineConfig::default_for_app(
        "MyApp",
        "YOUR_SHA256_HERE",  // compute once: sha256sum LanguageTool-6.6.zip
    ).unwrap();

    let engine = LanguageToolEngine::new(cfg);

    // One-time provision: download + verify + unzip (no-op if already installed)
    engine.provision(|state| eprintln!("Install: {state}")).await?;

    // Spawn the server (Java cold start: up to 30s)
    engine.start().await?;

    // Check text
    let matches = engine.check("I goes to school", "en-US").await?;
    for m in &matches {
        eprintln!(
            "[{}+{}] {} → {:?}",
            m.offset, m.length, m.message, m.replacements
        );
    }

    engine.stop().await?;
    Ok(())
}
```

## What gets downloaded

| Asset | Size | When |
|---|---|---|
| `LanguageTool-6.6.zip` (all languages) | ~252 MB | On first enable |
| Temurin 17 JRE | ~30–50 MB | Only if no system Java ≥17 found |
| n-gram datasets | **never** | Not downloaded — not needed for rule-based checking |

## Installation location

Files are stored in the OS app-data directory (not your project directory):

- **macOS:** `~/Library/Application Support/<app>/languagetool/`
- **Windows:** `%LOCALAPPDATA%\<app>\languagetool\`
- **Linux:** `~/.local/share/<app>/languagetool/`

## Engine config

```rust
EngineConfig {
    data_dir: PathBuf,          // install root
    lt_version: String,         // "6.6"
    lt_download_url: String,    // official zip URL
    lt_sha256: String,          // hex SHA-256 of the zip (empty = skip verify)
    startup_timeout: Duration,  // default 30s
    jre_download_url: Option<String>,  // Temurin 17 per-OS/arch; None to disable JRE download
    jre_sha256: Option<String>,
}
```

Use `EngineConfig::default_for_app(app_name, sha256)` for platform-appropriate defaults.

## Check options

```rust
// Basic check
engine.check("text", "en-US").await?;

// With disabled rules/categories
engine.check_with_opts("text", "es", &["MORFOLOGIK_RULE_ES".into()], &[]).await?;
```

## GrammarMatch fields

```rust
pub struct GrammarMatch {
    pub offset: usize,         // char index (not byte) into the original text
    pub length: usize,         // char length
    pub message: String,
    pub short_message: String,
    pub replacements: Vec<String>,
    pub rule_id: String,
    pub category_id: String,
    pub category_name: String,
    pub issue_type: String,    // "grammar", "style", "misspelling", "typographical", …
}
```

Offsets are normalized to **character** indices (not UTF-16 code units, not byte offsets) so they map cleanly to editor positions in multi-byte text like `niños`.

## Graceful fallback

If `check()` is called before the engine is `Ready`, it returns `Error::NotReady` immediately — it never blocks. This lets your app fall back to another checker without hanging.

```rust
match engine.check(text, lang).await {
    Ok(matches) => use_lt_matches(matches),
    Err(rust_languagetool::Error::NotReady { .. }) => fallback(),
    Err(e) => log_error(e),
}
```

## Features

- **No Tauri/GUI dependency** — pure Rust + tokio, usable by any application
- **Kill-on-drop** — no orphan Java processes survive a host crash
- **Loopback only** — server binds `127.0.0.1:<ephemeral>`, never `0.0.0.0`
- **Atomic install** — download to `.part`, verify SHA-256, then rename+unzip
- **System Java reuse** — checks `JAVA_HOME` and `PATH` for Java ≥17; downloads JRE only as fallback

## License

MIT. The downloaded LanguageTool artifacts are LGPL 2.1 — you download them, not bundle them, and they remain under their own license. Surface attribution in your app's About/Licenses section.
