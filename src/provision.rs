use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::config::EngineConfig;
use crate::error::{Error, Result};
use crate::state::{EngineState, SharedState};

const VERSION_FILE: &str = "version.json";
const LOCK_FILE: &str = ".lock";
const DIST_DIR: &str = "dist";
const JRE_DIR: &str = "jre";

#[derive(serde::Serialize, serde::Deserialize)]
struct VersionJson {
    lt_version: String,
    lt_sha256: String,
    java_path: String,
    using_system_java: bool,
}

/// Resolve the path to the LT server jar inside the dist directory.
pub fn server_jar_path(data_dir: &Path, lt_version: &str) -> PathBuf {
    data_dir
        .join(DIST_DIR)
        .join(format!("LanguageTool-{}", lt_version))
        .join("languagetool-server.jar")
}

/// Check whether LT is installed and the version.json matches the configured version.
pub fn is_installed(cfg: &EngineConfig) -> bool {
    let version_file = cfg.data_dir.join(VERSION_FILE);
    let jar = server_jar_path(&cfg.data_dir, &cfg.lt_version);
    if !jar.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&version_file) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<VersionJson>(&content) else {
        return false;
    };
    v.lt_version == cfg.lt_version
        && (cfg.lt_sha256.is_empty() || v.lt_sha256 == cfg.lt_sha256)
}

/// Restore the java_path from version.json without running a full provision.
/// Used on app restart when LT is already installed but state is fresh.
pub fn restore_java_path(cfg: &EngineConfig, state: &SharedState) {
    if let Ok(v) = read_version_json(&cfg.data_dir) {
        state.set_java(v.java_path, v.using_system_java);
    }
}

/// Run the full provision flow: detect/download Java, download+verify+unzip LT.
pub async fn provision(
    cfg: &EngineConfig,
    state: &SharedState,
    on_progress: &impl Fn(EngineState),
) -> Result<()> {
    tokio::fs::create_dir_all(&cfg.data_dir).await?;

    // Advisory lock to prevent concurrent provisions
    let lock_path = cfg.data_dir.join(LOCK_FILE);
    let _lock = AdvisoryLock::new(&lock_path);

    // Already installed and matching version: fast-path no-op
    if is_installed(cfg) {
        info!("LT {} already installed, skipping provision", cfg.lt_version);
        // Restore java path from version.json
        if let Ok(v) = read_version_json(&cfg.data_dir) {
            state.set_java(v.java_path, v.using_system_java);
        }
        return Ok(());
    }

    // Step 1: resolve Java
    let (java_path, system_java) = resolve_java(cfg).await?;
    state.set_java(java_path.clone(), system_java);
    info!("Using Java at {java_path} (system={system_java})");

    // Step 2: download LT
    let zip_part = cfg.data_dir.join("LanguageTool.zip.part");
    let zip_final = cfg.data_dir.join("LanguageTool.zip");

    info!("Downloading LT {} from {}", cfg.lt_version, cfg.lt_download_url);
    download_file(
        &cfg.lt_download_url,
        &zip_part,
        state,
        on_progress,
    )
    .await?;

    // Step 3: verify checksum
    if !cfg.lt_sha256.is_empty() {
        state.set(EngineState::Installing);
        on_progress(EngineState::Installing);
        let actual = sha256_of_file(&zip_part).await?;
        if actual != cfg.lt_sha256 {
            let _ = tokio::fs::remove_file(&zip_part).await;
            return Err(Error::ChecksumMismatch {
                expected: cfg.lt_sha256.clone(),
                actual,
            });
        }
        info!("LT zip checksum ok");
    }

    // Atomic rename
    tokio::fs::rename(&zip_part, &zip_final).await?;

    // Step 4: unzip
    let dist_dir = cfg.data_dir.join(DIST_DIR);
    tokio::fs::create_dir_all(&dist_dir).await?;
    unzip_file(&zip_final, &dist_dir).await?;
    tokio::fs::remove_file(&zip_final).await.ok();

    // Step 5: write version.json
    let v = VersionJson {
        lt_version: cfg.lt_version.clone(),
        lt_sha256: cfg.lt_sha256.clone(),
        java_path,
        using_system_java: system_java,
    };
    tokio::fs::write(
        cfg.data_dir.join(VERSION_FILE),
        serde_json::to_string_pretty(&v)?,
    )
    .await?;

    info!("LT {} provisioned successfully", cfg.lt_version);
    Ok(())
}

