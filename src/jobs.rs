//! In-memory job registry with on-disk layout:
//!
//! data/jobs/<uuid>/
//!   meta.json          job metadata (also used for restart recovery + cleanup age)
//!   orig/<idx>.<ext>   uploaded originals (deleted once compressed)
//!   out/<idx>.<ext>    compressed results (kept 24 h)
//!   ecopixel.zip       lazily built "download all" archive

use crate::compress::Format;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const RETENTION_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Working,
    Done,
    Error,
}

pub struct FileEntry {
    pub name: String,
    pub format: Format,
    pub orig_size: u64,
    status: Mutex<Status>,
    error: Mutex<Option<String>>,
    note: Mutex<Option<String>>,
    /// 0..=1000 (per-mille) so it fits an atomic.
    progress: AtomicU32,
    out_size: AtomicU64,
}

impl FileEntry {
    pub fn new(name: String, format: Format, orig_size: u64) -> Self {
        FileEntry {
            name,
            format,
            orig_size,
            status: Mutex::new(Status::Queued),
            error: Mutex::new(None),
            note: Mutex::new(None),
            progress: AtomicU32::new(0),
            out_size: AtomicU64::new(0),
        }
    }

    pub fn set_status(&self, s: Status) {
        *self.status.lock().unwrap() = s;
    }
    pub fn status(&self) -> Status {
        *self.status.lock().unwrap()
    }
    pub fn set_progress(&self, frac: f32) {
        let pm = (frac.clamp(0.0, 1.0) * 1000.0) as u32;
        // Progress only moves forward; concurrent search passes overlap ranges.
        self.progress.fetch_max(pm, Ordering::Relaxed);
    }
    pub fn finish(&self, out_size: u64, note: String) {
        self.out_size.store(out_size, Ordering::Relaxed);
        *self.note.lock().unwrap() = Some(note);
        self.progress.store(1000, Ordering::Relaxed);
        self.set_status(Status::Done);
    }
    pub fn fail(&self, err: String) {
        *self.error.lock().unwrap() = Some(err);
        self.set_status(Status::Error);
    }

    pub fn as_json(&self, index: usize) -> serde_json::Value {
        let status = self.status();
        let mut v = serde_json::json!({
            "index": index,
            "name": self.name,
            "format": self.format,
            "orig_size": self.orig_size,
            "status": status,
            "progress": self.progress.load(Ordering::Relaxed) as f64 / 10.0,
        });
        if status == Status::Done {
            v["out_size"] = self.out_size.load(Ordering::Relaxed).into();
            if let Some(n) = self.note.lock().unwrap().clone() {
                v["note"] = n.into();
            }
        }
        if let Some(e) = self.error.lock().unwrap().clone() {
            v["error"] = e.into();
        }
        v
    }
}

pub struct Job {
    pub id: Uuid,
    pub created_unix: u64,
    pub files: Vec<Arc<FileEntry>>,
    pub dir: PathBuf,
}

impl Job {
    pub fn orig_path(&self, idx: usize) -> PathBuf {
        let f = &self.files[idx];
        self.dir.join("orig").join(format!("{idx}.{}", f.format.extension()))
    }
    pub fn out_path(&self, idx: usize) -> PathBuf {
        let f = &self.files[idx];
        self.dir.join("out").join(format!("{idx}.{}", f.format.extension()))
    }
    pub fn done(&self) -> bool {
        self.files
            .iter()
            .all(|f| matches!(f.status(), Status::Done | Status::Error))
    }
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "job_id": self.id,
            "done": self.done(),
            "expires_unix": self.created_unix + RETENTION_SECS,
            "files": self.files.iter().enumerate().map(|(i, f)| f.as_json(i)).collect::<Vec<_>>(),
        })
    }
}

/// Serialized to meta.json so downloads survive a server restart.
#[derive(Serialize, Deserialize)]
pub struct JobMeta {
    pub created_unix: u64,
    pub files: Vec<FileMeta>,
}

#[derive(Serialize, Deserialize)]
pub struct FileMeta {
    pub name: String,
    pub ext: String,
    pub mime: String,
    pub orig_size: u64,
}

pub struct Registry {
    jobs: RwLock<HashMap<Uuid, Arc<Job>>>,
    pub jobs_dir: PathBuf,
}

