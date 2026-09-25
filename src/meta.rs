//! Container-level inspection and editing: animation / CMYK detection, ICC
//! color profiles, and carrying color information into re-encoded files.
//!
//! All parsers here read untrusted bytes, so every access is bounds-checked
//! and malformed input simply yields "not found".

/// Chunk list: (four-character code, payload).
pub type Chunks = Vec<([u8; 4], Vec<u8>)>;

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

/// Number of color components from the JPEG frame header (1 = gray,
/// 3 = YCbCr/RGB, 4 = CMYK/YCCK).
pub fn jpeg_components(jpeg: &[u8]) -> Option<u8> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2usize;
    loop {
        // Skip fill bytes before a marker.
        while *jpeg.get(i)? == 0xFF && *jpeg.get(i + 1)? == 0xFF {
            i += 1;
        }
        if *jpeg.get(i)? != 0xFF {
            return None;
        }
        let marker = *jpeg.get(i + 1)?;
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None; // start of scan / end of image before any frame header
        }
        let len = be16(jpeg, i + 2)? as usize;
        if len < 2 {
            return None;
        }
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof {
            return jpeg.get(i + 9).copied();
        }
        i = i.checked_add(2 + len)?;
    }
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Chunks before the first IDAT (where animation control and color
/// information live).
fn png_header_chunks(png: &[u8]) -> Chunks {
    let mut out = Vec::new();
    if !png.starts_with(&PNG_SIG) {
        return out;
    }
    let mut i = 8usize;
    while let (Some(len), Some(tag)) = (be32(png, i), png.get(i + 4..i + 8)) {
        let tag: [u8; 4] = tag.try_into().unwrap_or_default();
        if &tag == b"IDAT" || &tag == b"IEND" {
            break;
        }
        let start = i + 8;
        let Some(end) = start.checked_add(len as usize) else { break };
        let Some(data) = png.get(start..end) else { break };
        out.push((tag, data.to_vec()));
        i = end + 4; // skip CRC
    }
    out
}

/// APNG files carry an `acTL` chunk before the image data.
pub fn png_is_animated(png: &[u8]) -> bool {
    png_header_chunks(png).iter().any(|(t, _)| t == b"acTL")
}

/// Chunks that change how a PNG's colors display: carried over into
/// re-encoded PNGs so colors render exactly as before.
pub fn png_color_chunks(png: &[u8]) -> Chunks {
    png_header_chunks(png)
        .into_iter()
        .filter(|(t, _)| matches!(t, b"iCCP" | b"sRGB" | b"gAMA" | b"cHRM" | b"cICP"))
        .collect()
}

/// A PNG `iCCP` chunk payload for an ICC profile.
pub fn png_iccp(icc: &[u8]) -> ([u8; 4], Vec<u8>) {
    let mut data = b"ICC profile\0\0".to_vec(); // name, NUL, compression method 0
    data.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(icc, 9));
    (*b"iCCP", data)
}

