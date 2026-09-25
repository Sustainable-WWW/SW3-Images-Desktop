//! Self-updates from GitHub Releases. `latest.json` is published by the
//! release workflow; every download is verified against the public key in
//! tauri.conf.json before it is installed, so only builds signed with the
//! project's private key can ever be installed.
//!
//! Privacy: this check is the app's only network request. It sends nothing
//! but a plain HTTPS GET for the release manifest — no images, file names,
//! identifiers or usage data.

use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
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

/// Human wording for updater failures. A missing manifest (HTTP 404) means
/// no finished release is published — i.e. there is nothing newer.
fn describe(e: tauri_plugin_updater::Error) -> Result<Option<Update>, String> {
    match e {
        tauri_plugin_updater::Error::ReleaseNotFound => Ok(None),
        tauri_plugin_updater::Error::Reqwest(_) => {
            Err("Couldn't reach GitHub to check for updates — check your internet connection.".into())
        }
        other => Err(format!("Couldn't check for updates: {other}")),
    }
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<Option<UpdateInfo>, String> {
    let update = match app.updater().map_err(|e| e.to_string())?.check().await {
        Ok(u) => u,
        Err(e) => describe(e)?,
    };
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
    // Installing restarts the app, which would abandon a running batch.
    if app.state::<crate::BatchState>().running.load(Ordering::SeqCst) {
        return Err("Finish or stop the current batch before updating".into());
    }
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
        .map_err(|e| format!("The update couldn't be installed: {e}"))?;
    app.restart();
}
