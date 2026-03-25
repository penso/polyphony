use std::os::fd::AsFd;

use polyphony_core::Error as CoreError;

use crate::pty::{
    PtyBackend, PtyChild, PtyCommand, PtyExitStatus, PtyResizer, PtySpawnConfig, SpawnedPty,
};

pub struct PtyProcessBackend;

impl PtyBackend for PtyProcessBackend {
    fn spawn(&self, config: &PtySpawnConfig) -> Result<SpawnedPty, CoreError> {
        let (pty, pts) =
            pty_process::blocking::open().map_err(|e| CoreError::Adapter(e.to_string()))?;

        pty.resize(pty_process::Size::new(config.rows, config.cols))
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        let cmd = build_command(&config.command);
        let child = cmd
            .spawn(pts)
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        // Dup the PTY fd to get independent reader/writer handles.
        let read_fd =
            rustix::io::dup(pty.as_fd()).map_err(|e| CoreError::Adapter(e.to_string()))?;
        let write_fd =
            rustix::io::dup(pty.as_fd()).map_err(|e| CoreError::Adapter(e.to_string()))?;

        let reader: Box<dyn std::io::Read + Send> = Box::new(std::fs::File::from(read_fd));
        let writer: Box<dyn std::io::Write + Send> = Box::new(std::fs::File::from(write_fd));

        Ok(SpawnedPty {
            reader,
            writer,
            child: Box::new(PtyProcessChild { child }),
            resizer: Box::new(PtyProcessResizer { pty }),
        })
    }
}

fn build_command(cmd_config: &PtyCommand) -> pty_process::blocking::Command {
    let mut cmd = pty_process::blocking::Command::new(&cmd_config.program);
    cmd = cmd.args(&cmd_config.args);
    if let Some(cwd) = &cmd_config.cwd {
        cmd = cmd.current_dir(cwd);
    }
    for (key, value) in &cmd_config.env {
        cmd = cmd.env(key, value);
    }
    for key in &cmd_config.env_remove {
        cmd = cmd.env_remove(key);
    }
    cmd
}

struct PtyProcessChild {
    child: std::process::Child,
}

impl PtyChild for PtyProcessChild {
    fn try_wait(&mut self) -> Result<Option<PtyExitStatus>, CoreError> {
        self.child
            .try_wait()
            .map_err(|e| CoreError::Adapter(e.to_string()))
            .map(|opt| opt.map(|s| exit_status_to_pty(&s)))
    }

    fn wait(&mut self) -> Result<PtyExitStatus, CoreError> {
        self.child
            .wait()
            .map_err(|e| CoreError::Adapter(e.to_string()))
            .map(|s| exit_status_to_pty(&s))
    }

    fn kill(&mut self) -> Result<(), CoreError> {
        match self.child.kill() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(e) if e.raw_os_error() == Some(3) => Ok(()),
            Err(e) => Err(CoreError::Adapter(e.to_string())),
        }
    }
}

fn exit_status_to_pty(status: &std::process::ExitStatus) -> PtyExitStatus {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| format!("signal {s}"))
    };
    #[cfg(not(unix))]
    let signal = None;

    PtyExitStatus {
        exit_code: status.code().unwrap_or(-1) as u32,
        signal,
    }
}

struct PtyProcessResizer {
    pty: pty_process::blocking::Pty,
}

// pty_process::blocking::Pty is Send but not Sync. Resize takes &self and we
// only access it under a Mutex in practice.
unsafe impl Sync for PtyProcessResizer {}

impl PtyResizer for PtyProcessResizer {
    fn resize(&self, rows: u16, cols: u16) -> Result<(), CoreError> {
        self.pty
            .resize(pty_process::Size::new(rows, cols))
            .map_err(|e| CoreError::Adapter(e.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn pty_process_backend_runs_echo() {
        let backend = PtyProcessBackend;
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
        drop(spawned.writer);
        let output = handle.join().unwrap();
        assert!(output.contains("hello"), "output was: {output}");
    }

    #[test]
    fn pty_process_backend_captures_exit_code() {
        let backend = PtyProcessBackend;
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
    fn pty_process_backend_kill_running_process() {
        let backend = PtyProcessBackend;
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
        spawned.child.kill().unwrap();
    }

    #[test]
    fn pty_process_backend_resize() {
        let backend = PtyProcessBackend;
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
