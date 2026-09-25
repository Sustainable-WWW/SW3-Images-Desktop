//! SW3 Images compression engine.
//!
//! For every image the engine *measures* perceptual quality instead of
//! trusting a fixed "quality 80" setting: each candidate encoding is compared
//! with the original using SSIMULACRA 2 (see `quality.rs`), and the smallest
//! file that reaches [`TARGET_SCORE`] wins. If no re-encoding beats the
//! original file, the original bytes are returned untouched.
//!
//! Color is treated as part of quality:
//! - the metric works in the XYB color space, so chroma loss counts;
//! - embedded ICC color profiles (Display P3 phone photos, Adobe RGB...) are
//!   carried into the output, so colors don't shift or desaturate;
//! - JPEG chroma subsampling (4:2:0) is only used when it passes the metric,
//!   otherwise full-resolution chroma (4:4:4) is used; WebP uses libwebp's
//!   sharp (more accurate) RGB→YUV conversion;
//! - resizing happens in linear light with premultiplied alpha.

use crate::{color, meta, quality, resize};
use anyhow::{anyhow, bail, Context, Result};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, Rgba, RgbaImage};

/// SSIMULACRA 2 score every re-encoded image must reach. 80 is the metric's
/// "impossible to distinguish from the original in a side-by-side comparison
/// at 1:1" level (90 is reserved for flicker tests of pixel peepers).
pub const TARGET_SCORE: f64 = 80.0;

/// Images are judged at the size they are seen without zooming in: up to
/// this long edge (the width of a Full HD screen). Smaller images are judged
/// 1:1. Retina screens don't change this: they show more device pixels, but
/// each is too small to see, so what matters is the size on screen.
pub const DISPLAY_EDGE: u32 = 1920;

/// Refuse absurd inputs before allocating pixel buffers.
pub const MAX_PIXELS: u64 = 90_000_000; // 90 MP

/// Upper bound for any single decoder allocation.
const MAX_DECODE_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

impl Format {
    pub fn from_magic(bytes: &[u8]) -> Option<Format> {
        if bytes.len() < 12 {
            return None;
        }
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Format::Png)
        } else if bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Format::Webp)
        } else {
            None
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::Webp => "webp",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
            Format::Webp => "image/webp",
        }
    }

    fn image_format(self) -> image::ImageFormat {
        match self {
            Format::Jpeg => image::ImageFormat::Jpeg,
            Format::Png => image::ImageFormat::Png,
            Format::Webp => image::ImageFormat::WebP,
        }
    }
}

pub struct Compressed {
    pub data: Vec<u8>,
    /// Human-readable description of what the engine decided.
    pub note: String,
    /// Format of `data` (differs from the input when converting to WebP).
    pub format: Format,
    /// Pixel dimensions of `data` (after EXIF orientation and resizing).
    pub width: u32,
    pub height: u32,
}

/// What to do with an image besides compressing it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Options {
    /// Downscale images wider than this (never upscales, keeps aspect ratio).
    pub max_width: Option<u32>,
    /// Re-encode JPEG/PNG input as WebP.
    pub to_webp: bool,
}

/// Progress callback, called with values in `0.0..=1.0`.
pub type Progress<'a> = &'a (dyn Fn(f32) + Send + Sync);

/// Compress keeping the input format. See [`compress_with`].
pub fn compress(input: &[u8], format: Format, max_width: Option<u32>, progress: Progress) -> Result<Compressed> {
    compress_with(input, format, &Options { max_width, to_webp: false }, progress)
}

pub fn compress_with(input: &[u8], format: Format, opts: &Options, progress: Progress) -> Result<Compressed> {
    progress(0.02);
    let mut decoder = open_decoder(input, format)?;
    let (raw_w, raw_h) = decoder.dimensions();
    let pixels = raw_w as u64 * raw_h as u64;
    if pixels > MAX_PIXELS {
        bail!("image is too large ({} megapixels; the maximum is {})", pixels / 1_000_000, MAX_PIXELS / 1_000_000);
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let (width, height) = oriented(raw_w, raw_h, orientation);

    let resize_to = opts.max_width.and_then(|mw| resize::fit_width(width, height, mw.clamp(16, 8192)));
    let out_format = if opts.to_webp { Format::Webp } else { format };
    let converted = out_format != format;
    let transformed = converted || resize_to.is_some();

    // Re-encoding these would silently destroy something (the animation, or
    // the colors of a print-oriented CMYK file), so they are never touched.
    let protected = if format == Format::Png && meta::png_is_animated(input) {
        Some("Animated PNG")
    } else if format == Format::Webp && meta::webp_is_animated(input) {
        Some("Animated WebP")
    } else if format == Format::Jpeg && meta::jpeg_components(input) == Some(4) {
        Some("CMYK JPEG")
    } else {
        None
    };
    if let Some(kind) = protected {
        if transformed {
            bail!("{kind} files can't be resized or converted without losing quality — this one was left out");
        }
        return Ok(Compressed {
            data: input.to_vec(),
            note: format!("{kind} — kept original"),
            format,
            width,
            height,
        });
    }

    let icc = decoder.icc_profile().ok().flatten().filter(|p| !p.is_empty());
    let mut img = DynamicImage::from_decoder(decoder).context("could not decode image")?;
    img.apply_orientation(orientation);
    progress(0.08);

    if let Some((w, h)) = resize_to {
        img = resize::resize(&img, w, h)?;
    }
    progress(0.12);

    // Color profiles worth keeping: anything that isn't plain sRGB (which
    // browsers assume anyway), within a sane size.
    let icc = icc.filter(|p| p.len() <= meta::MAX_ICC_BYTES && !meta::icc_is_srgb(p));
    let p3 = icc.as_deref().is_some_and(meta::icc_is_display_p3);
    let judge = Judge::new(&img, p3)?;
    progress(0.20);

    let original = if transformed { None } else { Some(input) };
    let mut evals = 0u32;
    let mut tick = || {
        evals += 1;
        progress(0.20 + 0.78 * (1.0 - 0.8f32.powi(evals as i32)));
    };
    let encoded = match out_format {
        Format::Jpeg => encode_jpeg(&img, &judge, icc.as_deref(), original, &mut tick)?,
        Format::Png => {
            let chunks = if format == Format::Png { meta::png_color_chunks(input) } else { Vec::new() };
            let gray_profile = icc.as_deref().is_some_and(|p| meta::icc_space(p) == meta::IccSpace::Gray);
            encode_png(&img, &judge, &chunks, gray_profile, original, &mut tick)?
        }
        Format::Webp => encode_webp(&img, &judge, icc.as_deref(), &mut tick)?,
    };
    progress(1.0);

    let (w, h) = (img.width(), img.height());
    let done = |data: Vec<u8>, note: String| Compressed { data, note, format: out_format, width: w, height: h };
    let resized_note = || resize_to.map(|(w, h)| format!("Resized to {w}×{h} · ")).unwrap_or_default();
    if converted {
        return Ok(done(encoded.data, format!("{}Converted to {}", resized_note(), encoded.note)));
    }
    if resize_to.is_some() {
        return Ok(done(encoded.data, format!("{}{}", resized_note(), encoded.note)));
    }
    // Never ship a file bigger than what the user already had.
    if encoded.data.len() >= input.len() {
        Ok(done(input.to_vec(), "Already optimal — kept original".into()))
    } else {
        Ok(done(encoded.data, encoded.note))
    }
}

/// Perceptual score (SSIMULACRA 2, judged like the engine judges) of
/// `candidate` against `original`. Both must have the same dimensions.
pub fn perceptual_score(original: &DynamicImage, candidate: &DynamicImage) -> Result<f64> {
    Judge::new(original, false)?.score(candidate)
}

/// Display dimensions (EXIF orientation applied) read from the file header
/// only — no pixels are decoded. Works on a prefix of the file as long as it
/// contains the headers.
pub fn header_dimensions(bytes: &[u8], format: Format) -> Result<(u32, u32)> {
    let mut decoder = open_decoder(bytes, format)?;
    let (w, h) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    Ok(oriented(w, h, orientation))
}

fn open_decoder(input: &[u8], format: Format) -> Result<impl ImageDecoder + '_> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(input), format.image_format());
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader.into_decoder().context("could not decode image")
}

