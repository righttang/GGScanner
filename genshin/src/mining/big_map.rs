//! Big-map observations, pan geometry and anchor refinement shared by the
//! production executor and image regressions. Positions use game map units.
use super::{features::FeatureAtlas, navigation::TeleportAnchor, vision::Atlas};
#[cfg(test)]
use anyhow::Context;
use anyhow::{ensure, Result};
use image::{imageops, GrayImage, RgbImage};
use yas::cancel::CancelToken;

#[derive(Clone, Copy, Debug)]
pub(super) struct Projection {
    pub center: (f64, f64),
    pub scale: f64,
}
impl Projection {
    pub fn screen(self, world: (f64, f64)) -> (f64, f64) {
        (
            960.0 + (self.center.0 - world.0) / (4.0 * self.scale),
            540.0 + (self.center.1 - world.1) / (4.0 * self.scale),
        )
    }
    pub fn panned(self, delta: (f64, f64)) -> Self {
        Self {
            center: (
                self.center.0 + delta.0 * 4.0 * self.scale,
                self.center.1 + delta.1 * 4.0 * self.scale,
            ),
            ..self
        }
    }
    pub fn confirms_pan(self, predicted: Self) -> bool {
        (self.scale - predicted.scale).abs() <= 0.025
            && (self.center.0 - predicted.center.0).hypot(self.center.1 - predicted.center.1)
                < 160.0
    }
    pub fn confirms_direction(self, before: Self, predicted: Self) -> bool {
        let expected = (
            predicted.center.0 - before.center.0,
            predicted.center.1 - before.center.1,
        );
        let actual = (
            self.center.0 - before.center.0,
            self.center.1 - before.center.1,
        );
        let sent = expected.0.hypot(expected.1);
        if sent < 12.0 {
            return true;
        }
        let moved = actual.0.hypot(actual.1);
        (0.25..=2.5).contains(&(moved / sent))
            && expected.0 * actual.0 + expected.1 * actual.1 > 0.3 * sent * moved
    }
    pub fn clickable(point: (f64, f64)) -> bool {
        (100.0..1740.0).contains(&point.0)
            && (150.0..930.0).contains(&point.1)
            && !(point.0 < 380.0 && point.1 < 440.0)
    }
}

