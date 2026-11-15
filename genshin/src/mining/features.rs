//! Feature correspondences plus a verified north-up similarity transform for
//! big maps. Correlation alone is ambiguous along the ocean and map boundary.
use anyhow::{ensure, Result};
use image::{imageops, GrayImage, RgbImage};
use rayon::prelude::*;
use yas::cancel::CancelToken;

pub(super) struct FeatureAtlas {
    features: sift_features::SiftResult,
    origin: (u32, u32),
}

#[derive(Clone, Copy, Debug)]
struct Pair {
    q: (f64, f64),
    r: (f64, f64),
}
#[derive(Clone, Copy, Debug)]
struct Transform {
    a: f64,
    b: f64,
    x: f64,
    y: f64,
}
impl Transform {
    fn apply(self, p: (f64, f64)) -> (f64, f64) {
        (
            self.a * p.0 - self.b * p.1 + self.x,
            self.b * p.0 + self.a * p.1 + self.y,
        )
    }
    fn error(self, p: Pair) -> f64 {
        let r = self.apply(p.q);
        (r.0 - p.r.0).hypot(r.1 - p.r.1)
    }
    fn valid(self) -> bool {
        (0.09..=0.57).contains(&self.a.hypot(self.b)) && self.b.atan2(self.a).abs() < 0.035
    }
}

fn fit(pairs: &[Pair]) -> Option<Transform> {
    if pairs.len() < 2 {
        return None;
    }
    let n = pairs.len() as f64;
    let q = (
        pairs.iter().map(|p| p.q.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.q.1).sum::<f64>() / n,
    );
    let r = (
        pairs.iter().map(|p| p.r.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.r.1).sum::<f64>() / n,
    );
    let (mut dot, mut cross, mut norm) = (0.0, 0.0, 0.0);
    for p in pairs {
        let (qx, qy, rx, ry) = (p.q.0 - q.0, p.q.1 - q.1, p.r.0 - r.0, p.r.1 - r.1);
        dot += qx * rx + qy * ry;
        cross += qx * ry - qy * rx;
        norm += qx * qx + qy * qy;
    }
    if norm < 400.0 {
        return None;
    }
    let (a, b) = (dot / norm, cross / norm);
    let t = Transform {
        a,
        b,
        x: r.0 - a * q.0 + b * q.1,
        y: r.1 - b * q.0 - a * q.1,
    };
    t.valid().then_some(t)
}

fn consensus(pairs: &[Pair], cancel: Option<&CancelToken>) -> Result<Transform> {
    ensure!(
        pairs.len() >= 10,
        "Too few unique map feature matches ({})",
        pairs.len()
    );
    let mut best = Vec::new();
    for (i, p) in pairs.iter().enumerate() {
        ensure!(!cancel.is_some_and(|t| t.check_rmb()), "Mining stopped");
        for other in pairs.iter().skip(i + 1) {
            let Some(t) = fit(&[*p, *other]) else {
                continue;
            };
            let inliers: Vec<_> = pairs
                .iter()
                .copied()
                .filter(|p| t.error(*p) < 2.5)
                .collect();
            if inliers.len() > best.len() {
                best = inliers;
            }
        }
    }
    ensure!(
        best.len() >= 10 && best.len() * 100 >= pairs.len() * 40,
        "Map features disagree geometrically ({}/{})",
        best.len(),
        pairs.len()
    );
    let t = fit(&best).ok_or_else(|| anyhow::anyhow!("Map transform is degenerate"))?;
    let spread = best
        .iter()
        .map(|p| (p.q.0 - best[0].q.0).hypot(p.q.1 - best[0].q.1))
        .fold(0.0, f64::max);
    let residual = best.iter().map(|p| t.error(*p)).sum::<f64>() / best.len() as f64;
    ensure!(
        spread >= 100.0 && residual < 1.5,
        "Map feature coverage or residual is insufficient"
    );
    Ok(t)
}

