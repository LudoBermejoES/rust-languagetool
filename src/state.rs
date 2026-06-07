use std::sync::{Arc, Mutex};

/// Observable state of the LanguageTool engine.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineState {
    /// LT has not been provisioned (downloaded) yet.
    NotInstalled,
    /// Download in progress.
    Downloading { downloaded: u64, total: Option<u64> },
    /// Verifying checksum / unzipping.
    Installing,
    /// Server process spawned; waiting for it to become healthy.
    Starting,
    /// Server is up and accepting requests.
    Ready,
    /// Server was stopped cleanly.
    Stopped,
    /// An unrecoverable error occurred.
    Error { message: String },
}

impl std::fmt::Display for EngineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "not_installed"),
            Self::Downloading { downloaded, total } => {
                if let Some(t) = total {
                    write!(f, "downloading ({downloaded}/{t} bytes)")
                } else {
                    write!(f, "downloading ({downloaded} bytes)")
                }
            }
            Self::Installing => write!(f, "installing"),
            Self::Starting => write!(f, "starting"),
            Self::Ready => write!(f, "ready"),
            Self::Stopped => write!(f, "stopped"),
            Self::Error { message } => write!(f, "error: {message}"),
        }
    }
}

/// Thread-safe shared state used internally by the engine.
#[derive(Clone)]
pub(crate) struct SharedState {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    state: EngineState,
    port: Option<u16>,
    java_path: Option<String>,
    using_system_java: bool,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: EngineState::NotInstalled,
                port: None,
                java_path: None,
                using_system_java: false,
            })),
        }
    }

    pub fn get(&self) -> EngineState {
        self.inner.lock().unwrap().state.clone()
    }

    pub fn set(&self, state: EngineState) {
        self.inner.lock().unwrap().state = state;
    }

    pub fn port(&self) -> Option<u16> {
        self.inner.lock().unwrap().port
    }

    pub fn set_port(&self, port: u16) {
        self.inner.lock().unwrap().port = Some(port);
    }

    pub fn java_path(&self) -> Option<String> {
        self.inner.lock().unwrap().java_path.clone()
    }

    pub fn set_java(&self, path: String, system: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.java_path = Some(path);
        inner.using_system_java = system;
    }

    pub fn using_system_java(&self) -> bool {
        self.inner.lock().unwrap().using_system_java
    }
}
