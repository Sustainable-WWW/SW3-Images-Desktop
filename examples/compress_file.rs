//! Compress files from the command line (also handy for benchmarking):
//! `cargo run --release --example compress_file -- photo.jpg [more files...]`
//! Writes `photo.min.jpg` next to the original.

use sw3_images::compress::{compress, Format};

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: compress_file <image> [image...]");
        std::process::exit(1);
    }
    for path in paths {
        let input = std::fs::read(&path)?;
        let Some(format) = Format::from_magic(&input) else {
            eprintln!("{path}: not a JPG/PNG/WebP file, skipping");
            continue;
        };
        let t0 = std::time::Instant::now();
        let out = compress(&input, format, None, &|_| {})?;
        let ms = t0.elapsed().as_millis();
        let dest = match path.rsplit_once('.') {
            Some((stem, _)) => format!("{stem}.min.{}", format.extension()),
            None => format!("{path}.min.{}", format.extension()),
        };
        std::fs::write(&dest, &out.data)?;
        let saved = 100.0 * (1.0 - out.data.len() as f64 / input.len() as f64);
        println!("{path}: {} -> {} bytes ({saved:.1}% smaller, {ms} ms) — {}", input.len(), out.data.len(), out.note);
    }
    Ok(())
}
