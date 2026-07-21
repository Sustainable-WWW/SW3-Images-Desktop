//! Generates realistic sample images for manual/E2E testing:
//! `cargo run --example gen_samples -- <output-dir>`

use image::{DynamicImage, ImageBuffer, Rgb, Rgba};

fn photo(w: u32, h: u32) -> DynamicImage {
    let mut seed = 0x2545f491u32;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        // Sky -> horizon gradient with "foliage" noise band and a sun disc.
        let sky = 1.0 - fy;
        let sun = ((fx - 0.72).powi(2) + (fy - 0.25).powi(2)).sqrt();
        let sunlight = (0.12 - sun).max(0.0) * 900.0;
        let foliage = if fy > 0.62 { ((rng() % 40) as f32) - 20.0 } else { ((rng() % 8) as f32) - 4.0 };
        let r = 90.0 + 100.0 * sky + sunlight + foliage * 0.6;
        let g = 120.0 + 90.0 * sky + sunlight * 0.8 + foliage;
        let b = 170.0 + 70.0 * sky + sunlight * 0.4 + foliage * 0.4;
        Rgb([r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8])
    }))
}

fn screenshot(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgba8(ImageBuffer::from_fn(w, h, |x, y| {
        // Window chrome, sidebar, text lines: flat colors + hard edges.
        if y < 40 {
            Rgba([36, 41, 46, 255])
        } else if x < 220 {
            Rgba([246, 248, 250, 255])
        } else if (y % 28) < 12 && x > 260 && x < w - 60 && (x / 7 + y) % 9 != 0 {
            Rgba([60, 66, 72, 255])
        } else {
            Rgba([255, 255, 255, 255])
        }
    }))
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: gen_samples <dir>");
    std::fs::create_dir_all(&dir).unwrap();
    let p = |n: &str| format!("{dir}/{n}");

    let big = photo(1920, 1280);
    big.save_with_format(p("landscape-photo.jpg"), image::ImageFormat::Jpeg).unwrap();
    photo(4032, 3024).save_with_format(p("phone-photo.jpg"), image::ImageFormat::Jpeg).unwrap();
    photo(900, 600).save_with_format(p("holiday.jpg"), image::ImageFormat::Jpeg).unwrap();
    screenshot(1200, 800).save_with_format(p("app-screenshot.png"), image::ImageFormat::Png).unwrap();
    photo(800, 500).save_with_format(p("photo-as-png.png"), image::ImageFormat::Png).unwrap();

    let rgba = photo(1000, 700).to_rgba8();
    let webp = webp::Encoder::from_rgba(rgba.as_raw(), 1000, 700).encode(97.0);
    std::fs::write(p("banner.webp"), &*webp).unwrap();

    for f in std::fs::read_dir(&dir).unwrap().flatten() {
        println!("{}\t{} bytes", f.file_name().to_string_lossy(), f.metadata().unwrap().len());
    }
}