async fn resolve_java(cfg: &EngineConfig) -> Result<(String, bool)> {
    // 1. JAVA_HOME
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let java = PathBuf::from(&home).join("bin").join(java_bin_name());
        if java.exists() && probe_java_version(&java.to_string_lossy()).await >= 17 {
            return Ok((java.to_string_lossy().into_owned(), true));
        }
    }

    // 2. java on PATH
    if let Ok(output) = tokio::process::Command::new(java_bin_name())
        .arg("-version")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .await
    {
        // java -version writes to stderr
        let version_str = String::from_utf8_lossy(&output.stderr).to_string()
            + &String::from_utf8_lossy(&output.stdout);
        if let Some(major) = parse_java_major(&version_str) {
            if major >= 17 {
                return Ok((java_bin_name().to_string(), true));
            }
            warn!("Found Java {major} on PATH but need ≥17");
        }
    }

    // 3. Try bundled JRE if available
    let jre_java = cfg.data_dir.join(JRE_DIR).join("bin").join(java_bin_name());
    if jre_java.exists() && probe_java_version(&jre_java.to_string_lossy()).await >= 17 {
        return Ok((jre_java.to_string_lossy().into_owned(), false));
    }

    // 4. Download JRE if URL configured
    if let Some(ref url) = cfg.jre_download_url {
        info!("Downloading JRE from {url}");
        let jre_archive = cfg.data_dir.join("jre.part");
        // Use a no-op progress so JRE download shares the same state channel
        let dummy_state = SharedState::new();
        download_file(url, &jre_archive, &dummy_state, &|_| {}).await?;

        if let Some(ref sha) = cfg.jre_sha256 {
            let actual = sha256_of_file(&jre_archive).await?;
            if &actual != sha {
                let _ = tokio::fs::remove_file(&jre_archive).await;
                return Err(Error::ChecksumMismatch {
                    expected: sha.clone(),
                    actual,
                });
            }
        }

        let jre_dir = cfg.data_dir.join(JRE_DIR);
        tokio::fs::create_dir_all(&jre_dir).await?;
        unpack_jre(&jre_archive, &jre_dir).await?;
        tokio::fs::remove_file(&jre_archive).await.ok();

        // After unpacking the JRE tarball, find the java binary (may be nested in a dir)
        if let Some(java) = find_java_binary(&jre_dir) {
            return Ok((java, false));
        }
    }

    Err(Error::NoJavaRuntime)
}

fn java_bin_name() -> &'static str {
    if cfg!(target_os = "windows") { "java.exe" } else { "java" }
}

async fn probe_java_version(java: &str) -> u32 {
    let Ok(out) = tokio::process::Command::new(java)
        .arg("-version")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .await
    else {
        return 0;
    };
    let combined = String::from_utf8_lossy(&out.stderr).to_string()
        + &String::from_utf8_lossy(&out.stdout);
    parse_java_major(&combined).unwrap_or(0)
}

/// Parse the major version from `java -version` output.
/// Handles formats: `java version "17.0.x"`, `openjdk version "17.0.x"`, `"11.x"` etc.
fn parse_java_major(output: &str) -> Option<u32> {
    // Match quoted version string
    for line in output.lines() {
        if let Some(start) = line.find('"') {
            let rest = &line[start + 1..];
            if let Some(end) = rest.find('"') {
                let ver = &rest[..end];
                // "1.8.x" → 8; "17.0.x" → 17
                let first = ver.split('.').next()?;
                let major: u32 = first.parse().ok()?;
                if major == 1 {
                    // Old-style: "1.8" → 8
                    return ver.split('.').nth(1)?.parse().ok();
                }
                return Some(major);
            }
        }
    }
    None
}

