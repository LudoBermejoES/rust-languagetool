use std::path::PathBuf;
use std::time::Duration;

/// Configuration for the LanguageTool engine.
///
/// Use [`EngineConfig::new`] for full control, or [`EngineConfig::default_for_app`]
/// for sensible OS-appropriate defaults using the app data directory.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Base directory where LT and the optional JRE are installed.
    /// Layout: `<data_dir>/languagetool/{dist/,jre/,version.json,.lock}`
    pub data_dir: PathBuf,

    /// Pinned LanguageTool version string (e.g. "6.6").
    pub lt_version: String,

    /// Full URL of the LanguageTool distribution zip to download.
    pub lt_download_url: String,

    /// Expected SHA-256 hex digest of the zip. The host must supply this;
    /// compute it once from the official download and bake it in.
    /// Set to empty string to skip verification (not recommended for production).
    pub lt_sha256: String,

    /// How long to wait for the HTTP server to become healthy after spawn.
    pub startup_timeout: Duration,

    /// Optional JRE download URL for this OS/arch.
    /// If `None` and no system Java ≥17 is found, [`provision`] will return
    /// [`Error::NoJavaRuntime`].
    pub jre_download_url: Option<String>,

    /// Expected SHA-256 hex digest of the JRE archive (if `jre_download_url` is set).
    pub jre_sha256: Option<String>,
}

impl EngineConfig {
    /// Construct a config with explicit values. Supply SHA-256 for integrity checking.
    pub fn new(data_dir: PathBuf, lt_version: impl Into<String>, lt_download_url: impl Into<String>, lt_sha256: impl Into<String>) -> Self {
        Self {
            data_dir,
            lt_version: lt_version.into(),
            lt_download_url: lt_download_url.into(),
            lt_sha256: lt_sha256.into(),
            startup_timeout: Duration::from_secs(30),
            jre_download_url: default_jre_url(),
            jre_sha256: None,
        }
    }

    /// Build a config rooted in the OS app-data dir for the given app name.
    /// Uses the pinned LT 6.6 release with platform-appropriate JRE defaults.
    ///
    /// # Pinned release
    /// LT 6.6 — `https://languagetool.org/download/LanguageTool-6.6.zip`
    /// SHA-256: supply via `lt_sha256` after computing from the official download.
    pub fn default_for_app(app_name: &str, lt_sha256: impl Into<String>) -> Option<Self> {
        let base = dirs::data_dir()?.join(app_name);
        let data_dir = base.join("languagetool");
        Some(Self::new(
            data_dir,
            "6.6",
            "https://languagetool.org/download/LanguageTool-6.6.zip",
            lt_sha256,
        ))
    }
}

/// Returns a Temurin 17 JRE download URL for the current platform, if known.
fn default_jre_url() -> Option<String> {
    // Adoptium/Temurin 17 (LTS) minimal JRE archives, per OS + arch.
    // These are official GitHub release assets.
    let base = "https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.13%2B11";

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some(format!("{}/OpenJDK17U-jre_aarch64_mac_hotspot_17.0.13_11.tar.gz", base));

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Some(format!("{}/OpenJDK17U-jre_x64_mac_hotspot_17.0.13_11.tar.gz", base));

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(format!("{}/OpenJDK17U-jre_x64_linux_hotspot_17.0.13_11.tar.gz", base));

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some(format!("{}/OpenJDK17U-jre_aarch64_linux_hotspot_17.0.13_11.tar.gz", base));

    #[cfg(target_os = "windows")]
    return Some(format!("{}/OpenJDK17U-jre_x64_windows_hotspot_17.0.13_11.zip", base));

    #[allow(unreachable_code)]
    None
}
