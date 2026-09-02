mod app;
mod command_palette;
mod create_issue_modal;
mod detail;
mod dispatch_mode_picker;
mod event_loop;
mod format;
mod render;
mod rows;
mod session;
mod settings;
mod status;
mod theme;
mod tracker;
mod widgets;

use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub use event_loop::run;