pub(super) struct MapLocator {
    local: Option<(FeatureAtlas, (f64, f64))>,
    native_local: Option<(FeatureAtlas, (f64, f64))>,
    global: Option<FeatureAtlas>,
    close: Option<GrayImage>,
    zoom: Option<GrayImage>,
}
impl MapLocator {
    pub fn new() -> Self {
        Self {
            local: None,
            native_local: None,
            global: None,
            close: None,
            zoom: None,
        }
    }
    pub fn is_open(&mut self, frame: &RgbImage) -> Result<bool> {
        if self.close.is_none() {
            self.close = Some(load_template("MapCloseButton.png")?);
        }
        let area = imageops::grayscale(&imageops::crop_imm(frame, 1750, 0, 170, 130).to_image());
        Ok(super::motion::find_glyph(&area, self.close.as_ref().unwrap()).0 >= 0.80)
    }
    pub fn zoom_knob(&mut self, frame: &RgbImage) -> Result<(f64, f64)> {
        ensure!(self.is_open(frame)?, "World map UI is not visible");
        if self.zoom.is_none() {
            self.zoom = Some(load_template("MapScaleButton.png")?);
        }
        let template = self.zoom.as_ref().unwrap();
        let area = imageops::grayscale(&imageops::crop_imm(frame, 20, 453, 56, 175).to_image());
        let (score, x, y) = super::motion::find_glyph(&area, template);
        ensure!(score >= 0.80, "Map zoom control is not confirmed");
        Ok((
            20.0 + x as f64 + template.width() as f64 / 2.0,
            453.0 + y as f64 + template.height() as f64 / 2.0,
        ))
    }
    pub fn observe(
        &mut self,
        frame: &RgbImage,
        atlas: &Atlas,
        hint: (f64, f64),
        cancel: &CancelToken,
    ) -> Result<Projection> {
        ensure!(self.is_open(frame)?, "World map UI is not visible");
        if self.global.is_none() {
            self.global = Some(FeatureAtlas::global(Some(cancel))?);
        }
        if self
            .local
            .as_ref()
            .is_none_or(|(_, p)| (p.0 - hint.0).hypot(p.1 - hint.1) > 1200.0)
        {
            self.local = Some((
                self.global
                    .as_ref()
                    .unwrap()
                    .around(hint)
                    .or_else(|_| FeatureAtlas::new(&atlas.image, hint, Some(cancel)))?,
                hint,
            ));
        }
        // A large viewport retains coastlines when the central crop is mostly
        // sea. Text/icons become outliers and cannot relax geometric consensus.
        let patch = imageops::crop_imm(frame, 360, 100, 1360, 840).to_image();
        let local = self.local.as_ref().unwrap().0.locate(&patch, Some(cancel));
        let (x, y, scale) = match local {
            Ok(p) => p,
            Err(_) => {
                ensure!(!cancel.check_rmb(), "Mining stopped");
                if self.global.is_none() {
                    self.global = Some(FeatureAtlas::global(Some(cancel))?);
                }
                match self.global.as_ref().unwrap().locate(&patch, Some(cancel)) {
                    Ok(p) => p,
                    Err(_) => {
                        ensure!(!cancel.check_rmb(), "Mining stopped");
                        if self
                            .native_local
                            .as_ref()
                            .is_none_or(|(_, p)| (p.0 - hint.0).hypot(p.1 - hint.1) > 1200.0)
                        {
                            self.native_local =
                                Some((FeatureAtlas::new(&atlas.image, hint, Some(cancel))?, hint));
                        }
                        self.native_local
                            .as_ref()
                            .unwrap()
                            .0
                            .locate(&patch, Some(cancel))?
                    },
                }
            },
        };
        // The viewport crop is centred at (1040,520), not the frame centre.
        Ok(Projection {
            center: (x + 80.0 * 4.0 * scale, y - 20.0 * 4.0 * scale),
            scale,
        })
    }
}

pub(super) struct AnchorIcons {
    templates: std::collections::HashMap<String, GrayImage>,
}
impl AnchorIcons {
    pub fn new() -> Self {
        Self {
            templates: Default::default(),
        }
    }
    pub fn refine(
        &mut self,
        frame: &RgbImage,
        anchor: &TeleportAnchor,
        expected: (f64, f64),
        scale: f64,
    ) -> Result<(f64, f64)> {
        let name = anchor.icon();
        if !self.templates.contains_key(name) {
            self.templates.insert(name.into(), load_template(name)?);
        }
        ensure!(
            Projection::clickable(expected),
            "Anchor projection is outside the clickable map"
        );
        let (x, y) = (
            (expected.0.round() as u32).saturating_sub(52),
            (expected.1.round() as u32).saturating_sub(52),
        );
        let rgb = imageops::crop_imm(frame, x, y, 104, 104).to_image();
        let area = imageops::grayscale(&rgb);
        let template = &self.templates[name];
        let mut matched = super::motion::find_glyph(&area, template);
        if matched.0 < 0.72 {
            let mask = GrayImage::from_fn(104, 104, |x, y| {
                let p = rgb.get_pixel(x, y).0;
                image::Luma([if p[0] < 150 && p[1] > 170 && p[2] > 170 {
                    255
                } else {
                    0
                }])
            });
            let masked = super::motion::find_glyph_masked(&area, template, Some(&mask));
            if masked.0 > matched.0 {
                matched = masked;
            }
        }
        let (score, dx, dy) = matched;
        let point = (
            x as f64 + dx as f64 + template.width() as f64 / 2.0,
            y as f64 + dy as f64 + template.height() as f64 / 2.0,
        );
        ensure!(
            score >= 0.72
                && (point.0 - expected.0).hypot(point.1 - expected.1)
                    < anchor.correction_limit(scale),
            "Anchor icon not confirmed (score {score:.3})"
        );
        Ok(point)
    }
}