impl Registry {
    pub fn new(data_dir: &Path) -> std::io::Result<Registry> {
        let jobs_dir = data_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir)?;
        Ok(Registry {
            jobs: RwLock::new(HashMap::new()),
            jobs_dir,
        })
    }

    pub fn insert(&self, job: Arc<Job>) {
        self.jobs.write().unwrap().insert(job.id, job);
    }

    pub fn get(&self, id: Uuid) -> Option<Arc<Job>> {
        self.jobs.read().unwrap().get(&id).cloned()
    }

    pub fn remove(&self, id: Uuid) {
        self.jobs.write().unwrap().remove(&id);
    }

    /// Recover a job from disk after a restart. Files whose output exists are
    /// reported done; anything else was interrupted.
    pub fn load_from_disk(&self, id: Uuid) -> Option<Arc<Job>> {
        let dir = self.jobs_dir.join(id.to_string());
        let meta: JobMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        let files: Vec<Arc<FileEntry>> = meta
            .files
            .iter()
            .enumerate()
            .map(|(i, fm)| {
                let format = match fm.ext.as_str() {
                    "png" => Format::Png,
                    "webp" => Format::Webp,
                    _ => Format::Jpeg,
                };
                let entry = FileEntry::new(fm.name.clone(), format, fm.orig_size);
                let out = dir.join("out").join(format!("{i}.{}", fm.ext));
                match std::fs::metadata(&out) {
                    Ok(m) => entry.finish(m.len(), "Compressed".into()),
                    Err(_) => entry.fail("Interrupted by server restart — please re-upload".into()),
                }
                Arc::new(entry)
            })
            .collect();
        let job = Arc::new(Job {
            id,
            created_unix: meta.created_unix,
            files,
            dir,
        });
        self.insert(job.clone());
        Some(job)
    }

    /// Delete jobs older than the retention window, on disk and in memory.
    pub fn cleanup_expired(&self) -> usize {
        let now = now_unix();
        let mut removed = 0;
        let Ok(entries) = std::fs::read_dir(&self.jobs_dir) else {
            return 0;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let created = std::fs::read(path.join("meta.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<JobMeta>(&b).ok())
                .map(|m| m.created_unix)
                .or_else(|| {
                    entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                });
            let expired = created.map(|c| now.saturating_sub(c) > RETENTION_SECS).unwrap_or(true);
            if expired {
                if std::fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                }
                if let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|s| s.parse().ok()) {
                    self.remove(id);
                }
            }
        }
        removed
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ecopixel-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_entry_lifecycle() {
        let e = FileEntry::new("a.jpg".into(), Format::Jpeg, 1000);
        assert_eq!(e.status(), Status::Queued);
        e.set_status(Status::Working);
        e.set_progress(0.5);
        let j = e.as_json(0);
        assert_eq!(j["status"], "working");
        assert_eq!(j["progress"], 50.0);
        e.finish(400, "note".into());
        let j = e.as_json(0);
        assert_eq!(j["status"], "done");
        assert_eq!(j["out_size"], 400);
        assert_eq!(j["progress"], 100.0);
        assert_eq!(j["note"], "note");

        let f = FileEntry::new("b.png".into(), Format::Png, 10);
        f.fail("boom".into());
        assert_eq!(f.as_json(1)["error"], "boom");
    }

    #[test]
    fn progress_only_moves_forward() {
        let e = FileEntry::new("a.jpg".into(), Format::Jpeg, 1);
        e.set_progress(0.6);
        e.set_progress(0.3); // late report from an earlier search pass
        assert_eq!(e.as_json(0)["progress"], 60.0);
    }

    #[test]
    fn job_done_requires_all_files_settled() {
        let a = Arc::new(FileEntry::new("a.jpg".into(), Format::Jpeg, 1));
        let b = Arc::new(FileEntry::new("b.jpg".into(), Format::Jpeg, 1));
        let job = Job {
            id: Uuid::new_v4(),
            created_unix: now_unix(),
            files: vec![a.clone(), b.clone()],
            dir: PathBuf::new(),
        };
        assert!(!job.done());
        a.finish(1, String::new());
        assert!(!job.done());
        b.fail("x".into());
        assert!(job.done(), "done + error counts as settled");
    }

    fn write_job_dir(jobs_dir: &Path, id: Uuid, created_unix: u64, with_out: bool) {
        let dir = jobs_dir.join(id.to_string());
        std::fs::create_dir_all(dir.join("out")).unwrap();
        let meta = JobMeta {
            created_unix,
            files: vec![
                FileMeta { name: "one.png".into(), ext: "png".into(), mime: "image/png".into(), orig_size: 10 },
                FileMeta { name: "two.jpg".into(), ext: "jpg".into(), mime: "image/jpeg".into(), orig_size: 20 },
            ],
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        if with_out {
            std::fs::write(dir.join("out").join("0.png"), b"fakepng").unwrap();
        }
    }

    /// The 24-hour retention promise: expired job dirs are really deleted,
    /// fresh ones are kept.
    #[test]
    fn cleanup_deletes_only_expired_jobs() {
        let data = tmp_dir("cleanup");
        let reg = Registry::new(&data).unwrap();
        let old_id = Uuid::new_v4();
        let fresh_id = Uuid::new_v4();
        write_job_dir(&reg.jobs_dir, old_id, now_unix() - RETENTION_SECS - 60, true);
        write_job_dir(&reg.jobs_dir, fresh_id, now_unix() - 60, true);

        let removed = reg.cleanup_expired();
        assert_eq!(removed, 1);
        assert!(!reg.jobs_dir.join(old_id.to_string()).exists(), "expired job must be deleted");
        assert!(reg.jobs_dir.join(fresh_id.to_string()).exists(), "fresh job must survive");
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn restart_recovery_marks_missing_outputs_as_interrupted() {
        let data = tmp_dir("recover");
        let reg = Registry::new(&data).unwrap();
        let id = Uuid::new_v4();
        write_job_dir(&reg.jobs_dir, id, now_unix(), true); // out/0.png exists, out/1.jpg missing

        let job = reg.load_from_disk(id).expect("meta.json should recover the job");
        assert_eq!(job.files[0].status(), Status::Done);
        assert_eq!(job.files[1].status(), Status::Error);
        assert!(reg.get(id).is_some(), "recovered job is cached in memory");
        std::fs::remove_dir_all(&data).unwrap();
    }
}