async fn download_file(
    url: &str,
    dest: &Path,
    state: &SharedState,
    on_progress: &impl Fn(EngineState),
) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length();

    let mut file = tokio::fs::File::create(dest).await?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        let s = EngineState::Downloading { downloaded, total };
        state.set(s.clone());
        on_progress(s);
    }
    file.flush().await?;
    debug!("Downloaded {downloaded} bytes to {}", dest.display());
    Ok(())
}

async fn sha256_of_file(path: &Path) -> Result<String> {
    let data = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

async fn unzip_file(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let zip_path = zip_path.to_owned();
    let dest_dir = dest_dir.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let outpath = dest_dir.join(entry.name());
            if entry.is_dir() {
                std::fs::create_dir_all(&outpath)?;
            } else {
                if let Some(parent) = outpath.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut outfile = std::fs::File::create(&outpath)?;
                std::io::copy(&mut entry, &mut outfile)?;
                // Preserve executable bits on Unix
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(mode) = entry.unix_mode() {
                        std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode))?;
                    }
                }
            }
        }
        Ok::<_, Error>(())
    })
    .await
    .map_err(|e| Error::Provision(e.to_string()))??;
    Ok(())
}

async fn unpack_jre(archive: &Path, dest: &Path) -> Result<()> {
    let archive = archive.to_owned();
    let dest = dest.to_owned();

    // Windows: zip; Unix: tar.gz
    if archive.to_string_lossy().ends_with(".zip") {
        unzip_file(&archive, &dest).await
    } else {
        // tar.gz — use tokio::process::Command with system tar
        let status = tokio::process::Command::new("tar")
            .args(["xzf"])
            .arg(&archive)
            .arg("-C")
            .arg(&dest)
            .arg("--strip-components=1")
            .status()
            .await?;
        if status.success() { Ok(()) } else {
            Err(Error::Provision("tar extraction failed".into()))
        }
    }
}

fn find_java_binary(jre_dir: &Path) -> Option<String> {
    let bin = jre_dir.join("bin").join(java_bin_name());
    if bin.exists() {
        return Some(bin.to_string_lossy().into_owned());
    }
    // May be one level nested (e.g. jdk-17.0.13+11-jre/bin/java)
    if let Ok(entries) = std::fs::read_dir(jre_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("bin").join(java_bin_name());
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn read_version_json(data_dir: &Path) -> Result<VersionJson> {
    let content = std::fs::read_to_string(data_dir.join(VERSION_FILE))?;
    Ok(serde_json::from_str(&content)?)
}

/// Minimal advisory lock using a file. Dropped on scope exit.
struct AdvisoryLock {
    path: PathBuf,
}

impl AdvisoryLock {
    fn new(path: &Path) -> Self {
        let _ = std::fs::File::create(path);
        Self { path: path.to_owned() }
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_java_major_modern() {
        let out = r#"openjdk version "17.0.13" 2024-10-15\nOpenJDK Runtime Environment"#;
        assert_eq!(parse_java_major(out), Some(17));
    }

    #[test]
    fn parse_java_major_old_style() {
        let out = r#"java version "1.8.0_362""#;
        assert_eq!(parse_java_major(out), Some(8));
    }

    #[test]
    fn parse_java_major_21() {
        let out = r#"openjdk version "21.0.1" 2023-10-17"#;
        assert_eq!(parse_java_major(out), Some(21));
    }

    #[tokio::test]
    async fn is_installed_missing_jar() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EngineConfig::new(
            dir.path().to_path_buf(),
            "6.6",
            "https://example.com/lt.zip",
            "abc123",
        );
        assert!(!is_installed(&cfg));
    }
}
