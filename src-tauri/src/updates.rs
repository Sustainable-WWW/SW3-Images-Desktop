//! Self-updates from GitHub Releases. `latest.json` is published by the
//! release workflow; every download is verified against the public key in
//! tauri.conf.json before it is installed, so only builds signed with the
//! project's private key can ever be installed.

use serde::Serialize;
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UpdateEvent {
    #[serde(rename_all = "camelCase")]
    Progress { downloaded: u64, total: Option<u64> },
    Installing,
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<Option<UpdateInfo>, String> {
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let info = update.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone(),
    });
    *pending.0.lock().unwrap() = update;
    Ok(info)
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    on_event: Channel<UpdateEvent>,
) -> Result<(), String> {
    let update = pending.0.lock().unwrap().take().ok_or("No update available")?;
    let mut downloaded = 0u64;
    update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = on_event.send(UpdateEvent::Progress { downloaded, total });
            },
            || {
                let _ = on_event.send(UpdateEvent::Installing);
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}
