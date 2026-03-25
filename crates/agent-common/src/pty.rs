use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
};

use polyphony_core::Error as CoreError;

/// Exit status from a PTY child process.
#[derive(Debug, Clone)]
pub struct PtyExitStatus {
    pub exit_code: u32,
    /// Signal name if the process was terminated by a signal (Unix only).
    pub signal: Option<String>,
}

/// Configuration for opening a PTY.
pub struct PtySpawnConfig {
    pub rows: u16,
    pub cols: u16,
    pub command: PtyCommand,
}

/// Command to spawn inside the PTY.
pub struct PtyCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub env_remove: Vec<String>,
}

/// Handle to a spawned PTY child process.
///
/// All methods are blocking (synchronous). Callers must wrap them in
/// `tokio::task::spawn_blocking` when calling from async contexts.
pub trait PtyChild: Send + Sync {
    /// Poll for exit without blocking. Returns `None` if still running.
    fn try_wait(&mut self) -> Result<Option<PtyExitStatus>, CoreError>;

    /// Block until the child exits.
    fn wait(&mut self) -> Result<PtyExitStatus, CoreError>;

    /// Send SIGKILL / terminate the child. Idempotent if already exited.
    fn kill(&mut self) -> Result<(), CoreError>;
}

/// Handle for resizing the PTY window.
pub trait PtyResizer: Send + Sync {
    fn resize(&self, rows: u16, cols: u16) -> Result<(), CoreError>;
}

/// A spawned PTY session with its I/O handles.
pub struct SpawnedPty {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn PtyChild>,
    pub resizer: Box<dyn PtyResizer>,
}

/// Backend that can open a PTY and spawn a command inside it.
///
/// All methods are blocking. Callers wrap in `spawn_blocking`.
pub trait PtyBackend: Send + Sync {
    /// Open a PTY, spawn the given command, and return I/O handles.
    fn spawn(&self, config: &PtySpawnConfig) -> Result<SpawnedPty, CoreError>;
}

#[cfg(feature = "portable-pty-backend")]
pub fn default_pty_backend() -> Box<dyn PtyBackend> {
    Box::new(super::pty_portable::PortablePtyBackend)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;
    use crate::pty_mock::MockPtyBackend;

    fn test_config() -> PtySpawnConfig {
        PtySpawnConfig {
            rows: 24,
            cols: 80,
            command: PtyCommand {
                program: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
            },
        }
    }

    #[test]
    fn spawn_returns_readable_output() {
        let backend = MockPtyBackend {
            output: b"hello world".to_vec(),
            exit_code: 0,
        };
        let spawned = backend.spawn(&test_config()).unwrap();
        let mut buf = String::new();
        let mut reader = spawned.reader;
        reader.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "hello world");
    }

    #[test]
    fn spawn_returns_writable_writer() {
        let backend = MockPtyBackend {
            output: Vec::new(),
            exit_code: 0,
        };
        let mut spawned = backend.spawn(&test_config()).unwrap();
        spawned.writer.write_all(b"test input").unwrap();
        spawned.writer.flush().unwrap();
    }

    #[test]
    fn child_try_wait_returns_exit_status() {
        let backend = MockPtyBackend {
            output: Vec::new(),
            exit_code: 42,
        };
        let mut spawned = backend.spawn(&test_config()).unwrap();
        let status = spawned.child.try_wait().unwrap().unwrap();
        assert_eq!(status.exit_code, 42);
        assert!(status.signal.is_none());
    }

    #[test]
    fn child_wait_returns_exit_status() {
        let backend = MockPtyBackend {
            output: Vec::new(),
            exit_code: 0,
        };
        let mut spawned = backend.spawn(&test_config()).unwrap();
        let status = spawned.child.wait().unwrap();
        assert_eq!(status.exit_code, 0);
    }

    #[test]
    fn child_kill_is_idempotent() {
        let backend = MockPtyBackend {
            output: Vec::new(),
            exit_code: 0,
        };
        let mut spawned = backend.spawn(&test_config()).unwrap();
        spawned.child.kill().unwrap();
        spawned.child.kill().unwrap();
    }

    #[test]
    fn resize_succeeds() {
        let backend = MockPtyBackend {
            output: Vec::new(),
            exit_code: 0,
        };
        let spawned = backend.spawn(&test_config()).unwrap();
        spawned.resizer.resize(48, 120).unwrap();
    }
}
