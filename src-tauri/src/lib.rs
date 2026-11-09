//! SW3 Images desktop app: Tauri commands wrapping the compression engine.
//!
//! Security model: file paths never come from the webview. Images enter only
//! through native drag & drop (handled here, in Rust) or a native file
//! dialog; Rust keeps them in a library and the UI refers to them by id. The
//! UI can't choose where files are written either — the output folder is
//! only changeable through a native folder picker — and it has no access to
//! the file system, shell, updater or notification APIs (see
//! capabilities/default.json).

pub mod batch;
pub mod menu;
pub mod settings;
mod updates;

use batch::{Event, Item};
use serde::Serialize;
use settings::{Settings, SettingsState};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use sw3_images::compress::Options;
use sw3_images::files::{self, ImageInfo};
use tauri::ipc::Channel;
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

#[derive(Default)]
pub(crate) struct BatchState {
    pub(crate) running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

/// Every image the user added this session, by id.
#[derive(Default)]
pub(crate) struct Library {
    next_id: AtomicU32,
    paths: Mutex<HashMap<u32, PathBuf>>,
}

impl Library {
    fn add(&self, path: PathBuf) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.paths.lock().unwrap().insert(id, path);
        id
    }

    fn items(&self, ids: &[u32]) -> Result<Vec<Item>, String> {
        let paths = self.paths.lock().unwrap();
        ids.iter()
            .map(|id| paths.get(id).map(|p| Item { id: *id, path: p.clone() }).ok_or_else(|| "Unknown image".to_string()))
            .collect()
    }

