//! HTTP server: upload, status polling, downloads, retention sweep,
//! rate limiting and bot protection.

use crate::compress::{self, Format};
use crate::jobs::{self, FileEntry, FileMeta, Job, JobMeta, Registry, Status};
use crate::notify::Notifier;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path as AxPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::HashMap;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

pub const FREE_MAX_FILES: usize = 10;
pub const UNLOCKED_MAX_FILES: usize = 50;
pub const MAX_FILE_BYTES: u64 = 30 * 1024 * 1024; // 30 MB per image

/// Rate limit: this many new jobs per IP per window. Generous for a human
/// batching images, restrictive for a bot hammering the API.
const RATE_LIMIT_JOBS: usize = 10;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(15 * 60);

pub struct AppState {
    pub registry: Registry,
    sem: Arc<Semaphore>,
    data_dir: PathBuf,
    leads: Mutex<()>,
    limiter: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    trust_proxy: bool,
    notifier: Notifier,
}

pub fn new_state(data_dir: PathBuf) -> Arc<AppState> {
    let registry = Registry::new(&data_dir).expect("could not create data directory");
    // Compress one image at a time. Peak memory during a large photo's
    // quality search can hit a few hundred MB; running two in parallel used
    // to blow past the PM2 memory ceiling on the Pi, killing the process
    // mid-compression and marking everything "Interrupted". Sequential is
    // reliable and, since mozjpeg/libwebp/oxipng/DSSIM already use SIMD
    // internally, only marginally slower on wall-clock time.
    let concurrency = std::env::var("SW3_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1);
    Arc::new(AppState {
        registry,
        sem: Arc::new(Semaphore::new(concurrency)),
        data_dir,
        leads: Mutex::new(()),
        limiter: Mutex::new(HashMap::new()),
        trust_proxy: std::env::var("TRUST_PROXY").map(|v| v == "1").unwrap_or(false),
        notifier: Notifier::from_env(),
    })
}

pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/robots.txt", get(robots_txt))
        .route("/llms.txt", get(llms_txt))
        .route("/api/jobs", post(create_job))
        .route("/api/jobs/{id}", get(job_status))
        .route("/api/jobs/{id}/files/{idx}", get(download_file))
        .route("/api/jobs/{id}/zip", get(download_zip))
        .layer(DefaultBodyLimit::max(1600 * 1024 * 1024))
        .with_state(state)
}

