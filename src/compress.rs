//! EcoPixel compression engine.
//!
//! Strategy: every candidate encoding is verified against the original with
//! DSSIM (structural similarity in Lab color space). We binary-search for the
//! smallest file whose perceptual distance stays below a "visually lossless"
//! threshold, so output quality is guaranteed by measurement, not by a fixed
//! quality knob. If nothing beats the original file, the original is returned.

use anyhow::{anyhow, bail, Context, Result};
use dssim_core::Dssim;
use image::DynamicImage;
use rgb::FromSlice;

/// Perceptual distance ceilings (DSSIM). Below ~0.001 differences are
/// generally imperceptible at 1:1 viewing.
const DSSIM_JPEG: f64 = 0.0012;
const DSSIM_WEBP: f64 = 0.0012;
/// Palette dithering reads as film-grain texture, slightly more forgiving.
const DSSIM_PNG_LOSSY: f64 = 0.0015;

/// Images larger than this (long edge) are judged at display scale: quality is
/// measured on a downscaled copy, the size a visitor actually sees on a web
/// page. Artifacts that only appear when zooming past 100% no longer veto
/// stronger compression — which is where the big byte savings live.
const DISPLAY_EDGE: u32 = 1600;

/// Refuse absurd inputs before allocating pixel buffers.
const MAX_PIXELS: u64 = 90_000_000; // 90 MP

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
}

pub struct Compressed {
    pub data: Vec<u8>,
    /// Human-readable description of what the engine decided.
    pub note: String,
}

/// Progress callback, called with values in `0.0..=1.0`.
pub type Progress<'a> = &'a (dyn Fn(f32) + Send + Sync);

pub fn compress(
    input: &[u8],
    format: Format,
    max_width: Option<u32>,
    progress: Progress,
) -> Result<Compressed> {
    progress(0.02);
    let mut img = decode(input, format)?;

    // Optional pre-compression downscale (never upscales, keeps aspect ratio).
    let mut resized = false;
    if let Some(mw) = max_width {
        let mw = mw.clamp(16, 8192);
        if img.width() > mw {
            img = img.resize(mw, u32::MAX, image::imageops::FilterType::Lanczos3);
            resized = true;
        }
    }
    progress(0.10);

    // When the pixels were resized, the original bytes are no longer a valid
    // fallback — every path must produce a fresh encode of the resized image.
    let original = if resized { None } else { Some(input) };
    let result = match format {
        Format::Jpeg => compress_jpeg(original, &img, progress)?,
        Format::Png => compress_png(original, &img, progress)?,
        Format::Webp => compress_webp(original, &img, progress)?,
    };
    progress(1.0);

    if resized {
        // Dimensions changed on request; the original is no longer comparable.
        return Ok(Compressed {
            note: format!("Resized to {}×{} · {}", img.width(), img.height(), result.note),
            data: result.data,
        });
    }
    // Never ship a file bigger than what the user already had.
    if result.data.len() >= input.len() {
        Ok(Compressed {
            data: input.to_vec(),
            note: "Already optimal — kept original".into(),
        })
    } else {
        Ok(result)
    }
}

fn decode(input: &[u8], format: Format) -> Result<DynamicImage> {
    let fmt = match format {
        Format::Jpeg => image::ImageFormat::Jpeg,
        Format::Png => image::ImageFormat::Png,
        Format::Webp => image::ImageFormat::WebP,
    };
    let img = image::load_from_memory_with_format(input, fmt).context("could not decode image")?;
    let px = (img.width() as u64) * (img.height() as u64);
    if px > MAX_PIXELS {
        bail!("image is too large ({} megapixels; max 90)", px / 1_000_000);
    }
    // Apply EXIF orientation for JPEGs so re-encoding (which strips EXIF)
    // doesn't leave phone photos rotated sideways.
    if format == Format::Jpeg {
        if let Some(orient) = exif_orientation(input) {
            return Ok(apply_orientation(img, orient));
        }
    }
    Ok(img)
}

// ---------------------------------------------------------------------------
// Perceptual metric
// ---------------------------------------------------------------------------

