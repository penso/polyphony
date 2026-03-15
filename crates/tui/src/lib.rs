mod app;
mod render;
pub mod theme;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use thiserror::Error;

use {
    app::AppState,
    theme::{Theme, default_theme, detect_terminal_theme},
};

const LOG_BUFFER_CAPACITY: usize = 2_000;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// --- LogBuffer (public, used by CLI) ---

#[derive(Clone, Debug)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_capacity(LOG_BUFFER_CAPACITY)
    }
}

impl LogBuffer {
    pub fn with_capacity(max_lines: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(max_lines))),
            max_lines,
        }
    }

    pub fn push_line(&self, line: impl Into<String>) {
        let line = line.into();
        if line.trim().is_empty() {
            return;
        }
        let mut lines = lock_or_recover(&self.lines);
        lines.push_back(line);
        while lines.len() > self.max_lines {
            lines.pop_front();
        }
    }

    pub fn recent_lines(&self, limit: usize) -> Vec<String> {
        lock_or_recover(&self.lines)
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn all_lines(&self) -> Vec<String> {
        lock_or_recover(&self.lines).iter().cloned().collect()
    }

    pub fn drain_oldest_first(&self) -> Vec<String> {
        let mut lines = lock_or_recover(&self.lines);
        lines.drain(..).collect()
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

// --- Bootstrap (workflow initialization prompt) ---

mod bootstrap;
mod event_loop;
mod prelude;

#[cfg(test)]
mod tests;

pub use crate::{bootstrap::prompt_workflow_initialization, event_loop::run};
