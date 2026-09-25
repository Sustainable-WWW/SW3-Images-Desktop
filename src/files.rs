//! Everything that touches the user's file system: expanding dropped folders
//! into image files, cheap header-only probing for the file list, and writing
//! results into the output folder without ever overwriting anything.
//!
//! All paths here come from drag & drop or native dialogs, and all file
//! contents are untrusted: formats are sniffed from magic bytes, sizes are
//! capped, and output names are sanitized so they can never escape the
//! output folder.

use crate::compress::{self, Format};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Per-file ceiling. Decoding is additionally capped at 90 megapixels by the
/// engine, so this only guards against absurd inputs eating all memory.
pub const MAX_FILE_BYTES: u64 = 300 * 1024 * 1024;

/// How deep dropped folders are searched for images.
const MAX_DEPTH: usize = 32;

/// Header bytes read when probing: enough for the format signature and, for
/// JPEGs, the EXIF orientation tag (which lives in the first APP1 segment).
const PROBE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub name: String,
    pub format: Format,
    /// Display dimensions (EXIF orientation applied).
    pub width: u32,
    pub height: u32,
    pub size: u64,
}

/// Does this file name look like an image we handle? Used only when walking
/// folders; files the user picked explicitly are sniffed regardless.
pub fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
        .unwrap_or(false)
}

/// Expand dropped/picked paths into a flat, de-duplicated list of candidate
/// image files. Folders are walked recursively (hidden entries skipped,
/// symlinked folders not followed so loops are impossible). `exclude` is the
/// output folder: its contents are never re-ingested.
pub fn collect_images(inputs: &[PathBuf], exclude: Option<&Path>) -> Vec<PathBuf> {
    let exclude = exclude.and_then(|p| fs::canonicalize(p).ok());
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for input in inputs {
        let Ok(meta) = fs::metadata(input) else { continue };
        if meta.is_dir() {
            walk(input, exclude.as_deref(), 0, &mut out, &mut seen);
        } else if meta.is_file() {
            push_unique(input, &mut out, &mut seen);
        }
    }
    out
}

fn push_unique(path: &Path, out: &mut Vec<PathBuf>, seen: &mut std::collections::HashSet<PathBuf>) {
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.insert(key) {
        out.push(path.to_path_buf());
    }
}

fn walk(
    dir: &Path,
    exclude: Option<&Path>,
    depth: usize,
    out: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let (Some(ex), Ok(canon)) = (exclude, fs::canonicalize(dir)) {
        if canon == ex {
            return;
        }
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // file_type() does not follow symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            walk(&path, exclude, depth + 1, out, seen);
        } else if (ft.is_file() || ft.is_symlink())
            && has_image_extension(&path)
            && fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false)
        {
            push_unique(&path, out, seen);
        }
    }
}

/// Identify an image cheaply: magic bytes + header-only dimension read. Never
/// decodes pixels, so probing thousands of photos stays fast.
pub fn probe(path: &Path) -> Result<ImageInfo> {
    let meta = fs::metadata(path).context("file not found")?;
    if !meta.is_file() {
        bail!("not a file");
    }
    let size = meta.len();
    if size == 0 {
        bail!("file is empty");
    }
    if size > MAX_FILE_BYTES {
        bail!("file is over the {} MB limit", MAX_FILE_BYTES / 1024 / 1024);
    }
    let mut head = Vec::with_capacity(PROBE_BYTES);
    File::open(path)?.take(PROBE_BYTES as u64).read_to_end(&mut head)?;
    let format = Format::from_magic(&head).context("not a JPG, PNG or WEBP image")?;

    let reader = image::ImageReader::with_format(io::BufReader::new(File::open(path)?), image_format(format));
    let (mut width, mut height) = reader.into_dimensions().context("could not read image dimensions")?;
    if format == Format::Jpeg && matches!(compress::exif_orientation(&head), Some(5..=8)) {
        std::mem::swap(&mut width, &mut height);
    }
    Ok(ImageInfo {
        path: path.to_path_buf(),
        name: display_name(path),
        format,
        width,
        height,
        size,
    })
}

/// Read a whole image for compression, re-validating it: the file may have
/// changed since it was probed.
pub fn read_image(path: &Path) -> Result<(Vec<u8>, Format)> {
    let meta = fs::metadata(path).context("file not found — was it moved or deleted?")?;
    if !meta.is_file() {
        bail!("not a file");
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!("file is over the {} MB limit", MAX_FILE_BYTES / 1024 / 1024);
    }
    let bytes = fs::read(path)?;
    let format = Format::from_magic(&bytes).context("not a JPG, PNG or WEBP image")?;
    Ok((bytes, format))
}

fn image_format(f: Format) -> image::ImageFormat {
    match f {
        Format::Jpeg => image::ImageFormat::Jpeg,
        Format::Png => image::ImageFormat::Png,
        Format::Webp => image::ImageFormat::WebP,
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into())
}

/// Strip anything that could turn a file name into a path or an invalid
/// name on Windows/macOS: separators, drive colons, control characters,
/// reserved device names, leading dots and trailing dots/spaces.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'))
        .take(150)
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        return "image".into();
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// `IMG_4821.JPG` + WebP -> `IMG_4821.webp`. The output folder is separate
/// from the originals, so the original name is kept.
pub fn output_file_name(original: &str, format: Format) -> String {
    let clean = sanitize_filename(original);
    let stem = match clean.rsplit_once('.') {
        Some((stem, ext))
            if matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp") && !stem.is_empty() =>
        {
            stem.to_string()
        }
        _ => clean,
    };
    format!("{stem}.{}", format.extension())
}