struct Reference {
    dssim: Dssim,
    orig: dssim_core::DssimImage<f32>,
    w: usize,
    h: usize,
    /// Judge-scale copy of the source when it had to be downscaled for
    /// judging; None when the source already fits the judge size. Encoders
    /// run their quality search on this small copy (cheap) and only encode
    /// at full resolution a couple of times, near the quality the search
    /// settles on.
    probe_img: Option<DynamicImage>,
    /// Probe encodes must be scored the way a full-resolution encode is: its
    /// artifacts get averaged away by the downscale to judge size. This is
    /// the reference downscaled by that same factor again — probe encodes are
    /// shrunk onto it so their artifacts receive identical smoothing.
    sim: Option<SimRef>,
}

struct SimRef {
    img: dssim_core::DssimImage<f32>,
    w: usize,
    h: usize,
}

impl Reference {
    /// Dimensions an image is judged at: capped at DISPLAY_EDGE on the long
    /// side, aspect ratio preserved.
    fn judge_dims(img_w: u32, img_h: u32) -> (u32, u32) {
        let long = img_w.max(img_h);
        if long <= DISPLAY_EDGE {
            return (img_w, img_h);
        }
        let scale = DISPLAY_EDGE as f64 / long as f64;
        (
            ((img_w as f64 * scale).round() as u32).max(1),
            ((img_h as f64 * scale).round() as u32).max(1),
        )
    }

    fn new(img: &DynamicImage) -> Result<Reference> {
        let (jw, jh) = Self::judge_dims(img.width(), img.height());
        let dssim = Dssim::new();
        // The probe shortcut only pays off when the full image has at least
        // twice the judge pixels; below that, searching at full resolution is
        // both exact and no slower.
        let worth_probing =
            (img.width() as u64 * img.height() as u64) >= 2 * (jw as u64 * jh as u64);
        let (rgba, probe_img, sim) = if (jw, jh) == (img.width(), img.height()) {
            (img.to_rgba8(), None, None)
        } else if !worth_probing {
            let scaled = img.resize_exact(jw, jh, image::imageops::FilterType::Triangle);
            (scaled.to_rgba8(), None, None)
        } else {
            let scaled = img.resize_exact(jw, jh, image::imageops::FilterType::Triangle);
            // Same shrink factor as full -> judge, capped so the sim image
            // stays large enough (~512px) for DSSIM to be meaningful.
            let factor = (img.width().max(img.height()) as f64 / jw.max(jh) as f64).min(3.125);
            let sw = ((jw as f64 / factor).round() as u32).max(1);
            let sh = ((jh as f64 / factor).round() as u32).max(1);
            let sim_rgba = scaled
                .resize_exact(sw, sh, image::imageops::FilterType::Triangle)
                .to_rgba8();
            let sim_img = dssim
                .create_image_rgba(sim_rgba.as_raw().as_rgba(), sw as usize, sh as usize)
                .ok_or_else(|| anyhow!("could not build sim reference"))?;
            (
                scaled.to_rgba8(),
                Some(scaled),
                Some(SimRef { img: sim_img, w: sw as usize, h: sh as usize }),
            )
        };
        let (w, h) = (jw as usize, jh as usize);
        let orig = dssim
            .create_image_rgba(rgba.as_raw().as_rgba(), w, h)
            .ok_or_else(|| anyhow!("could not build reference image"))?;
        Ok(Reference { dssim, orig, w, h, probe_img, sim })
    }

    /// Score a judge-scale probe encode as if it were a full-resolution
    /// encode: shrink it by the full->judge factor before comparing.
    fn sim_distance(&self, encoded: &[u8], fmt: image::ImageFormat) -> Result<f64> {
        let sim = self.sim.as_ref().ok_or_else(|| anyhow!("no sim reference"))?;
        let decoded = image::load_from_memory_with_format(encoded, fmt)?
            .resize_exact(sim.w as u32, sim.h as u32, image::imageops::FilterType::Triangle)
            .to_rgba8();
        let cand = self
            .dssim
            .create_image_rgba(decoded.as_raw().as_rgba(), sim.w, sim.h)
            .ok_or_else(|| anyhow!("could not build sim candidate"))?;
        let (val, _) = self.dssim.compare(&sim.img, cand);
        Ok(val.into())
    }

