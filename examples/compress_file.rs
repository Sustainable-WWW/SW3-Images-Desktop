//! Compress files from the command line (also handy for benchmarking):
//! `cargo run --release --example compress_file -- [--webp] [--max-width N] photo.jpg [more files...]`
//! Writes `photo.min.<ext>` next to the original.

use sw3_images::compress::{compress_with, Format, Options};

fn main() -> anyhow::Result<()> {
    let mut opts = Options::default();
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--webp" => opts.to_webp = true,
            "--max-width" => opts.max_width = Some(args.next().and_then(|v| v.parse().ok()).expect("--max-width needs a number")),
            _ => paths.push(arg),
        }
    }
    if paths.is_empty() {
        eprintln!("usage: compress_file [--webp] [--max-width N] <image> [image...]");
        std::process::exit(1);
    }
    for path in paths {
        let input = std::fs::read(&path)?;
        let Some(format) = Format::from_magic(&input) else {
            eprintln!("{path}: not a JPG/PNG/WebP file, skipping");
            continue;
        };
        let t0 = std::time::Instant::now();
        let out = compress_with(&input, format, &opts, &|_| {})?;
        let ms = t0.elapsed().as_millis();
        let stem = path.rsplit_once('.').map_or(path.as_str(), |(s, _)| s);
        let dest = format!("{stem}.min.{}", out.format.extension());
        std::fs::write(&dest, &out.data)?;
        let saved = 100.0 * (1.0 - out.data.len() as f64 / input.len() as f64);
        println!("{path}: {} -> {} bytes ({saved:.1}% smaller, {ms} ms) — {}", input.len(), out.data.len(), out.note);
    }
    Ok(())
}
