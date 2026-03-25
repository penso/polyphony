use std::{
    io::{self, Cursor, Write},
    sync::{Arc, Mutex},
};

use polyphony_core::Error as CoreError;

use crate::pty::{PtyBackend, PtyChild, PtyExitStatus, PtyResizer, PtySpawnConfig, SpawnedPty};

/// A mock PTY backend for testing. Pre-configure the output the
/// "child" will produce and the exit status it will return.
pub struct MockPtyBackend {
    pub output: Vec<u8>,
    pub exit_code: u32,
}

impl PtyBackend for MockPtyBackend {
    fn spawn(&self, _config: &PtySpawnConfig) -> Result<SpawnedPty, CoreError> {
        let written = Arc::new(Mutex::new(Vec::<u8>::new()));
        Ok(SpawnedPty {
            reader: Box::new(Cursor::new(self.output.clone())),
            writer: Box::new(MockWriter(written)),
            child: Box::new(MockPtyChild {
                exit_code: self.exit_code,
            }),
            resizer: Box::new(MockResizer),
        })
    }
}

struct MockWriter(Arc<Mutex<Vec<u8>>>);

impl Write for MockWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct MockPtyChild {
    exit_code: u32,
}

impl PtyChild for MockPtyChild {
    fn try_wait(&mut self) -> Result<Option<PtyExitStatus>, CoreError> {
        Ok(Some(PtyExitStatus {
            exit_code: self.exit_code,
            signal: None,
        }))
    }

    fn wait(&mut self) -> Result<PtyExitStatus, CoreError> {
        Ok(PtyExitStatus {
            exit_code: self.exit_code,
            signal: None,
        })
    }

    fn kill(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

struct MockResizer;

impl PtyResizer for MockResizer {
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), CoreError> {
        Ok(())
    }
}
