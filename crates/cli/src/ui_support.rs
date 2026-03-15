use std::{future::Future, pin::Pin};

#[cfg(not(feature = "tui"))]
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[cfg(feature = "tui")]
pub(crate) use polyphony_tui::{Error as TuiError, LogBuffer, prompt_workflow_initialization, run};

pub(crate) type TuiRunFuture = Pin<Box<dyn Future<Output = Result<(), TuiError>> + Send>>;

#[cfg(feature = "tui")]
pub(crate) const fn tui_available() -> bool {
    true
}

#[cfg(not(feature = "tui"))]
pub(crate) const fn tui_available() -> bool {
    false
}

#[cfg(not(feature = "tui"))]
#[derive(Debug, thiserror::Error)]
pub(crate) enum TuiError {
    #[error("tui support is disabled for this build")]
    Disabled,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(not(feature = "tui"))]
#[derive(Clone, Default)]
pub(crate) struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
}

#[cfg(not(feature = "tui"))]
impl LogBuffer {
    pub(crate) fn from_lines(lines: Vec<String>) -> Self {
        Self {
            lines: Arc::new(Mutex::new(lines.into())),
        }
    }

    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    pub(crate) fn push_line(&self, line: String) {
        lock_or_recover(&self.lines).push_back(line);
    }

    pub(crate) fn drain_oldest_first(&self) -> Vec<String> {
        lock_or_recover(&self.lines).drain(..).collect()
    }
}

#[cfg(not(feature = "tui"))]
pub(crate) fn prompt_workflow_initialization(
    _workflow_path: &std::path::Path,
) -> Result<bool, TuiError> {
    Err(TuiError::Disabled)
}

#[cfg(not(feature = "tui"))]
pub(crate) async fn run(
    _snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    _command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
    _log_buffer: LogBuffer,
) -> Result<(), TuiError> {
    Err(TuiError::Disabled)
}

#[cfg(not(feature = "tui"))]
fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
