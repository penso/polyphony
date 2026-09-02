use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct TuiSettings {
    #[serde(default = "default_show_widget_timestamps")]
    pub show_widget_timestamps: bool,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            show_widget_timestamps: default_show_widget_timestamps(),
        }
    }
}

impl TuiSettings {
    pub(crate) fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let Some(path) = settings_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, contents)
    }
}

const fn default_show_widget_timestamps() -> bool {
    true
}

fn settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".polyphony").join("tui-next.json"))
}
