//! Perceptual quality measurement with SSIMULACRA 2.
//!
//! SSIMULACRA 2 (by Jon Sneyers, Cloudinary) is a full-reference metric built
//! for exactly this job: telling whether a compressed image can be told apart
//! from its original. It works in the XYB color space used by JPEG XL, so
//! chroma errors (color bleeding, hue shifts, banding in colored gradients)
//! count as much as a human would notice them, not only luminance.
//!
//! Scores: 100 = identical, 90 = visually lossless even in a flicker test,
//! 80 = impossible to tell apart side by side at 1:1, 70 = high quality.
//!
//! This is an adaptation of the `ssimulacra2` crate (BSD-2-Clause, the rust-av
//! project; see THIRD-PARTY-LICENSES). The engine scores many candidate
//! encodings against one original, so the reference half of the work (color
//! conversion, pyramid, blurred means and variances) is computed once and
//! cached instead of on every comparison. Blur and XYB conversion are the
//! crate's own, so results match it (see the equivalence test below).

use ssimulacra2::{Blur, LinearRgb, Xyb};

const NUM_SCALES: usize = 6;

/// Smallest image SSIMULACRA 2 can score; smaller inputs are upscaled with
/// nearest-neighbor sampling first (which preserves every pixel's value).
pub const MIN_SIDE: usize = 8;

pub struct Reference {
    width: usize,
    height: usize,
    scales: Vec<RefScale>,
}

struct RefScale {
    img: [Vec<f32>; 3],
    mu: [Vec<f32>; 3],
    sigma_sq: [Vec<f32>; 3],
}

impl Reference {
    /// `linear`: linear-light RGB (BT.709/sRGB primaries), row-major.
    pub fn new(linear: Vec<[f32; 3]>, width: usize, height: usize) -> Option<Reference> {
        if width < MIN_SIDE || height < MIN_SIDE || linear.len() != width * height {
            return None;
        }
        let mut blur = Blur::new(width, height);
        let mut scales = Vec::with_capacity(NUM_SCALES);
        let mut lin = LinearRgb::new(linear, width, height).ok()?;
        let (mut w, mut h) = (width, height);
        for scale in 0..NUM_SCALES {
            // Like the reference implementation: the size check happens
            // before halving, so the last scale may be smaller than 8 px.
            if w < MIN_SIDE || h < MIN_SIDE {
                break;
            }
            if scale > 0 {
                lin = downscale_by_2(&lin);
                (w, h) = (lin.width(), lin.height());
            }
            blur.shrink_to(w, h);
            let img = to_positive_planar(Xyb::from(lin.clone()));
            let sq = multiply(&img, &img);
            let sigma_sq = blur.blur(&sq);
            let mu = blur.blur(&img);
            scales.push(RefScale { img, mu, sigma_sq });
        }
        Some(Reference { width, height, scales })
    }

    /// Score a candidate of the same dimensions (linear-light RGB).
    pub fn score(&self, linear: Vec<[f32; 3]>) -> Option<f64> {
        if linear.len() != self.width * self.height {
            return None;
        }
        let mut blur = Blur::new(self.width, self.height);
        let mut lin = LinearRgb::new(linear, self.width, self.height).ok()?;
        let (mut w, mut h) = (self.width, self.height);
        let mut msssim = Vec::with_capacity(self.scales.len());
        for (scale, r) in self.scales.iter().enumerate() {
            if scale > 0 {
                lin = downscale_by_2(&lin);
                (w, h) = (lin.width(), lin.height());
            }
            blur.shrink_to(w, h);
            let img = to_positive_planar(Xyb::from(lin.clone()));
            let sigma12 = blur.blur(&multiply(&r.img, &img));
            let sigma2_sq = blur.blur(&multiply(&img, &img));
            let mu2 = blur.blur(&img);
            msssim.push(MsssimScale {
                avg_ssim: ssim_map(w, h, &r.mu, &mu2, &r.sigma_sq, &sigma2_sq, &sigma12),
                avg_edgediff: edge_diff_map(w, h, &r.img, &r.mu, &img, &mu2),
            });
        }
        Some(score(&msssim))
    }
}