pub async fn run() {
    // Load .env from the project root (silently skipped if absent).
    dotenvy::dotenv().ok();

    // Resolve the data directory to an absolute path immediately, so that
    // nothing later can be surprised by a changed working directory (PM2 in
    // particular can set cwd differently than the shell you launched it from).
    let raw_data_dir = std::env::var("SW3_DATA").unwrap_or_else(|_| "data".to_string());
    let cwd_display = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let data_dir = std::path::PathBuf::from(&raw_data_dir);
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(&data_dir)
    };
    println!("cwd: {cwd_display}");
    println!("data directory: {} (SW3_DATA={raw_data_dir})", data_dir.display());

    // Startup self-test: create the directory and prove it's writable before
    // accepting any traffic. Fail fast with a specific, actionable message
    // instead of surfacing an opaque "os error 2" on the first upload.
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!(
            "FATAL: could not create data directory {}: {e}\n\
             Check that the user running the process has write permission,\n\
             or set SW3_DATA to a path they own (e.g. SW3_DATA=/home/pi/sw3-images-data).",
            data_dir.display()
        );
        std::process::exit(1);
    }
    let probe = data_dir.join(".write-probe");
    if let Err(e) = std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        eprintln!(
            "FATAL: data directory {} is not writable: {e}",
            data_dir.display()
        );
        std::process::exit(1);
    }

    let state = new_state(data_dir);

    // 24-hour retention: sweep every 15 minutes. cleanup_expired does
    // filesystem work, so it belongs on the blocking pool — not on the
    // async runtime thread where it would stall other tasks.
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let s = state.clone();
                let removed = tokio::task::spawn_blocking(move || s.registry.cleanup_expired())
                    .await
                    .unwrap_or(0);
                if removed > 0 {
                    println!("cleanup: removed {removed} expired job(s)");
                }
                tokio::time::sleep(Duration::from_secs(15 * 60)).await;
            }
        });
    }

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2500);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("🌱 SW3 Images listening on http://localhost:{port}");
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: could not bind port {port}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        eprintln!("FATAL: http server error: {e}");
        std::process::exit(1);
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// Quick config + writability check. Curl this on the Pi to confirm the
/// server is up AND that its data directory is where you expect and usable:
///   curl -s http://localhost:2500/healthz | jq
async fn healthz(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let probe = state.data_dir.join(".healthz-probe");
    let writable = std::fs::write(&probe, b"ok")
        .and_then(|_| std::fs::remove_file(&probe))
        .is_ok();
    Json(serde_json::json!({
        "status": "ok",
        "data_dir": state.data_dir.display().to_string(),
        "data_dir_writable": writable,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Welcome all crawlers — visibility in search and AI engines is the goal.
async fn robots_txt() -> ([(header::HeaderName, &'static str); 1], &'static str) {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "User-agent: *\nAllow: /\n")
}

/// llms.txt — emerging convention giving AI crawlers a concise, quotable
/// description of what this service is and does.
async fn llms_txt() -> ([(header::HeaderName, &'static str); 1], &'static str) {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "# SW3 Images\n\n\
         > Free online image compressor and resizer for the web, by Sustainable WWW Sweden AB \
         and Michael Andersen. Compresses JPG, PNG and WebP images up to ~90% smaller with no \
         visible quality loss, using a perceptually-guided algorithm (DSSIM in Lab color space) \
         written in Rust — quality is measured, not guessed.\n\n\
         ## Key facts\n\n\
         - Compress up to 10 images at a time free (50 after leaving name + email), 30 MB each.\n\
         - Optional resize to a maximum width; aspect ratio is preserved and images are never upscaled.\n\
         - Same format in, same format out (JPEG via mozjpeg, PNG via imagequant + oxipng, WebP via libwebp).\n\
         - Metadata (EXIF, GPS) is stripped; uploads are private and auto-deleted after 24 hours.\n\
         - Files never get bigger: if the original is already optimal, it is returned unchanged.\n\n\
         ## Links\n\n\
         - Made by Sustainable WWW Sweden AB: https://sustainablewww.se\n\
         - Michael Andersen: https://michaelandersen.net\n\
         - Newsletter: https://michaelandersentech.substack.com\n",
    )
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// Client IP: socket peer, or the first X-Forwarded-For hop when running
/// behind a reverse proxy (TRUST_PROXY=1).
fn client_ip(state: &AppState, headers: &HeaderMap, sock: SocketAddr) -> IpAddr {
    if state.trust_proxy {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(ip) = xff.split(',').next().and_then(|s| s.trim().parse().ok()) {
                return ip;
            }
        }
    }
    sock.ip()
}

/// Sliding-window limiter. Returns false when the IP is over budget.
fn rate_limit_ok(state: &AppState, ip: IpAddr) -> bool {
    let now = Instant::now();
    let mut map = state.limiter.lock().unwrap();
    // Keep the map from growing unboundedly under a spoofed-IP flood.
    if map.len() > 10_000 {
        map.retain(|_, v| v.iter().any(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW));
    }
    let hits = map.entry(ip).or_default();
    hits.retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
    if hits.len() >= RATE_LIMIT_JOBS {
        return false;
    }
    hits.push(now);
    true
}

// ---------------------------------------------------------------------------
// POST /api/jobs — upload + start compression
// ---------------------------------------------------------------------------

async fn create_job(
    State(state): State<Arc<AppState>>,
    ConnectInfo(sock): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ip = client_ip(&state, &headers, sock);
    if !rate_limit_ok(&state, ip) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Slow down a little — try again in a few minutes.".into(),
        ));
    }

    let id = Uuid::new_v4();
    let dir = state.registry.jobs_dir.join(id.to_string());
    let orig_dir = dir.join("orig");
    tokio::fs::create_dir_all(&orig_dir).await.map_err(|e| {
        let msg = format!("could not create upload directory {}: {e}", orig_dir.display());
        eprintln!("{msg}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    // On any validation error, remove the partly-written job dir.
    let result = read_upload(&mut multipart, &orig_dir).await;
    let (files, contact, max_width) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(e);
        }
    };

    if let Some((name, email)) = &contact {
        save_lead(&state, name, email, files.len(), id);
        state.notifier.notify_lead(name, email, files.len());
    }

    let meta = JobMeta {
        created_unix: jobs::now_unix(),
        files: files
            .iter()
            .map(|f| FileMeta {
                name: f.name.clone(),
                ext: f.format.extension().into(),
                mime: f.format.mime().into(),
                orig_size: f.orig_size,
            })
            .collect(),
    };
    let meta_path = dir.join("meta.json");
    tokio::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap())
        .await
        .map_err(|e| {
            let msg = format!("could not write {}: {e}", meta_path.display());
            eprintln!("{msg}");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;

    let job = Arc::new(Job {
        id,
        created_unix: meta.created_unix,
        files,
        dir: dir.clone(),
    });
    state.registry.insert(job.clone());

    let out_dir = dir.join("out");
    tokio::fs::create_dir_all(&out_dir).await.map_err(|e| {
        let msg = format!("could not create output directory {}: {e}", out_dir.display());
        eprintln!("{msg}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    for idx in 0..job.files.len() {
        spawn_compression(state.clone(), job.clone(), idx, max_width);
    }

    Ok(Json(job.as_json()))
}

/// Streams multipart fields to disk. Returns file entries, optional contact
/// details, and the optional max-width resize target.
async fn read_upload(
    multipart: &mut Multipart,
    orig_dir: &std::path::Path,
) -> Result<(Vec<Arc<FileEntry>>, Option<(String, String)>, Option<u32>), ApiError> {
    let mut files: Vec<Arc<FileEntry>> = Vec::new();
    let mut name = String::new();
    let mut email = String::new();
    let mut max_width: Option<u32> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| bad(format!("invalid upload: {e}")))?
    {
        match field.name().unwrap_or("") {
            "name" => name = field.text().await.map_err(|e| bad(e.to_string()))?.trim().to_string(),
            "email" => email = field.text().await.map_err(|e| bad(e.to_string()))?.trim().to_string(),
            "max_width" => {
                let raw = field.text().await.map_err(|e| bad(e.to_string()))?;
                match raw.trim().parse::<u32>() {
                    Ok(w) if (16..=8192).contains(&w) => max_width = Some(w),
                    _ => return Err(bad("Max width must be a number between 16 and 8192")),
                }
            }
            // Honeypot: invisible to humans; only bots fill it in.
            "website" => {
                if !field.text().await.unwrap_or_default().trim().is_empty() {
                    return Err(bad("Something went wrong. Please try again."));
                }
            }
            "file" | "files" => {
                if files.len() >= UNLOCKED_MAX_FILES {
                    return Err(bad(format!("Too many files (max {UNLOCKED_MAX_FILES})")));
                }
                let filename = sanitize_filename(field.file_name().unwrap_or("image"));

                // Stream to disk; sniff the real format from the first chunk.
                let tmp_path = orig_dir.join(format!("{}.tmp", files.len()));
                let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|e| {
                    let msg = format!("could not create upload temp file {}: {e}", tmp_path.display());
                    eprintln!("{msg}");
                    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
                })?;
                let mut head: Vec<u8> = Vec::with_capacity(16);
                let mut written: u64 = 0;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| bad(format!("upload interrupted: {e}")))?
                {
                    if head.len() < 16 {
                        head.extend_from_slice(&chunk[..chunk.len().min(16 - head.len())]);
                    }
                    written += chunk.len() as u64;
                    if written > MAX_FILE_BYTES {
                        return Err(bad(format!("“{filename}” is over the 30 MB per-image limit")));
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                }
                file.flush().await.ok();
                drop(file);

                let format = Format::from_magic(&head)
                    .ok_or_else(|| bad(format!("“{filename}” is not a JPG, PNG or WEBP image")))?;
                if written == 0 {
                    return Err(bad(format!("“{filename}” is empty")));
                }

                let final_path = orig_dir.join(format!("{}.{}", files.len(), format.extension()));
                tokio::fs::rename(&tmp_path, &final_path).await.map_err(|e| {
                    let msg = format!(
                        "could not rename upload {} -> {}: {e}",
                        tmp_path.display(),
                        final_path.display()
                    );
                    eprintln!("{msg}");
                    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
                })?;
                files.push(Arc::new(FileEntry::new(filename, format, written)));
            }
            _ => {}
        }
    }

    if files.is_empty() {
        return Err(bad("No images were uploaded"));
    }

    let unlocked = !name.is_empty() && looks_like_email(&email);
    let limit = if unlocked { UNLOCKED_MAX_FILES } else { FREE_MAX_FILES };
    if files.len() > limit {
        return Err(bad(format!(
            "You uploaded {} files — the limit is {FREE_MAX_FILES}. Add your name and email to unlock {UNLOCKED_MAX_FILES} files at a time.",
            files.len()
        )));
    }

    Ok((files, unlocked.then(|| (name, email)), max_width))
}

fn spawn_compression(state: Arc<AppState>, job: Arc<Job>, idx: usize, max_width: Option<u32>) {
    tokio::spawn(async move {
        let permit = state.sem.clone().acquire_owned().await.unwrap();
        let entry = job.files[idx].clone();
        entry.set_status(Status::Working);

        let orig_path = job.orig_path(idx);
        let out_path = job.out_path(idx);
        let format = entry.format;
        let entry_for_progress = entry.clone();

        // Build the tmp path by string append rather than Path::with_extension:
        // with_extension("jpg.tmp") on a filename like "0.jpg" would replace
        // "jpg" with "jpg.tmp" -> "0.jpg.tmp", which happens to be what we
        // want, but the behavior is subtle enough to bite you. Do it plainly.
        let mut out_path_tmp = out_path.clone().into_os_string();
        out_path_tmp.push(".tmp");
        let out_path_tmp = std::path::PathBuf::from(out_path_tmp);
        let out_path_tmp_for_cleanup = out_path_tmp.clone();

        // Hard per-file ceiling. If a codec ever hangs (infinite loop deep in
        // a C library, pathological input), the whole queue would otherwise
        // stop forever. 5 minutes is very generous for a single image on a
        // Pi — well above the p99, well below "the site is down". Note that
        // if the timeout fires, the blocking task is not cancelled (spawn_-
        // blocking tasks can't be); it runs to completion and its result is
        // discarded. That's fine: the semaphore is released, the queue moves
        // on, and only that one thread stays busy until the codec returns.
        let orig_path_for_err = orig_path.clone();
        let out_path_for_err = out_path.clone();
        let out_path_tmp_for_err = out_path_tmp.clone();
        let compression = tokio::task::spawn_blocking(move || {
            let input = std::fs::read(&orig_path).map_err(|e| {
                anyhow::anyhow!("read {}: {e}", orig_path_for_err.display())
            })?;
            let progress = move |f: f32| entry_for_progress.set_progress(f);
            let out = compress::compress(&input, format, max_width, &progress)?;
            // Write to a .tmp and rename, so a mid-write SIGTERM/OOM can never
            // leave a truncated output that load_from_disk would report as
            // "done" with the wrong bytes on the next start.
            std::fs::write(&out_path_tmp, &out.data).map_err(|e| {
                anyhow::anyhow!("write {}: {e}", out_path_tmp_for_err.display())
            })?;
            std::fs::rename(&out_path_tmp, &out_path).map_err(|e| {
                anyhow::anyhow!(
                    "rename {} -> {}: {e}",
                    out_path_tmp_for_err.display(),
                    out_path_for_err.display()
                )
            })?;
            Ok::<_, anyhow::Error>((out.data.len() as u64, out.note))
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(300), compression).await;

        match result {
            Ok(Ok(Ok((size, note)))) => entry.finish(size, note),
            Ok(Ok(Err(e))) => {
                eprintln!("compression failed: job={} idx={} name={} err={e:#}", job.id, idx, entry.name);
                entry.fail(e.to_string());
            }
            Ok(Err(join_err)) => {
                eprintln!("compression thread crashed: job={} idx={} name={} err={join_err}", job.id, idx, entry.name);
                entry.fail("Compression crashed — please try again.".into());
            }
            Err(_) => {
                eprintln!("compression timeout: job={} idx={} name={}", job.id, idx, entry.name);
                entry.fail("Compression took too long and was cancelled — please try again with a smaller image.".into());
            }
        }
        // Originals are only needed as compressor input — drop them right
        // away. Also mop up any leftover .tmp from a failed rename.
        let _ = tokio::fs::remove_file(job.orig_path(idx)).await;
        let _ = tokio::fs::remove_file(&out_path_tmp_for_cleanup).await;
        drop(permit);
    });
}

pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'))
        .take(120)
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "image".into() } else { cleaned }
}

pub fn looks_like_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Leads are appended to data/leads.jsonl — one JSON object per line with
/// timestamp, name, email, file count and job id. Never deleted by the
/// retention sweep.
fn save_lead(state: &AppState, name: &str, email: &str, file_count: usize, job_id: Uuid) {
    let _guard = state.leads.lock().unwrap();
    let line = serde_json::json!({
        "ts": jobs::now_unix(),
        "name": name,
        "email": email,
        "files": file_count,
        "job_id": job_id,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.data_dir.join("leads.jsonl"))
    {
        let _ = writeln!(f, "{line}");
    }
}

// ---------------------------------------------------------------------------
// GET /api/jobs/{id} — status polling
// ---------------------------------------------------------------------------

fn find_job(state: &AppState, id: Uuid) -> Option<Arc<Job>> {
    state.registry.get(id).or_else(|| state.registry.load_from_disk(id))
}

async fn job_status(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = find_job(&state, id).ok_or(ApiError(StatusCode::NOT_FOUND, "job not found or expired".into()))?;
    Ok(Json(job.as_json()))
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

async fn download_file(
    State(state): State<Arc<AppState>>,
    AxPath((id, idx)): AxPath<(Uuid, usize)>,
) -> Result<Response, ApiError> {
    let job = find_job(&state, id).ok_or(ApiError(StatusCode::NOT_FOUND, "job not found or expired".into()))?;
    let entry = job.files.get(idx).ok_or(ApiError(StatusCode::NOT_FOUND, "no such file".into()))?;
    if entry.status() != Status::Done {
        return Err(bad("file is not ready yet"));
    }
    let bytes = tokio::fs::read(job.out_path(idx))
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "file expired".into()))?;

    let download_name = download_filename(&entry.name, entry.format);
    Ok((
        [
            (header::CONTENT_TYPE, entry.format.mime().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", download_name.replace('"', "")),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `photo.png` -> `photo.min.png` so users can keep both versions side by side.
pub fn download_filename(original: &str, format: Format) -> String {
    let stem = match original.rsplit_once('.') {
        Some((stem, ext))
            if matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp") && !stem.is_empty() =>
        {
            stem
        }
        _ => original,
    };
    format!("{stem}.min.{}", format.extension())
}

async fn download_zip(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<Uuid>,
) -> Result<Response, ApiError> {
    let job = find_job(&state, id).ok_or(ApiError(StatusCode::NOT_FOUND, "job not found or expired".into()))?;
    let zip_path = job.dir.join("sw3-images.zip");

    if tokio::fs::metadata(&zip_path).await.is_err() {
        let job2 = job.clone();
        let zip_tmp = job.dir.join("sw3-images.zip.tmp");
        let zip_tmp2 = zip_tmp.clone();
        tokio::task::spawn_blocking(move || build_zip(&job2, &zip_tmp2))
            .await
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "zip build crashed".into()))?
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let _ = tokio::fs::rename(&zip_tmp, &zip_path).await;
    }

    let bytes = tokio::fs::read(&zip_path)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "archive expired".into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"sw3-images-compressed.zip\"".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

fn build_zip(job: &Job, dest: &std::path::Path) -> anyhow::Result<()> {
    // Stored, not deflated: the images are already maximally compressed.
    let file = std::fs::File::create(dest)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut used = std::collections::HashSet::new();

    for (idx, entry) in job.files.iter().enumerate() {
        if entry.status() != Status::Done {
            continue;
        }
        let Ok(bytes) = std::fs::read(job.out_path(idx)) else {
            continue;
        };
        let mut name = download_filename(&entry.name, entry.format);
        let mut n = 2;
        while !used.insert(name.clone()) {
            name = format!("({n}) {}", download_filename(&entry.name, entry.format));
            n += 1;
        }
        zip.start_file(name, opts)?;
        std::io::Write::write_all(&mut zip, &bytes)?;
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_sanitized() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("photo: \"nice\".jpg"), "photo nice.jpg");
        assert_eq!(sanitize_filename(""), "image");
        assert_eq!(sanitize_filename("C:\\Users\\x\\pic.png"), "pic.png");
    }

    #[test]
    fn email_validation() {
        assert!(looks_like_email("michael@sustainablewww.org"));
        assert!(!looks_like_email("nope"));
        assert!(!looks_like_email("a@b"));
        assert!(!looks_like_email("@x.com"));
    }

    #[test]
    fn download_names() {
        assert_eq!(download_filename("photo.jpeg", Format::Jpeg), "photo.min.jpg");
        assert_eq!(download_filename("logo.PNG", Format::Png), "logo.min.png");
        assert_eq!(download_filename("noext", Format::Webp), "noext.min.webp");
    }

    #[test]
    fn rate_limiter_blocks_after_budget() {
        let state = new_state(std::env::temp_dir().join(format!("ecopixel-rl-{}", Uuid::new_v4())));
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        for _ in 0..RATE_LIMIT_JOBS {
            assert!(rate_limit_ok(&state, ip));
        }
        assert!(!rate_limit_ok(&state, ip), "request over budget should be blocked");
        let other: IpAddr = "203.0.113.8".parse().unwrap();
        assert!(rate_limit_ok(&state, other), "other IPs are unaffected");
        let _ = std::fs::remove_dir_all(state.data_dir.clone());
    }
}