fn load_template(name: &str) -> Result<GrayImage> {
    let path = std::path::PathBuf::from("data/mining/map-icons-cc3099a1").join(name);
    if !path.is_file() {
        super::routes::write_cache(&path,&super::routes::download(&format!(
            "https://raw.githubusercontent.com/babalae/better-genshin-impact/cc3099a1a8e6470bdbd649dd232aa06011cd60a0/BetterGenshinImpact/GameTask/QuickTeleport/Assets/1920x1080/{name}"))?)?;
    }
    Ok(image::open(path)?.to_luma8())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pan_checks_use_the_sent_displacement_and_reject_wrong_direction() {
        let p = Projection {
            center: (4400.0, 3100.0),
            scale: 0.25,
        };
        let expected = p.panned((200.0, -100.0));
        assert_eq!(expected.center, (4600.0, 3000.0));
        assert!(expected.confirms_pan(expected));
        assert!(!p.panned((-200.0, 100.0)).confirms_pan(expected));
        assert!(!Projection {
            scale: 0.35,
            ..expected
        }
        .confirms_pan(expected));
        assert_eq!(expected.screen(p.center), (1160.0, 440.0));
        assert!(!Projection::clickable((180.0, 220.0)));
        assert!(!Projection::clickable((1040.0, 1010.0)));
        let small = p.panned((30.0, 0.0));
        let reverse = p.panned((-30.0, 0.0));
        assert!(reverse.confirms_pan(small));
        assert!(!reverse.confirms_direction(p, small));
        assert!(!p.confirms_direction(p, small));
    }
    #[test]
    #[ignore = "requires local map screenshots and pinned assets"]
    fn regression_local_map_viewports() -> Result<()> {
        let manifest =
            std::env::var("GGSCANNER_NAV_FIXTURES").context("fixture manifest missing")?;
        #[derive(serde::Deserialize)]
        struct Fixture {
            path: String,
            hint: [f64; 2],
            center: [f64; 2],
            tolerance: f64,
        }
        let fixtures: Vec<Fixture> = serde_json::from_slice(&std::fs::read(manifest)?)?;
        let atlas = Atlas::new()?;
        let mut locator = MapLocator::new();
        let cancel = CancelToken::new();
        for f in fixtures {
            let frame = image::open(&f.path)?.to_rgb8();
            let p = locator.observe(&frame, &atlas, (f.hint[0], f.hint[1]), &cancel)?;
            println!("{}: {p:?}", f.path);
            ensure!(
                (p.center.0 - f.center[0]).hypot(p.center.1 - f.center[1]) < f.tolerance,
                "Wrong map projection: {p:?}"
            );
        }
        Ok(())
    }
    #[test]
    #[ignore = "requires a local zoomed map frame and pinned teleport catalog"]
    fn regression_local_anchor_icon() -> Result<()> {
        let frame = image::open(std::env::var("GGSCANNER_ANCHOR_FRAME")?)?.to_rgb8();
        let catalog = super::super::navigation::TeleportCatalog::new()?;
        let anchor = catalog.nearest((4399.645, 3084.0193))?;
        let point = AnchorIcons::new().refine(&frame, anchor, (1040.0, 900.0), 0.369)?;
        println!("Anchor icon {point:?}");
        ensure!(
            (point.0 - 1040.0).hypot(point.1 - 900.0) < 12.0,
            "Wrong anchor icon"
        );
        Ok(())
    }
    #[test]
    #[ignore = "requires local map zoom fixtures"]
    fn verifies_local_zoom_controls() -> Result<()> {
        let a = image::open(std::env::var("GGSCANNER_ZOOM_NORMAL")?)?.to_rgb8();
        let b = image::open(std::env::var("GGSCANNER_ZOOM_MAX")?)?.to_rgb8();
        let mut locator = MapLocator::new();
        let normal = locator.zoom_knob(&a)?;
        let max = locator.zoom_knob(&b)?;
        println!("Normal {normal:?}, maximum {max:?}");
        ensure!(
            (normal.1 - 510.0).abs() < 4.0 && (max.1 - 468.0).abs() < 4.0,
            "Wrong zoom control"
        );
        Ok(())
    }
}
