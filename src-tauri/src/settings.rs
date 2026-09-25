//! Persisted preferences (currently just the output folder), stored as JSON
//! in the OS app-config directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

pub const DEFAULT_FOLDER_NAME: &str = "SW3 Compressed";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub output_dir: Option<PathBuf>,
}

pub struct SettingsState(pub Mutex<Settings>);

fn settings_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    settings_file(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, settings: &Settings) -> std::io::Result<()> {
    let Some(path) = settings_file(app) else { return Ok(()) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(settings)?)
}

/// `~/Pictures/SW3 Compressed` on both macOS and Windows, falling back to
/// Documents or the home folder on unusual setups.
pub fn default_output_dir(app: &AppHandle) -> PathBuf {
    let p = app.path();
    p.picture_dir()
        .or_else(|_| p.document_dir())
        .or_else(|_| p.home_dir())
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(DEFAULT_FOLDER_NAME)
}

/// The folder compressed files go to. It is (re)created on every call, so the
/// app keeps working even if the user deletes the folder while it runs.
pub fn output_dir(app: &AppHandle) -> std::io::Result<PathBuf> {
    let state = app.state::<SettingsState>();
    let configured = state.0.lock().unwrap().output_dir.clone();
    let dir = match configured {
        Some(d) if d.is_absolute() => d,
        _ => default_output_dir(app),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// "Pictures / SW3 Compressed" — the last two path components, as shown in
/// the Save-to card.
pub fn short_label(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => path.display().to_string(),
        [one] => one.clone(),
        [.., a, b] => format!("{a} / {b}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let p: PathBuf = ["/", "Users", "m", "Pictures", "SW3 Compressed"].iter().collect();
        assert_eq!(short_label(&p), "Pictures / SW3 Compressed");
        let p: PathBuf = ["/", "Photos"].iter().collect();
        assert_eq!(short_label(&p), "Photos");
    }
}