fn oriented(w: u32, h: u32, o: Orientation) -> (u32, u32) {
    match o {
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    }
}

/// Encoder output before the top-level bookkeeping in [`compress_with`].
struct Encoded {
    data: Vec<u8>,
    note: String,
}

// ---------------------------------------------------------------------------
// Judging
// ---------------------------------------------------------------------------

/// Scores candidates against one original: the lower of the SSIMULACRA 2
/// score (artifacts, detail, chroma structure) and the color score (casts
/// and shifted colors, see `color.rs`). Transparent images are judged on
/// both a white and a black background (the worse score counts), so errors
/// in semi-transparent areas can't hide; invisible pixels don't matter.
struct Judge {
    width: u32,
    height: u32,
    judged: (u32, u32),
    /// Nearest-neighbor upscale factor for images smaller than the metric's
    /// minimum size (values are preserved exactly).
    zoom: usize,
    p3: bool,
    refs: Vec<(Option<u8>, quality::Reference, color::ColorRef)>,
}

fn judge_dims(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= DISPLAY_EDGE {
        return (w, h);
    }
    let scale = DISPLAY_EDGE as f64 / long as f64;
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let c = i as f64 / 255.0;
            *v = (if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }) as f32;
        }
        t
    })
}

impl Judge {
    fn new(img: &DynamicImage, p3: bool) -> Result<Judge> {
        let (width, height) = (img.width(), img.height());
        let judged = judge_dims(width, height);
        let zoom = quality::MIN_SIDE.div_ceil(judged.0.min(judged.1).max(1) as usize).max(1);
        let backgrounds: Vec<Option<u8>> = if has_transparency(img) { vec![Some(255), Some(0)] } else { vec![None] };
        let mut judge = Judge { width, height, judged, zoom, p3, refs: Vec::new() };
        for bg in backgrounds {
            let (lin, w, h) = judge.linear(img, bg)?;
            let colors = color::ColorRef::new(&lin, w, h);
            let reference = quality::Reference::new(lin, w, h).ok_or_else(|| anyhow!("could not build quality reference"))?;
            judge.refs.push((bg, reference, colors));
        }
        Ok(judge)
    }

    /// Flatten onto the background, bring to judging size, and convert to
    /// linear-light RGB with BT.709 primaries.
    fn linear(&self, img: &DynamicImage, bg: Option<u8>) -> Result<(Vec<[f32; 3]>, usize, usize)> {
        let flat = match bg {
            None => img.to_rgb8(),
            Some(b) => {
                let rgba = img.to_rgba8();
                image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                    let p = rgba.get_pixel(x, y).0;
                    let a = p[3] as u32;
                    let mix = |c: u8| ((c as u32 * a + b as u32 * (255 - a) + 127) / 255) as u8;
                    image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
                })
            }
        };
        let flat = if self.judged != (self.width, self.height) {
            resize::resize_for_judging(&DynamicImage::ImageRgb8(flat), self.judged.0, self.judged.1)?.to_rgb8()
        } else {
            flat
        };
        let lut = srgb_to_linear_lut();
        let (fw, fh) = (flat.width() as usize, flat.height() as usize);
        let (w, h) = (fw * self.zoom, fh * self.zoom);
        let raw = flat.as_raw();
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = (y / self.zoom) * fw;
            for x in 0..w {
                let i = (row + x / self.zoom) * 3;
                let (r, g, b) = (lut[raw[i] as usize], lut[raw[i + 1] as usize], lut[raw[i + 2] as usize]);
                out.push(if self.p3 {
                    // Display P3 → linear sRGB/BT.709 primaries (D65).
                    [
                        1.224_940_2 * r - 0.224_940_4 * g,
                        -0.042_056_9 * r + 1.042_057_1 * g,
                        -0.019_637_6 * r - 0.078_636_1 * g + 1.098_273_5 * b,
                    ]
                } else {
                    [r, g, b]
                });
            }
        }
        Ok((out, w, h))
    }

    fn score(&self, candidate: &DynamicImage) -> Result<f64> {
        if (candidate.width(), candidate.height()) != (self.width, self.height) {
            bail!("candidate has the wrong dimensions");
        }
        let mut worst = f64::INFINITY;
        for (bg, reference, colors) in &self.refs {
            let (lin, _, _) = self.linear(candidate, *bg)?;
            worst = worst.min(colors.score(&lin, TARGET_SCORE));
            worst = worst.min(reference.score(lin).ok_or_else(|| anyhow!("could not score candidate"))?);
        }
        Ok(worst)
    }

    fn score_encoded(&self, bytes: &[u8], format: Format) -> Result<f64> {
        let img = image::load_from_memory_with_format(bytes, format.image_format())
            .context("could not decode a candidate encoding")?;
        self.score(&img)
    }
}

