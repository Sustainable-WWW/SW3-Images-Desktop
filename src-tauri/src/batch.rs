//! Parallel batch compression. Independent of Tauri so the whole pipeline —
//! queueing, parallel workers, memory budgeting, cancellation, output
//! writing — is unit tested.

use serde::Serialize;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;
use sw3_images::compress::{self, Format, Options};
use sw3_images::files;

/// Images compressed at the same time. A native app can use the machine:
/// up to 5 in parallel, leaving one core free so the UI stays responsive.
/// Memory is bounded separately by the [`MemoryBudget`].
pub fn worker_count() -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    cores.saturating_sub(1).clamp(1, 5)
}

const MB: u64 = 1024 * 1024;

/// Peak memory of compressing one image, from measurements (decoding,
/// encoder buffers, quality judging; large PNGs are the hungriest):
/// roughly 130 bytes per pixel plus fixed overhead.
pub fn memory_cost(pixels: u64) -> u64 {
    64 * MB + pixels * 130
}

/// Caps the memory all workers may use at once, so five large photos in
/// parallel can't exhaust RAM. Reservations are served strictly in order:
/// a big image waits its turn instead of being starved by small ones.
pub struct MemoryBudget {
    total: u64,
    state: Mutex<BudgetState>,
    changed: Condvar,
}

struct BudgetState {
    free: u64,
    next_ticket: u64,
    serving: u64,
}

pub struct Reservation<'a> {
    budget: &'a MemoryBudget,
    amount: u64,
}

impl MemoryBudget {
    pub fn new(total: u64) -> MemoryBudget {
        MemoryBudget {
            total,
            state: Mutex::new(BudgetState { free: total, next_ticket: 0, serving: 0 }),
            changed: Condvar::new(),
        }
    }

    /// 40% of physical RAM, between 1.5 GB and 16 GB.
    pub fn for_this_machine() -> MemoryBudget {
        let ram = {
            use sysinfo::{MemoryRefreshKind, RefreshKind, System};
            System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()))
                .total_memory()
        };
        let total = if ram == 0 { 3 * 1024 * MB } else { (ram / 10 * 4).clamp(1536 * MB, 16 * 1024 * MB) };
        MemoryBudget::new(total)
    }

    /// Block until `cost` bytes are available (capped at the whole budget,
    /// so an image bigger than the budget still runs — alone).
    pub fn reserve(&self, cost: u64) -> Reservation<'_> {
        let amount = cost.min(self.total);
        let mut st = self.state.lock().unwrap();
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        while st.serving != ticket || st.free < amount {
            st = self.changed.wait(st).unwrap();
        }
        st.free -= amount;
        st.serving += 1;
        drop(st);
        self.changed.notify_all();
        Reservation { budget: self, amount }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.state.lock().unwrap().free += self.amount;
        self.budget.changed.notify_all();
    }
}

/// The app-wide budget shared by batches and single-file retries.
pub fn global_budget() -> &'static MemoryBudget {
    static BUDGET: OnceLock<MemoryBudget> = OnceLock::new();
    BUDGET.get_or_init(MemoryBudget::for_this_machine)
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: u32,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    #[serde(rename_all = "camelCase")]
    Started { id: u32 },
    #[serde(rename_all = "camelCase")]
    Progress { id: u32, progress: f32 },
    #[serde(rename_all = "camelCase")]
    Done {
        id: u32,
        out_name: String,
        out_size: u64,
        orig_size: u64,
        width: u32,
        height: u32,
        format: Format,
        note: String,
    },
    #[serde(rename_all = "camelCase")]
    Failed { id: u32, error: String },
    #[serde(rename_all = "camelCase")]
    Cancelled { id: u32 },
    #[serde(rename_all = "camelCase")]
    Finished(Summary),
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub done: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub orig_bytes: u64,
    pub out_bytes: u64,
    pub elapsed_ms: u64,
}

/// Compress every item into `out_dir` using `workers` threads. Blocks until
/// all items are settled; emits events from worker threads as it goes.
pub fn run(
    items: Vec<Item>,
    opts: Options,
    out_dir: &Path,
    workers: usize,
    cancel: &AtomicBool,
    budget: &MemoryBudget,
    emit: &(dyn Fn(Event) + Sync),
) -> Summary {
    let t0 = Instant::now();
    let queue = Mutex::new(VecDeque::from(items));
    let summary = Mutex::new(Summary::default());

    std::thread::scope(|s| {
        for _ in 0..workers.max(1) {
            s.spawn(|| loop {
                let Some(item) = queue.lock().unwrap().pop_front() else { break };
                if cancel.load(Ordering::Relaxed) {
                    summary.lock().unwrap().cancelled += 1;
                    emit(Event::Cancelled { id: item.id });
                    continue;
                }
                let ev = process(&item, &opts, out_dir, budget, emit);
                {
                    let mut sm = summary.lock().unwrap();
                    match &ev {
                        Event::Done { orig_size, out_size, .. } => {
                            sm.done += 1;
                            sm.orig_bytes += orig_size;
                            sm.out_bytes += out_size;
                        }
                        _ => sm.failed += 1,
                    }
                }
                emit(ev);
            });
        }
    });

    let mut summary = summary.into_inner().unwrap();
    summary.elapsed_ms = t0.elapsed().as_millis() as u64;
    summary
}