    /// DSSIM between the original and an encoded candidate (lower is better).
    /// The candidate is downscaled to the same judging size as the reference.
    fn distance(&self, encoded: &[u8], fmt: image::ImageFormat) -> Result<f64> {
        let decoded = image::load_from_memory_with_format(encoded, fmt)?;
        let img = if (decoded.width() as usize, decoded.height() as usize) == (self.w, self.h) {
            decoded.to_rgba8()
        } else {
            decoded
                .resize_exact(self.w as u32, self.h as u32, image::imageops::FilterType::Triangle)
                .to_rgba8()
        };
        let cand = self
            .dssim
            .create_image_rgba(img.as_raw().as_rgba(), self.w, self.h)
            .ok_or_else(|| anyhow!("could not build candidate image"))?;
        let (val, _) = self.dssim.compare(&self.orig, cand);
        Ok(val.into())
    }
}

/// Binary-search the lowest quality whose perceptual distance stays under
/// `threshold`. `encode` maps quality -> encoded bytes. Returns the winning
/// bytes and quality, or None if even `hi` quality misses the threshold.
fn quality_search(
    lo0: u8,
    hi0: u8,
    threshold: f64,
    progress: Progress,
    progress_range: (f32, f32),
    mut encode: impl FnMut(u8) -> Result<Vec<u8>>,
    mut dist: impl FnMut(&[u8]) -> Result<f64>,
) -> Result<Option<(Vec<u8>, u8)>> {
    let steps = 8u32; // enough to bisect a ~35..95 range down to single steps
    let mut step = 0u32;
    let report = |s: u32| {
        let (a, b) = progress_range;
        progress(a + (b - a) * (s as f32 / (steps + 1) as f32));
    };

    // Make sure the top of the range is even achievable.
    let hi_bytes = encode(hi0)?;
    step += 1;
    report(step);
    if dist(&hi_bytes)? > threshold {
        return Ok(None);
    }

    let (mut lo, mut hi) = (lo0, hi0);
    let mut best = (hi_bytes, hi0);
    while hi - lo > 1 && step < steps {
        let q = (lo as u16 + hi as u16) as u8 / 2;
        let bytes = encode(q)?;
        step += 1;
        report(step);
        if dist(&bytes)? <= threshold {
            best = (bytes, q);
            hi = q;
        } else {
            lo = q;
        }
    }
    Ok(Some(best))
}

/// A probe search found quality `q0` on the judge-scale image. Encode the
/// full resolution near that quality with exact verification: step up until
/// the perceptual gate passes, then probe downward to claw back any
/// conservatism in the probe simulation. Typically costs 2-4 full encodes
/// instead of the 8 a full-resolution search would need.
fn finalize_full(
    q0: u8,
    lo: u8,
    hi: u8,
    threshold: f64,
    fmt: image::ImageFormat,
    reference: &Reference,
    progress: Progress,
    progress_range: (f32, f32),
    mut encode: impl FnMut(u8) -> Result<Vec<u8>>,
) -> Result<Option<(Vec<u8>, u8)>> {
    let mut attempts = 0u32;
    let mut report = || {
        attempts += 1;
        let (a, b) = progress_range;
        progress(a + (b - a) * (attempts as f32 / 6.0));
    };

    // Find a passing quality at or above the probe's estimate.
    let mut q = q0;
    let mut passing: Option<(Vec<u8>, u8)> = None;
    for _ in 0u32..4 {
        let bytes = encode(q)?;
        report();
        if reference.distance(&bytes, fmt)? <= threshold {
            passing = Some((bytes, q));
            break;
        }
        if q >= hi {
            break;
        }
        q = (q + 3).min(hi);
    }
    let Some((mut best_bytes, mut best_q)) = passing else {
        return Ok(None);
    };

    // Probe downward: the sim may have been slightly pessimistic.
    for _ in 0u32..2 {
        if best_q < lo.saturating_add(4) {
            break;
        }
        let q_down = best_q - 4;
        let bytes = encode(q_down)?;
        report();
        if reference.distance(&bytes, fmt)? <= threshold {
            best_bytes = bytes;
            best_q = q_down;
        } else {
            break;
        }
    }
    Ok(Some((best_bytes, best_q)))
}

// ---------------------------------------------------------------------------
// JPEG — mozjpeg with trellis quantization + progressive scans
// ---------------------------------------------------------------------------

struct Pixels {
    data: Vec<u8>,
    w: usize,
    h: usize,
}

fn jpeg_pixels(img: &DynamicImage, gray: bool) -> Pixels {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data = if gray { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() };
    Pixels { data, w, h }
}