/// Insert chunks right after IHDR (skipping any type the file already has).
pub fn png_with_chunks(png: &[u8], chunks: &[([u8; 4], Vec<u8>)]) -> Option<Vec<u8>> {
    if !png.starts_with(&PNG_SIG) || &png.get(12..16)? != b"IHDR" {
        return None;
    }
    let existing = png_header_chunks(png);
    let ihdr_end = 8 + 12 + be32(png, 8)? as usize;
    png.get(..ihdr_end)?;
    let mut out = Vec::with_capacity(png.len() + chunks.iter().map(|c| c.1.len() + 12).sum::<usize>());
    out.extend_from_slice(&png[..ihdr_end]);
    for (tag, data) in chunks {
        if existing.iter().any(|(t, _)| t == tag) {
            continue;
        }
        out.extend_from_slice(&(u32::try_from(data.len()).ok()?).to_be_bytes());
        let body_start = out.len();
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let crc = crc32(&out[body_start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(&png[ihdr_end..]);
    Some(out)
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, slot) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

// ---------------------------------------------------------------------------
// WebP (RIFF)
// ---------------------------------------------------------------------------

fn webp_chunks(webp: &[u8]) -> Option<Chunks> {
    if webp.get(0..4)? != b"RIFF" || webp.get(8..12)? != b"WEBP" {
        return None;
    }
    let end = (8 + le32(webp, 4)? as usize).min(webp.len());
    let mut out = Vec::new();
    let mut i = 12usize;
    while i + 8 <= end {
        let tag: [u8; 4] = webp.get(i..i + 4)?.try_into().ok()?;
        let len = le32(webp, i + 4)? as usize;
        let data = webp.get(i + 8..i.checked_add(8 + len)?)?;
        out.push((tag, data.to_vec()));
        i += 8 + len + (len & 1);
    }
    Some(out)
}

pub fn webp_is_animated(webp: &[u8]) -> bool {
    let Some(chunks) = webp_chunks(webp) else { return false };
    chunks.iter().any(|(t, d)| (t == b"VP8X" && d.first().is_some_and(|f| f & 0x02 != 0)) || t == b"ANIM" || t == b"ANMF")
}

/// Embed an ICC profile, converting a simple-format file to the extended
/// (VP8X) container as the spec requires.
pub fn webp_with_icc(webp: &[u8], icc: &[u8]) -> Option<Vec<u8>> {
    let mut chunks = webp_chunks(webp)?;
    chunks.retain(|(t, _)| t != b"ICCP");
    if chunks.first()?.0 != *b"VP8X" {
        let (w, h, alpha) = webp_bitstream_info(&chunks)?;
        let mut vp8x = vec![0u8; 10];
        vp8x[0] = if alpha { 0x10 } else { 0 };
        vp8x[4..7].copy_from_slice(&(w - 1).to_le_bytes()[..3]);
        vp8x[7..10].copy_from_slice(&(h - 1).to_le_bytes()[..3]);
        chunks.insert(0, (*b"VP8X", vp8x));
    }
    chunks[0].1[0] |= 0x20; // ICC profile flag
    chunks.insert(1, (*b"ICCP", icc.to_vec()));

    let mut body = Vec::new();
    for (tag, data) in &chunks {
        body.extend_from_slice(tag);
        body.extend_from_slice(&u32::try_from(data.len()).ok()?.to_le_bytes());
        body.extend_from_slice(data);
        if data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::try_from(body.len() + 4).ok()?.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    Some(out)
}

/// Canvas size and alpha flag from a simple-format VP8/VP8L bitstream.
fn webp_bitstream_info(chunks: &Chunks) -> Option<(u32, u32, bool)> {
    let alpha_chunk = chunks.iter().any(|(t, _)| t == b"ALPH");
    for (tag, d) in chunks {
        match tag {
            b"VP8 " => {
                if d.get(3..6)? != [0x9D, 0x01, 0x2A] {
                    return None;
                }
                let w = u16::from_le_bytes(d.get(6..8)?.try_into().ok()?) as u32 & 0x3FFF;
                let h = u16::from_le_bytes(d.get(8..10)?.try_into().ok()?) as u32 & 0x3FFF;
                return (w > 0 && h > 0).then_some((w, h, alpha_chunk));
            }
            b"VP8L" => {
                if *d.first()? != 0x2F {
                    return None;
                }
                let bits = le32(d, 1)?;
                return Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1, (bits >> 28) & 1 == 1));
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ICC profiles
// ---------------------------------------------------------------------------

/// Profiles larger than this are not re-embedded: web images don't carry
/// multi-megabyte print profiles, and embedding one would dwarf the image.
pub const MAX_ICC_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccSpace {
    Rgb,
    Gray,
    Other,
}

pub fn icc_space(icc: &[u8]) -> IccSpace {
    match icc.get(16..20) {
        Some(b"RGB ") => IccSpace::Rgb,
        Some(b"GRAY") => IccSpace::Gray,
        _ => IccSpace::Other,
    }
}

fn icc_tag<'a>(icc: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
    let count = be32(icc, 128)? as usize;
    for n in 0..count.min(256) {
        let e = 132 + n * 12;
        if icc.get(e..e + 4)? == sig {
            let off = be32(icc, e + 4)? as usize;
            let size = be32(icc, e + 8)? as usize;
            return icc.get(off..off.checked_add(size)?);
        }
    }
    None
}

fn s15f16(b: &[u8], at: usize) -> Option<f64> {
    Some(be32(b, at)? as i32 as f64 / 65536.0)
}

fn icc_xyz(icc: &[u8], sig: &[u8; 4]) -> Option<[f64; 3]> {
    let t = icc_tag(icc, sig)?;
    if t.get(0..4)? != b"XYZ " {
        return None;
    }
    Some([s15f16(t, 8)?, s15f16(t, 12)?, s15f16(t, 16)?])
}

/// Evaluate a TRC tag (`curv` or `para`) at input x in 0..1.
fn icc_trc(icc: &[u8], sig: &[u8; 4], x: f64) -> Option<f64> {
    let t = icc_tag(icc, sig)?;
    match t.get(0..4)? {
        b"curv" => {
            let n = be32(t, 8)? as usize;
            match n {
                0 => Some(x),
                1 => Some(x.powf(be16(t, 12)? as f64 / 256.0)),
                _ => {
                    let pos = x * (n - 1) as f64;
                    let i = (pos.floor() as usize).min(n - 2);
                    let frac = pos - i as f64;
                    let a = be16(t, 12 + i * 2)? as f64 / 65535.0;
                    let b = be16(t, 12 + (i + 1) * 2)? as f64 / 65535.0;
                    Some(a + (b - a) * frac)
                }
            }
        }
        b"para" => {
            let kind = be16(t, 8)?;
            let p = |k: usize| s15f16(t, 12 + k * 4);
            let g = p(0)?;
            Some(match kind {
                0 => x.powf(g),
                3 => {
                    let (a, b, c, d) = (p(1)?, p(2)?, p(3)?, p(4)?);
                    if x >= d { (a * x + b).powf(g) } else { c * x }
                }
                4 => {
                    let (a, b, c, d, e, f) = (p(1)?, p(2)?, p(3)?, p(4)?, p(5)?, p(6)?);
                    if x >= d { (a * x + b).powf(g) + e } else { c * x + f }
                }
                _ => return None,
            })
        }
        _ => None,
    }
}

fn srgb_eotf(x: f64) -> f64 {
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn has_srgb_trc(icc: &[u8]) -> bool {
    [b"rTRC", b"gTRC", b"bTRC"].iter().all(|sig| {
        [0.02, 0.1, 0.25, 0.5, 0.75, 0.95].iter().all(|&x| {
            icc_trc(icc, sig, x).is_some_and(|y| (y - srgb_eotf(x)).abs() < 0.006)
        })
    })
}

fn primaries_match(icc: &[u8], expected: [[f64; 3]; 3]) -> bool {
    [b"rXYZ", b"gXYZ", b"bXYZ"].iter().zip(expected).all(|(sig, exp)| {
        icc_xyz(icc, sig).is_some_and(|v| v.iter().zip(exp).all(|(a, b)| (a - b).abs() < 0.006))
    })
}

/// sRGB primaries, D50-adapted as stored in ICC profiles.
const SRGB_D50: [[f64; 3]; 3] = [[0.4361, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]];
/// Display P3 primaries, D50-adapted.
const P3_D50: [[f64; 3]; 3] = [[0.5151, 0.2412, -0.0011], [0.2920, 0.6922, 0.0419], [0.1571, 0.0666, 0.7841]];

/// True only when the profile really is sRGB (primaries and tone curve), so
/// dropping it changes nothing: browsers assume sRGB for untagged images.
/// A profile merely *named* sRGB with a different curve is kept.
pub fn icc_is_srgb(icc: &[u8]) -> bool {
    icc_space(icc) == IccSpace::Rgb && primaries_match(icc, SRGB_D50) && has_srgb_trc(icc)
}

/// Display P3 (Apple devices, most phone photos): same tone curve as sRGB,
/// wider primaries. Used so quality is judged in the right color space.
pub fn icc_is_display_p3(icc: &[u8]) -> bool {
    icc_space(icc) == IccSpace::Rgb && primaries_match(icc, P3_D50) && has_srgb_trc(icc)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal but valid ICC v2 profile with given primaries and a
    /// parametric sRGB tone curve.
    pub(crate) fn make_profile(primaries: [[f64; 3]; 3], srgb_curve: bool) -> Vec<u8> {
        let fixed = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
        let xyz = |v: [f64; 3]| {
            let mut t = b"XYZ \0\0\0\0".to_vec();
            for c in v {
                t.extend_from_slice(&fixed(c));
            }
            t
        };
        let trc = if srgb_curve {
            let mut t = b"para\0\0\0\0\0\x03\0\0".to_vec();
            for p in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
                t.extend_from_slice(&fixed(p));
            }
            t
        } else {
            b"curv\0\0\0\0\0\0\0\0".to_vec() // identity = linear light
        };
        let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"rXYZ", xyz(primaries[0])),
            (b"gXYZ", xyz(primaries[1])),
            (b"bXYZ", xyz(primaries[2])),
            (b"rTRC", trc.clone()),
            (b"gTRC", trc.clone()),
            (b"bTRC", trc),
        ];
        let mut p = vec![0u8; 128];
        p[12..16].copy_from_slice(b"mntr");
        p[16..20].copy_from_slice(b"RGB ");
        p[20..24].copy_from_slice(b"XYZ ");
        p[36..40].copy_from_slice(b"acsp");
        p.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut offset = 128 + 4 + tags.len() * 12;
        let mut data = Vec::new();
        for (sig, t) in &tags {
            p.extend_from_slice(*sig);
            p.extend_from_slice(&(offset as u32).to_be_bytes());
            p.extend_from_slice(&(t.len() as u32).to_be_bytes());
            data.extend_from_slice(t);
            offset += t.len();
        }
        p.extend_from_slice(&data);
        let len = p.len() as u32;
        p[0..4].copy_from_slice(&len.to_be_bytes());
        p
    }

    #[test]
    fn classifies_profiles_by_content_not_name() {
        let srgb = make_profile(SRGB_D50, true);
        let p3 = make_profile(P3_D50, true);
        let linear_srgb = make_profile(SRGB_D50, false);
        assert!(icc_is_srgb(&srgb));
        assert!(!icc_is_srgb(&p3));
        assert!(icc_is_display_p3(&p3));
        assert!(!icc_is_srgb(&linear_srgb), "linear-light sRGB primaries must be kept");
        assert_eq!(icc_space(&srgb), IccSpace::Rgb);
        for junk in [&b""[..], b"acsp", &[0xFFu8; 300][..]] {
            assert!(!icc_is_srgb(junk) && !icc_is_display_p3(junk));
        }
    }

    #[test]
    fn jpeg_component_count() {
        #[rustfmt::skip]
        let sof = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0,
                   0xFF, 0xC2, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x10, 0x04];
        assert_eq!(jpeg_components(&sof), Some(4));
        assert_eq!(jpeg_components(&[0xFF, 0xD8, 0xFF, 0xDA]), None);
        for bad in [&b""[..], &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01][..], &[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF][..]] {
            let _ = jpeg_components(bad); // must not panic
        }
    }

    fn png_bytes() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_pixel(4, 4, image::Rgb([10u8, 200, 30])));
        let mut b = std::io::Cursor::new(Vec::new());
        img.write_to(&mut b, image::ImageFormat::Png).unwrap();
        b.into_inner()
    }

    #[test]
    fn png_chunks_roundtrip_through_a_real_decoder() {
        let icc = make_profile(P3_D50, true);
        let tagged = png_with_chunks(&png_bytes(), &[png_iccp(&icc)]).unwrap();
        let mut dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&tagged)).unwrap();
        use image::ImageDecoder;
        assert_eq!(dec.icc_profile().unwrap().as_deref(), Some(&icc[..]), "iCCP readable with valid CRC");
        assert_eq!(png_color_chunks(&tagged).len(), 1);
        assert!(!png_is_animated(&tagged));
        // Inserting again does not duplicate.
        let twice = png_with_chunks(&tagged, &[png_iccp(&icc)]).unwrap();
        assert_eq!(twice.len(), tagged.len());
        assert!(png_with_chunks(b"not a png", &[]).is_none());
    }

    #[test]
    fn webp_icc_embedding_is_valid() {
        let icc = make_profile(P3_D50, true);
        for alpha in [false, true] {
            let px: Vec<u8> = (0..16 * 12).flat_map(|i| [i as u8, 90, 200, if alpha { (i * 3) as u8 } else { 255 }]).collect();
            let enc = webp::Encoder::from_rgba(&px, 16, 12);
            for encoded in [enc.encode(80.0).to_vec(), enc.encode_lossless().to_vec()] {
                let tagged = webp_with_icc(&encoded, &icc).unwrap();
                let mut dec = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&tagged)).unwrap();
                use image::ImageDecoder;
                assert_eq!(dec.dimensions(), (16, 12));
                assert_eq!(dec.icc_profile().unwrap().as_deref(), Some(&icc[..]));
                let img = image::DynamicImage::from_decoder(dec).unwrap();
                assert_eq!(img.color().has_alpha(), alpha, "alpha flag preserved");
                assert!(!webp_is_animated(&tagged));
            }
        }
        assert!(webp_with_icc(b"RIFF\x04\0\0\0WEBP", &icc).is_none());
    }

    /// Every parser here reads untrusted bytes: no input may make one panic.
    #[test]
    fn parsers_never_panic_on_corrupt_input() {
        let icc = make_profile(P3_D50, true);
        let png = png_with_chunks(&png_bytes(), &[png_iccp(&icc)]).unwrap();
        let webp = webp_with_icc(&webp::Encoder::from_rgb(&[9u8; 8 * 8 * 3], 8, 8).encode(70.0), &icc).unwrap();
        let jpeg = {
            let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_pixel(8, 8, image::Rgb([1u8, 2, 3])));
            let mut b = std::io::Cursor::new(Vec::new());
            img.write_to(&mut b, image::ImageFormat::Jpeg).unwrap();
            b.into_inner()
        };
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for base in [&png, &webp, &jpeg, &icc] {
            let mut variants: Vec<Vec<u8>> = (0..base.len()).step_by(3).map(|n| base[..n].to_vec()).collect();
            for _ in 0..200 {
                let mut v = base.to_vec();
                for _ in 0..4 {
                    let i = rnd() as usize % v.len();
                    v[i] = rnd() as u8;
                }
                variants.push(v);
            }
            variants.push((0..512).map(|_| rnd() as u8).collect());
            for v in &variants {
                let _ = (png_is_animated(v), png_color_chunks(v), png_with_chunks(v, &[png_iccp(&icc)]));
                let _ = (webp_is_animated(v), webp_with_icc(v, &icc), jpeg_components(v));
                let _ = (icc_is_srgb(v), icc_is_display_p3(v), icc_space(v));
            }
        }
    }

    #[test]
    fn animation_detection() {
        let mut apng = png_bytes();
        // Splice an acTL chunk after IHDR.
        apng = png_with_chunks(&apng, &[(*b"acTL", vec![0, 0, 0, 2, 0, 0, 0, 0])]).unwrap();
        assert!(png_is_animated(&apng));
        let mut anim = b"RIFF\x24\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0\0\0\0\0\0\0ANIM\x06\0\0\0\0\0\0\0\0\0".to_vec();
        assert!(webp_is_animated(&anim));
        anim.truncate(20);
        let _ = webp_is_animated(&anim);
    }
}