/// XYB, shifted so every component is roughly in 0..1 (the SSIM formula
/// needs pixel-wise differences <= 1), split into planes.
fn to_positive_planar(xyb: Xyb) -> [Vec<f32>; 3] {
    let n = xyb.width() * xyb.height();
    let mut out = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
    for p in xyb.data() {
        out[0].push(p[0].mul_add(14.0, 0.42));
        out[1].push(p[1] + 0.01);
        out[2].push((p[2] - p[1]) + 0.55);
    }
    out
}

fn multiply(a: &[Vec<f32>; 3], b: &[Vec<f32>; 3]) -> [Vec<f32>; 3] {
    let m = |x: &Vec<f32>, y: &Vec<f32>| x.iter().zip(y).map(|(p, q)| p * q).collect::<Vec<f32>>();
    [m(&a[0], &b[0]), m(&a[1], &b[1]), m(&a[2], &b[2])]
}

/// 2x2 box downscale in linear light (edge pixels replicated).
fn downscale_by_2(input: &LinearRgb) -> LinearRgb {
    let (in_w, in_h) = (input.width(), input.height());
    let (out_w, out_h) = (in_w.div_ceil(2), in_h.div_ceil(2));
    let data = input.data();
    let mut out = vec![[0.0f32; 3]; out_w * out_h];
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0.0f32; 3];
            for iy in 0..2 {
                for ix in 0..2 {
                    let x = (ox * 2 + ix).min(in_w - 1);
                    let y = (oy * 2 + iy).min(in_h - 1);
                    let p = data[y * in_w + x];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
            }
            out[oy * out_w + ox] = [sum[0] * 0.25, sum[1] * 0.25, sum[2] * 0.25];
        }
    }
    LinearRgb::new(out, out_w, out_h).expect("dimensions match")
}

fn ssim_map(
    width: usize,
    height: usize,
    m1: &[Vec<f32>; 3],
    m2: &[Vec<f32>; 3],
    s11: &[Vec<f32>; 3],
    s22: &[Vec<f32>; 3],
    s12: &[Vec<f32>; 3],
) -> [f64; 6] {
    const C2: f32 = 0.0009;
    let one_per_pixels = 1.0f64 / (width * height) as f64;
    let mut out = [0f64; 6];
    for c in 0..3 {
        let (mut sum1, mut sum4) = (0.0f64, 0.0f64);
        for i in 0..width * height {
            let (mu1, mu2) = (m1[c][i], m2[c][i]);
            let mu_diff = mu1 - mu2;
            // SSIM without the luminance denominator (see the SSIMULACRA 2
            // paper: values are already perceptually uniform).
            let num_m = mu_diff.mul_add(-mu_diff, 1.0);
            let num_s = 2f32.mul_add(s12[c][i] - mu1 * mu2, C2);
            let denom_s = (s11[c][i] - mu1 * mu1) + (s22[c][i] - mu2 * mu2) + C2;
            let d = (1.0f64 - f64::from((num_m * num_s) / denom_s)).max(0.0);
            sum1 += d;
            sum4 += d.powi(4);
        }
        out[c * 2] = one_per_pixels * sum1;
        out[c * 2 + 1] = (one_per_pixels * sum4).sqrt().sqrt();
    }
    out
}

