//! Color fidelity check, complementing SSIMULACRA 2.
//!
//! SSIM-family metrics (SSIMULACRA 2 included) are built to catch
//! compression *artifacts* — blocking, ringing, blur, banding — and are
//! nearly blind to smooth color shifts: a 2% red cast over a whole photo
//! still scores ~92. Encoders can cause exactly that kind of error (a flat
//! brand color nudged to the nearest palette entry, chroma quantization
//! shifting a saturated region), so colors get their own gate.
//!
//! Colors are compared as CIEDE2000 differences (the standard perceptual
//! color-difference formula; 1.0 ≈ a just-noticeable difference) between
//! *block-averaged* colors, which ignores pixel-level noise and texture —
//! that is SSIMULACRA 2's job — and measures what the eye reads as "the color
//! of that area".

/// Limits, in CIEDE2000 units. At the limit the color score equals the
/// engine's target score; beyond it the candidate fails.
const MAX_CAST: f64 = 1.0; // shift of the image's overall color
const MAX_P99_8: f64 = 3.0; // worst 1% of 8×8 areas
const MAX_16: f64 = 4.0; // any single 16×16 area (small logos, brand colors)

pub struct ColorRef {
    width: usize,
    height: usize,
    blocks8: Vec<[f64; 3]>,
    blocks16: Vec<[f64; 3]>,
    mean: [f64; 3],
}

impl ColorRef {
    /// `linear`: linear-light RGB (BT.709 primaries), row-major.
    pub fn new(linear: &[[f32; 3]], width: usize, height: usize) -> ColorRef {
        let blocks8 = block_labs(linear, width, height, 8);
        let blocks16 = block_labs(linear, width, height, 16);
        let mean = mean_lab(&blocks8);
        ColorRef { width, height, blocks8, blocks16, mean }
    }

    /// Color score on the same 0..100 scale as SSIMULACRA 2, calibrated so
    /// that hitting any limit maps to `target`.
    pub fn score(&self, linear: &[[f32; 3]], target: f64) -> f64 {
        let b8 = block_labs(linear, self.width, self.height, 8);
        let b16 = block_labs(linear, self.width, self.height, 16);
        let cast = ciede2000(self.mean, mean_lab(&b8));
        let mut d8: Vec<f64> = self.blocks8.iter().zip(&b8).map(|(a, b)| ciede2000(*a, *b)).collect();
        d8.sort_by(|a, b| a.total_cmp(b));
        let p99 = d8.get(((d8.len() as f64) * 0.99) as usize).or(d8.last()).copied().unwrap_or(0.0);
        let max16 = self.blocks16.iter().zip(&b16).map(|(a, b)| ciede2000(*a, *b)).fold(0.0, f64::max);
        let worst = (cast / MAX_CAST).max(p99 / MAX_P99_8).max(max16 / MAX_16);
        100.0 - (100.0 - target) * worst
    }
}

fn block_labs(linear: &[[f32; 3]], width: usize, height: usize, size: usize) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(width.div_ceil(size) * height.div_ceil(size));
    for by in (0..height).step_by(size) {
        for bx in (0..width).step_by(size) {
            let mut sum = [0f64; 3];
            let mut n = 0f64;
            for y in by..(by + size).min(height) {
                for p in &linear[y * width + bx..y * width + (bx + size).min(width)] {
                    sum[0] += p[0] as f64;
                    sum[1] += p[1] as f64;
                    sum[2] += p[2] as f64;
                    n += 1.0;
                }
            }
            out.push(linear_to_lab([sum[0] / n, sum[1] / n, sum[2] / n]));
        }
    }
    out
}