// ---------------------------------------------------------------------------
// Quality search
// ---------------------------------------------------------------------------

struct Eval {
    q: u8,
    score: f64,
    bytes: Vec<u8>,
}

/// Find the lowest quality in `lo..=hi` whose encode reaches TARGET_SCORE.
///
/// Score rises roughly linearly with encoder quality, so instead of a fixed
/// bisection the search steps by the estimated distance to the target and
/// then interpolates between the best failing and passing qualities: usually
/// 3-5 encodes. Returns None if even `hi` misses the target.
fn search(
    lo: u8,
    hi: u8,
    start: u8,
    mut encode: impl FnMut(u8) -> Result<Vec<u8>>,
    mut score: impl FnMut(&[u8]) -> Result<f64>,
    tick: &mut dyn FnMut(),
) -> Result<Option<Eval>> {
    const SLOPE: f64 = 0.5; // typical score gain per quality step
    let mut pass: Option<Eval> = None;
    let mut fail: Option<(u8, f64)> = None;
    let mut q = start.clamp(lo, hi);
    for _ in 0..8 {
        let bytes = encode(q)?;
        let s = score(&bytes)?;
        tick();
        if s >= TARGET_SCORE {
            if pass.as_ref().is_none_or(|p| q < p.q) {
                pass = Some(Eval { q, score: s, bytes });
            }
        } else if fail.is_none_or(|(fq, _)| q > fq) {
            fail = Some((q, s));
        }
        let next = match (&pass, fail) {
            (Some(p), Some((fq, fs))) => {
                if p.q <= fq.saturating_add(1) {
                    break;
                }
                let est = fq as f64 + (TARGET_SCORE - fs) * (p.q - fq) as f64 / (p.score - fs).max(1e-6);
                (est.ceil() as i64).clamp(fq as i64 + 1, p.q as i64 - 1) as u8
            }
            (Some(p), None) => {
                if p.q <= lo {
                    break;
                }
                let step = ((p.score - TARGET_SCORE) / SLOPE).floor().max(1.0) as i64;
                (p.q as i64 - step).max(lo as i64) as u8
            }
            (None, Some((fq, fs))) => {
                if fq >= hi {
                    break;
                }
                let step = ((TARGET_SCORE - fs) / SLOPE).ceil().max(1.0) as i64;
                (fq as i64 + step).min(hi as i64) as u8
            }
            (None, None) => unreachable!("every evaluation either passes or fails"),
        };
        if next == q {
            break;
        }
        q = next;
    }
    Ok(pass)
}

// ---------------------------------------------------------------------------
// JPEG — mozjpeg with trellis quantization + progressive scans
// ---------------------------------------------------------------------------

fn encode_jpeg(
    img: &DynamicImage,
    judge: &Judge,
    icc: Option<&[u8]>,
    original: Option<&[u8]>,
    tick: &mut dyn FnMut(),
) -> Result<Encoded> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let space = icc.map(meta::icc_space);
    let rgb_profile = icc.filter(|_| space == Some(meta::IccSpace::Rgb));
    let gray_profile = icc.filter(|_| space == Some(meta::IccSpace::Gray));
    // Single-channel JPEGs are ~1/3 smaller. Only when every pixel is gray
    // and no RGB profile gives those grays a different meaning.
    let try_gray = rgb_profile.is_none() && is_grayscale(img);

    let mut attempt = |gray: bool| -> Result<Option<(Eval, &'static str)>> {
        let px = if gray { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() };
        let profile = if gray { gray_profile } else { rgb_profile };
        let enc = |q: u8, full_chroma: bool| encode_mozjpeg(&px, w, h, gray, q, full_chroma, profile);
        let first = search(40, 95, 80, |q| enc(q, false), |b| judge.score_encoded(b, Format::Jpeg), tick)?;
        if gray {
            return Ok(first.map(|e| (e, "grayscale")));
        }
        // Fine colored detail (red text, logos, saturated edges) can defeat
        // 4:2:0 chroma subsampling; full-resolution chroma then often gives
        // a smaller file at a lower quality setting.
        let second = if first.as_ref().is_none_or(|e| e.q >= 84) {
            let start = first.as_ref().map_or(85, |e| e.q.saturating_sub(12).max(50));
            search(50, 98, start, |q| enc(q, true), |b| judge.score_encoded(b, Format::Jpeg), tick)?
        } else {
            None
        };
        Ok(match (first, second) {
            (Some(a), Some(b)) if b.bytes.len() < a.bytes.len() => Some((b, "4:4:4")),
            (Some(a), _) => Some((a, "4:2:0")),
            (None, Some(b)) => Some((b, "4:4:4")),
            (None, None) => None,
        })
    };

    let mut found = if try_gray { attempt(true)? } else { None };
    if found.is_none() {
        found = attempt(false)?;
    }
    match found {
        Some((e, mode)) => Ok(Encoded {
            data: e.bytes,
            note: format!("JPEG q{} {mode}, progressive · quality score {:.0}", e.q, e.score),
        }),
        None => match original {
            Some(orig) => Ok(Encoded { data: orig.to_vec(), note: "Kept original (re-encoding would cost quality)".into() }),
            // Resized or converted: there is no original to fall back to.
            None => Ok(Encoded {
                data: encode_mozjpeg(&img.to_rgb8().into_raw(), w, h, false, 98, true, rgb_profile)?,
                note: "JPEG q98 4:4:4".into(),
            }),
        },
    }
}

