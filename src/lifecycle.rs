use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::config::EngineConfig;
use crate::error::{Error, Result};
use crate::provision::server_jar_path;
use crate::state::{EngineState, SharedState};

/// Acquire an ephemeral free port on loopback by binding momentarily.
pub fn acquire_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

pub(crate) struct ServerProcess {
    child: Arc<Mutex<Option<Child>>>,
    port: u16,
}

impl ServerProcess {
    /// Spawn the LT HTTP server and return the wrapper.
    pub async fn spawn(cfg: &EngineConfig, state: &SharedState) -> Result<Self> {
        let port = acquire_free_port()?;
        let jar = server_jar_path(&cfg.data_dir, &cfg.lt_version);

        let java = state.java_path().ok_or(Error::Provision(
            "java path not set; call provision() first".into(),
        ))?;

        info!("Spawning LT server on 127.0.0.1:{port} via {java}");

        let child = Command::new(&java)
            .args([
                "-cp",
                &jar.to_string_lossy(),
                "org.languagetool.server.HTTPServer",
                "--port",
                &port.to_string(),
                "--allow-origin",
                "*",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)  // tokio kill-on-drop
            .spawn()?;

        state.set_port(port);
        Ok(Self { child: Arc::new(Mutex::new(Some(child))), port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Kill the child process gracefully (then forcefully if needed).
    pub async fn stop(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            info!("Stopping LT server (port {})", self.port);
            // tokio Child::kill sends SIGKILL; try graceful via /quit first
            let client = reqwest::Client::new();
            let _ = client
                .get(format!("http://127.0.0.1:{}/v2/quit", self.port))
                .timeout(Duration::from_secs(3))
                .send()
                .await;
            // Give it a moment to exit cleanly
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _: std::io::Result<()> = child.kill().await;
            let _: std::io::Result<std::process::ExitStatus> = child.wait().await;
        }
    }

    /// Check if the child has exited (indicates a crash while running).
    #[allow(dead_code)]
    pub async fn has_exited(&self) -> bool {
        let mut guard = self.child.lock().await;
        if let Some(ref mut child) = *guard {
            matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        } else {
            true
        }
    }
}

/// Poll `GET /v2/languages` until the server responds with 200 or timeout elapses.
pub async fn wait_for_ready(port: u16, timeout: Duration, state: &SharedState) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/v2/languages");
    let client = reqwest::Client::new();
    let deadline = Instant::now() + timeout;

    state.set(EngineState::Starting);

    loop {
        if Instant::now() >= deadline {
            return Err(Error::StartupTimeout { seconds: timeout.as_secs() });
        }
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                info!("LT server ready on port {port}");
                state.set(EngineState::Ready);
                return Ok(());
            }
            Ok(resp) => {
                debug!("LT server not ready yet: HTTP {}", resp.status());
            }
            Err(e) => {
                debug!("LT server not ready yet: {e}");
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_free_port_is_nonzero() {
        let port = acquire_free_port().unwrap();
        assert!(port > 1024, "expected unprivileged port, got {port}");
    }
}