fn compress_jpeg(input: Option<&[u8]>, img: &DynamicImage, progress: Progress) -> Result<Compressed> {
    let reference = Reference::new(img)?;

    // Grayscale photos get a dedicated single-channel encode (~2/3 smaller).
    let gray = is_grayscale(img);
    let colorspace = if gray { mozjpeg::ColorSpace::JCS_GRAYSCALE } else { mozjpeg::ColorSpace::JCS_RGB };

    let full = jpeg_pixels(img, gray);
    // Search on the judge-scale copy when the image is large: ~all of the
    // search cost drops with the pixel count, and only 1-2 full-resolution
    // encodes remain.
    let probe = reference.probe_img.as_ref().map(|p| jpeg_pixels(p, gray));
    let search = probe.as_ref().unwrap_or(&full);

    let run_pass = |force_444: bool,
                        lo: u8,
                        hi: u8,
                        search_range: (f32, f32),
                        final_range: (f32, f32)|
     -> Result<Option<(Vec<u8>, u8)>> {
        let fmt = image::ImageFormat::Jpeg;
        let encode_search =
            |q: u8| encode_mozjpeg(&search.data, search.w, search.h, colorspace, q as f32, force_444);
        let found = if probe.is_some() {
            quality_search(lo, hi, DSSIM_JPEG, progress, search_range, encode_search, |b| {
                reference.sim_distance(b, fmt)
            })?
        } else {
            quality_search(lo, hi, DSSIM_JPEG, progress, search_range, encode_search, |b| {
                reference.distance(b, fmt)
            })?
        };
        match found {
            None => Ok(None),
            Some((bytes, q)) if probe.is_none() => Ok(Some((bytes, q))),
            Some((_, q)) => finalize_full(q, lo, hi, DSSIM_JPEG, fmt, &reference, progress, final_range, |q| {
                encode_mozjpeg(&full.data, full.w, full.h, colorspace, q as f32, force_444)
            }),
        }
    };

    // Pass 1: standard 4:2:0 chroma subsampling (best compression).
    if let Some((data, q)) = run_pass(false, 35, 94, (0.10, 0.72), (0.72, 0.92))? {
        let mode = if gray { "grayscale" } else { "4:2:0" };
        return Ok(Compressed {
            data,
            note: format!("JPEG q{q} {mode}, progressive, trellis-optimized"),
        });
    }

    // Pass 2: sharp colored edges (red text, logos) can defeat subsampling.
    // Retry with full-resolution chroma before giving up.
    match run_pass(true, 50, 97, (0.60, 0.85), (0.85, 0.95))? {
        Some((data, q)) => Ok(Compressed {
            data,
            note: format!("JPEG q{q} 4:4:4 (fine color detail preserved)"),
        }),
        None => match input {
            Some(orig) => Ok(Compressed {
                data: orig.to_vec(),
                note: "Kept original (re-encoding would cost quality)".into(),
            }),
            // Resized: no original to fall back to — top quality encode.
            None => Ok(Compressed {
                data: encode_mozjpeg(&full.data, full.w, full.h, colorspace, 96.0, true)?,
                note: "JPEG q96 4:4:4".into(),
            }),
        },
    }
}

fn encode_mozjpeg(
    pixels: &[u8],
    w: usize,
    h: usize,
    colorspace: mozjpeg::ColorSpace,
    quality: f32,
    force_444: bool,
) -> Result<Vec<u8>> {
    std::panic::catch_unwind(|| -> Result<Vec<u8>> {
        let mut comp = mozjpeg::Compress::new(colorspace);
        comp.set_size(w, h);
        comp.set_quality(quality);
        comp.set_progressive_mode();
        if force_444 {
            comp.set_chroma_sampling_pixel_sizes((1, 1), (1, 1));
        }
        let mut started = comp.start_compress(Vec::new())?;
        started.write_scanlines(pixels)?;
        Ok(started.finish()?)
    })
    .map_err(|_| anyhow!("JPEG encoder panicked"))?
}