fn encode_mozjpeg(
    pixels: &[u8],
    w: usize,
    h: usize,
    gray: bool,
    quality: u8,
    full_chroma: bool,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>> {
    std::panic::catch_unwind(|| -> Result<Vec<u8>> {
        let space = if gray { mozjpeg::ColorSpace::JCS_GRAYSCALE } else { mozjpeg::ColorSpace::JCS_RGB };
        let mut comp = mozjpeg::Compress::new(space);
        comp.set_size(w, h);
        comp.set_quality(quality as f32);
        comp.set_progressive_mode();
        if full_chroma && !gray {
            comp.set_chroma_sampling_pixel_sizes((1, 1), (1, 1));
        }
        let mut started = comp.start_compress(Vec::new())?;
        if let Some(profile) = icc {
            write_jpeg_icc(&mut started, profile);
        }
        started.write_scanlines(pixels)?;
        Ok(started.finish()?)
    })
    .map_err(|_| anyhow!("JPEG encoder panicked"))?
}

/// Embed an ICC profile as APP2 `ICC_PROFILE` markers. Chunk numbers are
/// 1-based per the ICC spec; readers (libjpeg-turbo in browsers, zune-jpeg)
/// discard profiles numbered from 0, which the `mozjpeg` crate's own helper
/// writes — so it is not used.
fn write_jpeg_icc<W>(started: &mut mozjpeg::compress::CompressStarted<W>, profile: &[u8]) {
    const MAX_CHUNK: usize = 65_533 - 14; // marker payload limit minus header
    let chunks: Vec<&[u8]> = profile.chunks(MAX_CHUNK).collect();
    if chunks.is_empty() || chunks.len() > 255 {
        return;
    }
    let total = chunks.len() as u8;
    for (i, chunk) in chunks.iter().enumerate() {
        let mut buf = Vec::with_capacity(chunk.len() + 14);
        buf.extend_from_slice(b"ICC_PROFILE\0");
        buf.push(i as u8 + 1);
        buf.push(total);
        buf.extend_from_slice(chunk);
        started.write_marker(mozjpeg::Marker::APP(2), &buf);
    }
}

/// Every pixel gray within JPEG decode noise. Exhaustive: a small colored
/// logo on a black-and-white photo must not be turned gray.
fn is_grayscale(img: &DynamicImage) -> bool {
    use image::ColorType::*;
    if matches!(img.color(), L8 | L16 | La8 | La16) {
        return true;
    }
    img.to_rgb8().pixels().all(|p| {
        let (r, g, b) = (p[0] as i16, p[1] as i16, p[2] as i16);
        (r - g).abs() <= 3 && (g - b).abs() <= 3 && (r - b).abs() <= 3
    })
}

// ---------------------------------------------------------------------------
// PNG — lossless (oxipng), 8-bit reduction, and palette quantization
// ---------------------------------------------------------------------------

fn encode_png(
    img: &DynamicImage,
    judge: &Judge,
    color_chunks: &meta::Chunks,
    gray_profile: bool,
    original: Option<&[u8]>,
    tick: &mut dyn FnMut(),
) -> Result<Encoded> {
    let with_color = |png: Vec<u8>| meta::png_with_chunks(&png, color_chunks).unwrap_or(png);
    let mut candidates: Vec<Encoded> = Vec::new();

    // Lossless: the original bytes (or a fresh encode of the resized pixels)
    // recompressed. Pixel-exact by construction.
    let src = match original {
        Some(orig) => orig.to_vec(),
        None => with_color(plain_png(img)?),
    };
    let lossless = oxipng_optimize(&src).unwrap_or(src);
    tick();
    candidates.push(Encoded { data: lossless, note: "PNG lossless (oxipng)".into() });

    // 16 bits per channel is invisible on 8-bit (and most 10-bit) displays.
    if img.color().bytes_per_pixel() / img.color().channel_count() > 1 {
        let img8 = if has_transparency(img) { DynamicImage::ImageRgba8(img.to_rgba8()) } else { DynamicImage::ImageRgb8(img.to_rgb8()) };
        let s = judge.score(&img8)?;
        tick();
        if s >= TARGET_SCORE {
            let png = with_color(plain_png(&img8)?);
            candidates.push(Encoded { data: oxipng_optimize(&png).unwrap_or(png), note: format!("PNG 8-bit · quality score {s:.0}") });
        }
    }

    // Palette quantization: often dramatically smaller for graphics and
    // screenshots. Skipped when the image already fits a lossless palette
    // (oxipng finds that exactly), and for gray-profiled images (a palette
    // can't carry a gray ICC profile).
    if !gray_profile && !has_at_most_colors(img, 256) {
        if let Some((png, note)) = quantize_png(img, judge, tick)? {
            let png = with_color(png);
            candidates.push(Encoded { data: oxipng_optimize(&png).unwrap_or(png), note });
        }
    }

    Ok(candidates.into_iter().min_by_key(|c| c.data.len()).expect("lossless candidate always exists"))
}

fn plain_png(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)?;
    Ok(buf.into_inner())
}

/// Palette candidates at several dithering levels (heavy dither hides
/// banding in photos, none keeps flat graphics crisp); each must pass the
/// perceptual gate, the smallest survivor wins.
fn quantize_png(img: &DynamicImage, judge: &Judge, tick: &mut dyn FnMut()) -> Result<Option<(Vec<u8>, String)>> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let qimg = quantizr::Image::new(rgba.as_raw(), w, h).map_err(|e| anyhow!("quantizer: {e:?}"))?;
    let mut opts = quantizr::Options::default();
    opts.set_max_colors(255).map_err(|e| anyhow!("quantizer: {e:?}"))?;
    let mut result = quantizr::QuantizeResult::quantize(&qimg, &opts);

    let mut best: Option<(Vec<u8>, String)> = None;
    let mut indexes = vec![0u8; w * h];
    for dither in [1.0f32, 0.5, 0.0] {
        result.set_dithering_level(dither).map_err(|e| anyhow!("quantizer: {e:?}"))?;
        result.remap_image(&qimg, &mut indexes).map_err(|e| anyhow!("quantizer: {e:?}"))?;
        let pal = result.get_palette();
        let entries: Vec<rgb::RGBA8> = pal.entries[..pal.count as usize].iter().map(|c| rgb::RGBA8::new(c.r, c.g, c.b, c.a)).collect();
        let recon = RgbaImage::from_fn(w as u32, h as u32, |x, y| {
            let c = entries[indexes[y as usize * w + x as usize] as usize];
            Rgba([c.r, c.g, c.b, c.a])
        });
        let s = judge.score(&DynamicImage::ImageRgba8(recon))?;
        tick();
        if s < TARGET_SCORE {
            continue;
        }
        let mut enc = lodepng::Encoder::new();
        enc.set_palette(&entries)?;
        let png = enc.encode(&indexes, w, h)?;
        if best.as_ref().is_none_or(|(b, _)| png.len() < b.len()) {
            best = Some((png, format!("PNG {} colors · quality score {s:.0}", entries.len())));
        }
    }
    Ok(best)
}

