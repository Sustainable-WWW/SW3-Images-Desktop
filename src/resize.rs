//! Color-correct resizing.
//!
//! Resampling is done in linear light with premultiplied alpha:
//! - averaging gamma-encoded sRGB values darkens fine bright detail and
//!   shifts the hue of fine colored detail (a red/green pattern turns
//!   brownish instead of yellow);
//! - averaging non-premultiplied RGBA bleeds the invisible color of fully
//!   transparent pixels into visible edges (the classic dark fringe around
//!   downscaled logos).
//!
//! Pixels are mapped sRGB -> 16-bit linear, resized, and mapped back, all
//! with SIMD via `fast_image_resize`.

use anyhow::{anyhow, Result};
use fast_image_resize::images::Image;
use fast_image_resize::{create_srgb_mapper, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageBuffer};
use std::sync::OnceLock;

fn srgb() -> &'static fast_image_resize::PixelComponentMapper {
    static MAPPER: OnceLock<fast_image_resize::PixelComponentMapper> = OnceLock::new();
    MAPPER.get_or_init(create_srgb_mapper)
}

/// Target size for a max-width downscale, or None when the image already
/// fits (never upscales). Aspect ratio is preserved.
pub fn fit_width(width: u32, height: u32, max_width: u32) -> Option<(u32, u32)> {
    if width <= max_width {
        return None;
    }
    let h = ((height as u64 * max_width as u64 + width as u64 / 2) / width as u64).max(1) as u32;
    Some((max_width, h))
}

/// High-quality downscale (Lanczos3) for output images.
pub fn resize(img: &DynamicImage, width: u32, height: u32) -> Result<DynamicImage> {
    resize_with(img, width, height, FilterType::Lanczos3)
}

/// Area-averaging downscale used to bring images to judging size.
pub fn resize_for_judging(img: &DynamicImage, width: u32, height: u32) -> Result<DynamicImage> {
    resize_with(img, width, height, FilterType::Bilinear)
}

fn resize_with(img: &DynamicImage, width: u32, height: u32, filter: FilterType) -> Result<DynamicImage> {
    let alpha = img.color().has_alpha();
    let wide = img.color().bytes_per_pixel() / img.color().channel_count() > 1;
    let (sw, sh) = (img.width(), img.height());

    // Normalize to one of four layouts (gray becomes RGB; values are identical
    // per channel so gray stays gray).
    let (src_buf, src_type, lin_type) = match (alpha, wide) {
        (false, false) => (img.to_rgb8().into_raw(), PixelType::U8x3, PixelType::U16x3),
        (true, false) => (img.to_rgba8().into_raw(), PixelType::U8x4, PixelType::U16x4),
        (false, true) => (u16_bytes(img.to_rgb16().into_raw()), PixelType::U16x3, PixelType::U16x3),
        (true, true) => (u16_bytes(img.to_rgba16().into_raw()), PixelType::U16x4, PixelType::U16x4),
    };
    let src = Image::from_vec_u8(sw, sh, src_buf, src_type).map_err(|e| anyhow!("resize: {e:?}"))?;
    let mut lin = Image::new(sw, sh, lin_type);
    srgb().forward_map(&src, &mut lin).map_err(|e| anyhow!("resize: {e:?}"))?;
    drop(src);

    let mut lin_dst = Image::new(width, height, lin_type);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)).use_alpha(alpha);
    Resizer::new().resize(&lin, &mut lin_dst, &options).map_err(|e| anyhow!("resize: {e:?}"))?;
    drop(lin);

    let mut dst = Image::new(width, height, src_type);
    srgb().backward_map(&lin_dst, &mut dst).map_err(|e| anyhow!("resize: {e:?}"))?;
    let raw = dst.into_vec();

    let bad = || anyhow!("resize: buffer size mismatch");
    Ok(match (alpha, wide) {
        (false, false) => DynamicImage::ImageRgb8(ImageBuffer::from_raw(width, height, raw).ok_or_else(bad)?),
        (true, false) => DynamicImage::ImageRgba8(ImageBuffer::from_raw(width, height, raw).ok_or_else(bad)?),
        (false, true) => DynamicImage::ImageRgb16(ImageBuffer::from_raw(width, height, from_u16_bytes(&raw)).ok_or_else(bad)?),
        (true, true) => DynamicImage::ImageRgba16(ImageBuffer::from_raw(width, height, from_u16_bytes(&raw)).ok_or_else(bad)?),
    })
}

fn u16_bytes(v: Vec<u16>) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for x in v {
        out.extend_from_slice(&x.to_ne_bytes());
    }
    out
}

fn from_u16_bytes(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, Rgba};

    #[test]
    fn fit_width_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_width(4032, 3024, 1920), Some((1920, 1440)));
        assert_eq!(fit_width(1000, 3, 999), Some((999, 3)));
        assert_eq!(fit_width(10_000, 1, 16), Some((16, 1)), "height never rounds to 0");
        assert_eq!(fit_width(800, 600, 800), None);
        assert_eq!(fit_width(800, 600, 4000), None);
    }

    #[test]
    fn transparent_pixels_do_not_bleed_into_edges() {
        // Opaque white square on a transparent background whose hidden color
        // is black. Non-premultiplied resizing would produce gray fringes.
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(64, 64, |x, _| {
            if x < 32 { Rgba([255, 255, 255, 255]) } else { Rgba([0, 0, 0, 0]) }
        }));
        let out = resize(&img, 16, 16).unwrap().to_rgba8();
        for p in out.pixels() {
            if p[3] > 8 {
                assert!(p[0] >= 250 && p[1] >= 250 && p[2] >= 250, "fringe pixel {p:?}");
            }
        }
    }

    #[test]
    fn fine_color_detail_averages_in_linear_light() {
        // 1px red/green stripes. In linear light they average to a bright
        // yellow (~188,188,0); gamma-space averaging would give (128,128,0).
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(64, 64, |x, _| {
            if x % 2 == 0 { Rgb([255, 0, 0]) } else { Rgb([0, 255, 0]) }
        }));
        let out = resize(&img, 16, 16).unwrap().to_rgb8();
        let p = out.get_pixel(8, 8);
        assert!((180..=196).contains(&p[0]) && (180..=196).contains(&p[1]) && p[2] < 8, "got {p:?}");
    }

    #[test]
    fn preserves_layout_and_gray_stays_gray() {
        let gray = DynamicImage::ImageLuma8(ImageBuffer::from_fn(40, 20, |x, y| image::Luma([((x * 5 + y * 3) % 256) as u8])));
        let out = resize(&gray, 20, 10).unwrap();
        assert_eq!((out.width(), out.height()), (20, 10));
        assert!(out.to_rgb8().pixels().all(|p| p[0] == p[1] && p[1] == p[2]));

        let deep = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(30, 30, Rgba([40_000u16, 20_000, 1_000, 65_535])));
        let out = resize(&deep, 10, 10).unwrap();
        assert!(matches!(out, DynamicImage::ImageRgba16(_)), "16-bit stays 16-bit");
        let p = out.to_rgba16().get_pixel(5, 5).0;
        assert!((p[0] as i32 - 40_000).abs() < 40 && p[3] == 65_535, "flat color unchanged: {p:?}");
    }
}