/// Compress one file. Always returns `Done` or `Failed` — panics inside the
/// codecs are caught so one bad image can never take down the batch.
pub fn process(item: &Item, opts: &Options, out_dir: &Path, budget: &MemoryBudget, emit: &(dyn Fn(Event) + Sync)) -> Event {
    // Reserve memory before starting; the header read is cheap. Unreadable
    // files reserve the minimum and fail properly below.
    let pixels = files::probe(&item.path).map(|i| i.width as u64 * i.height as u64).unwrap_or(0);
    let _reservation = budget.reserve(memory_cost(pixels));
    emit(Event::Started { id: item.id });
    let id = item.id;
    let last = AtomicU32::new(0);
    let progress = |f: f32| {
        // Only forward whole-percent steps to keep IPC traffic small.
        let pct = (f.clamp(0.0, 1.0) * 100.0) as u32;
        if pct > last.fetch_max(pct, Ordering::Relaxed) {
            emit(Event::Progress { id, progress: pct as f32 / 100.0 });
        }
    };

    let result = (|| -> anyhow::Result<Event> {
        let (input, format) = files::read_image(&item.path)?;
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| compress::compress_with(&input, format, opts, &progress)))
            .map_err(|_| anyhow::anyhow!("the image could not be processed (codec crashed)"))??;
        let name = files::output_file_name(&files::display_name(&item.path), out.format);
        let written = files::write_unique(out_dir, &name, &out.data)
            .map_err(|e| anyhow::anyhow!("could not save to the output folder: {e}"))?;
        Ok(Event::Done {
            id,
            out_name: files::display_name(&written),
            out_size: out.data.len() as u64,
            orig_size: input.len() as u64,
            width: out.width,
            height: out.height,
            format: out.format,
            note: out.note,
        })
    })();

    result.unwrap_or_else(|e| Event::Failed { id, error: friendly(&e) })
}