fn oxipng_optimize(png: &[u8]) -> Result<Vec<u8>> {
    let mut opts = oxipng::Options::from_preset(if png.len() < 4 * 1024 * 1024 { 4 } else { 2 });
    // Keep only chunks that affect how the image displays (color profile,
    // gamma...), drop metadata.
    opts.strip = oxipng::StripChunks::Safe;
    opts.optimize_alpha = true;
    opts.timeout = Some(std::time::Duration::from_secs(60));
    // Zopfli squeezes out a few more percent; only worth its CPU on small files.
    if png.len() < 128 * 1024 {
        opts.deflate = oxipng::Deflaters::Zopfli { iterations: std::num::NonZeroU8::new(15).unwrap() };
    }
    Ok(oxipng::optimize_from_memory(png, &opts)?)
}

fn has_at_most_colors(img: &DynamicImage, max: usize) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(max + 1);
    for p in img.to_rgba8().pixels() {
        if seen.insert(p.0) && seen.len() > max {
            return false;
        }
    }
    true
}

fn has_transparency(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(b) => b.pixels().any(|p| p[3] < 255),
        DynamicImage::ImageLumaA8(b) => b.pixels().any(|p| p[1] < 255),
        DynamicImage::ImageRgba16(b) => b.pixels().any(|p| p[3] < u16::MAX),
        DynamicImage::ImageLumaA16(b) => b.pixels().any(|p| p[1] < u16::MAX),
        other if other.color().has_alpha() => other.to_rgba8().pixels().any(|p| p[3] < 255),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// WebP — lossy quality search, near-lossless and lossless for graphics
// ---------------------------------------------------------------------------

fn encode_webp(img: &DynamicImage, judge: &Judge, icc: Option<&[u8]>, tick: &mut dyn FnMut()) -> Result<Encoded> {
    let (w, h) = (img.width(), img.height());
    let alpha = has_transparency(img);
    let px = if alpha { img.to_rgba8().into_raw() } else { img.to_rgb8().into_raw() };
    let layout = if alpha { webp::PixelLayout::Rgba } else { webp::PixelLayout::Rgb };
    let encoder = webp::Encoder::new(&px, layout, w, h);
    let pixels = w as u64 * h as u64;
    // Lossless effort (libwebp's quality doubles as effort in lossless
    // mode): maximum on small images, cheaper presets as they grow.
    let (lossless_quality, lossless_method) = match pixels {
        0..=1_000_000 => (100.0, 6),
        1_000_001..=3_000_000 => (90.0, 5),
        _ => (75.0, 4),
    };

    let run = |lossless: bool, quality: f32, near_lossless: i32, method: i32| -> Result<Vec<u8>> {
        let mut c = webp::WebPConfig::new().map_err(|_| anyhow!("webp config init failed"))?;
        c.quality = quality;
        c.method = method;
        c.alpha_quality = 100;
        if lossless {
            c.lossless = 1;
            c.near_lossless = near_lossless;
        } else {
            // Accurate RGB→YUV conversion: much less color bleeding and
            // desaturation on saturated edges than libwebp's default.
            c.use_sharp_yuv = 1;
        }
        let mem = encoder.encode_advanced(&c).map_err(|e| anyhow!("webp encode failed: {e:?}"))?;
        Ok(mem.to_vec())
    };
    let score = |b: &[u8]| judge.score_encoded(b, Format::Webp);

    let mut candidates: Vec<Encoded> = Vec::new();
    // Search at a mid effort level (~3x faster), then spend maximum effort
    // once at the chosen quality: usually smaller at the same score. It is
    // re-verified like every other candidate.
    let mut lossy = search(40, 100, 82, |q| run(false, q as f32, 100, 4), score, tick)?;
    if let Some(e) = &lossy {
        let data = run(false, e.q as f32, 100, 6)?;
        if data.len() < e.bytes.len() {
            let s = score(&data)?;
            tick();
            if s >= TARGET_SCORE {
                lossy = Some(Eval { q: e.q, score: s, bytes: data });
            }
        }
    }
    let graphic = likely_graphic(img);
    let explore = graphic || lossy.as_ref().is_none_or(|e| e.q >= 92);
    if let Some(e) = lossy {
        candidates.push(Encoded { data: e.bytes, note: format!("WebP q{} · quality score {:.0}", e.q, e.score) });
    }
    if explore {
        // Near-lossless keeps full color resolution (no 4:2:0) — the fix
        // for content lossy WebP can't carry, like fine colored text.
        for level in [60, 80] {
            let data = run(true, lossless_quality, level, lossless_method)?;
            let s = score(&data)?;
            tick();
            if s >= TARGET_SCORE {
                candidates.push(Encoded { data, note: format!("WebP near-lossless · quality score {s:.0}") });
                break;
            }
        }
        if graphic || candidates.is_empty() {
            let data = run(true, lossless_quality, 100, lossless_method)?;
            tick();
            candidates.push(Encoded { data, note: "WebP lossless".into() });
        }
    }
    let mut best = candidates.into_iter().min_by_key(|c| c.data.len()).expect("a WebP candidate always exists");
    if let Some(profile) = icc.filter(|p| meta::icc_space(p) == meta::IccSpace::Rgb) {
        best.data = meta::webp_with_icc(&best.data, profile).ok_or_else(|| anyhow!("could not embed color profile"))?;
    }
    Ok(best)
}

/// Heuristic: sampled unique-color count is low => logo/screenshot/graphic.
fn likely_graphic(img: &DynamicImage) -> bool {
    let rgba = img.to_rgba8();
    let mut colors = std::collections::HashSet::new();
    let step = (rgba.pixels().len() / 20_000).max(1);
    for p in rgba.pixels().step_by(step) {
        colors.insert(p.0);
        if colors.len() > 1500 {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::tests::make_profile;
    use image::{ImageBuffer, Rgb};

    /// Deterministic photo-like image: gradients, shapes, mild noise.
    fn photo(w: u32, h: u32) -> DynamicImage {
        let mut seed = 0x9e3779b9u32;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let base_r = 60.0 + 140.0 * fx + 20.0 * (fy * 12.0).sin();
            let base_g = 90.0 + 110.0 * fy;
            let base_b = 130.0 + 80.0 * ((fx + fy) * 6.0).cos();
            let n = (rng() % 9) as f32 - 4.0;
            let circle = if (fx - 0.5).powi(2) + (fy - 0.5).powi(2) < 0.04 { 45.0 } else { 0.0 };
            Rgb([
                (base_r + n + circle).clamp(0.0, 255.0) as u8,
                (base_g + n).clamp(0.0, 255.0) as u8,
                (base_b + n - circle).clamp(0.0, 255.0) as u8,
            ])
        }))
    }

    fn encode(img: &DynamicImage, fmt: image::ImageFormat) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, fmt).unwrap();
        buf.into_inner()
    }

    fn jpeg_with_icc(img: &DynamicImage, icc: &[u8]) -> Vec<u8> {
        let rgb = img.to_rgb8();
        encode_mozjpeg(rgb.as_raw(), rgb.width() as usize, rgb.height() as usize, false, 95, true, Some(icc)).unwrap()
    }

    fn icc_of(bytes: &[u8]) -> Option<Vec<u8>> {
        let fmt = Format::from_magic(bytes).unwrap();
        open_decoder(bytes, fmt).unwrap().icc_profile().unwrap()
    }

    fn decode(bytes: &[u8]) -> DynamicImage {
        image::load_from_memory(bytes).unwrap()
    }

    const NOOP: Progress = &|_| {};

    /// The core promise, re-verified from scratch on the output.
    fn assert_meets_target(original: &DynamicImage, out: &[u8]) {
        let s = perceptual_score(original, &decode(out)).unwrap();
        assert!(s >= TARGET_SCORE - 1e-9, "score {s} below target");
    }

    #[test]
    fn magic_sniffing() {
        assert_eq!(Format::from_magic(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0]), Some(Format::Jpeg));
        let png = encode(&photo(16, 16), image::ImageFormat::Png);
        assert_eq!(Format::from_magic(&png), Some(Format::Png));
        let webp = webp::Encoder::from_rgba(photo(16, 16).to_rgba8().as_raw(), 16, 16).encode(90.0).to_vec();
        assert_eq!(Format::from_magic(&webp), Some(Format::Webp));
        assert_eq!(Format::from_magic(b"GIF89a-not-supported"), None);
    }

    #[test]
    fn jpeg_photo_shrinks_and_meets_the_quality_target() {
        let img = photo(320, 240);
        let input = encode_mozjpeg(img.to_rgb8().as_raw(), 320, 240, false, 97, true, None).unwrap();
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(out.data.len() < input.len(), "expected shrink, got {} -> {}", input.len(), out.data.len());
        assert_meets_target(&decode(&input), &out.data);
        assert_eq!((out.width, out.height), (320, 240));
    }

    #[test]
    fn grayscale_jpeg_uses_single_channel() {
        let gray = DynamicImage::ImageLuma8(ImageBuffer::from_fn(200, 150, |x, y| image::Luma([((x * 255 / 200 + y) % 255) as u8])));
        let input = encode(&DynamicImage::ImageRgb8(gray.to_rgb8()), image::ImageFormat::Jpeg);
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(out.note.contains("grayscale") || out.note.contains("original"), "note: {}", out.note);
    }

    #[test]
    fn a_small_colored_detail_keeps_a_photo_in_color() {
        // Black-and-white photo with one small red mark: must stay color.
        let mut img = DynamicImage::ImageLuma8(ImageBuffer::from_fn(300, 200, |x, y| image::Luma([((x + y) % 256) as u8]))).to_rgb8();
        for y in 97..103 {
            for x in 147..153 {
                img.put_pixel(x, y, Rgb([220, 20, 30]));
            }
        }
        let img = DynamicImage::ImageRgb8(img);
        assert!(!is_grayscale(&img));
        let out = compress(&encode(&img, image::ImageFormat::Png), Format::Png, None, NOOP).unwrap();
        let p = decode(&out.data).to_rgb8().get_pixel(150, 100).0;
        assert!(p[0] > 150 && p[1] < 80, "red mark must survive, got {p:?}");
    }

    #[test]
    fn fine_colored_detail_gets_full_resolution_chroma() {
        // Thin saturated strokes on a contrasting background: 4:2:0 can't
        // reach the target, 4:4:4 can.
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(240, 160, |x, y| {
            if (x / 2) % 3 == 0 && (y / 8) % 2 == 0 { Rgb([230, 20, 30]) } else { Rgb([30, 60, 200]) }
        }));
        let input = encode_mozjpeg(img.to_rgb8().as_raw(), 240, 160, false, 99, true, None).unwrap();
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(!out.note.contains("4:2:0"), "note: {}", out.note);
        assert_meets_target(&decode(&input), &out.data);
    }

    #[test]
    fn png_graphic_shrinks_a_lot() {
        let img = ImageBuffer::from_fn(300, 200, |x, y| {
            let c = match (x / 50 + y / 40) % 4 {
                0 => [255u8, 255, 255, 255],
                1 => [22, 163, 74, 255],
                2 => [20, 35, 26, 255],
                _ => [220, 240, 225, 255],
            };
            Rgba(c)
        });
        let input = encode(&DynamicImage::ImageRgba8(img), image::ImageFormat::Png);
        let out = compress(&input, Format::Png, None, NOOP).unwrap();
        assert!(out.data.len() < input.len() / 2, "expected big shrink, got {} -> {}", input.len(), out.data.len());
        assert_eq!(Format::from_magic(&out.data), Some(Format::Png));
        assert_eq!(perceptual_score(&decode(&input), &decode(&out.data)).unwrap(), 100.0, "few colors: lossless palette");
    }

    #[test]
    fn png_photo_meets_the_quality_target() {
        let img = photo(280, 200);
        let input = encode(&img, image::ImageFormat::Png);
        let out = compress(&input, Format::Png, None, NOOP).unwrap();
        assert!(out.data.len() <= input.len());
        assert_meets_target(&img, &out.data);
    }

    #[test]
    fn sixteen_bit_png_is_reduced_to_eight_bits() {
        let img = DynamicImage::ImageRgb16(ImageBuffer::from_fn(120, 80, |x, y| Rgb([(x * 500) as u16, (y * 800) as u16, 30_000])));
        let input = encode(&img, image::ImageFormat::Png);
        let out = compress(&input, Format::Png, None, NOOP).unwrap();
        assert!(out.data.len() < input.len() / 2, "{} -> {}", input.len(), out.data.len());
        assert_meets_target(&img, &out.data);
    }

    #[test]
    fn webp_photo_shrinks() {
        let img = photo(320, 240);
        let input = webp::Encoder::from_rgba(img.to_rgba8().as_raw(), 320, 240).encode(98.0).to_vec();
        let out = compress(&input, Format::Webp, None, NOOP).unwrap();
        assert!(out.data.len() < input.len(), "expected shrink, got {} -> {}", input.len(), out.data.len());
        assert_eq!(Format::from_magic(&out.data), Some(Format::Webp));
    }

    #[test]
    fn exif_orientation_is_applied() {
        let img = photo(320, 240);
        let plain = encode(&img, image::ImageFormat::Jpeg);
        // Splice a minimal APP1/EXIF segment (orientation = 6) after SOI.
        #[rustfmt::skip]
        let app1: Vec<u8> = vec![
            0xFF, 0xE1, 0x00, 0x22,                          // APP1, length 34
            b'E', b'x', b'i', b'f', 0x00, 0x00,              // Exif header
            b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00,  // TIFF LE, IFD @8
            0x01, 0x00,                                      // 1 entry
            0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,  // tag 0x0112, SHORT, count 1
            0x06, 0x00, 0x00, 0x00,                          // value 6 (rotate 90 CW)
            0x00, 0x00, 0x00, 0x00,                          // no next IFD
        ];
        let mut with_exif = plain[..2].to_vec();
        with_exif.extend_from_slice(&app1);
        with_exif.extend_from_slice(&plain[2..]);
        let out = compress(&with_exif, Format::Jpeg, None, NOOP).unwrap();
        let decoded = decode(&out.data);
        assert_eq!((decoded.width(), decoded.height()), (240, 320), "orientation should rotate pixels");
        assert_eq!((out.width, out.height), (240, 320));
    }

    #[test]
    fn malformed_exif_never_panics() {
        let plain = encode(&photo(64, 48), image::ImageFormat::Jpeg);
        for len in [2u16, 5, 8, 9, 0xFFFF] {
            let mut j = plain[..2].to_vec();
            j.extend_from_slice(&[0xFF, 0xE1]);
            j.extend_from_slice(&len.to_be_bytes());
            j.extend_from_slice(b"Exif\0\0II*\0\xFF\xFF\xFF\x7F");
            j.extend_from_slice(&plain[2..]);
            let _ = compress(&j, Format::Jpeg, None, NOOP); // Ok or Err, never a panic
        }
    }

    #[test]
    fn max_width_resizes_before_compressing() {
        let img = photo(400, 300);
        let input = encode(&img, image::ImageFormat::Jpeg);
        let out = compress(&input, Format::Jpeg, Some(200), NOOP).unwrap();
        let dec = decode(&out.data);
        assert_eq!((dec.width(), dec.height()), (200, 150), "aspect ratio preserved");
        assert!(out.note.contains("Resized to 200×150"), "note: {}", out.note);

        // Never upscales: a max width larger than the image is a no-op.
        let out = compress(&input, Format::Jpeg, Some(4000), NOOP).unwrap();
        assert_eq!((out.width, out.height), (400, 300));
        assert!(!out.note.contains("Resized"), "note: {}", out.note);
    }

    #[test]
    fn never_inflates_a_file() {
        let img = photo(64, 48);
        let first = compress(&encode(&img, image::ImageFormat::Jpeg), Format::Jpeg, None, NOOP).unwrap();
        let second = compress(&first.data, Format::Jpeg, None, NOOP).unwrap();
        assert!(second.data.len() <= first.data.len());
    }

    #[test]
    fn jpeg_converts_to_webp_and_meets_the_quality_target() {
        let img = photo(320, 240);
        let input = encode_mozjpeg(img.to_rgb8().as_raw(), 320, 240, false, 97, true, None).unwrap();
        let opts = Options { max_width: None, to_webp: true };
        let out = compress_with(&input, Format::Jpeg, &opts, NOOP).unwrap();
        assert_eq!(out.format, Format::Webp);
        assert_eq!(Format::from_magic(&out.data), Some(Format::Webp));
        assert!(out.note.starts_with("Converted to WebP"), "note: {}", out.note);
        assert_meets_target(&decode(&input), &out.data);
    }

    #[test]
    fn png_with_alpha_converts_to_webp_and_resizes() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(400, 200, |x, y| {
            Rgba([(x % 255) as u8, (y % 255) as u8, 120, if x < 200 { 255 } else { 0 }])
        }));
        let input = encode(&img, image::ImageFormat::Png);
        let opts = Options { max_width: Some(200), to_webp: true };
        let out = compress_with(&input, Format::Png, &opts, NOOP).unwrap();
        assert_eq!(out.format, Format::Webp);
        assert_eq!((out.width, out.height), (200, 100));
        let dec = image::load_from_memory_with_format(&out.data, image::ImageFormat::WebP).unwrap();
        assert!(dec.color().has_alpha(), "transparency must survive the conversion");
        assert!(out.note.contains("Resized to 200×100"), "note: {}", out.note);
    }

    #[test]
    fn wide_gamut_color_profiles_are_kept_in_every_format() {
        let p3 = make_profile([[0.5151, 0.2412, -0.0011], [0.2920, 0.6922, 0.0419], [0.1571, 0.0666, 0.7841]], true);
        let img = photo(160, 120);
        let jpeg = jpeg_with_icc(&img, &p3);

        let out = compress(&jpeg, Format::Jpeg, Some(100), NOOP).unwrap();
        assert_eq!(icc_of(&out.data).as_deref(), Some(&p3[..]), "JPEG keeps the P3 profile");

        let to_webp = compress_with(&jpeg, Format::Jpeg, &Options { max_width: None, to_webp: true }, NOOP).unwrap();
        assert_eq!(icc_of(&to_webp.data).as_deref(), Some(&p3[..]), "WebP gets the P3 profile");

        let png = meta::png_with_chunks(&encode(&img, image::ImageFormat::Png), &[meta::png_iccp(&p3)]).unwrap();
        let out = compress(&png, Format::Png, Some(100), NOOP).unwrap();
        assert_eq!(icc_of(&out.data).as_deref(), Some(&p3[..]), "PNG keeps the P3 profile");
    }

    #[test]
    fn plain_srgb_profiles_are_dropped() {
        let srgb = make_profile([[0.4361, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]], true);
        let out = compress(&jpeg_with_icc(&photo(160, 120), &srgb), Format::Jpeg, Some(100), NOOP).unwrap();
        assert_eq!(icc_of(&out.data), None, "sRGB is the default; the profile only costs bytes");
    }

    #[test]
    fn animated_and_cmyk_images_are_never_damaged() {
        let png = encode(&photo(40, 30), image::ImageFormat::Png);
        let apng = meta::png_with_chunks(&png, &[(*b"acTL", vec![0, 0, 0, 2, 0, 0, 0, 0])]).unwrap();
        let out = compress(&apng, Format::Png, None, NOOP).unwrap();
        assert_eq!(out.data, apng, "animation kept byte-for-byte");
        assert!(compress(&apng, Format::Png, Some(20), NOOP).is_err(), "resizing would drop frames");

        let cmyk_px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 200) as u8, 40, 80, 10]).collect();
        let cmyk = std::panic::catch_unwind(|| {
            let mut c = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_CMYK);
            c.set_size(40, 30);
            let mut s = c.start_compress(Vec::new()).unwrap();
            s.write_scanlines(&cmyk_px).unwrap();
            s.finish().unwrap()
        })
        .unwrap();
        assert_eq!(meta::jpeg_components(&cmyk), Some(4));
        let out = compress(&cmyk, Format::Jpeg, None, NOOP).unwrap();
        assert_eq!(out.data, cmyk);
        assert!(compress_with(&cmyk, Format::Jpeg, &Options { max_width: None, to_webp: true }, NOOP).is_err());
    }

    #[test]
    fn tiny_images_work_in_every_format() {
        for (w, h) in [(1, 1), (3, 5), (7, 7)] {
            let img = photo(w, h);
            let png = encode(&img, image::ImageFormat::Png);
            compress(&png, Format::Png, None, NOOP).unwrap();
            compress_with(&png, Format::Png, &Options { max_width: None, to_webp: true }, NOOP).unwrap();
            let jpeg = encode(&img, image::ImageFormat::Jpeg);
            compress(&jpeg, Format::Jpeg, None, NOOP).unwrap();
        }
    }

    #[test]
    fn oversized_images_are_rejected_before_decoding() {
        let mut png = encode(&photo(8, 8), image::ImageFormat::Png);
        // Patch IHDR to claim 100 000 × 100 000 pixels (10 gigapixels).
        png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        let crc = meta::crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        let t = std::time::Instant::now();
        let err = compress(&png, Format::Png, None, NOOP).err().expect("must be rejected");
        assert!(format!("{err:#}").contains("too large"), "{err:#}");
        // Decoding 10 gigapixels would take minutes (or fail to allocate);
        // generous bound so a busy CI machine can't make this flaky.
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "rejected from the header alone");
    }

    #[test]
    fn garbage_input_is_an_error_not_a_panic() {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.extend(std::iter::repeat_n(0x42, 500));
        assert!(compress(&bytes, Format::Jpeg, None, NOOP).is_err());
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend(std::iter::repeat_n(0, 64));
        assert!(compress(&png, Format::Png, None, NOOP).is_err());
        assert!(compress(b"RIFF\x10\0\0\0WEBPVP8 \x04\0\0\0\0\0\0\0", Format::Webp, None, NOOP).is_err());
    }

    #[test]
    fn color_casts_fail_even_when_structure_is_perfect() {
        let img = photo(200, 150);
        let cast = DynamicImage::ImageRgb8(image::RgbImage::from_fn(200, 150, |x, y| {
            let p = img.to_rgb8().get_pixel(x, y).0;
            Rgb([(p[0] as f32 * 0.97) as u8, p[1], p[2]])
        }));
        let s = perceptual_score(&img, &cast).unwrap();
        assert!(s < TARGET_SCORE, "a 3% red cast must not pass (score {s})");
        assert!(perceptual_score(&img, &img).unwrap() > 99.9);
    }

    #[test]
    fn search_finds_the_lowest_passing_quality() {
        // Synthetic monotone score curve: passes from q=73 up.
        let curve = |q: u8| 44.0 + q as f64 * 0.5;
        let mut calls = 0;
        let found = search(
            40,
            95,
            80,
            |q| Ok(vec![q]),
            |b| {
                calls += 1;
                Ok(curve(b[0]))
            },
            &mut || {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.q, 72, "44 + 72*0.5 = 80 is the first pass");
        assert!(calls <= 5, "took {calls} evaluations");
        let none = search(40, 95, 80, |q| Ok(vec![q]), |_| Ok(10.0), &mut || {}).unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn judging_size() {
        assert_eq!(judge_dims(800, 600), (800, 600));
        assert_eq!(judge_dims(1920, 1080), (1920, 1080));
        assert_eq!(judge_dims(4000, 2250), (1920, 1080));
        assert_eq!(judge_dims(3000, 6000), (960, 1920));
    }
}