    fn forget(&self, ids: &[u32]) {
        let mut paths = self.paths.lock().unwrap();
        for id in ids {
            paths.remove(id);
        }
    }
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputDir {
    path: String,
    label: String,
}

fn output_dir_info(app: &AppHandle) -> CmdResult<OutputDir> {
    let dir = settings::output_dir(app).map_err(|e| format!("Could not create the output folder: {e}"))?;
    Ok(OutputDir { label: settings::short_label(&dir), path: dir.display().to_string() })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    output: OutputDir,
    workers: usize,
}

#[tauri::command]
fn app_info(app: AppHandle) -> CmdResult<AppInfo> {
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        output: output_dir_info(&app)?,
        workers: batch::worker_count(),
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scanned {
    id: u32,
    /// Display only (tooltips); never accepted back from the UI.
    path: String,
    name: String,
    format: sw3_images::compress::Format,
    width: u32,
    height: u32,
    size: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanResult {
    images: Vec<Scanned>,
    skipped: usize,
}

/// Expand folders and probe every candidate in parallel (header reads only),
/// registering each image in the library.
fn scan(app: &AppHandle, paths: Vec<PathBuf>) -> ScanResult {
    let exclude = settings::output_dir(app).ok();
    let candidates = files::collect_images(&paths, exclude.as_deref());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = candidates.len().div_ceil(threads).max(1);
    let probed: Vec<Option<ImageInfo>> = std::thread::scope(|s| {
        let handles: Vec<_> = candidates
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(|p| files::probe(p).ok()).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let total = probed.len();
    let library = app.state::<Library>();
    let images: Vec<Scanned> = probed
        .into_iter()
        .flatten()
        .map(|i| Scanned {
            path: i.path.display().to_string(),
            id: library.add(i.path),
            name: i.name,
            format: i.format,
            width: i.width,
            height: i.height,
            size: i.size,
        })
        .collect();
    ScanResult { skipped: total - images.len(), images }
}

/// Files dropped on the window: scanned in Rust, results pushed to the UI.
fn on_drop(app: &AppHandle, paths: Vec<PathBuf>) {
    if app.state::<BatchState>().running.load(Ordering::SeqCst) {
        let _ = app.emit_to("main", "drop-ignored", ());
        return;
    }
    let _ = app.emit_to("main", "scan-started", ());
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = scan(&app, paths);
        let _ = app.emit_to("main", "scanned", result);
    });
}

#[tauri::command]
async fn pick_images(app: AppHandle) -> CmdResult<Option<ScanResult>> {
    tauri::async_runtime::spawn_blocking(move || {
        let picked = app
            .dialog()
            .file()
            .set_title("Add images")
            .add_filter("Images", &["jpg", "jpeg", "png", "webp", "JPG", "JPEG", "PNG", "WEBP"])
            .blocking_pick_files()?; // None = the user cancelled
        let paths = picked.into_iter().filter_map(|p| p.into_path().ok()).collect();
        Some(scan(&app, paths))
    })
    .await
    .map_err(err)
}

/// Drop images the UI no longer shows (removed tiles, Clear all).
#[tauri::command]
fn forget_images(library: State<'_, Library>, ids: Vec<u32>) {
    library.forget(&ids);
}

/// The only way to change where files are written: a native folder picker.
#[tauri::command]
async fn pick_output_dir(app: AppHandle) -> CmdResult<Option<OutputDir>> {
    let current = settings::output_dir(&app).ok();
    let app2 = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app2.dialog().file().set_title("Save compressed images to");
        if let Some(dir) = current {
            dialog = dialog.set_directory(dir);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(err)?;
    let Some(dir) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    let new = Settings { output_dir: Some(dir) };
    settings::save(&app, &new).map_err(err)?;
    *app.state::<SettingsState>().0.lock().unwrap() = new;
    output_dir_info(&app).map(Some)
}

#[tauri::command]
fn open_output_dir(app: AppHandle) -> CmdResult<()> {
    let dir = settings::output_dir(&app).map_err(err)?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(err)
}

/// Open one of the app's own web pages (About window links). Only names from
/// the allowlist in menu.rs are accepted.
#[tauri::command]
fn open_link(app: AppHandle, name: String) -> CmdResult<()> {
    let url = menu::link_url(&name).ok_or("Unknown link")?;
    app.opener().open_url(url, None::<&str>).map_err(err)
}

/// Show the open-source licenses of everything bundled in the app.
#[tauri::command]
fn open_licenses(app: AppHandle) -> CmdResult<()> {
    menu::open_licenses(&app).map_err(err)
}

fn validate(opts: &Options) -> CmdResult<()> {
    match opts.max_width {
        Some(w) if !(16..=8192).contains(&w) => Err("Max width must be between 16 and 8192 px".into()),
        _ => Ok(()),
    }
}

#[tauri::command]
fn start_batch(
    app: AppHandle,
    state: State<'_, BatchState>,
    library: State<'_, Library>,
    ids: Vec<u32>,
    options: Options,
    on_event: Channel<Event>,
) -> CmdResult<usize> {
    validate(&options)?;
    if ids.is_empty() {
        return Err("No images to compress".into());
    }
    let items = library.items(&ids)?;
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("A batch is already running".into());
    }
    let out_dir = match settings::output_dir(&app) {
        Ok(d) => d,
        Err(e) => {
            state.running.store(false, Ordering::SeqCst);
            return Err(format!("Could not create the output folder: {e}"));
        }
    };
    state.cancel.store(false, Ordering::SeqCst);
    let (running, cancel) = (state.running.clone(), state.cancel.clone());
    let workers = batch::worker_count();

    std::thread::spawn(move || {
        let total = items.len() as u64;
        let settled = std::sync::atomic::AtomicU64::new(0);
        let window = app.get_webview_window("main");
        let set_bar = |status, progress| {
            if let Some(w) = &window {
                let _ = w.set_progress_bar(ProgressBarState { status: Some(status), progress });
            }
        };
        set_bar(ProgressBarStatus::Normal, Some(0));
        let emit = |ev: Event| {
            if matches!(ev, Event::Done { .. } | Event::Failed { .. } | Event::Cancelled { .. }) {
                let n = settled.fetch_add(1, Ordering::Relaxed) + 1;
                set_bar(ProgressBarStatus::Normal, Some(n * 100 / total));
            }
            let _ = on_event.send(ev);
        };
        let summary = batch::run(items, options, &out_dir, workers, &cancel, batch::global_budget(), &emit);
        set_bar(ProgressBarStatus::None, None);
        running.store(false, Ordering::SeqCst);
        if summary.done + summary.failed > 0 && !cancel.load(Ordering::SeqCst) {
            notify_finished(&app, &summary);
        }
        let _ = on_event.send(Event::Finished(summary));
    });
    Ok(workers)
}

#[tauri::command]
fn stop_batch(state: State<'_, BatchState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// Re-run a single failed image with the same options.
#[tauri::command]
async fn retry_file(app: AppHandle, id: u32, options: Options, on_event: Channel<Event>) -> CmdResult<()> {
    validate(&options)?;
    let item = app.state::<Library>().items(&[id])?.remove(0);
    let out_dir = settings::output_dir(&app).map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        let emit = |ev: Event| {
            let _ = on_event.send(ev);
        };
        let ev = batch::process(&item, &options, &out_dir, batch::global_budget(), &emit);
        emit(ev);
    })
    .await
    .map_err(err)
}

/// Decimal units, matching the UI (and Finder).
fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e7 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

pub fn finished_message(s: &batch::Summary) -> (String, String) {
    let noun = if s.done == 1 { "image" } else { "images" };
    let title = format!("{} {noun} compressed", s.done);
    let mut body = if s.orig_bytes > 0 {
        let pct = 100.0 * (1.0 - s.out_bytes as f64 / s.orig_bytes as f64);
        format!("{} → {} · {:.0}% smaller", human_bytes(s.orig_bytes), human_bytes(s.out_bytes), pct.max(0.0))
    } else {
        String::new()
    };
    if s.failed > 0 {
        if !body.is_empty() {
            body.push_str(" · ");
        }
        body.push_str(&format!("{} failed", s.failed));
    }
    (title, body)
}

fn notify_finished(app: &AppHandle, summary: &batch::Summary) {
    let (title, body) = finished_message(summary);
    let _ = app.notification().builder().title(title).body(body).show();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(BatchState::default())
        .manage(Library::default())
        .manage(updates::PendingUpdate::default())
        .setup(|app| {
            let loaded = settings::load(app.handle());
            app.manage(SettingsState(std::sync::Mutex::new(loaded)));
            // Create the output folder right away so it exists before the
            // first batch (and the user can find it in Finder/Explorer).
            let _ = settings::output_dir(app.handle());
            menu::install(app.handle())?;
            Ok(())
        })
        // Drops on a window's own webview arrive as *window* events; Tauri only
        // sends `WebviewEvent::DragDrop` for child webviews, which we don't use.
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                if window.label() == "main" && !paths.is_empty() {
                    on_drop(window.app_handle(), paths.clone());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            pick_images,
            forget_images,
            pick_output_dir,
            open_output_dir,
            start_batch,
            stop_batch,
            retry_file,
            open_link,
            open_licenses,
            updates::check_for_update,
            updates::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SW3 Images");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text() {
        let s = batch::Summary { done: 128, orig_bytes: 342_000_000, out_bytes: 71_000_000, ..Default::default() };
        let (t, b) = finished_message(&s);
        assert_eq!(t, "128 images compressed");
        assert_eq!(b, "342 MB → 71 MB · 79% smaller");
        let s = batch::Summary { done: 1, failed: 2, orig_bytes: 3_400_000, out_bytes: 1_700_000, ..Default::default() };
        let (t, b) = finished_message(&s);
        assert_eq!(t, "1 image compressed");
        assert_eq!(b, "3.4 MB → 1.7 MB · 50% smaller · 2 failed");
    }

    #[test]
    fn options_are_validated() {
        assert!(validate(&Options { max_width: Some(1920), to_webp: true }).is_ok());
        assert!(validate(&Options { max_width: None, to_webp: false }).is_ok());
        assert!(validate(&Options { max_width: Some(0), to_webp: false }).is_err());
        assert!(validate(&Options { max_width: Some(100_000), to_webp: false }).is_err());
    }

    #[test]
    fn the_ui_can_only_refer_to_images_the_user_added() {
        let lib = Library::default();
        let a = lib.add(PathBuf::from("/photos/a.jpg"));
        let b = lib.add(PathBuf::from("/photos/b.jpg"));
        assert_ne!(a, b);
        let items = lib.items(&[b, a]).unwrap();
        assert_eq!(items[0].path, PathBuf::from("/photos/b.jpg"));
        assert!(lib.items(&[a, 999]).is_err(), "unknown ids are rejected, not guessed");
        lib.forget(&[a]);
        assert!(lib.items(&[a]).is_err());
        assert!(lib.items(&[b]).is_ok());
    }
}
