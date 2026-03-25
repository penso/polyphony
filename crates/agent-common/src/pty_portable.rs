use polyphony_core::Error as CoreError;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::pty::{
    PtyBackend, PtyChild, PtyCommand, PtyExitStatus, PtyResizer, PtySpawnConfig, SpawnedPty,
};

pub struct PortablePtyBackend;

impl PtyBackend for PortablePtyBackend {
    fn spawn(&self, config: &PtySpawnConfig) -> Result<SpawnedPty, CoreError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let command_builder = build_command(&config.command);
        let child = pair
            .slave
            .spawn_command(command_builder)
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let killer = child.clone_killer();

        Ok(SpawnedPty {
            reader,
            writer,
            child: Box::new(PortablePtyChild { child, killer }),
            resizer: Box::new(PortablePtyResizer {
                master: pair.master,
            }),
        })
    }
}

fn build_command(cmd: &PtyCommand) -> CommandBuilder {
    let mut builder = CommandBuilder::new(&cmd.program);
    builder.args(&cmd.args);
    if let Some(cwd) = &cmd.cwd {
        builder.cwd(cwd);
    }
    for (key, value) in &cmd.env {
        builder.env(key, value);
    }
    for key in &cmd.env_remove {
        builder.env_remove(key);
    }
    builder
}

struct PortablePtyChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

impl PtyChild for PortablePtyChild {
    fn try_wait(&mut self) -> Result<Option<PtyExitStatus>, CoreError> {
        portable_pty::Child::try_wait(&mut *self.child)
            .map_err(|error| CoreError::Adapter(error.to_string()))
            .map(|opt| opt.map(|s| to_exit_status(&s)))
    }

    fn wait(&mut self) -> Result<PtyExitStatus, CoreError> {
        portable_pty::Child::wait(&mut *self.child)
            .map_err(|error| CoreError::Adapter(error.to_string()))
            .map(|s| to_exit_status(&s))
    }

    fn kill(&mut self) -> Result<(), CoreError> {
        match portable_pty::ChildKiller::kill(&mut *self.killer) {
            Ok(()) => Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::InvalidInput
                    || error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(3) =>
            {
                Ok(())
            },
            Err(error) => Err(CoreError::Adapter(error.to_string())),
        }
    }
}

fn to_exit_status(status: &portable_pty::ExitStatus) -> PtyExitStatus {
    PtyExitStatus {
        exit_code: status.exit_code(),
        signal: status.signal().map(ToOwned::to_owned),
    }
}

struct PortablePtyResizer {
    master: Box<dyn portable_pty::MasterPty + Send>,
}

/// `MasterPty` is `Send` but not `Sync` in portable-pty. Resize takes `&self`
/// and the master is only accessed under an `Arc<Mutex<>>` in practice, so this
/// is safe.
unsafe impl Sync for PortablePtyResizer {}

impl PtyResizer for PortablePtyResizer {
    fn resize(&self, rows: u16, cols: u16) -> Result<(), CoreError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| CoreError::Adapter(error.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn portable_backend_runs_echo() {
        let backend = PortablePtyBackend;
        let config = PtySpawnConfig {
            rows: 24,
            cols: 80,
            command: PtyCommand {
                program: "bash".into(),
                args: vec!["-c".into(), "echo hello".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                env_remove: Vec::new(),
            },
        };
        let mut spawned = backend.spawn(&config).unwrap();

        // Read output in a thread — the PTY master FD stays open after the
        // child exits (writer half), so read_to_string would block forever.
        let mut reader = spawned.reader;
        let handle = std::thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            let mut output = String::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            output
        });

        let status = spawned.child.wait().unwrap();
        assert_eq!(status.exit_code, 0);
        // Drop writer to unblock the reader thread
        drop(spawned.writer);
        let output = handle.join().unwrap();
        assert!(output.contains("hello"), "output was: {output}");
    }

    #[test]
    fn portable_backend_captures_exit_code() {
        let backend = PortablePtyBackend;
        let config = PtySpawnConfig {
            rows: 24,
            cols: 80,
            command: PtyCommand {
                program: "bash".into(),
                args: vec!["-c".into(), "exit 42".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                env_remove: Vec::new(),
            },
        };
        let mut spawned = backend.spawn(&config).unwrap();
        let status = spawned.child.wait().unwrap();
        assert_eq!(status.exit_code, 42);
    }

    #[test]
    fn portable_backend_kill_running_process() {
        let backend = PortablePtyBackend;
        let config = PtySpawnConfig {
            rows: 24,
            cols: 80,
            command: PtyCommand {
                program: "sleep".into(),
                args: vec!["60".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                env_remove: Vec::new(),
            },
        };
        let mut spawned = backend.spawn(&config).unwrap();
        spawned.child.kill().unwrap();
        // kill again to verify idempotency
        spawned.child.kill().unwrap();
    }

    #[test]
    fn portable_backend_resize() {
        let backend = PortablePtyBackend;
        let config = PtySpawnConfig {
            rows: 24,
            cols: 80,
            command: PtyCommand {
                program: "sleep".into(),
                args: vec!["60".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                env_remove: Vec::new(),
            },
        };
        let mut spawned = backend.spawn(&config).unwrap();
        spawned.resizer.resize(48, 120).unwrap();
        spawned.child.kill().unwrap();
    }
}
