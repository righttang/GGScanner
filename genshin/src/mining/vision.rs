use super::routes::{download, write_cache};
use anyhow::{ensure, Result};
use image::{
    imageops::{self, FilterType},
    GrayImage, RgbImage,
};
use ort::{session::Session, value::Value};
use rayon::prelude::*;
use std::path::PathBuf;

const ASSETS: &str = "https://raw.githubusercontent.com/babalae/bettergi-libraries/13937839dba42df5384940e3cff2e4eff7e29e19";

pub(super) fn asset(relative: &str) -> Result<PathBuf> {
    let path =
        PathBuf::from("data/mining/assets/13937839dba42df5384940e3cff2e4eff7e29e19").join(relative);
    if !path.is_file() {
        write_cache(&path, &download(&format!("{ASSETS}/{relative}"))?)?;
    }
    Ok(path)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Ore {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: f32,
}

pub(super) fn aiming_active(frame: &RgbImage) -> bool {
    // The aim exit R keycap is independent of the scene and charge animation.
    // Bright cliffs can resemble the center crosshair in an ordinary world view.
    let mut white = 0;
    let mut dark = 0;
    for y in 1028..1052 {
        for x in 1706..1742 {
            let p = frame.get_pixel(x, y).0;
            let minimum = *p.iter().min().unwrap();
            let maximum = *p.iter().max().unwrap();
            if minimum > 210 && maximum - minimum < 45 {
                white += 1;
            }
            if maximum < 120 {
                dark += 1;
            }
        }
    }
    white >= 450 && dark >= 40 && white + dark >= 650
}

/// Long, thin red bars in the scene; excludes the player HUD at the bottom.
pub(super) fn enemy_health_bar(frame: &RgbImage) -> bool {
    // Enemy HP fills are flat pink UI rectangles. Fire and Mavuika's ring can
    // contain long red edges, so redness alone is not evidence of an enemy.
    for y in 120..850 {
        let mut run = 0;
        let mut first = [0u8; 3];
        for x in 240..1650 {
            let p = frame.get_pixel(x, y).0;
            let red = p[0] >= 235
                && (50..=90).contains(&p[1])
                && (10..=30).contains(&(p[2] as i16 - p[1] as i16));
            if red && (run == 0 || (0..3).all(|i| p[i].abs_diff(first[i]) <= 8)) {
                if run == 0 {
                    first = p;
                }
                run += 1;
            } else {
                run = 0;
            }
            if run == 38 {
                let center = x - run / 2;
                let color = frame.get_pixel(center, y).0;
                let similar = |column, row| {
                    let pixel = frame.get_pixel(column, row).0;
                    (0..3).all(|i| pixel[i].abs_diff(color[i]) <= 8)
                };
                let (mut top, mut bottom) = (y, y);
                while top > 100 && y - top < 20 && similar(center, top - 1) {
                    top -= 1;
                }
                while bottom < 870 && bottom - y < 20 && similar(center, bottom + 1) {
                    bottom += 1;
                }
                let height = bottom - top + 1;
                if !(3..=18).contains(&height) || run < 2 * height {
                    continue;
                }
                let columns = x - run + 4..x - 4;
                let area = (run - 8) * height;
                let filled = columns
                    .flat_map(|column| (top..=bottom).map(move |row| (column, row)))
                    .filter(|&(column, row)| similar(column, row))
                    .count() as u32;
                if filled * 10 >= area * 9 {
                    return true;
                }
            }
        }
    }
    false
}

/// Horizontal bands of bright UI glyphs, with padding for a single-line OCR model.
pub(super) fn text_rows(
    frame: &RgbImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Vec<(u32, u32)> {
    let mut rows = Vec::new();
    let mut start = None;
    let mut last = 0;
    for line in y..y + height {
        let bright = (x..x + width)
            .filter(|&column| {
                let p = frame.get_pixel(column, line).0;
                *p.iter().min().unwrap() > 185
                    && p.iter().max().unwrap() - p.iter().min().unwrap() < 65
            })
            .count()
            >= 8;
        if bright {
            start.get_or_insert(line);
            last = line;
        }
        if (line > last + 3 || line == y + height - 1) && start.is_some() {
            let first = start.take().unwrap();
            if (12..=42).contains(&(last - first + 1)) {
                rows.push((first.saturating_sub(3), last - first + 7));
            }
        }
    }
    rows
}

pub(super) struct OreDetector {
    model: Session,
}
impl OreDetector {
    pub fn new() -> Result<Self> {
        let path = asset("BetterGI.Assets.Model/Assets/Model/Mine/bgi_mine.onnx")?;
        Ok(Self {
            model: Session::builder()?
                .with_intra_threads(2)?
                .commit_from_file(path)?,
        })
    }
    pub fn detect(&mut self, frame: &RgbImage) -> Result<Vec<Ore>> {
        let size = 640;
        let scale = (size as f64 / frame.width() as f64).min(size as f64 / frame.height() as f64);
        let width = (frame.width() as f64 * scale).round() as u32;
        let height = (frame.height() as f64 * scale).round() as u32;
        let resized = imageops::resize(frame, width, height, FilterType::Triangle);
        let left = (size - width) / 2;
        let top = (size - height) / 2;
        let mut tensor = ndarray::Array4::<f32>::from_elem((1, 3, 640, 640), 114.0 / 255.0);
        for (x, y, p) in resized.enumerate_pixels() {
            for channel in 0..3 {
                tensor[[0, channel, (y + top) as usize, (x + left) as usize]] =
                    p[channel] as f32 / 255.0;
            }
        }
        let result = self.model.run(ort::inputs![Value::from_array(tensor)?])?;
        let (shape, data) = result[0].try_extract_tensor::<f32>()?;
        ensure!(
            shape.len() == 3 && shape[1] == 5,
            "Expected single-class YOLO output [1,5,N], received {shape:?}"
        );
        let count = shape[2] as usize;
        let mut ores = Vec::new();
        for i in 0..count {
            let confidence = data[4 * count + i];
            if confidence < 0.70 {
                continue;
            }
            let x = (data[i] as f64 - left as f64) / scale;
            let y = (data[count + i] as f64 - top as f64) / scale;
            let width = data[2 * count + i] as f64 / scale;
            let height = data[3 * count + i] as f64 / scale;
            if width > 1.0
                && height > 1.0
                && x > 0.0
                && y > 0.0
                && x < frame.width() as f64
                && y < frame.height() as f64
            {
                ores.push(Ore {
                    x,
                    y,
                    width,
                    height,
                    confidence,
                });
            }
        }
        ores.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        let mut kept: Vec<Ore> = Vec::new();
        for ore in ores {
            if kept.iter().all(|other| {
                (other.x - ore.x).abs() > (other.width + ore.width) * 0.25
                    || (other.y - ore.y).abs() > (other.height + ore.height) * 0.25
            }) {
                kept.push(ore);
            }
        }
        Ok(kept)
    }
}

/// Map coordinates are the BGI 256-pixel atlas convention. The atlas origin
/// corresponds to the Teyvat world's (0,0), with both world axes reversed.
#[cfg(test)]
fn world_to_atlas(x: f64, y: f64) -> (f64, f64) {
    ((32768.0 - 2.0 * x) / 8.0, (24576.0 - 2.0 * y) / 8.0)
}
#[cfg(test)]
fn atlas_to_world(x: f64, y: f64) -> (f64, f64) {
    ((32768.0 - x * 8.0) / 2.0, (24576.0 - y * 8.0) / 2.0)
}

#[cfg(test)]
pub(super) fn big_map_scales() -> Vec<f64> {
    (10..=55).map(|n| n as f64 / 100.0).collect()
}

fn mini_map_overlay_mask(query: &RgbImage) -> GrayImage {
    let mut mask = GrayImage::new(query.width(), query.height());
    let is_marker = |pixel: &image::Rgb<u8>| {
        pixel.0.iter().all(|&channel| channel > 205)
            || (pixel[0] < 110 && pixel[1] > 180 && pixel[2] > 180)
    };
    for (x, y, pixel) in query.enumerate_pixels() {
        if x + 1 < query.width()
            && y + 1 < query.height()
            && is_marker(pixel)
            && is_marker(query.get_pixel(x + 1, y))
            && is_marker(query.get_pixel(x, y + 1))
            && is_marker(query.get_pixel(x + 1, y + 1))
        {
            // Include the dark outline around bright map icons as well.
            for sy in y.saturating_sub(3)..=(y + 4).min(query.height() - 1) {
                for sx in x.saturating_sub(3)..=(x + 4).min(query.width() - 1) {
                    mask.put_pixel(sx, sy, image::Luma([255]));
                }
            }
        }
    }
    mask
}

/// Avoid starting a pan on an anchor or character marker, whose UI can consume
/// the press instead of initiating a map drag.
pub(super) fn map_drag_origin(frame: &RgbImage) -> (f64, f64) {
    let candidates = [
        (1050u32, 650u32),
        (1200, 700),
        (800, 750),
        (600, 700),
        (1100, 800),
    ];
    let &(x, y) = candidates
        .iter()
        .min_by_key(|&&(cx, cy)| {
            let mut markers = 0;
            for y in cy - 24..=cy + 24 {
                for x in cx - 24..=cx + 24 {
                    let p = frame.get_pixel(x, y);
                    if p.0.iter().all(|&v| v > 190) || (p[0] < 110 && p[1] > 180 && p[2] > 180) {
                        markers += 1;
                    }
                }
            }
            markers
        })
        .unwrap();
    (x as f64, y as f64)
}

pub(super) struct Atlas {
    pub image: GrayImage,
    origin: (f64, f64),
    world_scale: f64,
    minimum_correlation: f64,
}

impl Atlas {
    pub fn new() -> Result<Self> {
        Ok(Self {
            image: image::open(asset(
                "BetterGI.Assets.Map/Assets/Map/Teyvat/Teyvat_0_256.png",
            )?)?
            .to_luma8(),
            origin: (4096.0, 3072.0),
            world_scale: 0.25,
            minimum_correlation: 0.55,
        })
    }

    /// Sparse zero-mean normalized correlation. A coarse pass chooses the
    /// candidate, then full pixel precision refines its center. Icons and the
    /// player marker are excluded from the circular mini-map sample.
    pub fn locate(
        &self,
        query: &RgbImage,
        center: Option<(f64, f64)>,
        radius: i32,
        scales: &[f64],
        circular: bool,
        cancel: Option<&yas::cancel::CancelToken>,
    ) -> Result<(f64, f64, f64, f64)> {
        let gray = imageops::grayscale(query);
        let overlay_mask = circular.then(|| mini_map_overlay_mask(query));
        let (ax, ay) = center
            .map(|p| {
                (
                    self.origin.0 - p.0 * self.world_scale,
                    self.origin.1 - p.1 * self.world_scale,
                )
            })
            .unwrap_or((
                self.image.width() as f64 / 2.0,
                self.image.height() as f64 / 2.0,
            ));
        let mut best = (-1.0, 0.0, 0.0, 0.0);
        for &scale in scales {
            let w = (gray.width() as f64 * scale).round() as u32;
            let h = (gray.height() as f64 * scale).round() as u32;
            if w < 12 || h < 12 {
                continue;
            }
            let small = imageops::resize(&gray, w, h, FilterType::Triangle);
            let mask = overlay_mask
                .as_ref()
                .map(|mask| imageops::resize(mask, w, h, FilterType::Nearest));
            let mut samples = Vec::new();
            let mut available = 0;
            let sample_stride = (w.max(h) / 16).max(2) as usize;
            for y in (2..h - 2).step_by(sample_stride) {
                for x in (2..w - 2).step_by(sample_stride) {
                    let d = ((x as f64 / w as f64 - 0.5).powi(2)
                        + (y as f64 / h as f64 - 0.5).powi(2))
                    .sqrt();
                    if !circular || (0.12..0.44).contains(&d) {
                        available += 1;
                        if mask
                            .as_ref()
                            .is_some_and(|mask| mask.get_pixel(x, y)[0] != 0)
                        {
                            continue;
                        }
                        samples.push((
                            x as i32 - w as i32 / 2,
                            y as i32 - h as i32 / 2,
                            small.get_pixel(x, y)[0] as f64,
                        ));
                    }
                }
            }
            if samples.len() < 32 || samples.len() * 100 < available * 40 {
                continue;
            }
            let n = samples.len() as f64;
            let mean = samples.iter().map(|p| p.2).sum::<f64>() / n;
            for sample in &mut samples {
                sample.2 -= mean;
            }
            let norm = samples.iter().map(|p| p.2 * p.2).sum::<f64>().sqrt();
            if norm < 1.0 {
                continue;
            }
            let minx = ((ax as i32 - radius).max(w as i32 / 2 + 1)) as usize;
            let maxx = ((ax as i32 + radius).min(self.image.width() as i32 - w as i32 / 2 - 2))
                .max(0) as usize;
            let miny = ((ay as i32 - radius).max(h as i32 / 2 + 1)) as usize;
            let maxy = ((ay as i32 + radius).min(self.image.height() as i32 - h as i32 / 2 - 2))
                .max(0) as usize;
            let pixels = self.image.as_raw();
            let atlas_width = self.image.width() as usize;
            let score = |x: i32, y: i32| {
                let mut sum = 0.0;
                let mut squared = 0.0;
                let mut dot = 0.0;
                for &(dx, dy, q) in &samples {
                    let p = pixels[(y + dy) as usize * atlas_width + (x + dx) as usize] as f64;
                    sum += p;
                    squared += p * p;
                    dot += p * q;
                }
                dot / (norm * (squared - sum * sum / n).max(0.01).sqrt())
            };
            let stride = if radius > 400 { 8 } else { 1 };
            let rows: Vec<_> = (miny..maxy).step_by(stride).collect();
            let mut candidate = rows
                .par_iter()
                .map(|&y| -> Result<(f64, i32, i32)> {
                    ensure!(!cancel.is_some_and(|t| t.check_rmb()), "Mining stopped");
                    let mut row_best = (-1.0, 0, 0);
                    for x in (minx..maxx).step_by(stride) {
                        let s = score(x as i32, y as i32);
                        if s > row_best.0 {
                            row_best = (s, x as i32, y as i32);
                        }
                    }
                    Ok(row_best)
                })
                .try_reduce(|| (-1.0, 0, 0), |a, b| Ok(if a.0 > b.0 { a } else { b }))?;
            if candidate.0 < 0.0 {
                continue;
            }
            let coarse = candidate;
            for y in (coarse.2 - stride as i32).max(miny as i32)
                ..=(coarse.2 + stride as i32).min(maxy as i32 - 1)
            {
                for x in (coarse.1 - stride as i32).max(minx as i32)
                    ..=(coarse.1 + stride as i32).min(maxx as i32 - 1)
                {
                    let s = score(x, y);
                    if s > candidate.0 {
                        candidate = (s, x, y);
                    }
                }
            }
            if candidate.0 > best.0 {
                best = (candidate.0, candidate.1 as f64, candidate.2 as f64, scale);
            }
        }
        let world = (
            (self.origin.0 - best.1) / self.world_scale,
            (self.origin.1 - best.2) / self.world_scale,
        );
        ensure!(
            best.0 >= self.minimum_correlation,
            "Map location unconfirmed (correlation {:.3}, position {:.2}/{:.2}, scale {:.3})",
            best.0,
            world.0,
            world.1,
            best.3
        );
        Ok((world.0, world.1, best.3, best.0))
    }
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Layer {
    layer_id: String,
    left: f64,
    top: f64,
    scale: f64,
    floor: i32,
}

/// City mini-maps use their own projection and magnification. Match those
/// templates before the surrounding overworld, preserving world coordinates.
pub(super) struct MiniMapLocator {
    layers: Vec<Layer>,
    loaded: std::collections::HashMap<String, Atlas>,
    scales: Vec<f64>,
    pub last_floor: Option<i32>,
    floor: Option<i32>,
}

fn confirms_mini_map_match(correlation: f64, displacement: f64, radius: f64) -> bool {
    correlation >= 0.65 || (correlation >= 0.60 && radius <= 40.0 && displacement <= 12.0)
}

impl MiniMapLocator {
    pub fn new() -> Result<Self> {
        let mut layers = Vec::new();
        for manifest in [
            "mapback_info.json",
            "mapback_6_0_info.json",
            "template_match_studio_mapback.json",
            "city_info.json",
            "city_5_8_info.json",
            "city_6_0_info.json",
            "template_match_studio_cities.json",
            "layer_map_info.json",
            "layer_map_info_liyue.json",
            "layer_map_5_8_info.json",
            "layer_map_6_0_info.json",
            "template_match_studio_layers.json",
        ] {
            let path = asset(&format!("BetterGI.Assets.Map/Assets/Map/Teyvat/{manifest}"))?;
            let records: Vec<Layer> = serde_json::from_slice(&std::fs::read(path)?)?;
            layers.extend(records);
        }
        Ok(Self {
            layers,
            loaded: Default::default(),
            scales: vec![1.60, 1.6333, 1.666667, 1.70, 1.7333],
            floor: None,
            last_floor: None,
        })
    }
    pub fn set_floor(&mut self, floor: Option<i32>) {
        self.floor = floor;
        self.last_floor = None;
    }

    pub fn locate(
        &mut self,
        frame: &RgbImage,
        expected: (f64, f64),
        radius: f64,
        token: Option<&yas::cancel::CancelToken>,
    ) -> Result<(f64, f64)> {
        let mini = imageops::crop_imm(frame, 64, 20, 210, 210).to_image();
        let mut candidates: Vec<_> = self
            .layers
            .iter()
            .filter(|r| {
                if self.floor.is_some_and(|floor| r.floor != floor) {
                    return false;
                }
                let dx = r.left - expected.0;
                let dy = r.top - expected.1;
                let extent = if r.scale > 1.0 { 900.0 } else { 8192.0 };
                dx > -radius && dy > -radius && dx < extent && dy < extent
            })
            .cloned()
            .collect();
        candidates.sort_by(|a, b| {
            b.scale.total_cmp(&a.scale).then_with(|| {
                (a.left - expected.0)
                    .hypot(a.top - expected.1)
                    .total_cmp(&(b.left - expected.0).hypot(b.top - expected.1))
            })
        });
        let mut errors = Vec::new();
        let mut confirmed = Vec::new();
        for layer in candidates.into_iter().take(16) {
            ensure!(!token.is_some_and(|t| t.check_rmb()), "Mining stopped");
            if !self.loaded.contains_key(&layer.layer_id) {
                let path = asset(&format!(
                    "BetterGI.Assets.Map/Assets/Map/Teyvat/{}_gray.webp",
                    layer.layer_id
                ))?;
                let image = image::open(path)?.to_luma8();
                if self.loaded.len() >= 4 {
                    self.loaded.clear();
                }
                self.loaded.insert(
                    layer.layer_id.clone(),
                    Atlas {
                        image,
                        origin: (layer.left * layer.scale, layer.top * layer.scale),
                        world_scale: layer.scale,
                        minimum_correlation: 0.60,
                    },
                );
            }
            let atlas = &self.loaded[&layer.layer_id];
            let result = atlas.locate(
                &mini,
                Some(expected),
                (radius * layer.scale) as i32,
                &self.scales,
                true,
                token,
            );
            match result {
                Ok((x, y, _, correlation)) => {
                    let displacement = (x - expected.0).hypot(y - expected.1);
                    // A short walking pulse can retain a nearby position with
                    // weaker terrain contrast; teleports and wide recovery
                    // searches still need the stronger match.
                    if confirms_mini_map_match(correlation, displacement, radius) {
                        if self.floor.is_some() {
                            self.last_floor = Some(layer.floor);
                            return Ok((x, y));
                        }
                        confirmed.push((correlation, x, y, layer.floor));
                    }
                    errors.push(format!(
                        "{}: uncertain map match (correlation {correlation:.3}, displacement {displacement:.2})",
                        layer.layer_id
                    ));
                },
                Err(error) => errors.push(format!("{}: {error}", layer.layer_id)),
            }
        }
        confirmed.sort_by(|a, b| b.0.total_cmp(&a.0));
        if let Some(best) = confirmed.first() {
            ensure!(
                !confirmed
                    .iter()
                    .skip(1)
                    .any(|other| other.3 != best.3 && best.0 - other.0 < 0.04),
                "Mini-map floor is ambiguous; specify info.floor for this route"
            );
            self.last_floor = Some(best.3);
            return Ok((best.1, best.2));
        }
        anyhow::bail!("Map position or floor unconfirmed: {}", errors.join("; "))
    }
}

pub(super) fn estimate_camera_rotation(
    before: &RgbImage,
    after: &RgbImage,
    probe_pixels: f64,
) -> Result<(f64, f64)> {
    let mut delta = [0.0; 360];
    for (degree, value) in delta.iter_mut().enumerate() {
        let angle = (degree as f64).to_radians();
        for radius in 25..78 {
            let x = (168.0 + radius as f64 * angle.cos()).round() as u32;
            let y = (125.0 + radius as f64 * angle.sin()).round() as u32;
            let a = before.get_pixel(x, y);
            let b = after.get_pixel(x, y);
            *value +=
                (b[0] as f64 + b[1] as f64 + b[2] as f64 - a[0] as f64 - a[1] as f64 - a[2] as f64)
                    / 3.0;
        }
    }
    let mut best = (f64::NEG_INFINITY, 0, 0);
    for shift in 5..90i32 {
        for heading in 0..360i32 {
            let mut score = 0.0;
            for offset in -45..45 {
                score += delta[(heading + offset).rem_euclid(360) as usize];
                score -= delta[(heading - shift + offset).rem_euclid(360) as usize];
            }
            let normalized = score / (shift as f64).sqrt();
            if normalized > best.0 {
                best = (normalized, heading, shift);
            }
        }
    }
    ensure!(
        best.0 > 1500.0,
        "Camera heading could not be confirmed (score {:.1})",
        best.0
    );
    yas::log_debug!(
        "挖矿镜头校准：朝向 {}，转角 {}，匹配分数 {}",
        "Mining camera calibration: heading {}, turn {}, match score {}",
        best.1 - best.2,
        best.2,
        best.0
    );
    Ok((
        ((best.1 - best.2) as f64).to_radians(),
        probe_pixels / best.2 as f64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires local enemy HP and Mavuika ring regression frames"]
    fn distinguish_local_enemy_bar_from_pyro_ring() {
        let positive = image::open(std::env::var("GGSCANNER_ENEMY_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let negative = image::open(std::env::var("GGSCANNER_PYRO_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        assert!(
            enemy_health_bar(&positive),
            "Visible enemy HP must be detected"
        );
        assert!(
            !enemy_health_bar(&negative),
            "Pyro effects must not create an enemy HP bar"
        );
        if let Ok(path) = std::env::var("GGSCANNER_PYRO_EXTRA_FRAME") {
            let frame = image::open(path).unwrap().to_rgb8();
            assert!(
                !enemy_health_bar(&frame),
                "Red scenery must not create an enemy HP bar"
            );
        }
    }
    #[test]
    fn map_pan_avoids_bright_markers_at_the_drag_origin() {
        let mut frame = RgbImage::from_pixel(1920, 1080, image::Rgb([70, 90, 100]));
        for y in 630..=670 {
            for x in 1030..=1070 {
                frame.put_pixel(x, y, image::Rgb([245, 245, 245]));
            }
        }
        assert_ne!(map_drag_origin(&frame), (1050.0, 650.0));
    }
    #[test]
    fn weak_mini_map_matches_require_continuous_motion_and_cannot_confirm_teleports() {
        assert!(confirms_mini_map_match(0.620, 4.0, 20.0));
        assert!(!confirms_mini_map_match(0.620, 4.0, 50.0));
        assert!(!confirms_mini_map_match(0.620, 13.0, 20.0));
        assert!(!confirms_mini_map_match(0.59, 1.0, 20.0));
        assert!(confirms_mini_map_match(0.691, 1.8, 50.0));
    }
    #[test]
    fn mini_map_masks_bright_icons_with_their_outline() {
        let mut query = RgbImage::from_pixel(40, 40, image::Rgb([80, 130, 90]));
        for offset_y in 0..2 {
            for offset_x in 0..2 {
                query.put_pixel(10 + offset_x, 10 + offset_y, image::Rgb([245, 245, 245]));
                query.put_pixel(30 + offset_x, 30 + offset_y, image::Rgb([40, 230, 250]));
            }
        }
        let mask = mini_map_overlay_mask(&query);
        assert_eq!(mask.get_pixel(10, 10)[0], 255);
        assert_eq!(mask.get_pixel(13, 13)[0], 255);
        assert_eq!(mask.get_pixel(27, 27)[0], 255);
        assert_eq!(mask.get_pixel(20, 20)[0], 0);
    }
    #[test]
    fn localizes_terrain_under_map_icons_and_rejects_fully_obscured_maps() {
        let atlas = Atlas {
            image: GrayImage::from_fn(400, 400, |x, y| {
                image::Luma([(((x * 17) ^ (y * 23) ^ (x * y)) % 145 + 30) as u8])
            }),
            origin: (400.0, 400.0),
            world_scale: 1.0,
            minimum_correlation: 0.70,
        };
        let crop = imageops::crop_imm(&atlas.image, 158, 158, 84, 84).to_image();
        let mut query = image::DynamicImage::ImageLuma8(crop).to_rgb8();
        for (cx, color) in [(15u32, [245, 245, 245]), (66, [40, 230, 250])] {
            for y in 37u32..=47 {
                for x in cx - 5..=cx + 5 {
                    let edge = x.abs_diff(cx) > 2 || y.abs_diff(42) > 2;
                    query.put_pixel(x, y, image::Rgb(if edge { [20, 30, 40] } else { color }));
                }
            }
        }
        let found = atlas
            .locate(&query, Some((200.0, 200.0)), 20, &[1.0], true, None)
            .unwrap();
        assert_eq!((found.0, found.1), (200.0, 200.0));
        let obscured = RgbImage::from_pixel(84, 84, image::Rgb([245, 245, 245]));
        assert!(atlas
            .locate(&obscured, Some((200.0, 200.0)), 20, &[1.0], true, None)
            .is_err());
    }
    #[test]
    fn bright_terrain_does_not_enter_or_toggle_aiming_state() {
        let cliff = RgbImage::from_pixel(1920, 1080, image::Rgb([255, 255, 255]));
        assert!(!aiming_active(&cliff));
    }
    #[test]
    #[ignore = "requires a local map or loot-notification frame"]
    fn read_visible_text_rows() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let ocr = crate::scanner::common::ocr_factory::create_ocr_model("ppocrv4").unwrap();
        for (x, y, w, h) in [(1310, 170, 530, 760), (140, 370, 360, 340)] {
            for (top, height) in text_rows(&frame, x, y, w, h) {
                let text = ocr
                    .image_to_text(
                        &imageops::crop_imm(&frame, x, top, w, height).to_image(),
                        false,
                    )
                    .unwrap();
                println!("{x},{top},{w},{height}: {text}");
            }
        }
    }
    #[test]
    fn camera_probe_does_not_confuse_turn_size_with_heading() {
        for heading in [30.0_f64, 130.0, 350.0] {
            let scene = |angle: f64| {
                RgbImage::from_fn(300, 240, |x, y| {
                    let theta = (y as f64 - 125.0)
                        .atan2(x as f64 - 168.0)
                        .to_degrees()
                        .rem_euclid(360.0);
                    let difference = (theta - angle + 180.0).rem_euclid(360.0) - 180.0;
                    let terrain = ((x * 3 + y * 7) % 20) as u8 + 50;
                    let value = terrain + if difference.abs() < 45.0 { 60 } else { 0 };
                    image::Rgb([value, value, value])
                })
            };
            for (pixels, turn) in [(160.0, 22.0), (320.0, 44.0)] {
                let (actual, sensitivity) = estimate_camera_rotation(
                    &scene(heading),
                    &scene((heading + turn).rem_euclid(360.0)),
                    pixels,
                )
                .unwrap();
                let error = (actual.to_degrees() - heading + 180.0).rem_euclid(360.0) - 180.0;
                assert!(
                    error.abs() < 3.0,
                    "Expected {heading}, got {}",
                    actual.to_degrees()
                );
                assert!((sensitivity - pixels / turn).abs() < 0.7);
            }
        }
    }
    #[test]
    #[ignore = "requires a local landing frame and route"]
    fn localize_route_landing() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let route = super::super::Route::parse(
            &std::fs::read(std::env::var("GGSCANNER_MINING_ROUTE_FILE").unwrap()).unwrap(),
        )
        .unwrap();
        let waypoint = std::env::var("GGSCANNER_MINING_WAYPOINT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let point = &route.positions[waypoint];
        let mut locator = MiniMapLocator::new().unwrap();
        let position = locator.locate(
            &frame,
            (point.x, point.y),
            std::env::var("GGSCANNER_MINING_RADIUS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60.0),
            None,
        );
        if std::env::var_os("GGSCANNER_MINING_EXPECT_REJECTION").is_some() {
            assert!(position.is_err(), "Unexpected location: {position:?}");
            println!("Rejected mismatched landing: {}", position.unwrap_err());
            return;
        }
        let position = position.unwrap();
        println!(
            "Landing position {position:?}; anchor ({},{})",
            point.x, point.y
        );
        assert!((position.0 - point.x).hypot(position.1 - point.y) < 30.0);
    }
    #[test]
    #[ignore = "requires a running HDR game window"]
    fn capture_controller_party() {
        crate::scanner::common::pixel_profile::set_hdr_mode(true);
        let info = crate::cli::GoodScannerApplication::get_game_info().unwrap();
        println!("Window {:?}", info.window);
        let ctrl = crate::scanner::common::game_controller::GenshinGameController::new(
            info,
            yas::capture::CaptureMethod::Wgc,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let frame = ctrl.capture_game().unwrap();
        println!("Frame {:?}", frame.dimensions());
        frame.save("data/mining/controller-frame.png").unwrap();
        let ocr = crate::scanner::common::ocr_factory::create_ocr_model("ppocrv4").unwrap();
        for slot in 0..4 {
            let text = ocr
                .image_to_text(
                    &imageops::crop_imm(&frame, 1640, 240 + slot * 92, 145, 45).to_image(),
                    false,
                )
                .unwrap();
            println!("Slot {slot}: {text}");
        }
    }
    #[test]
    #[ignore = "requires a local game frame"]
    fn read_party_names() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        for backend in ["ppocrv4", "ppocrv5", "ppocrv6tiny"] {
            let ocr = crate::scanner::common::ocr_factory::create_ocr_model(backend).unwrap();
            for (x, y, w, h) in [
                (1640, 424, 145, 45),
                (1640, 443, 130, 28),
                (1660, 442, 115, 30),
                (1665, 445, 100, 22),
            ] {
                let text = ocr
                    .image_to_text(&imageops::crop_imm(&frame, x, y, w, h).to_image(), false)
                    .unwrap();
                println!("{backend} {x},{y},{w},{h}: {text}");
            }
        }
    }
    #[test]
    #[ignore = "requires a local game frame and downloads the pinned model"]
    fn recognize_local_frame() {
        let path = std::env::var("GGSCANNER_MINING_FRAME").expect("GGSCANNER_MINING_FRAME");
        let frame = image::open(path).unwrap().to_rgb8();
        let mut detector = OreDetector::new().unwrap();
        let ores = detector.detect(&frame).unwrap();
        println!("Detected ore: {ores:?}");
        assert!(
            !ores.is_empty(),
            "The supplied positive frame must contain ore"
        );
    }

    #[test]
    #[ignore = "requires a local map frame and pinned atlas"]
    fn localize_game_map() {
        let path = std::env::var("GGSCANNER_MINING_MAP_FRAME").expect("GGSCANNER_MINING_MAP_FRAME");
        let frame = image::open(path).unwrap().to_rgb8();
        let atlas = Atlas::new().unwrap();
        let patch = imageops::crop_imm(&frame, 660, 330, 600, 420).to_image();
        let found = atlas
            .locate(&patch, None, 10000, &big_map_scales(), false, None)
            .unwrap();
        println!("Map center and scale: {found:?}");
        if let Ok(path) = std::env::var("GGSCANNER_MINING_FRAME") {
            let frame = image::open(path).unwrap().to_rgb8();
            let mini = imageops::crop_imm(&frame, 64, 20, 210, 210).to_image();
            let scales: Vec<_> = (75..=95).map(|n| n as f64 / 200.0).collect();
            let location = atlas
                .locate(&mini, Some((found.0, found.1)), 200, &scales, true, None)
                .unwrap();
            println!("Mini-map position and scale: {location:?}");
        }
    }

    #[test]
    fn localizes_a_map_crop() {
        let atlas = Atlas {
            image: GrayImage::from_fn(300, 300, |x, y| {
                image::Luma([((x * x + 3 * y * y + 7 * x * y) % 251) as u8])
            }),
            origin: (4096.0, 3072.0),
            world_scale: 0.25,
            minimum_correlation: 0.55,
        };
        let crop = imageops::crop_imm(&atlas.image, 95, 115, 42, 42).to_image();
        let rgb = image::DynamicImage::ImageLuma8(crop).to_rgb8();
        let expected = atlas_to_world(116.0, 136.0);
        let found = atlas
            .locate(&rgb, Some(expected), 15, &[1.0], true, None)
            .unwrap();
        assert!((found.0 - expected.0).abs() < 0.01 && (found.1 - expected.1).abs() < 0.01);
    }
    #[test]
    fn world_coordinate_round_trip() {
        for p in [(0.0, 0.0), (9511.085, 1483.8521), (-1000.0, 500.0)] {
            let a = world_to_atlas(p.0, p.1);
            let w = atlas_to_world(a.0, a.1);
            assert!((w.0 - p.0).abs() < 0.001 && (w.1 - p.1).abs() < 0.001);
        }
    }
}
