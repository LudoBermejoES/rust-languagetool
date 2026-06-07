//! # rust-languagetool
//!
//! Download-on-demand LanguageTool grammar checking for offline Rust desktop apps.
//!
//! ## Architecture
//!
//! LanguageTool is **never bundled** with your application. When the user enables it,
//! [`LanguageToolEngine::provision`] downloads the official distribution (and a JRE if
//! no suitable Java is on the system) into an OS app-data directory. While your app runs,
//! [`LanguageToolEngine::start`] spawns `org.languagetool.server.HTTPServer` on an
//! ephemeral loopback port. Grammar checking is then available via
//! [`LanguageToolEngine::check`] over the local HTTP `/v2/check` endpoint.
//!
//! ## State machine
//!
//! ```text
//! NotInstalled → Downloading{…} → Installing → (Error on failure)
//!                                     │
//!                                  [provision ok]
//!                                     │
//!                                  Stopped (ready to start)
//!                                     │
//!                                   start()
//!                                     │
//!                                 Starting → Ready
//!                                               │
//!                                             stop()
//!                                               │
//!                                           Stopped
//! ```
//!
//! ## Minimal usage
//!
//! ```rust,no_run
//! use rust_languagetool::{LanguageToolEngine, EngineConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Build config (supply real SHA-256 computed from the official download)
//!     let cfg = EngineConfig::default_for_app("MyApp", "").unwrap();
//!     let engine = LanguageToolEngine::new(cfg);
//!
//!     // Download + verify + unzip (one-time, skipped if already installed)
//!     engine.provision(|state| println!("Progress: {state}")).await?;
//!
//!     // Spawn server, poll until ready (Java cold start: up to 30s)
//!     engine.start().await?;
//!
//!     // Check text
//!     let matches = engine.check("I goes to school", "en-US").await?;
//!     for m in &matches {
//!         println!("[{}+{}] {} → {:?}", m.offset, m.length, m.message, m.replacements);
//!     }
//!
//!     // Stop the server on exit (also happens on Drop)
//!     engine.stop().await?;
//!     Ok(())
//! }
//! ```

mod client;
mod config;
mod error;
mod lifecycle;
mod provision;
mod state;

pub use client::GrammarMatch;
pub use config::EngineConfig;
pub use error::{Error, Result};
pub use state::EngineState;

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

use lifecycle::{ServerProcess, wait_for_ready};
use state::SharedState;

/// The LanguageTool grammar engine.
///
/// Cheap to clone — all state is behind an `Arc`.
#[derive(Clone)]
pub struct LanguageToolEngine {
    config: Arc<EngineConfig>,
    state: SharedState,
    server: Arc<Mutex<Option<ServerProcess>>>,
}

impl LanguageToolEngine {
    /// Create a new engine with the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config: Arc::new(config),
            state: SharedState::new(),
            server: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the current observable state.
    pub fn state(&self) -> EngineState {
        self.state.get()
    }

    /// Returns `true` if LanguageTool is installed (provisioned) on this machine.
    pub fn is_installed(&self) -> bool {
        provision::is_installed(&self.config)
    }

    /// Returns `true` if the engine is currently `Ready` and accepting checks.
    pub fn is_ready(&self) -> bool {
        self.state.get() == EngineState::Ready
    }

    /// Returns `true` if the engine was provisioned using a system Java runtime
    /// (vs a downloaded JRE).
    pub fn using_system_java(&self) -> bool {
        self.state.using_system_java()
    }

    /// Provision LanguageTool (and a JRE if needed) into the configured data directory.
    ///
    /// - If already installed with the same version, this is a fast no-op.
    /// - Downloads emit [`EngineState::Downloading`] via `on_progress`.
    /// - n-gram datasets are **never** downloaded.
    pub async fn provision(&self, on_progress: impl Fn(EngineState) + Send + 'static) -> Result<()> {
        self.state.set(EngineState::Downloading { downloaded: 0, total: None });
        let result = provision::provision(&self.config, &self.state, &on_progress).await;
        match &result {
            Ok(()) => {
                // After a successful provision, transition to Stopped (ready to start).
                if !matches!(self.state.get(), EngineState::Ready | EngineState::Starting) {
                    self.state.set(EngineState::Stopped);
                }
            }
            Err(e) => {
                error!("Provision failed: {e}");
                self.state.set(EngineState::Error { message: e.to_string() });
            }
        }
        result
    }

    /// Spawn the LanguageTool HTTP server and wait until it is healthy.
    ///
    /// Transitions: `* → Starting → Ready` (or `Error` on timeout/crash).
    pub async fn start(&self) -> Result<()> {
        if !self.is_installed() {
            return Err(Error::Provision("call provision() before start()".into()));
        }

        self.state.set(EngineState::Starting);
        let server = match ServerProcess::spawn(&self.config, &self.state).await {
            Ok(s) => s,
            Err(e) => {
                self.state.set(EngineState::Error { message: e.to_string() });
                return Err(e);
            }
        };

        let port = server.port();
        let mut guard = self.server.lock().await;
        *guard = Some(server);
        drop(guard);

        match wait_for_ready(port, self.config.startup_timeout, &self.state).await {
            Ok(()) => {
                info!("LT engine ready on port {port}");
                Ok(())
            }
            Err(e) => {
                self.state.set(EngineState::Error { message: e.to_string() });
                if let Some(srv) = self.server.lock().await.take() {
                    srv.stop().await;
                }
                Err(e)
            }
        }
    }

    /// Stop the LanguageTool server. Safe to call even if already stopped.
    pub async fn stop(&self) -> Result<()> {
        let mut guard = self.server.lock().await;
        if let Some(srv) = guard.take() {
            srv.stop().await;
        }
        self.state.set(EngineState::Stopped);
        Ok(())
    }

    /// Check `text` for grammar errors in the given `language` (e.g. `"en-US"`, `"es"`).
    ///
    /// Returns [`Error::NotReady`] immediately if the engine is not [`EngineState::Ready`].
    pub async fn check(&self, text: &str, language: &str) -> Result<Vec<GrammarMatch>> {
        self.check_with_opts(text, language, &[], &[]).await
    }

    /// Like [`check`] but with optional rule/category suppression lists.
    pub async fn check_with_opts(
        &self,
        text: &str,
        language: &str,
        disabled_rules: &[String],
        disabled_categories: &[String],
    ) -> Result<Vec<GrammarMatch>> {
        let port = self.state.port().ok_or_else(|| Error::NotReady {
            state: self.state.get().to_string(),
        })?;
        let lt = client::LtClient::new(port);
        lt.check(text, language, disabled_rules, disabled_categories, &self.state)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EngineConfig {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        EngineConfig::new(dir, "6.6", "https://example.com/lt.zip", "deadbeef")
    }

    #[tokio::test]
    async fn check_before_ready_returns_not_ready() {
        let engine = LanguageToolEngine::new(test_config());
        let err = engine.check("hello", "en-US").await.unwrap_err();
        match err {
            Error::NotReady { .. } => {}
            other => panic!("expected NotReady, got {other}"),
        }
    }

    #[tokio::test]
    async fn state_starts_not_installed() {
        let engine = LanguageToolEngine::new(test_config());
        assert_eq!(engine.state(), EngineState::NotInstalled);
        assert!(!engine.is_installed());
        assert!(!engine.is_ready());
    }

    #[tokio::test]
    async fn provision_fails_on_bad_url() {
        let engine = LanguageToolEngine::new(test_config());
        let result = engine.provision(|_| {}).await;
        assert!(result.is_err());
        assert!(matches!(engine.state(), EngineState::Error { .. }));
    }
}