impl FeatureAtlas {
    pub fn new(
        image: &GrayImage,
        target: (f64, f64),
        cancel: Option<&CancelToken>,
    ) -> Result<Self> {
        ensure!(!cancel.is_some_and(|t| t.check_rmb()), "Mining stopped");
        let (cx, cy) = (4096.0 - target.0 * 0.25, 3072.0 - target.1 * 0.25);
        ensure!(
            cx >= 0.0 && cy >= 0.0 && cx < image.width() as f64 && cy < image.height() as f64,
            "Map target is outside the atlas"
        );
        let (x, y) = (
            (cx as u32).saturating_sub(768),
            (cy as u32).saturating_sub(768),
        );
        let crop = imageops::crop_imm(
            image,
            x,
            y,
            1536.min(image.width() - x),
            1536.min(image.height() - y),
        )
        .to_image();
        let features = sift_features::sift(&crop, Some(12000));
        ensure!(
            features.keypoints.len() >= 10,
            "Map reference has too few terrain features"
        );
        ensure!(!cancel.is_some_and(|t| t.check_rmb()), "Mining stopped");
        Ok(Self {
            features,
            origin: (x, y),
        })
    }
    pub fn locate(
        &self,
        query: &RgbImage,
        cancel: Option<&CancelToken>,
    ) -> Result<(f64, f64, f64)> {
        ensure!(!cancel.is_some_and(|t| t.check_rmb()), "Mining stopped");
        let center = (query.width() as f64 / 2.0, query.height() as f64 / 2.0);
        let query = sift_features::sift(&imageops::grayscale(query), Some(2000));
        let mut matches: Vec<_> = query
            .descriptors
            .outer_iter()
            .enumerate()
            .collect::<Vec<_>>()
            .par_iter()
            .filter_map(|(i, d)| {
                if cancel.is_some_and(|t| t.is_cancelled()) {
                    return None;
                }
                let mut best = (u32::MAX, usize::MAX);
                let mut second = u32::MAX;
                for (j, r) in self.features.descriptors.outer_iter().enumerate() {
                    let dist = d
                        .iter()
                        .zip(r.iter())
                        .map(|(&a, &b)| {
                            let v = a as i32 - b as i32;
                            (v * v) as u32
                        })
                        .sum::<u32>();
                    if dist < best.0 {
                        second = best.0;
                        best = (dist, j);
                    } else if dist < second {
                        second = dist;
                    }
                }
                ((best.0 as u64) * 100 < (second as u64) * 49).then_some((best.0, *i, best.1))
            })
            .collect();
        matches.sort_unstable();
        let mut seen = std::collections::HashSet::new();
        let pairs: Vec<_> = matches
            .into_iter()
            .filter(|(_, _, j)| seen.insert(*j))
            .take(180)
            .map(|(_, i, j)| {
                let (q, r) = (&query.keypoints[i], &self.features.keypoints[j]);
                Pair {
                    q: (q.x as f64, q.y as f64),
                    r: (r.x as f64, r.y as f64),
                }
            })
            .collect();
        let t = consensus(&pairs, cancel)?;
        let p = t.apply(center);
        Ok((
            (4096.0 - self.origin.0 as f64 - p.0) * 4.0,
            (3072.0 - self.origin.1 as f64 - p.1) * 4.0,
            t.a.hypot(t.b),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometric_consensus_rejects_outliers_and_wrong_scale() {
        let t = Transform {
            a: 0.375,
            b: 0.0,
            x: 30.0,
            y: 60.0,
        };
        let mut p: Vec<_> = (0..25)
            .map(|i| {
                let q = ((i % 5) as f64 * 60.0, (i / 5) as f64 * 50.0);
                Pair { q, r: t.apply(q) }
            })
            .collect();
        p.extend((0..10).map(|i| Pair {
            q: (i as f64 * 23.0, 77.0),
            r: (i as f64 * 41.0, 150.0),
        }));
        let actual = consensus(&p, None).unwrap();
        assert!((actual.a - t.a).abs() < 0.001 && actual.error(p[12]) < 0.01);
        assert!(consensus(&p[25..], None).is_err());
    }
    #[test]
    #[ignore = "requires a local map frame and pinned atlas"]
    fn localize_map_features() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_MAP_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let atlas = super::super::vision::Atlas::new().unwrap();
        let matcher = FeatureAtlas::new(&atlas.image, (3623.97, 4793.26), None).unwrap();
        let q = imageops::crop_imm(&frame, 660, 330, 600, 420).to_image();
        let p = matcher.locate(&q, None).unwrap();
        println!("Feature map position: {p:?}");
        let expected: Vec<f64> = std::env::var("GGSCANNER_EXPECT_MAP")
            .unwrap()
            .split(',')
            .map(|s| s.parse().unwrap())
            .collect();
        assert!((p.0 - expected[0]).hypot(p.1 - expected[1]) < 12.0);
    }
}
