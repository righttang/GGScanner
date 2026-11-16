// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted in Rust from BetterGI's CameraOrientationCalculator and the polar
// edge method in CameraOrientationFromGia, revision d300886ffcad76f9f16518b43a0affb8c8aef84d.
// Upstream: https://github.com/babalae/better-genshin-impact
// License: licenses/BetterGI-GPL-3.0.txt. Changes: no OpenCV, explicit masked
// sampling, bounded histogram storage and ambiguity rejection.
use anyhow::{ensure, Result};
use image::{imageops, RgbImage};

fn rgb_at(frame: &RgbImage, x: f64, y: f64) -> [f64; 3] {
    let (ix, iy) = (x.floor() as u32, y.floor() as u32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    std::array::from_fn(|c| {
        let a = frame.get_pixel(ix, iy)[c] as f64 * (1.0 - fx)
            + frame.get_pixel(ix + 1, iy)[c] as f64 * fx;
        let b = frame.get_pixel(ix, iy + 1)[c] as f64 * (1.0 - fx)
            + frame.get_pixel(ix + 1, iy + 1)[c] as f64 * fx;
        (a * (1.0 - fy) + b * fy).round()
    })
}
fn gray(rgb: [f64; 3]) -> f64 {
    (rgb[0] * 0.299 + rgb[1] * 0.587 + rgb[2] * 0.114).round()
}
fn hue(rgb: [f64; 3]) -> usize {
    let max = rgb.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let min = rgb.into_iter().fold(f64::INFINITY, f64::min);
    if max - min < 0.01 {
        return 0;
    }
    let h = if max == rgb[0] {
        (rgb[1] - rgb[2]) / (max - min)
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / (max - min) + 2.0
    } else {
        (rgb[0] - rgb[1]) / (max - min) + 4.0
    };
    ((h.rem_euclid(6.0) * 256.0 / 6.0).round() as usize) % 256
}
pub(super) fn heading(frame: &RgbImage) -> Result<f64> {
    alpha_heading(frame).or_else(|_| polar_heading(frame))
}
fn alpha_heading(frame: &RgbImage) -> Result<f64> {
    const BREAKS: [f64; 13] = [
        18.632, 20.157, 24.093, 34.617, 38.566, 41.94, 47.654, 51.087, 58.561, 63.925, 67.759,
        71.77, 75.214,
    ];
    let mini = imageops::crop_imm(frame, 64, 20, 210, 210).to_image();
    let mask = super::vision::mini_map_overlay_mask(&mini);
    let mut samples = Vec::with_capacity(21600);
    let mut hist_a = vec![0u32; 256 * 256];
    let mut hist_b = vec![0u32; 256 * 256];
    for degree in 0..360 {
        let angle = (degree as f64).to_radians();
        for r in 0..60 {
            let radius = 19.0 + 59.0 * r as f64 / 59.0;
            let (x, y) = (168.0 + radius * angle.cos(), 125.0 + radius * angle.sin());
            if mask.get_pixel((x - 64.0).round() as u32, (y - 20.0).round() as u32)[0] != 0 {
                continue;
            }
            let rgb = rgb_at(frame, x, y);
            let a1 = 229.0 + BREAKS.iter().filter(|&&b| b < radius).count() as f64;
            let a2 = 137.0 + 1.43 * radius;
            let fa = gray(rgb) * 255.0 / a1;
            let fb = ((gray(rgb) - 255.0) * 255.0 / a2 + 255.0) * 255.0 / a1;
            let h = hue(rgb);
            if (0.0..256.0).contains(&fa) {
                hist_a[h * 256 + fa as usize] += 1;
            }
            if (0.0..256.0).contains(&fb) {
                hist_b[h * 256 + fb as usize] += 1;
            }
            samples.push((degree, h, fa, fb));
        }
    }
    let mut score = [0.0; 360];
    let mut counts = [0.0; 360];
    for (degree, h, fa, fb) in samples {
        if fa < 0.0 || fb >= 256.0 {
            continue;
        }
        counts[degree] += 1.0;
        score[degree] += if fa >= 256.0 {
            255.0
        } else if fb < 0.0 {
            -255.0
        } else {
            let (a, b) = (hist_a[h * 256 + fa as usize], hist_b[h * 256 + fb as usize]);
            if a > b {
                0.0
            } else if a == b {
                100.0
            } else {
                255.0
            }
        };
    }
    let sector = |angle: i32| {
        let n = (-45..=45)
            .map(|d| counts[(angle + d).rem_euclid(360) as usize])
            .sum::<f64>();
        if n < 91.0 * 24.0 {
            return -1.0;
        }
        (-45..=45)
            .map(|d| score[(angle + d).rem_euclid(360) as usize])
            .sum::<f64>()
            / (n * 255.0)
    };
    let (angle, best) = (0..360)
        .map(|a| (a, sector(a)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    let other = (0..360)
        .filter(|&a| ((a - angle + 180).rem_euclid(360) - 180).abs() > 75)
        .map(sector)
        .fold(-1.0, f64::max);
    ensure!(
        best >= 0.2 && best - other > 0.035,
        "Camera alpha distribution is ambiguous"
    );
    Ok((angle as f64).to_radians())
}

fn polar_heading(frame: &RgbImage) -> Result<f64> {
    let mut positive = [0.0; 360];
    let mut negative = [0.0; 360];
    for radius in 20..78 {
        let values: Vec<_> = (0..360)
            .map(|a| {
                let a = (a as f64).to_radians();
                gray(rgb_at(
                    frame,
                    168.0 + radius as f64 * a.cos(),
                    125.0 + radius as f64 * a.sin(),
                ))
            })
            .collect();
        let derivative: Vec<_> = (0..360)
            .map(|a| {
                values[(a + 2) % 360] + 2.0 * values[(a + 1) % 360]
                    - 2.0 * values[(a + 359) % 360]
                    - values[(a + 358) % 360]
            })
            .collect();
        for a in 0..360 {
            let d = derivative[a];
            if d > 3.0 && d > derivative[(a + 359) % 360] && d >= derivative[(a + 1) % 360] {
                positive[a] += 1.0;
            }
            if d < -3.0 && d < derivative[(a + 359) % 360] && d <= derivative[(a + 1) % 360] {
                negative[a] += 1.0;
            }
        }
    }
    let edge = |values: &[f64; 360], a: i32| {
        (-2..=2)
            .map(|d| values[(a + d).rem_euclid(360) as usize] * (3 - d.abs()) as f64 / 3.0)
            .sum::<f64>()
    };
    let score = |a: i32| edge(&positive, a - 45) * edge(&negative, a + 45);
    let (angle, best) = (0..360)
        .map(|a| (a, score(a)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    let other = (0..360)
        .filter(|&a| ((a - angle + 180).rem_euclid(360) - 180).abs() > 45)
        .map(score)
        .fold(0.0, f64::max);
    ensure!(
        best >= 100.0 && best > other * 1.25,
        "Camera polar edges are ambiguous"
    );
    Ok((angle as f64).to_radians())
}