fn friendly(e: &anyhow::Error) -> String {
    let msg = format!("{e:#}");
    let mut chars = msg.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Unknown error".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("sw3-batch-{tag}-{nanos}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_jpeg(path: &Path, w: u32, h: u32, seed: u32) {
        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(w, h, |x, y| {
            let v = (x * 3 + y * 5 + seed * 17) % 256;
            image::Rgb([v as u8, ((x + seed) % 256) as u8, ((y * 2) % 256) as u8])
        }));
        img.save_with_format(path, image::ImageFormat::Jpeg).unwrap();
    }

    type Log = Mutex<Vec<Event>>;

    fn push(log: &Log, e: Event) {
        log.lock().unwrap().push(e);
    }

    fn budget() -> MemoryBudget {
        MemoryBudget::new(8 * 1024 * MB)
    }

    #[test]
    fn memory_budget_limits_concurrency_and_serves_in_order() {
        let budget = MemoryBudget::new(100);
        let in_use = std::sync::atomic::AtomicU64::new(0);
        let peak = std::sync::atomic::AtomicU64::new(0);
        let order = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for (i, cost) in [60u64, 60, 10, 10, 250, 10].into_iter().enumerate() {
                let (budget, in_use, peak, order) = (&budget, &in_use, &peak, &order);
                s.spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(i as u64 * 5));
                    let r = budget.reserve(cost);
                    let now = in_use.fetch_add(r.amount, Ordering::SeqCst) + r.amount;
                    peak.fetch_max(now, Ordering::SeqCst);
                    order.lock().unwrap().push(i);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    in_use.fetch_sub(r.amount, Ordering::SeqCst);
                });
            }
        });
        assert!(peak.load(Ordering::SeqCst) <= 100, "never more than the budget in use");
        assert_eq!(order.into_inner().unwrap(), vec![0, 1, 2, 3, 4, 5], "first come, first served; oversize runs alone");
    }

    #[test]
    fn memory_cost_scales_with_pixels() {
        assert!(memory_cost(12_000_000) > 1_500 * MB && memory_cost(12_000_000) < 1_700 * MB);
        assert!(memory_cost(0) > 0);
        assert!(MemoryBudget::for_this_machine().total >= 1536 * MB);
    }

    #[test]
    fn worker_count_is_between_one_and_five() {
        let n = worker_count();
        assert!((1..=5).contains(&n), "{n}");
    }

    #[test]
    fn batch_compresses_in_parallel_and_writes_all_outputs() {
        let src = tmp("src");
        let out = tmp("out");
        let mut items = Vec::new();
        for i in 0..8u32 {
            let p = src.join(format!("photo-{i}.jpg"));
            write_jpeg(&p, 240, 160, i);
            items.push(Item { id: i, path: p });
        }
        // Same name twice from different folders must not collide.
        std::fs::create_dir_all(src.join("other")).unwrap();
        let dup = src.join("other/photo-0.jpg");
        write_jpeg(&dup, 240, 160, 99);
        items.push(Item { id: 100, path: dup });

        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let events = Log::default();
        let emit = |e: Event| {
            match &e {
                Event::Started { .. } => {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                Event::Done { .. } | Event::Failed { .. } => {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
                _ => {}
            }
            push(&events, e);
        };
        let summary = run(items, Options::default(), &out, 4, &AtomicBool::new(false), &budget(), &emit);

        assert_eq!(summary.done, 9);
        assert_eq!(summary.failed, 0);
        assert!(peak.load(Ordering::SeqCst) > 1, "images should be compressed concurrently");
        assert!(peak.load(Ordering::SeqCst) <= 4, "never more workers than requested");
        let written: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
        assert_eq!(written.len(), 9);
        assert!(out.join("photo-0.jpg").exists() && out.join("photo-0 (2).jpg").exists());
        // Originals are never modified.
        assert!(src.join("photo-3.jpg").exists());
        let evs = events.lock().unwrap();
        assert!(evs.iter().any(|e| matches!(e, Event::Progress { .. })), "progress is reported");
        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn webp_conversion_writes_webp_files() {
        let src = tmp("webp-src");
        let out = tmp("webp-out");
        let p = src.join("IMG_1.JPG");
        write_jpeg(&p, 400, 300, 1);
        let opts = Options { max_width: Some(200), to_webp: true };
        let events = Log::default();
        let summary = run(vec![Item { id: 1, path: p }], opts, &out, 2, &AtomicBool::new(false), &budget(), &|e| push(&events, e));
        assert_eq!(summary.done, 1);
        let bytes = std::fs::read(out.join("IMG_1.webp")).unwrap();
        assert_eq!(Format::from_magic(&bytes), Some(Format::Webp));
        let evs = events.lock().unwrap();
        let done = evs.iter().find(|e| matches!(e, Event::Done { .. })).unwrap();
        if let Event::Done { width, height, format, out_name, .. } = done {
            assert_eq!((*width, *height, *format), (200, 150, Format::Webp));
            assert_eq!(out_name, "IMG_1.webp");
        }
        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn bad_files_fail_individually_without_stopping_the_batch() {
        let src = tmp("bad-src");
        let out = tmp("bad-out");
        let good = src.join("good.jpg");
        write_jpeg(&good, 120, 80, 3);
        let fake = src.join("fake.jpg");
        std::fs::write(&fake, b"#!/bin/sh\necho this is not an image").unwrap();
        let corrupt = src.join("corrupt.jpg");
        let mut bytes = std::fs::read(&good).unwrap();
        bytes.truncate(300);
        std::fs::write(&corrupt, &bytes).unwrap();
        let items = vec![
            Item { id: 1, path: fake },
            Item { id: 2, path: corrupt },
            Item { id: 3, path: src.join("missing.jpg") },
            Item { id: 4, path: good },
        ];
        let events = Log::default();
        let summary = run(items, Options::default(), &out, 2, &AtomicBool::new(false), &budget(), &|e| push(&events, e));
        assert_eq!((summary.done, summary.failed), (1, 3));
        let evs = events.lock().unwrap();
        for id in [1, 2, 3] {
            assert!(evs.iter().any(|e| matches!(e, Event::Failed { id: i, error } if *i == id && !error.is_empty())));
        }
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "failed files leave nothing behind");
        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn cancel_skips_queued_items() {
        let src = tmp("cancel-src");
        let out = tmp("cancel-out");
        let items: Vec<Item> = (0..6u32)
            .map(|i| {
                let p = src.join(format!("{i}.jpg"));
                write_jpeg(&p, 64, 64, i);
                Item { id: i, path: p }
            })
            .collect();
        let cancel = AtomicBool::new(false);
        let events = Log::default();
        let emit = |e: Event| {
            if matches!(e, Event::Done { .. }) {
                cancel.store(true, Ordering::SeqCst); // user hits Stop after the first file
            }
            push(&events, e);
        };
        let summary = run(items, Options::default(), &out, 1, &cancel, &budget(), &emit);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.cancelled, 5);
        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn output_dir_equal_to_source_dir_never_overwrites_originals() {
        let dir = tmp("same");
        let p = dir.join("keep.jpg");
        write_jpeg(&p, 200, 120, 5);
        let original = std::fs::read(&p).unwrap();
        let summary = run(vec![Item { id: 1, path: p.clone() }], Options::default(), &dir, 1, &AtomicBool::new(false), &budget(), &|_| {});
        assert_eq!(summary.done, 1);
        assert_eq!(std::fs::read(&p).unwrap(), original, "original must be byte-identical");
        assert!(dir.join("keep (2).jpg").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn events_serialize_for_the_ui() {
        let e = Event::Progress { id: 3, progress: 0.5 };
        assert_eq!(serde_json::to_value(&e).unwrap(), serde_json::json!({"kind": "progress", "id": 3, "progress": 0.5}));
        let e = Event::Finished(Summary { done: 2, ..Default::default() });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "finished");
        assert_eq!(v["done"], 2);
        assert_eq!(v["elapsedMs"], 0);
    }
}