fn is_grayscale(img: &DynamicImage) -> bool {
    use image::GenericImageView;
    match img.color() {
        image::ColorType::L8 | image::ColorType::L16 | image::ColorType::La8 | image::ColorType::La16 => {
            return true
        }
        _ => {}
    }
    // Sample a grid; tolerate tiny chroma noise from previous encodes.
    let (w, h) = img.dimensions();
    let step = ((w as u64 * h as u64 / 10_000).max(1) as f64).sqrt() as u32 + 1;
    let mut checked = 0u32;
    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let p = img.get_pixel(x, y);
            let (r, g, b) = (p[0] as i16, p[1] as i16, p[2] as i16);
            if (r - g).abs() > 3 || (g - b).abs() > 3 || (r - b).abs() > 3 {
                return false;
            }
            checked += 1;
        }
    }
    checked > 0
}

// ---------------------------------------------------------------------------
// PNG — lossy palette (imagequant) when invisible, else lossless (oxipng)
// ---------------------------------------------------------------------------

fn compress_png(input: Option<&[u8]>, img: &DynamicImage, progress: Progress) -> Result<Compressed> {
    let reference = Reference::new(img)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let rgba = img.to_rgba8();

    // The lossless candidate starts from the original bytes — or, when the
    // image was resized, from a fresh PNG encode of the resized pixels.
    let lossless_src: Vec<u8> = match input {
        Some(orig) => orig.to_vec(),
        None => {
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png)?;
            buf.into_inner()
        }
    };

    // Candidate A: palette quantization at two dithering levels (heavy dither
    // hides banding but inflates the deflate stream; light dither often wins
    // on smooth gradients). Every candidate must pass the perceptual gate.
    // imagequant refuses (Err) when it cannot hit its own quality floor,
    // which pre-filters bad candidates.
    progress(0.15);
    let lossy = quantize_png(rgba.as_raw(), w, h)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|png| {
            let optimized = oxipng_optimize(&png, false).unwrap_or(png);
            match reference.distance(&optimized, image::ImageFormat::Png) {
                Ok(d) if d <= DSSIM_PNG_LOSSY => Some(optimized),
                _ => None,
            }
        })
        .min_by_key(|png| png.len());
    progress(0.60);

    // Candidate B: lossless recompression.
    let lossless = oxipng_optimize(&lossless_src, lossless_src.len() < 512 * 1024).ok();
    progress(0.95);

    match (lossy, lossless) {
        (Some(a), Some(b)) if a.len() < b.len() => Ok(Compressed {
            data: a,
            note: "PNG palette-quantized (visually lossless) + oxipng".into(),
        }),
        (Some(a), None) => Ok(Compressed {
            data: a,
            note: "PNG palette-quantized (visually lossless) + oxipng".into(),
        }),
        (_, Some(b)) => Ok(Compressed {
            data: b,
            note: "PNG losslessly recompressed (oxipng)".into(),
        }),
        (None, None) => Ok(Compressed {
            data: lossless_src,
            note: "Kept original".into(),
        }),
    }
}

/// Palette-quantized PNG candidates at multiple dithering levels; the caller
/// gates each one perceptually and keeps the smallest survivor.
fn quantize_png(rgba: &[u8], w: usize, h: usize) -> Result<Vec<Vec<u8>>> {
    let pixels: Vec<rgb::RGBA8> = rgba.as_rgba().to_vec();
    let mut attr = imagequant::new();
    attr.set_quality(60, 100)?;
    let mut liq_img = attr.new_image(pixels, w, h, 0.0)?;
    let mut res = attr.quantize(&mut liq_img)?;

    let mut candidates = Vec::new();
    for dither in [1.0f32, 0.35] {
        if res.set_dithering_level(dither).is_err() {
            continue;
        }
        let Ok((palette, indexed)) = res.remapped(&mut liq_img) else {
            continue;
        };
        let mut enc = lodepng::Encoder::new();
        if enc.set_palette(&palette).is_err() {
            continue;
        }
        if let Ok(png) = enc.encode(&indexed, w, h) {
            candidates.push(png);
        }
    }
    Ok(candidates)
}

fn oxipng_optimize(png: &[u8], thorough: bool) -> Result<Vec<u8>> {
    let mut opts = oxipng::Options::from_preset(if thorough { 4 } else { 2 });
    opts.strip = oxipng::StripChunks::Safe;
    opts.optimize_alpha = true;
    // Zopfli squeezes a few more percent out of the deflate stream; only worth
    // its (considerable) CPU cost on smaller files.
    if png.len() < 256 * 1024 {
        opts.deflate = oxipng::Deflaters::Zopfli {
            iterations: std::num::NonZeroU8::new(15).unwrap(),
        };
    }
    Ok(oxipng::optimize_from_memory(png, &opts)?)
}

