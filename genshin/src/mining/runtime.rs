use super::{Route, RouteStore};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use yas::cancel::CancelToken;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MiningRequest {
    pub route_ids: Vec<String>,
    /// A single stationary scan is useful for calibrating a new display.
    #[serde(default)]
    pub stationary: bool,
}

impl MiningRequest {
    pub fn routes(&self) -> Result<Vec<Route>> {
        ensure!(
            self.route_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.route_ids.len(),
            "Duplicate route IDs"
        );
        ensure!(
            self.route_ids.len() <= 100 && (self.stationary || !self.route_ids.is_empty()),
            "Select 1–100 routes"
        );
        ensure!(
            !self.stationary || self.route_ids.is_empty(),
            "Stationary mining cannot include routes"
        );
        self.route_ids
            .iter()
            .map(|id| {
                let route = RouteStore::default().load(id)?;
                ensure!(
                    route.issues().is_empty(),
                    "Unsupported route {}: {}",
                    route.info.name,
                    route.issues().join(", ")
                );
                Ok(route)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiningReport {
    pub phase: String,
    pub route: String,
    pub waypoint: usize,
    pub completed_points: usize,
    pub total_points: usize,
    pub shots: usize,
    pub points: Vec<MiningPointResult>,
    pub loot: std::collections::BTreeMap<String, usize>,
    pub error: Option<String>,
    pub stopped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiningPointResult {
    pub route: String,
    pub waypoint: usize,
    pub outcome: String,
    pub shots: usize,
}

pub type MiningProgress<'a> = dyn Fn(&MiningReport) + 'a;

struct MovementProgress {
    anchor: (f64, f64),
    unchanged: usize,
}

impl MovementProgress {
    fn new(position: (f64, f64)) -> Self {
        Self {
            anchor: position,
            unchanged: 0,
        }
    }
    fn observe(&mut self, position: (f64, f64)) -> Result<()> {
        if (self.anchor.0 - position.0).hypot(self.anchor.1 - position.1) >= 1.0 {
            self.anchor = position;
            self.unchanged = 0;
        } else {
            self.unchanged += 1;
        }
        ensure!(self.unchanged < 8, "Character is stuck near the waypoint");
        Ok(())
    }
}

pub fn execute(
    request: MiningRequest,
    token: CancelToken,
    progress: &MiningProgress<'_>,
) -> MiningReport {
    let mut report = MiningReport {
        phase: "preparing".into(),
        ..Default::default()
    };
    progress(&report);
    #[cfg(windows)]
    let result = windows::run(&request, &token, progress, &mut report);
    #[cfg(not(windows))]
    let result: Result<()> = Err(anyhow::anyhow!("Mining currently requires Windows"));
    report.stopped = token.is_cancelled();
    report.error = result
        .err()
        .filter(|_| !report.stopped)
        .map(|error| format!("{error:#}"));
    report.phase = if report.stopped {
        "stopped"
    } else if report.error.is_some() {
        "failed"
    } else {
        "completed"
    }
    .into();
    progress(&report);
    report
}

#[cfg(windows)]
mod windows {
    use super::super::navigation::{TeleportAnchor, TeleportCatalog};
    use super::super::vision::{Atlas, MiniMapLocator, OreDetector};
    use super::*;
    use crate::scanner::common::{
        game_controller::GenshinGameController, ocr_factory, pixel_profile,
    };
    use enigo::Key;
    use image::{
        imageops::{self, FilterType},
        RgbImage,
    };
    use std::time::{Duration, Instant};
    use yas::{capture::CaptureMethod, ocr::ImageToText, system_control::key_hold::KeyDriver};

    struct Game<'a> {
        ctrl: GenshinGameController,
        token: &'a CancelToken,
        ocr: Box<dyn ImageToText<RgbImage> + Send>,
        sight_refreshed: Option<Instant>,
        aiming: bool,
    }
    impl Drop for Game<'_> {
        fn drop(&mut self) {
            let _ = self.ctrl.system_control.key_up(Key::Layout('w'));
            let _ = self.ctrl.system_control.key_up(Key::Shift);
            let _ = self.ctrl.system_control.left_button(false);
            let _ = self.ctrl.system_control.middle_button(false);
            self.leave_aim();
        }
    }
    impl Game<'_> {
        fn leave_aim(&mut self) {
            if self.aiming && self.ctrl.game_info.native_window().is_foreground() {
                if let Ok(frame) = self.ctrl.capture_game() {
                    let frame = imageops::resize(&frame, 1920, 1080, FilterType::Triangle);
                    if super::super::vision::aiming_active(&frame)
                        && self.ctrl.game_info.native_window().is_foreground()
                    {
                        let _ = self.ctrl.system_control.key_press(Key::Layout('r'));
                    }
                }
                self.aiming = false;
            }
        }
        fn check(&self) -> Result<()> {
            ensure!(!self.token.check_rmb(), "Mining stopped");
            ensure!(
                self.ctrl.game_info.native_window().is_foreground(),
                "Game window lost focus"
            );
            Ok(())
        }
        fn wait(&self, ms: u64) -> Result<()> {
            let deadline = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < deadline {
                self.check()?;
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }
        fn key(&mut self, key: Key) -> Result<()> {
            self.check()?;
            self.ctrl.system_control.key_press(key)
        }
        fn forward(&mut self, ms: u64, sprint: bool) -> Result<()> {
            self.check()?;
            self.ctrl.system_control.key_down(Key::Layout('w'))?;
            let result = (|| {
                if sprint {
                    self.ctrl.system_control.key_down(Key::Shift)?;
                }
                self.wait(ms)
            })();
            let released = self.ctrl.system_control.key_up(Key::Layout('w'));
            let sprint_released = self.ctrl.system_control.key_up(Key::Shift);
            result.and(released).and(sprint_released)
        }
        fn frame(&self) -> Result<RgbImage> {
            self.check()?;
            Ok(imageops::resize(
                &self.ctrl.capture_game()?,
                1920,
                1080,
                FilterType::Triangle,
            ))
        }
        fn read(&self, frame: &RgbImage, x: u32, y: u32, w: u32, h: u32) -> Result<String> {
            self.ocr
                .image_to_text(&imageops::crop_imm(frame, x, y, w, h).to_image(), false)
        }
        fn click(&mut self, x: f64, y: f64) -> Result<()> {
            self.check()?;
            self.ctrl.click_client_at(
                self.ctrl.scaler.scale_x(x).round() as u32,
                self.ctrl.scaler.scale_y(y).round() as u32,
            )
        }
        fn rotate(&mut self, x: f64, y: f64) -> Result<()> {
            let mut remaining_x = x.round() as i32;
            let mut remaining_y = y.round() as i32;
            while remaining_x != 0 || remaining_y != 0 {
                self.check()?;
                let step_x = remaining_x.clamp(-600, 600);
                let step_y = remaining_y.clamp(-300, 300);
                self.ctrl
                    .system_control
                    .mouse_move_relative(step_x, step_y)?;
                remaining_x -= step_x;
                remaining_y -= step_y;
                self.wait(30)?;
            }
            self.wait(150)
        }

        fn ore_frame(&mut self) -> Result<RgbImage> {
            if self
                .sight_refreshed
                .is_none_or(|time| time.elapsed() >= Duration::from_millis(2500))
            {
                self.ctrl.system_control.middle_button(false)?;
                self.wait(120)?;
                self.ctrl.system_control.middle_button(true)?;
                self.sight_refreshed = Some(Instant::now());
                self.wait(1500)?;
            }
            let frame = self.frame()?;
            ensure!(
                super::super::vision::aiming_active(&frame),
                "Aiming mode was interrupted"
            );
            Ok(frame)
        }
        fn linnea(&mut self) -> Result<()> {
            let mut names = Vec::new();
            for _ in 0..3 {
                self.wait(400)?;
                let frame = self.frame()?;
                names.clear();
                for slot in 0..4 {
                    let name = self.read(&frame, 1640, 259 + slot * 92, 130, 28)?;
                    if name.contains("莉奈") || name.contains("莉奈婭") {
                        self.key(Key::Layout(char::from(b'1' + slot as u8)))?;
                        self.wait(800)?;
                        return Ok(());
                    }
                    names.push(name);
                }
            }
            std::fs::create_dir_all("data/mining").ok();
            self.frame()?.save("data/mining/last-failure.png").ok();
            anyhow::bail!("Active party needs Linnea. Observed names: {names:?}")
        }
    }

    pub(super) fn run(
        request: &MiningRequest,
        token: &CancelToken,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        let routes = request.routes()?;
        report.total_points = if request.stationary {
            1
        } else {
            routes.iter().map(|r| r.positions.len()).sum()
        };
        report.phase = "loadingModels".into();
        progress(report);
        let mut detector = OreDetector::new()?;
        let atlas = if request.stationary {
            None
        } else {
            Some(Atlas::new()?)
        };
        let mut mini_locator = if request.stationary {
            None
        } else {
            Some(MiniMapLocator::new()?)
        };
        ensure!(!token.is_cancelled(), "Mining stopped");
        let info = crate::cli::GoodScannerApplication::get_game_info()?;
        let mut ctrl = GenshinGameController::new(
            info,
            if pixel_profile::is_hdr_mode() {
                CaptureMethod::Wgc
            } else {
                CaptureMethod::BitBlt
            },
        )?;
        ctrl.set_cancel_token(token.clone());
        ctrl.focus_game_window();
        let mut game = Game {
            ctrl,
            token,
            ocr: ocr_factory::create_ocr_model("ppocrv4")?,
            sight_refreshed: None,
            aiming: false,
        };
        game.check()?;
        if !game.ctrl.return_to_main_ui(3) {
            anyhow::bail!("Could not return to the main world");
        }
        game.linnea()?;
        if request.stationary {
            report.route = "stationary".into();
            report.waypoint = 1;
            mine(&mut game, &mut detector, 1, 24, progress, report)?;
            report.completed_points = 1;
            return Ok(());
        }
        let atlas = atlas.unwrap();
        let mini_locator = mini_locator.as_mut().unwrap();
        let teleports = TeleportCatalog::new()?;
        let mut position = None;
        for route in routes {
            report.route = route.info.name.clone();
            for (index, point) in route.positions.iter().enumerate() {
                game.check()?;
                report.waypoint = index + 1;
                report.phase = if point.kind == "teleport" {
                    "teleporting"
                } else {
                    "moving"
                }
                .into();
                progress(report);
                if point.kind == "teleport" {
                    teleport(
                        &mut game,
                        &atlas,
                        mini_locator,
                        (point.x, point.y),
                        teleports.nearest((point.x, point.y))?,
                    )?;
                    position = Some(locate(&game, mini_locator, Some((point.x, point.y)), 50)?);
                    let landed = position.unwrap();
                    ensure!(
                        (landed.0 - point.x).hypot(landed.1 - point.y) <= 40.0,
                        "Teleport landing does not match the requested anchor"
                    );
                } else if point.kind == "orientation" {
                    let current = locate(&game, mini_locator, position, 40)?;
                    face(&mut game, current, (point.x, point.y))?;
                } else {
                    position = Some(move_to(
                        &mut game,
                        mini_locator,
                        position.context("Missing teleport position")?,
                        (point.x, point.y),
                        point.move_mode != "walk",
                        point.kind == "target",
                    )?);
                }
                if point.action == "linnea_mining" {
                    let (shots, rounds) = point.shots()?;
                    mine(&mut game, &mut detector, shots, rounds, progress, report)?;
                }
                report.completed_points += 1;
                progress(report);
            }
        }
        Ok(())
    }

    fn locate(
        game: &Game<'_>,
        mini_locator: &mut MiniMapLocator,
        expected: Option<(f64, f64)>,
        radius: i32,
    ) -> Result<(f64, f64)> {
        let frame = game.frame()?;
        mini_locator.locate(
            &frame,
            expected.context("A previous map location is required")?,
            radius as f64,
            Some(game.token),
        )
    }

    fn camera_heading(game: &mut Game<'_>) -> Result<(f64, f64)> {
        // Camera rotation changes the mini-map's viewing cone, while terrain
        // stays still. Difference the two frames to remove terrain brightness.
        let before = game.frame()?;
        game.rotate(160.0, 0.0)?;
        let after = game.frame()?;
        game.rotate(-160.0, 0.0)?;
        super::super::vision::estimate_camera_rotation(&before, &after)
    }

    fn angle_difference(target: f64, current: f64) -> f64 {
        (target - current + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI
    }

    fn face(game: &mut Game<'_>, current: (f64, f64), target: (f64, f64)) -> Result<()> {
        let desired = (current.1 - target.1).atan2(current.0 - target.0);
        for _ in 0..12 {
            let (angle, sensitivity) = camera_heading(game)?;
            let delta = angle_difference(desired, angle);
            yas::log_debug!(
                "挖矿转向：当前位置 {:?}，目标 {:?}，镜头 {}，目标角 {}，误差 {}",
                "Mining turn: position {:?}, target {:?}, camera {}, desired {}, error {}",
                current,
                target,
                angle.to_degrees(),
                desired.to_degrees(),
                delta.to_degrees()
            );
            if delta.abs() < 0.10 {
                return Ok(());
            }
            game.rotate(delta.to_degrees() * sensitivity, 0.0)?;
        }
        anyhow::bail!("Could not confirm camera orientation")
    }

    fn move_to(
        game: &mut Game<'_>,
        mini_locator: &mut MiniMapLocator,
        mut current: (f64, f64),
        target: (f64, f64),
        sprint: bool,
        precise: bool,
    ) -> Result<(f64, f64)> {
        let started = Instant::now();
        let mut movement = MovementProgress::new(current);
        loop {
            let distance = (target.0 - current.0).hypot(target.1 - current.1);
            if distance < if precise { 2.0 } else { 4.0 } {
                return Ok(current);
            }
            ensure!(
                started.elapsed() < Duration::from_secs(120),
                "Waypoint timed out"
            );
            face(game, current, target)?;
            game.forward(
                if distance < 6.0 { 150 } else { 450 },
                sprint && distance > 20.0,
            )?;
            game.wait(100)?;
            let next = locate(game, mini_locator, Some(current), 20)
                .or_else(|_| locate(game, mini_locator, Some(current), 60))?;
            ensure!(
                (current.0 - next.0).hypot(current.1 - next.1) < 24.0,
                "Mini-map position jumped unexpectedly"
            );
            movement.observe(next)?;
            current = next;
        }
    }

    fn teleport(
        game: &mut Game<'_>,
        atlas: &Atlas,
        mini_locator: &mut MiniMapLocator,
        target: (f64, f64),
        anchor: &TeleportAnchor,
    ) -> Result<()> {
        let map_target = anchor.map_position();
        game.key(Key::Layout('m'))?;
        game.wait(1000)?;
        if let Some(region) = [
            "蒙德",
            "璃月",
            "稻妻",
            "须弥",
            "枫丹",
            "纳塔",
            "挪德卡莱",
            "至冬",
        ]
        .into_iter()
        .find(|region| anchor.country.contains(region))
        {
            game.click(1770.0, 1010.0)?;
            game.wait(500)?;
            let frame = game.frame()?;
            let mut selected = false;
            for y in [158, 262, 364, 466, 586, 690, 792, 914] {
                for x in [1320, 1620] {
                    let name = game.read(&frame, x, y, 260, 45)?;
                    if name.contains(region) {
                        game.click(x as f64 + 100.0, y as f64 + 25.0)?;
                        game.wait(700)?;
                        selected = true;
                        break;
                    }
                }
                if selected {
                    break;
                }
            }
            ensure!(selected, "Map region {region} could not be selected");
        }
        // Match the map's center against the atlas at several supported zooms.
        for _ in 0..40 {
            let frame = game.frame()?;
            let patch = imageops::crop_imm(&frame, 660, 330, 600, 420).to_image();
            let (x, y, scale) = atlas.locate(
                &patch,
                None,
                10000,
                &super::super::vision::big_map_scales(),
                false,
                Some(game.token),
            )?;
            let screen_x = 960.0 + (x - map_target.0) * 0.25 / scale;
            let screen_y = 540.0 + (y - map_target.1) * 0.25 / scale;
            if (120.0..1700.0).contains(&screen_x) && (150.0..900.0).contains(&screen_y) {
                game.click(screen_x, screen_y)?;
                game.wait(600)?;
                let mut panel = game.frame()?;
                let mut label = game.read(&panel, 1650, 985, 190, 55)?;
                if !label.contains("传送") {
                    for (top, height) in
                        super::super::vision::text_rows(&panel, 1310, 170, 530, 760)
                    {
                        if anchor.matches_menu(&game.read(&panel, 1310, top, 530, height)?) {
                            game.click(1500.0, (top + height / 2) as f64)?;
                            game.wait(500)?;
                            panel = game.frame()?;
                            label = game.read(&panel, 1650, 985, 190, 55)?;
                            break;
                        }
                    }
                }
                ensure!(
                    label.contains("传送"),
                    "Teleport button absent; anchor may be locked or obscured"
                );
                game.click(1710.0, 1010.0)?;
                game.wait(4000)?;
                for _ in 0..20 {
                    if let Ok(landed) =
                        locate(game, mini_locator, Some(anchor.landing_position()), 50)
                    {
                        if (landed.0 - target.0).hypot(landed.1 - target.1) <= 40.0 {
                            return Ok(());
                        }
                    }
                    game.wait(500)?;
                }
                anyhow::bail!("Teleport landing could not be confirmed");
            }
            let dx = (960.0 - screen_x).clamp(-600.0, 600.0);
            let dy = (540.0 - screen_y).clamp(-300.0, 300.0);
            let origin = game.ctrl.game_info.window.origin();
            let sx = game.ctrl.scaler.scale_x(960.0) as i32 + origin.x;
            let sy = game.ctrl.scaler.scale_y(540.0) as i32 + origin.y;
            game.ctrl.system_control.mouse_drag(
                sx,
                sy,
                sx + game.ctrl.scaler.scale_x(dx) as i32,
                sy + game.ctrl.scaler.scale_y(dy) as i32,
            )?;
            game.wait(500)?;
        }
        anyhow::bail!("Target anchor could not be reached on the map")
    }

    fn mine(
        game: &mut Game<'_>,
        detector: &mut OreDetector,
        shot_limit: usize,
        rounds: usize,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        report.phase = "searchingOre".into();
        progress(report);
        game.aiming = super::super::vision::aiming_active(&game.frame()?);
        if !game.aiming {
            game.key(Key::Layout('r'))?;
            game.aiming = true;
        }
        game.wait(600)?;
        ensure!(
            super::super::vision::aiming_active(&game.frame()?),
            "Could not enter aiming mode"
        );
        let initial_shots = report.shots;
        let initial_loot = report.loot.values().sum::<usize>();
        let mut last_aim = None;
        let mut saw_ore = false;
        let mut trace = Vec::new();
        let result = (|| -> Result<()> {
            for _ in 0..rounds {
                if report.shots - initial_shots >= shot_limit {
                    break;
                }
                let frame = game.ore_frame()?;
                let mut ores = detector.detect(&frame)?;
                ores.sort_by(|a, b| {
                    (a.x - 960.0)
                        .hypot(a.y - 540.0)
                        .total_cmp(&(b.x - 960.0).hypot(b.y - 540.0))
                });
                let Some(mut target) = ores.first().copied() else {
                    game.rotate(-560.0, 0.0)?;
                    continue;
                };
                saw_ore = true;
                report.phase = "aiming".into();
                progress(report);
                let mut aligned = false;
                let mut gain_x = 0.45;
                let mut gain_y = 0.8;
                let mut total_x = 0.0;
                let mut total_y = 0.0;
                for _ in 0..18 {
                    let dx = target.x - 960.0;
                    let dy = target.y - 540.0;
                    trace.push(serde_json::json!({"x":target.x,"y":target.y,"width":target.width,"height":target.height,"gainX":gain_x,"gainY":gain_y}));
                    if dx.abs() < (target.width * 0.25).max(4.0)
                        && dy.abs() < (target.height * 0.25).max(4.0)
                    {
                        aligned = true;
                        break;
                    }
                    let mouse_x = (dx * gain_x).clamp(-120.0, 120.0);
                    let mouse_y = (dy * gain_y).clamp(-80.0, 80.0);
                    game.rotate(mouse_x, mouse_y)?;
                    total_x += mouse_x.round();
                    total_y += mouse_y.round();
                    let next_frame = game.ore_frame()?;
                    let new = detector.detect(&next_frame)?;
                    let Some(next) = new.into_iter().min_by(|a, b| {
                        (a.x - 960.0)
                            .hypot(a.y - 540.0)
                            .total_cmp(&(b.x - 960.0).hypot(b.y - 540.0))
                    }) else {
                        break;
                    };
                    let moved_x = target.x - next.x;
                    let moved_y = target.y - next.y;
                    if dx.abs() > 10.0 && moved_x.abs() > 2.0 && mouse_x * moved_x > 0.0 {
                        gain_x = (gain_x * 0.5 + (mouse_x / moved_x).clamp(0.08, 2.0) * 0.5)
                            .clamp(0.08, 2.0);
                    }
                    if dy.abs() > 10.0 && moved_y.abs() > 2.0 && mouse_y * moved_y > 0.0 {
                        gain_y = (gain_y * 0.5 + (mouse_y / moved_y).clamp(0.08, 2.0) * 0.5)
                            .clamp(0.08, 2.0);
                    }
                    target = next;
                }
                if !aligned {
                    game.rotate(-total_x, -total_y)?;
                    game.rotate(-560.0, 0.0)?;
                    continue;
                }
                report.phase = "shooting".into();
                progress(report);
                game.ctrl.system_control.middle_button(false)?;
                game.sight_refreshed = None;
                game.wait(300)?;
                if total_y < 0.0 {
                    game.rotate(0.0, -25.0)?;
                }
                last_aim = Some(game.frame()?);
                // In R aiming mode the arrow is already charged while seeking
                // and aligning. A short click releases that arrow.
                game.ctrl.system_control.mouse_click()?;
                report.shots += 1;
                progress(report);
                // One arrow can collect several ores. Poll the acquisition
                // feed throughout Lumi's animation, de-duplicating visible rows.
                let mut confirmed = std::collections::BTreeMap::<String, usize>::new();
                let ore_names = [
                    "铁块",
                    "白铁块",
                    "水晶块",
                    "魔晶块",
                    "紫晶块",
                    "萃凝晶",
                    "虹滴晶",
                    "星银矿石",
                ];
                let loot_deadline = Instant::now() + Duration::from_secs(15);
                let mut last_gain: Option<Instant> = None;
                for poll in 0..75 {
                    if Instant::now() >= loot_deadline {
                        break;
                    }
                    game.wait(200)?;
                    let frame = game.frame()?;
                    if cfg!(debug_assertions) && poll % 10 == 0 {
                        imageops::crop_imm(&frame, 70, 350, 460, 400)
                            .to_image()
                            .save(format!(
                                "data/mining/last-loot-feed-{}.png",
                                (poll / 10) % 5
                            ))
                            .ok();
                    }
                    let mut visible = std::collections::BTreeMap::<String, usize>::new();
                    for (top, height) in super::super::vision::text_rows(&frame, 140, 370, 360, 340)
                    {
                        let text = game.read(&frame, 140, top, 360, height)?;
                        // Prefer the longer name so 白铁块 does not become 铁块.
                        if let Some(name) = ore_names
                            .iter()
                            .filter(|n| text.contains(**n))
                            .max_by_key(|n| n.len())
                        {
                            let quantity = text.split_once(name).and_then(|(_, tail)| {
                                tail.trim_start_matches(|c: char| {
                                    c.is_whitespace() || matches!(c, '×' | 'x' | 'X' | '*')
                                })
                                .trim()
                                .parse::<usize>()
                                .ok()
                            });
                            if let Some(quantity) = quantity.filter(|q| (1..=99).contains(q)) {
                                *visible.entry(name.to_string()).or_default() += quantity;
                            }
                        }
                    }
                    for (name, count) in visible {
                        let previous = confirmed.entry(name.clone()).or_default();
                        if count > *previous {
                            *report.loot.entry(name).or_default() += count - *previous;
                            *previous = count;
                            last_gain = Some(Instant::now());
                        }
                    }
                    progress(report);
                    if last_gain.is_some_and(|time| time.elapsed() >= Duration::from_secs(4)) {
                        break;
                    }
                }
            }
            Ok(())
        })();
        // R toggles aim. Always release any held mouse state before leaving.
        let _ = game.ctrl.system_control.left_button(false);
        let _ = game.ctrl.system_control.middle_button(false);
        game.sight_refreshed = None;
        game.leave_aim();
        let result = result.and_then(|_| game.wait(300));
        let shots = report.shots - initial_shots;
        if saw_ore && report.loot.values().sum::<usize>() == initial_loot {
            std::fs::create_dir_all("data/mining").ok();
            if let Ok(json) = serde_json::to_vec_pretty(&trace) {
                let _ = super::super::routes::write_cache(
                    std::path::Path::new("data/mining/last-aim-trace.json"),
                    &json,
                );
            }
            if let Some(frame) = last_aim {
                frame.save("data/mining/last-unconfirmed-aim.png").ok();
            }
            if let Ok(frame) = game.frame() {
                frame.save("data/mining/last-unconfirmed-after.png").ok();
            }
        }
        report.points.push(MiningPointResult {
            route: report.route.clone(),
            waypoint: report.waypoint,
            shots,
            outcome: if result.is_err() {
                "interrupted"
            } else if report.loot.values().sum::<usize>() > initial_loot {
                "collected"
            } else if shots > 0 {
                "unconfirmed"
            } else if saw_ore {
                "aimFailed"
            } else {
                "noOre"
            }
            .into(),
        });
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_forward_steps_accumulate_before_stuck_detection() {
        let mut moving = MovementProgress::new((0.0, 0.0));
        for step in 1..100 {
            assert!(moving.observe((step as f64 / 3.0, 0.0)).is_ok());
        }
        let mut blocked = MovementProgress::new((0.0, 0.0));
        for _ in 0..7 {
            assert!(blocked.observe((0.1, 0.0)).is_ok());
        }
        assert!(blocked.observe((0.1, 0.0)).is_err());
    }
}