/// Write `data` into `dir` under `name`, or `name (2)`, `name (3)`… if taken.
/// `create_new` makes the name reservation atomic, so parallel workers can
/// never clobber each other — nor any existing file, originals included.
pub fn write_unique(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    let name = sanitize_filename(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for n in 1..10_000u32 {
        let candidate = if n == 1 { format!("{stem}{ext}") } else { format!("{stem} ({n}){ext}") };
        let path = dir.join(&candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data).and_then(|_| f.sync_all()) {
                    drop(f);
                    let _ = fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many files with the same name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("sw3-files-{tag}-{nanos}"));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        }));
        let mut buf = io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn filenames_are_sanitized() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("photo: \"nice\".jpg"), "photo nice.jpg");
        assert_eq!(sanitize_filename(""), "image");
        assert_eq!(sanitize_filename(".."), "image");
        assert_eq!(sanitize_filename("C:\\Users\\x\\pic.png"), "pic.png");
        assert_eq!(sanitize_filename(".hidden.png"), "hidden.png");
        assert_eq!(sanitize_filename("trailing. . "), "trailing");
        assert_eq!(sanitize_filename("CON.jpg"), "_CON.jpg");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("a\u{0}b\nc.png"), "abc.png");
    }

    #[test]
    fn output_names() {
        assert_eq!(output_file_name("photo.jpeg", Format::Jpeg), "photo.jpg");
        assert_eq!(output_file_name("IMG_4821.JPG", Format::Webp), "IMG_4821.webp");
        assert_eq!(output_file_name("logo.PNG", Format::Png), "logo.png");
        assert_eq!(output_file_name("noext", Format::Webp), "noext.webp");
        assert_eq!(output_file_name("my.holiday.png", Format::Webp), "my.holiday.webp");
    }

    #[test]
    fn write_unique_never_overwrites() {
        let dir = tmp("unique");
        let a = write_unique(&dir, "a.jpg", b"one").unwrap();
        let b = write_unique(&dir, "a.jpg", b"two").unwrap();
        let c = write_unique(&dir, "a.jpg", b"three").unwrap();
        assert_eq!(a.file_name().unwrap(), "a.jpg");
        assert_eq!(b.file_name().unwrap(), "a (2).jpg");
        assert_eq!(c.file_name().unwrap(), "a (3).jpg");
        assert_eq!(fs::read(&a).unwrap(), b"one", "first file untouched");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_unique_cannot_escape_the_output_folder() {
        let root = tmp("escape");
        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        for evil in ["../evil.jpg", "..\\evil.jpg", "/etc/evil.jpg", "C:\\evil.jpg", "sub/../../evil.jpg"] {
            let p = write_unique(&out, evil, b"x").unwrap();
            assert_eq!(p.parent().unwrap(), out.as_path(), "{evil} escaped to {}", p.display());
        }
        assert!(!root.join("evil.jpg").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parallel_writers_get_distinct_files() {
        let dir = tmp("parallel");
        let paths: Vec<PathBuf> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..16).map(|i| {
                let dir = &dir;
                s.spawn(move || write_unique(dir, "same.png", format!("{i}").as_bytes()).unwrap())
            }).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), 16);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_walks_folders_and_skips_output_and_hidden() {
        let root = tmp("collect");
        fs::create_dir_all(root.join("nested/deeper")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("SW3 Compressed")).unwrap();
        fs::write(root.join("a.jpg"), b"x").unwrap();
        fs::write(root.join("b.PNG"), b"x").unwrap();
        fs::write(root.join("notes.txt"), b"x").unwrap();
        fs::write(root.join(".hidden.jpg"), b"x").unwrap();
        fs::write(root.join("nested/deeper/c.webp"), b"x").unwrap();
        fs::write(root.join(".git/d.png"), b"x").unwrap();
        fs::write(root.join("SW3 Compressed/e.jpg"), b"x").unwrap();

        let found = collect_images(&[root.clone(), root.join("a.jpg")], Some(&root.join("SW3 Compressed")));
        let names: Vec<String> = found.iter().map(|p| display_name(p)).collect();
        assert_eq!(names, vec!["a.jpg", "b.PNG", "c.webp"], "duplicates, hidden, output and non-images skipped");
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_do_not_hang() {
        let root = tmp("loop");
        fs::write(root.join("a.png"), b"x").unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let found = collect_images(std::slice::from_ref(&root), None);
        assert_eq!(found.len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn probe_reads_dimensions_and_rejects_fakes() {
        let dir = tmp("probe");
        let good = dir.join("real.png");
        fs::write(&good, png_bytes(64, 40)).unwrap();
        let info = probe(&good).unwrap();
        assert_eq!((info.width, info.height, info.format), (64, 40, Format::Png));
        assert_eq!(info.name, "real.png");

        // A text file renamed to .jpg is identified by content, not name.
        let fake = dir.join("fake.jpg");
        fs::write(&fake, b"<script>alert(1)</script> definitely not a jpeg").unwrap();
        assert!(probe(&fake).is_err());
        let empty = dir.join("empty.png");
        fs::write(&empty, b"").unwrap();
        assert!(probe(&empty).is_err());
        assert!(probe(&dir).is_err(), "directories are not images");
        assert!(read_image(&fake).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