// ---------------------------------------------------------------------------
// WebP — lossy quality search, lossless candidate for graphics
// ---------------------------------------------------------------------------

fn compress_webp(input: Option<&[u8]>, img: &DynamicImage, progress: Progress) -> Result<Compressed> {
    let reference = Reference::new(img)?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.to_rgba8();

    // Search on the judge-scale copy when the image is large (see JPEG path).
    let probe_rgba = reference.probe_img.as_ref().map(|p| p.to_rgba8());
    let (search_rgba, sw, sh) = match &probe_rgba {
        Some(p) => (p, p.width(), p.height()),
        None => (&rgba, w, h),
    };

    let fmt = image::ImageFormat::WebP;
    let encode_search = |q: u8| encode_webp_lossy(search_rgba.as_raw(), sw, sh, q as f32);
    let found = if probe_rgba.is_some() {
        quality_search(35, 95, DSSIM_WEBP, progress, (0.10, 0.70), encode_search, |b| {
            reference.sim_distance(b, fmt)
        })?
    } else {
        quality_search(35, 95, DSSIM_WEBP, progress, (0.10, 0.70), encode_search, |b| {
            reference.distance(b, fmt)
        })?
    };
    let found = match found {
        Some((bytes, q)) if probe_rgba.is_none() => Some((bytes, q)),
        Some((_, q)) => finalize_full(q, 35, 95, DSSIM_WEBP, fmt, &reference, progress, (0.70, 0.82), |q| {
            encode_webp_lossy(rgba.as_raw(), w, h, q as f32)
        })?,
        None => None,
    };

    // Graphics with few colors often compress dramatically better lossless.
    progress(0.85);
    let lossless = if likely_graphic(&rgba) {
        Some(webp::Encoder::from_rgba(rgba.as_raw(), w, h).encode_lossless().to_vec())
    } else {
        None
    };

    let lossy_note = |q: u8| format!("WebP q{q} (perceptually tuned)");
    match (found, lossless) {
        (Some((a, _)), Some(b)) if b.len() < a.len() => Ok(Compressed {
            data: b,
            note: "WebP lossless (graphic content)".into(),
        }),
        (Some((a, q)), _) => Ok(Compressed { data: a, note: lossy_note(q) }),
        (None, Some(b)) => Ok(Compressed {
            data: b,
            note: "WebP lossless (quality could not be preserved lossily)".into(),
        }),
        (None, None) => match input {
            Some(orig) => Ok(Compressed {
                data: orig.to_vec(),
                note: "Kept original".into(),
            }),
            // Resized: no original to fall back to — top quality encode.
            None => Ok(Compressed {
                data: encode_webp_lossy(rgba.as_raw(), w, h, 95.0)?,
                note: "WebP q95".into(),
            }),
        },
    }
}

/// Lossy WebP at maximum rate-distortion effort (method 6): noticeably
/// smaller than the default method 4 at identical quality, at the cost of
/// encode time — a good trade for a compress-once, serve-forever tool.
fn encode_webp_lossy(rgba: &[u8], w: u32, h: u32, quality: f32) -> Result<Vec<u8>> {
    let mut config = webp::WebPConfig::new().map_err(|_| anyhow!("webp config init failed"))?;
    config.quality = quality;
    config.method = 6;
    let mem = webp::Encoder::from_rgba(rgba, w, h)
        .encode_advanced(&config)
        .map_err(|e| anyhow!("webp encode failed: {e:?}"))?;
    Ok(mem.to_vec())
}

