//! HUD observations for navigation. A requested movement mode is never evidence
//! that the character actually entered that state.
use super::routes::{download, write_cache};
use anyhow::Result;
use image::{imageops, GrayImage, RgbImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Motion {
    Ground,
    Gliding,
    Climbing,
    Underwater,
}
pub(super) const UNDERWATER_SKILL_X: u32 = 1780;

pub(super) fn vehicle_active(frame: &RgbImage) -> bool {
    // Binary shape descriptor of the mounted E (dismount) glyph. The optional
    // T prompt can disappear while riding, and cannot establish this state.
    // Only HUD geometry is retained; no account/game-world image is embedded.
    const GLYPH: [u16; 12] = [0, 0, 0, 0, 24, 48, 48, 112, 496, 248, 240, 96];
    let mut intersection = 0;
    let mut union = 0;
    for (row, expected) in GLYPH.into_iter().enumerate() {
        let mut observed = 0u16;
        for column in 0..14 {
            let x = 1680 + column * 4;
            let y = 962 + row as u32 * 4;
            let white = (y..y + 2)
                .flat_map(|yy| (x..x + 2).map(move |xx| (xx, yy)))
                .filter(|&(xx, yy)| {
                    let p = frame.get_pixel(xx, yy).0;
                    *p.iter().min().unwrap() > 210
                        && p.iter().max().unwrap() - p.iter().min().unwrap() < 35
                })
                .count();
            if white >= 3 {
                observed |= 1 << column;
            }
        }
        intersection += (observed & expected).count_ones();
        union += (observed | expected).count_ones();
    }
    union > 0 && intersection as f64 / union as f64 >= 0.72
}

pub(super) struct MotionDetector {
    space: GrayImage,
    cancel: GrayImage,
    interact: GrayImage,
    ground: GrayImage,
    skill_key: GrayImage,
}
impl MotionDetector {
    pub fn new() -> Result<Self> {
        fn load(group: &str, name: &str) -> Result<GrayImage> {
            let path = std::path::PathBuf::from("data/mining/hud-cc3099a1").join(name);
            if !path.is_file() {
                let base=format!("https://raw.githubusercontent.com/babalae/better-genshin-impact/cc3099a1a8e6470bdbd649dd232aa06011cd60a0/BetterGenshinImpact/GameTask/{group}/Assets/1920x1080");
                write_cache(&path, &download(&format!("{base}/{name}"))?)?;
            }
            Ok(image::open(path)?.to_luma8())
        }
        Ok(Self {
            space: load("Common/Element", "key_space.png")?,
            cancel: load("Common/Element", "key_x.png")?,
            interact: load("AutoPick", "F.png")?,
            ground: load("QuickTeleport", "MapUndergroundToGroundButton.png")?,
            // Footer keycaps use a smaller font than interaction prompts.
            skill_key: imageops::resize(
                &load("AutoPick", "E.png")?,
                24,
                24,
                imageops::FilterType::Triangle,
            ),
        })
    }
    pub fn ore_prompt_key(&self, frame: &RgbImage, top: u32, height: u32) -> bool {
        let y = top.saturating_sub(8);
        let area = imageops::grayscale(
            &imageops::crop_imm(frame, 1080, y, 90, (height + 16).min(1080 - y)).to_image(),
        );
        match_glyph(&area, &self.interact) >= 0.84
    }
    pub fn ground_button(&self, frame: &RgbImage) -> Option<(f64, f64)> {
        let area = imageops::grayscale(&imageops::crop_imm(frame, 1800, 250, 90, 570).to_image());
        let (score, x, y) = find_glyph(&area, &self.ground);
        (score >= 0.84).then_some((
            1800.0 + x as f64 + self.ground.width() as f64 / 2.0,
            250.0 + y as f64 + self.ground.height() as f64 / 2.0,
        ))
    }
    pub fn borrowed_skill_present(&self, frame: &RgbImage) -> bool {
        self.skill_key_present(frame, UNDERWATER_SKILL_X)
    }
    pub fn ground_skill_present(&self, frame: &RgbImage) -> bool {
        self.skill_key_present(frame, 1680)
    }
    fn skill_key_present(&self, frame: &RgbImage, x: u32) -> bool {
        let area = imageops::grayscale(&imageops::crop_imm(frame, x, 1018, 74, 40).to_image());
        match_glyph(&area, &self.skill_key) >= 0.84
    }
    pub fn observe(&self, frame: &RgbImage, labels: &str) -> Motion {
        let area = imageops::grayscale(&imageops::crop_imm(frame, 1570, 1010, 200, 70).to_image());
        let space = match_glyph(&area, &self.space) >= 0.84;
        let cancel = match_glyph(&area, &self.cancel) >= 0.88;
        if labels.to_ascii_lowercase().contains("ctrl")
            || labels.contains("下潜")
            || labels.contains("下沉")
            // Underwater E has a different footer position from ground E/Q.
            // Its matched key glyph survives unreadable Ctrl OCR. Actual crab
            // identity is still checked separately before skill input.
            || self.borrowed_skill_present(frame)
        {
            Motion::Underwater
        } else if space && cancel {
            Motion::Climbing
        } else if space {
            Motion::Gliding
        } else {
            Motion::Ground
        }
    }
}

fn match_glyph(frame: &GrayImage, template: &GrayImage) -> f64 {
    find_glyph(frame, template).0
}
pub(super) fn find_glyph(frame: &GrayImage, template: &GrayImage) -> (f64, u32, u32) {
    find_glyph_masked(frame, template, None)
}
pub(super) fn find_glyph_masked(
    frame: &GrayImage,
    template: &GrayImage,
    mask: Option<&GrayImage>,
) -> (f64, u32, u32) {
    if frame.width() < template.width() || frame.height() < template.height() {
        return (0.0, 0, 0);
    }
    if mask.is_some_and(|m| m.dimensions() != frame.dimensions()) {
        return (0.0, 0, 0);
    }
    let n = (template.width() * template.height()) as f64;
    let mean = template.pixels().map(|p| p[0] as f64).sum::<f64>() / n;
    let values: Vec<_> = template
        .enumerate_pixels()
        .map(|(x, y, p)| (x, y, p[0] as f64 - mean))
        .collect();
    let norm = values.iter().map(|p| p.2 * p.2).sum::<f64>().sqrt();
    let mut best = (-1.0, 0, 0);
    for y in 0..=frame.height() - template.height() {
        for x in 0..=frame.width() - template.width() {
            let (mut sum, mut square, mut dot) = (0.0, 0.0, 0.0);
            let (mut q_sum, mut q_square, mut count) = (0.0, 0.0, 0.0);
            for &(dx, dy, q) in &values {
                if mask.is_some_and(|m| m.get_pixel(x + dx, y + dy)[0] != 0) {
                    continue;
                }
                let p = frame.get_pixel(x + dx, y + dy)[0] as f64;
                sum += p;
                square += p * p;
                dot += p * q;
                if mask.is_some() {
                    q_sum += q;
                    q_square += q * q;
                    count += 1.0;
                }
            }
            let score = if mask.is_some() {
                if count < n * 0.70 {
                    continue;
                }
                (dot - sum * q_sum / count)
                    / ((square - sum * sum / count).max(0.01)
                        * (q_square - q_sum * q_sum / count).max(0.01))
                    .sqrt()
            } else {
                dot / (norm * (square - sum * sum / n).max(0.01).sqrt()).max(0.01)
            };
            if score > best.0 {
                best = (score, x, y);
            }
        }
    }
    best
}

pub(super) fn crab_reference_matches(frame: &RgbImage) -> Result<bool> {
    let path = std::path::Path::new("data/mining/crab-skill-reference.png");
    if !path.is_file() {
        return Ok(false);
    }
    Ok(crab_icon_matches(frame, &image::open(path)?.to_rgb8()))
}

fn crab_icon_matches(frame: &RgbImage, template: &RgbImage) -> bool {
    let skill = imageops::crop_imm(frame, UNDERWATER_SKILL_X, 938, 74, 84).to_image();
    match_glyph(&imageops::grayscale(&skill), &imageops::grayscale(template)) >= 0.84
        || white_glyph_score(&skill, template) >= 0.88
}

/// UI glyphs stay neutral white while the translucent background changes.
/// Compare the whole foreground shape, including unexpected white pixels,
/// rather than allowing bright plants to dominate grayscale correlation.
fn white_glyph_score(frame: &RgbImage, template: &RgbImage) -> f64 {
    if frame.width() < template.width() || frame.height() < template.height() {
        return 0.0;
    }
    let white = |p: &image::Rgb<u8>| {
        let lo = *p.0.iter().min().unwrap();
        let hi = *p.0.iter().max().unwrap();
        lo > 210 && hi - lo < 45
    };
    let reference: Vec<_> = template.pixels().map(white).collect();
    let count = reference.iter().filter(|&&p| p).count();
    if count < 24 || count * 10 > reference.len() * 7 {
        return 0.0;
    }
    let mut best = 0.0_f64;
    for y in 0..=frame.height() - template.height() {
        for x in 0..=frame.width() - template.width() {
            let (mut intersection, mut union) = (0, count);
            for ty in 0..template.height() {
                for tx in 0..template.width() {
                    if white(frame.get_pixel(x + tx, y + ty)) {
                        if reference[(ty * template.width() + tx) as usize] {
                            intersection += 1;
                        } else {
                            union += 1;
                        }
                    }
                }
            }
            best = best.max(intersection as f64 / union as f64);
        }
    }
    best
}

pub(super) fn save_crab_reference(frame: &RgbImage) -> Result<()> {
    // Keep the keyboard label outside the reference. The operator confirms
    // the ready crab glyph, without a cooldown overlay, before calibration.
    let icon = imageops::crop_imm(frame, UNDERWATER_SKILL_X + 10, 955, 58, 58).to_image();
    let mut bytes = std::io::Cursor::new(Vec::new());
    icon.write_to(&mut bytes, image::ImageFormat::Png)?;
    write_cache(
        std::path::Path::new("data/mining/crab-skill-reference.png"),
        bytes.get_ref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn white_skill_shape_survives_cyan_background_but_rejects_brightness_and_wrong_shapes() {
        let mut template = RgbImage::from_pixel(17, 17, image::Rgb([20, 50, 60]));
        for y in 2..15 {
            for x in 2..15 {
                if x == y || x + y == 16 || y == 8 {
                    template.put_pixel(x, y, image::Rgb([255, 255, 255]));
                }
            }
        }
        let mut bright = RgbImage::from_pixel(23, 24, image::Rgb([20, 240, 255]));
        for (x, y, p) in template.enumerate_pixels() {
            if p.0 == [255, 255, 255] {
                bright.put_pixel(x + 3, y + 4, image::Rgb([245, 245, 250]));
            }
        }
        assert!(white_glyph_score(&bright, &template) > 0.99);
        assert!(
            white_glyph_score(
                &RgbImage::from_pixel(23, 24, image::Rgb([255, 255, 255])),
                &template
            ) < 0.3
        );
        assert_eq!(white_glyph_score(&RgbImage::new(23, 24), &template), 0.0);
        let wrong = RgbImage::from_fn(23, 24, |x, _| {
            if x % 4 == 0 {
                image::Rgb([255, 255, 255])
            } else {
                image::Rgb([20, 240, 255])
            }
        });
        assert!(white_glyph_score(&wrong, &template) < 0.5);
    }
    #[test]
    #[ignore = "requires a local skill frame and confirmed crab reference"]
    fn verify_local_crab_icon() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let template = image::open(std::env::var("GGSCANNER_CRAB_REFERENCE").unwrap())
            .unwrap()
            .to_rgb8();
        let skill = imageops::crop_imm(&frame, UNDERWATER_SKILL_X, 938, 74, 84).to_image();
        println!(
            "Crab icon NCC {}, white-shape {}",
            match_glyph(
                &imageops::grayscale(&skill),
                &imageops::grayscale(&template)
            ),
            white_glyph_score(&skill, &template)
        );
        assert_eq!(
            crab_icon_matches(&frame, &template),
            std::env::var("GGSCANNER_EXPECT_CRAB").unwrap() == "true"
        );
    }
    #[test]
    fn masked_glyph_requires_visible_shape_and_rejects_complete_occlusion() {
        let glyph = GrayImage::from_fn(9, 7, |x, y| {
            image::Luma([if x == y || x + y == 8 { 20 } else { 230 }])
        });
        let mut frame = GrayImage::from_pixel(40, 30, image::Luma([80]));
        imageops::replace(&mut frame, &glyph, 12, 9);
        let mut mask = GrayImage::new(40, 30);
        for y in 9..12 {
            for x in 12..15 {
                frame.put_pixel(x, y, image::Luma([255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }
        let (score, x, y) = find_glyph_masked(&frame, &glyph, Some(&mask));
        assert!(score > 0.99 && (x, y) == (12, 9));
        mask.fill(255);
        assert!(find_glyph_masked(&frame, &glyph, Some(&mask)).0 < 0.1);
    }
    #[test]
    fn vehicle_requires_shape_not_brightness() {
        assert!(!vehicle_active(&RgbImage::from_pixel(
            1920,
            1080,
            image::Rgb([255, 255, 255])
        )));
        assert!(!vehicle_active(&RgbImage::from_pixel(
            1920,
            1080,
            image::Rgb([255, 120, 10])
        )));
    }
    #[test]
    #[ignore = "requires a local landing or non-ground HUD frame"]
    fn verify_local_landing_frame() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let detector = MotionDetector::new().unwrap();
        if let Ok(expected) = std::env::var("GGSCANNER_EXPECT_MOTION") {
            assert_eq!(format!("{:?}", detector.observe(&frame, "")), expected);
            return;
        }
        assert_eq!(
            detector.observe(&frame, "") == Motion::Ground && detector.ground_skill_present(&frame),
            std::env::var("GGSCANNER_EXPECT_GROUND").unwrap() == "true"
        );
    }
    #[test]
    #[ignore = "requires a local game frame"]
    fn verify_local_vehicle_frame() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        assert_eq!(
            vehicle_active(&frame),
            std::env::var("GGSCANNER_EXPECT_VEHICLE").unwrap() == "true"
        );
    }
    #[test]
    #[ignore = "requires a local ore-pickup frame"]
    fn verify_local_pickup_key() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        assert!(MotionDetector::new()
            .unwrap()
            .ore_prompt_key(&frame, 520, 42));
    }
    #[test]
    #[ignore = "requires a local layered-map frame"]
    fn verify_local_ground_button() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let p = MotionDetector::new()
            .unwrap()
            .ground_button(&frame)
            .unwrap();
        assert!(
            (p.0 - 1844.0).abs() < 5.0 && (p.1 - 442.0).abs() < 5.0,
            "{p:?}"
        );
    }
    #[test]
    #[ignore = "requires a local underwater frame"]
    fn verify_local_borrowed_skill_key() {
        let frame = image::open(std::env::var("GGSCANNER_MOTION_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        assert_eq!(
            MotionDetector::new()
                .unwrap()
                .borrowed_skill_present(&frame),
            std::env::var("GGSCANNER_EXPECT_SKILL").unwrap() == "true"
        );
    }
    #[test]
    fn glyph_requires_shape_not_a_bright_patch() {
        let glyph = GrayImage::from_fn(9, 7, |x, y| {
            image::Luma([if x == y || x + y == 8 { 20 } else { 230 }])
        });
        let mut frame = GrayImage::from_pixel(30, 20, image::Luma([80]));
        imageops::replace(&mut frame, &glyph, 11, 6);
        assert!(match_glyph(&frame, &glyph) > 0.99);
        assert!(match_glyph(&GrayImage::from_pixel(30, 20, image::Luma([230])), &glyph) < 0.1);
    }
}