fn edge_diff_map(
    width: usize,
    height: usize,
    img1: &[Vec<f32>; 3],
    mu1: &[Vec<f32>; 3],
    img2: &[Vec<f32>; 3],
    mu2: &[Vec<f32>; 3],
) -> [f64; 12] {
    let one_per_pixels = 1.0f64 / (width * height) as f64;
    let mut out = [0f64; 12];
    for c in 0..3 {
        let mut sum = [0.0f64; 4];
        for i in 0..width * height {
            let d1 = (1.0 + f64::from((img2[c][i] - mu2[c][i]).abs()))
                / (1.0 + f64::from((img1[c][i] - mu1[c][i]).abs()))
                - 1.0;
            // d1 > 0: edges where the original is smooth (ringing, banding,
            // blocking). d1 < 0: original edges that got lost (blurring).
            let artifact = d1.max(0.0);
            let detail_lost = (-d1).max(0.0);
            sum[0] += artifact;
            sum[1] += artifact.powi(4);
            sum[2] += detail_lost;
            sum[3] += detail_lost.powi(4);
        }
        out[c * 4] = one_per_pixels * sum[0];
        out[c * 4 + 1] = (one_per_pixels * sum[1]).sqrt().sqrt();
        out[c * 4 + 2] = one_per_pixels * sum[2];
        out[c * 4 + 3] = (one_per_pixels * sum[3]).sqrt().sqrt();
    }
    out
}

struct MsssimScale {
    avg_ssim: [f64; 6],
    avg_edgediff: [f64; 12],
}