fn mean_lab(labs: &[[f64; 3]]) -> [f64; 3] {
    let n = labs.len().max(1) as f64;
    let s = labs.iter().fold([0f64; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
    [s[0] / n, s[1] / n, s[2] / n]
}

/// Linear BT.709 RGB → CIE L*a*b* (D65).
pub fn linear_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb;
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = (0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b) / 1.088_83;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 color difference (Sharma, Wu & Dalal 2005).
pub fn ciede2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    use std::f64::consts::PI;
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let c_bar = ((a1 * a1 + b1 * b1).sqrt() + (a2 * a2 + b2 * b2).sqrt()) / 2.0;
    let c7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c7 / (c7 + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hue = |b: f64, a: f64| {
        if b == 0.0 && a == 0.0 {
            0.0
        } else {
            let h = b.atan2(a);
            if h < 0.0 { h + 2.0 * PI } else { h }
        }
    };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));

    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > PI {
            d - 2.0 * PI
        } else if d < -PI {
            d + 2.0 * PI
        } else {
            d
        }
    };
    let dh_big = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).sin();

    let l_bar = (l1 + l2) / 2.0;
    let cp_bar = (c1p + c2p) / 2.0;
    let hp_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= PI {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 2.0 * PI {
        (h1p + h2p + 2.0 * PI) / 2.0
    } else {
        (h1p + h2p - 2.0 * PI) / 2.0
    };
    let t = 1.0 - 0.17 * (hp_bar - PI / 6.0).cos()
        + 0.24 * (2.0 * hp_bar).cos()
        + 0.32 * (3.0 * hp_bar + PI / 30.0).cos()
        - 0.20 * (4.0 * hp_bar - 63.0 * PI / 180.0).cos();
    let d_theta = (PI / 6.0) * (-((hp_bar.to_degrees() - 275.0) / 25.0).powi(2)).exp();
    let cp7 = cp_bar.powi(7);
    let r_c = 2.0 * (cp7 / (cp7 + 25f64.powi(7))).sqrt();
    let s_l = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * cp_bar;
    let s_h = 1.0 + 0.015 * cp_bar * t;
    let r_t = -(2.0 * d_theta).sin() * r_c;
    ((dl / s_l).powi(2) + (dc / s_c).powi(2) + (dh_big / s_h).powi(2) + r_t * (dc / s_c) * (dh_big / s_h)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciede2000_matches_published_test_data() {
        // From Sharma, Wu & Dalal, "The CIEDE2000 Color-Difference Formula:
        // Implementation Notes, Supplementary Test Data".
        let cases = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0011], 7.2195),
            ([50.0, -0.001, 2.49], [50.0, 0.0009, -2.49], 4.8045),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([22.7233, 20.0904, -46.694], [23.0331, 14.973, -42.5619], 2.0373),
            ([2.0776, 0.0795, -1.135], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (a, b, expected) in cases {
            let d = ciede2000(a, b);
            assert!((d - expected).abs() < 1e-4, "{a:?} vs {b:?}: {d} != {expected}");
            assert!((ciede2000(b, a) - expected).abs() < 1e-4, "symmetric");
        }
    }

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Vec<[f32; 3]> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    #[test]
    fn noise_passes_but_a_color_cast_fails() {
        let (w, h) = (128, 96);
        let base = image(w, h, |x, y| [0.1 + 0.6 * x as f32 / w as f32, 0.2 + 0.5 * y as f32 / h as f32, 0.3]);
        let r = ColorRef::new(&base, w, h);
        assert!((r.score(&base, 80.0) - 100.0).abs() < 1e-9);

        // Pixel noise that averages out within blocks: colors look the same.
        let noisy = image(w, h, |x, y| {
            let p = base[y * w + x];
            let n = if (x + y) % 2 == 0 { 0.01 } else { -0.01 };
            [p[0] + n, p[1] - n, p[2] + n]
        });
        assert!(r.score(&noisy, 80.0) > 95.0);

        // A 3% red cast over everything: must fail the target.
        let cast: Vec<[f32; 3]> = base.iter().map(|p| [p[0] * 0.94, p[1], p[2]]).collect();
        assert!(r.score(&cast, 80.0) < 80.0, "cast score {}", r.score(&cast, 80.0));

        // A small area changing color (a logo's brand color): must fail too.
        let logo = image(w, h, |x, y| {
            let p = base[y * w + x];
            if (40..56).contains(&x) && (40..56).contains(&y) { [p[0] * 0.8, p[1] * 1.1, p[2]] } else { p }
        });
        assert!(r.score(&logo, 80.0) < 80.0, "logo score {}", r.score(&logo, 80.0));
    }
}