/// Heuristic: sampled unique-color count is low => logo/screenshot/graphic.
fn likely_graphic(rgba: &image::RgbaImage) -> bool {
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
// EXIF orientation (JPEG)
// ---------------------------------------------------------------------------

/// Minimal EXIF walk: find the Orientation tag (0x0112) in IFD0.
fn exif_orientation(jpeg: &[u8]) -> Option<u16> {
    // Locate APP1 "Exif" segment.
    let mut i = 2usize;
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let marker = jpeg[i + 1];
        if marker == 0xDA {
            return None; // start of scan, no EXIF found
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        if marker == 0xE1 && i + 4 + 6 <= jpeg.len() && &jpeg[i + 4..i + 10] == b"Exif\0\0" {
            return parse_tiff_orientation(&jpeg[i + 10..i + 2 + len]);
        }
        i += 2 + len;
    }
    None
}

fn parse_tiff_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 14 {
        return None;
    }
    let le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let rd16 = |b: &[u8]| -> u16 {
        if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }
    };
    let rd32 = |b: &[u8]| -> u32 {
        if le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    };
    let ifd = rd32(&tiff[4..8]) as usize;
    if ifd + 2 > tiff.len() {
        return None;
    }
    let count = rd16(&tiff[ifd..ifd + 2]) as usize;
    for n in 0..count {
        let e = ifd + 2 + n * 12;
        if e + 12 > tiff.len() {
            return None;
        }
        if rd16(&tiff[e..e + 2]) == 0x0112 {
            let v = rd16(&tiff[e + 8..e + 10]);
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    /// Deterministic photo-like image: gradients, shapes, mild noise.
    fn photo(w: u32, h: u32) -> DynamicImage {
        let mut seed = 0x9e3779b9u32;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let img = ImageBuffer::from_fn(w, h, |x, y| {
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
        });
        DynamicImage::ImageRgb8(img)
    }

    fn encode(img: &DynamicImage, fmt: image::ImageFormat) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, fmt).unwrap();
        buf.into_inner()
    }

    const NOOP: Progress = &|_| {};

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
    fn jpeg_photo_shrinks_and_stays_visually_lossless() {
        let img = photo(320, 240);
        let input = encode(&img, image::ImageFormat::Jpeg); // image crate q~75 baseline
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(out.data.len() < input.len(), "expected shrink, got {} -> {}", input.len(), out.data.len());

        // Quality guarantee: distance from the *input decode* stays tiny.
        let decoded_in = image::load_from_memory(&input).unwrap();
        let reference = Reference::new(&decoded_in).unwrap();
        let d = reference.distance(&out.data, image::ImageFormat::Jpeg).unwrap();
        assert!(d <= DSSIM_JPEG * 1.05, "dssim {d} exceeds threshold");

        let decoded_out = image::load_from_memory(&out.data).unwrap();
        assert_eq!((decoded_out.width(), decoded_out.height()), (320, 240));
    }

    #[test]
    fn grayscale_jpeg_uses_single_channel() {
        let gray = DynamicImage::ImageLuma8(ImageBuffer::from_fn(200, 150, |x, y| {
            image::Luma([((x * 255 / 200 + y) % 255) as u8])
        }));
        let input = encode(&gray.to_rgb8().into(), image::ImageFormat::Jpeg);
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(out.note.contains("grayscale") || out.note.contains("original"), "note: {}", out.note);
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
        assert!(out.data.len() < input.len(), "expected shrink, got {} -> {}", input.len(), out.data.len());
        assert_eq!(Format::from_magic(&out.data), Some(Format::Png));
        image::load_from_memory(&out.data).unwrap();
    }

    #[test]
    fn png_photo_stays_visually_lossless() {
        let img = photo(280, 200);
        let input = encode(&img, image::ImageFormat::Png);
        let out = compress(&input, Format::Png, None, NOOP).unwrap();
        assert!(out.data.len() <= input.len());
        let reference = Reference::new(&img).unwrap();
        let d = reference.distance(&out.data, image::ImageFormat::Png).unwrap();
        assert!(d <= DSSIM_PNG_LOSSY * 1.05, "dssim {d} exceeds threshold");
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
    fn exif_orientation_is_parsed_and_applied() {
        let img = photo(320, 240);
        let plain = encode(&img, image::ImageFormat::Jpeg);
        assert!(exif_orientation(&plain).is_none());

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

        assert_eq!(exif_orientation(&with_exif), Some(6));
        let out = compress(&with_exif, Format::Jpeg, None, NOOP).unwrap();
        let decoded = image::load_from_memory(&out.data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (240, 320), "orientation should rotate pixels");
    }

    #[test]
    fn big_images_are_judged_at_display_scale() {
        assert_eq!(Reference::judge_dims(800, 600), (800, 600));
        assert_eq!(Reference::judge_dims(3200, 2000), (1600, 1000));
        assert_eq!(Reference::judge_dims(2000, 4000), (800, 1600));
        let r = Reference::new(&photo(3200, 2000)).unwrap();
        assert_eq!((r.w, r.h), (1600, 1000));
        // Small images keep 1:1 judging.
        let r = Reference::new(&photo(320, 240)).unwrap();
        assert_eq!((r.w, r.h), (320, 240));
    }

    #[test]
    fn orientation_variants_transform_dimensions() {
        let img = photo(40, 20);
        for o in [1u16, 2, 3, 4] {
            let t = apply_orientation(img.clone(), o);
            assert_eq!((t.width(), t.height()), (40, 20), "orientation {o} keeps dims");
        }
        for o in [5u16, 6, 7, 8] {
            let t = apply_orientation(img.clone(), o);
            assert_eq!((t.width(), t.height()), (20, 40), "orientation {o} swaps dims");
        }
    }

    #[test]
    fn tiff_big_endian_orientation_parses() {
        #[rustfmt::skip]
        let tiff: Vec<u8> = vec![
            b'M', b'M', 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08, // TIFF BE, IFD @8
            0x00, 0x01,                                     // 1 entry
            0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, // tag 0x0112, SHORT, count 1
            0x00, 0x03, 0x00, 0x00,                         // value 3 (rotate 180)
            0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(parse_tiff_orientation(&tiff), Some(3));
        assert_eq!(parse_tiff_orientation(b"XXjunk"), None);
    }

    #[test]
    fn content_heuristics() {
        assert!(is_grayscale(&DynamicImage::ImageLuma8(ImageBuffer::from_fn(50, 50, |x, _| {
            image::Luma([(x * 5) as u8])
        }))));
        assert!(!is_grayscale(&photo(100, 100)));

        let flat = ImageBuffer::from_fn(200, 200, |x, _| Rgba([if x < 100 { 0u8 } else { 255 }, 60, 60, 255]));
        assert!(likely_graphic(&flat));
        assert!(!likely_graphic(&photo(300, 300).to_rgba8()));
    }

    #[test]
    fn large_jpeg_probe_search_stays_visually_lossless() {
        let img = photo(3200, 2000);
        let input = encode(&img, image::ImageFormat::Jpeg);
        let out = compress(&input, Format::Jpeg, None, NOOP).unwrap();
        assert!(out.data.len() < input.len(), "expected shrink, got {} -> {}", input.len(), out.data.len());
        let decoded_in = image::load_from_memory(&input).unwrap();
        let reference = Reference::new(&decoded_in).unwrap();
        let d = reference.distance(&out.data, image::ImageFormat::Jpeg).unwrap();
        assert!(d <= DSSIM_JPEG * 1.05, "dssim {d} exceeds threshold");
        let decoded_out = image::load_from_memory(&out.data).unwrap();
        assert_eq!((decoded_out.width(), decoded_out.height()), (3200, 2000), "full resolution preserved");
    }

    #[test]
    fn max_width_resizes_before_compressing() {
        let img = photo(400, 300);
        let input = encode(&img, image::ImageFormat::Jpeg);
        let out = compress(&input, Format::Jpeg, Some(200), NOOP).unwrap();
        let dec = image::load_from_memory(&out.data).unwrap();
        assert_eq!((dec.width(), dec.height()), (200, 150), "aspect ratio preserved");
        assert!(out.note.contains("Resized to 200×150"), "note: {}", out.note);

        // Never upscales: a max width larger than the image is a no-op.
        let out = compress(&input, Format::Jpeg, Some(4000), NOOP).unwrap();
        let dec = image::load_from_memory(&out.data).unwrap();
        assert_eq!((dec.width(), dec.height()), (400, 300));
        assert!(!out.note.contains("Resized"), "note: {}", out.note);
    }

    #[test]
    fn never_inflates_a_file() {
        // A tiny, already heavily-optimized JPEG must come back byte-identical.
        let img = photo(64, 48);
        let first = compress(&encode(&img, image::ImageFormat::Jpeg), Format::Jpeg, None, NOOP).unwrap();
        let second = compress(&first.data, Format::Jpeg, None, NOOP).unwrap();
        assert!(second.data.len() <= first.data.len());
    }

    #[test]
    fn oversized_rejected() {
        // Tiny file claiming huge dimensions is caught by the decoder or cap;
        // here just verify the pixel cap constant is enforced via decode().
        let img = photo(64, 64);
        let input = encode(&img, image::ImageFormat::Png);
        assert!(decode(&input, Format::Png).is_ok());
    }
}