/// Weighted sum of the 108 sub-scores, mapped to the 0..100 scale. Weights
/// are SSIMULACRA 2's, fitted to subjective scores (CID22 and others).
fn score(scales: &[MsssimScale]) -> f64 {
    #[rustfmt::skip]
    const WEIGHT: [f64; 108] = [
        0.0, 0.000_737_660_670_740_658_6, 0.0, 0.0, 0.000_779_348_168_286_730_9, 0.0,
        0.0, 0.000_437_115_573_010_737_9, 0.0, 1.104_172_642_665_734_6, 0.000_662_848_341_292_71, 0.000_152_316_327_837_187_52,
        0.0, 0.001_640_643_745_659_975_4, 0.0, 1.842_245_552_053_929_8, 11.441_172_603_757_666, 0.0,
        0.000_798_910_943_601_516_3, 0.000_176_816_438_078_653, 0.0, 1.878_759_497_954_638_7, 10.949_069_906_051_42, 0.0,
        0.000_728_934_699_150_807_2, 0.967_793_708_062_683_3, 0.0, 0.000_140_034_242_854_358_84, 0.998_176_697_785_496_7, 0.000_319_497_559_344_350_53,
        0.000_455_099_211_379_206_3, 0.0, 0.0, 0.001_364_876_616_324_339_8, 0.0, 0.0,
        0.0, 0.0, 0.0, 7.466_890_328_078_848, 0.0, 17.445_833_984_131_262,
        0.000_623_560_163_404_146_6, 0.0, 0.0, 6.683_678_146_179_332, 0.000_377_244_079_796_112_96, 1.027_889_937_768_264,
        225.205_153_008_492_74, 0.0, 0.0, 19.213_238_186_143_016, 0.001_140_152_458_661_836_1, 0.001_237_755_635_509_985,
        176.393_175_984_506_94, 0.0, 0.0, 24.433_009_998_704_76, 0.285_208_026_121_177_57, 0.000_448_543_692_383_340_8,
        0.0, 0.0, 0.0, 34.779_063_444_837_72, 44.835_625_328_877_896, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.000_868_055_657_329_169_8, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.000_531_319_187_435_874_7, 0.0, 0.000_165_338_141_613_791_12, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.000_417_917_180_325_133_6, 0.001_729_082_823_472_283_3, 0.0,
        0.002_082_700_584_663_643_7, 0.0, 0.0, 8.826_982_764_996_862, 23.192_433_439_989_26, 0.0,
        95.108_049_881_108_6, 0.986_397_803_440_068_2, 0.983_438_279_246_535_3, 0.001_228_640_504_827_849_3, 171.266_725_589_730_7, 0.980_785_887_243_537_9,
        0.0, 0.0, 0.0, 0.000_513_006_458_899_067_9, 0.0, 0.000_108_540_578_584_115_37,
    ];
    let mut ssim = 0.0f64;
    let mut i = 0usize;
    for c in 0..3 {
        for scale in scales {
            for n in 0..2 {
                ssim = WEIGHT[i].mul_add(scale.avg_ssim[c * 2 + n].abs(), ssim);
                i += 1;
                ssim = WEIGHT[i].mul_add(scale.avg_edgediff[c * 4 + n].abs(), ssim);
                i += 1;
                ssim = WEIGHT[i].mul_add(scale.avg_edgediff[c * 4 + n + 2].abs(), ssim);
                i += 1;
            }
        }
    }
    ssim *= 0.956_238_261_683_484_4;
    ssim = (6.248_496_625_763_138e-5 * ssim * ssim).mul_add(
        ssim,
        2.326_765_642_916_932f64.mul_add(ssim, -0.020_884_521_182_843_837 * ssim * ssim),
    );
    if ssim > 0.0 {
        ssim.powf(0.627_633_646_783_138_7).mul_add(-10.0, 100.0)
    } else {
        100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: usize, h: usize, seed: u32) -> Vec<[f32; 3]> {
        let mut s = seed;
        (0..w * h)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                let n = (s % 1000) as f32 / 20_000.0;
                [(0.2 + 0.6 * x + n).min(1.0), (0.1 + 0.7 * y).min(1.0), (0.5 + 0.3 * (x * 9.0).sin() * y).clamp(0.0, 1.0)]
            })
            .collect()
    }

    fn crate_score(a: &[[f32; 3]], b: &[[f32; 3]], w: usize, h: usize) -> f64 {
        let a = LinearRgb::new(a.to_vec(), w, h).unwrap();
        let b = LinearRgb::new(b.to_vec(), w, h).unwrap();
        ssimulacra2::compute_frame_ssimulacra2(a, b).unwrap()
    }

    #[test]
    fn matches_the_reference_implementation() {
        for (w, h) in [(64, 48), (200, 130), (33, 17)] {
            let a = pattern(w, h, 7);
            let reference = Reference::new(a.clone(), w, h).unwrap();
            for (seed, gain) in [(9, 1.0f32), (11, 0.9), (13, 1.05)] {
                let b: Vec<[f32; 3]> = pattern(w, h, seed)
                    .iter()
                    .map(|p| [(p[0] * gain).min(1.0), p[1], (p[2] * 0.97).min(1.0)])
                    .collect();
                let ours = reference.score(b.clone()).unwrap();
                let theirs = crate_score(&a, &b, w, h);
                assert!((ours - theirs).abs() < 1e-6, "{w}x{h}: ours {ours} vs crate {theirs}");
            }
        }
    }

    #[test]
    fn identical_scores_100_and_distortion_lowers_the_score() {
        let a = pattern(80, 60, 3);
        let reference = Reference::new(a.clone(), 80, 60).unwrap();
        assert!(reference.score(a.clone()).unwrap() > 99.99, "identical input scores (float-rounding close to) 100");
        // Artifacts are penalized...
        let blocky: Vec<[f32; 3]> = a.iter().enumerate().map(|(i, p)| {
            let (x, y) = (i % 80, i / 80);
            let q = a[(y / 8 * 8) * 80 + x / 8 * 8];
            [q[0], p[1], q[2]]
        }).collect();
        assert!(reference.score(blocky).unwrap() < 80.0, "blocking must be penalized");
        // ...while smooth color casts barely are — which is why the engine
        // pairs this metric with the CIEDE2000 color gate in color.rs.
        let cast: Vec<[f32; 3]> = a.iter().map(|p| [p[0] * 0.9, p[1], p[2]]).collect();
        assert!(reference.score(cast).unwrap() > 80.0);
    }

    #[test]
    fn rejects_tiny_or_mismatched_input() {
        assert!(Reference::new(pattern(7, 20, 1), 7, 20).is_none());
        let r = Reference::new(pattern(20, 20, 1), 20, 20).unwrap();
        assert!(r.score(pattern(20, 19, 1)).is_none());
    }
}

