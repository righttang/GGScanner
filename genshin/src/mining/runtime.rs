use super::{Route, RouteStore};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use yas::cancel::CancelToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MiningStrategy {
    #[default]
    Auto,
    Linnea,
    Zhongli,
    Claymore,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MiningRequest {
    pub route_ids: Vec<String>,
    /// A single stationary scan is useful for calibrating a new display.
    #[serde(default)]
    pub stationary: bool,
    #[serde(default)]
    pub strategy: MiningStrategy,
    #[serde(default)]
    pub calibrate_crab: bool,
}

impl MiningRequest {
    pub fn routes(&self) -> Result<Vec<Route>> {
        ensure!(
            !self.calibrate_crab || self.stationary,
            "Crab calibration requires stationary mode"
        );
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
    #[serde(default)]
    pub map_floor: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiningPointResult {
    pub route: String,
    pub waypoint: usize,
    pub outcome: String,
    pub shots: usize,
    #[serde(default)]
    pub method: String,
}

pub type MiningProgress<'a> = dyn Fn(&MiningReport) + 'a;

struct MovementProgress {
    anchor: (f64, f64),
    unchanged: usize,
    target: (f64, f64),
    closest: f64,
    not_closer: usize,
}

impl MovementProgress {
    fn new(position: (f64, f64), target: (f64, f64)) -> Self {
        Self {
            anchor: position,
            unchanged: 0,
            target,
            closest: (position.0 - target.0).hypot(position.1 - target.1),
            not_closer: 0,
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
        let distance = (position.0 - self.target.0).hypot(position.1 - self.target.1);
        if distance < self.closest - 0.5 {
            self.closest = distance;
            self.not_closer = 0;
        } else {
            self.not_closer += 1;
        }
        ensure!(
            self.not_closer < 10,
            "Movement is not approaching the waypoint"
        );
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
    use super::super::actions::{self, ActionDriver, Button, GameKey, Role};
    use super::super::motion::{Motion, MotionDetector};
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
        selected_role: Option<Role>,
        script_deadline: Option<Instant>,
        loot_tracker: super::super::loot::LootTracker,
        started: Instant,
        skill_ready: std::collections::BTreeMap<Role, Instant>,
        motion: MotionDetector,
        vehicle: bool,
        underwater_route: bool,
    }
    impl Drop for Game<'_> {
        fn drop(&mut self) {
            let _ = self.ctrl.system_control.key_up(Key::Layout('w'));
            let _ = self.ctrl.system_control.key_up(Key::Shift);
            let _ = self.ctrl.system_control.left_button(false);
            let _ = self.ctrl.system_control.middle_button(false);
            self.leave_aim();
            if self.vehicle
                && self.selected_role == Some(Role::Mavuika)
                && self.ctrl.game_info.native_window().is_foreground()
            {
                if let Ok(frame) = self.ctrl.capture_game() {
                    let frame = imageops::resize(&frame, 1920, 1080, FilterType::Triangle);
                    if super::super::motion::vehicle_active(&frame) {
                        let _ = self.ctrl.system_control.key_press(Key::Layout('e'));
                    }
                }
            }
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
                self.script_deadline
                    .is_none_or(|deadline| Instant::now() < deadline),
                "Route action script exceeded 120 seconds"
            );
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
        fn motion(&self) -> Result<Motion> {
            let frame = self.frame()?;
            // Read keycaps separately; a wide crop merges Ctrl, Space and the
            // scene/UID into an unreliable line of text.
            let labels = format!(
                "{} {}",
                self.read(&frame, 1560, 1024, 100, 34)?,
                self.read(&frame, 1660, 1024, 100, 34)?
            );
            Ok(self.motion.observe(&frame, &labels))
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
        fn select_role(&mut self, role: Role) -> Result<()> {
            if self.selected_role == Some(role) {
                return Ok(());
            }
            let mut names = Vec::new();
            for _ in 0..3 {
                self.wait(400)?;
                let frame = self.frame()?;
                names.clear();
                for slot in 0..4 {
                    let name = self.read(&frame, 1640, 259 + slot * 92, 130, 28)?;
                    if role.matches(&name) {
                        self.key(Key::Layout(char::from(b'1' + slot as u8)))?;
                        self.wait(800)?;
                        self.selected_role = Some(role);
                        return Ok(());
                    }
                    names.push(name);
                }
            }
            std::fs::create_dir_all("data/mining").ok();
            self.frame()?.save("data/mining/last-failure.png").ok();
            anyhow::bail!(
                "Active party needs {}. Observed names: {names:?}",
                role.name()
            )
        }
        fn confirm_vehicle(&mut self, expected: bool) -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stable = 0;
            self.wait(250)?;
            while Instant::now() < deadline {
                if super::super::motion::vehicle_active(&self.frame()?) == expected {
                    stable += 1;
                    if stable >= 2 {
                        return Ok(());
                    }
                } else {
                    stable = 0;
                }
                self.wait(150)?;
            }
            anyhow::bail!(if expected {
                "Mavuika vehicle HUD could not be confirmed"
            } else {
                "Mavuika vehicle exit could not be confirmed"
            })
        }
        fn record_loot(&mut self, frame: &RgbImage, report: &mut MiningReport) -> Result<bool> {
            let mut visible = std::collections::BTreeMap::new();
            for (top, height) in super::super::vision::text_rows(frame, 140, 370, 360, 340) {
                let text = self.read(frame, 140, top, 360, height)?;
                if let Some((name, count)) = super::super::loot::parse_line(&text) {
                    *visible.entry(name.to_string()).or_default() += count;
                }
            }
            let gain = self
                .loot_tracker
                .observe(self.started.elapsed().as_millis() as u64, visible);
            let changed = !gain.is_empty();
            for (name, count) in gain {
                *report.loot.entry(name).or_default() += count;
            }
            Ok(changed)
        }
        fn wait_skill(&mut self, role: Role) -> Result<()> {
            if let Some(&ready) = self.skill_ready.get(&role) {
                let remaining = ready.saturating_duration_since(Instant::now()).as_millis() as u64;
                if remaining > 0 {
                    self.wait(remaining)?;
                }
            } else {
                let frame = self.frame()?;
                let x = if self.underwater_route {
                    super::super::motion::UNDERWATER_SKILL_X
                } else {
                    1680
                };
                let text = self.read(&frame, x, 938, 74, 70)?;
                let digits: String = text
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if let Ok(seconds) = digits.parse::<f64>() {
                    if seconds.is_finite() && (0.0..=20.0).contains(&seconds) {
                        self.wait((seconds * 1000.0) as u64 + 200)?;
                    }
                }
            }
            Ok(())
        }
        fn check_underwater_skill(&mut self) -> Result<()> {
            if self.underwater_route {
                ensure!(
                    self.motion()? == Motion::Underwater,
                    "Underwater skill requires a confirmed underwater HUD"
                );
                self.wait_skill(Role::Dehya)?;
                ensure!(
                    self.motion.borrowed_skill_present(&self.frame()?),
                    "No borrowed underwater E skill is visible"
                );
                ensure!(super::super::motion::crab_reference_matches(&self.frame()?)?,"Armored crab skill is unconfirmed; run the A00 acquisition route and calibrate its visible skill icon first");
            }
            Ok(())
        }
    }

    impl ActionDriver for Game<'_> {
        fn after_action(&mut self, action: &actions::Action) -> Result<()> {
            if matches!(
                action,
                actions::Action::Hold {
                    key: GameKey::Skill,
                    ..
                }
            ) && self.selected_role == Some(Role::Mavuika)
            {
                self.confirm_vehicle(true)?;
                self.vehicle = true;
            }
            if self.vehicle
                && matches!(
                    action,
                    actions::Action::Press {
                        key: GameKey::Skill
                    }
                )
            {
                self.confirm_vehicle(false)?;
                self.vehicle = false;
            }
            Ok(())
        }
        fn select(&mut self, role: Role) -> Result<()> {
            self.select_role(role)
        }
        fn wait(&mut self, milliseconds: u64) -> Result<()> {
            Game::wait(self, milliseconds)
        }
        fn move_camera(&mut self, x: i32, y: i32) -> Result<()> {
            self.rotate(x as f64, y as f64)
        }
        fn press(&mut self, key: GameKey) -> Result<()> {
            if key == GameKey::Skill {
                self.check_underwater_skill()?;
                if !self.underwater_route
                    && !self.vehicle
                    && self.selected_role == Some(Role::Mavuika)
                {
                    self.wait_skill(Role::Mavuika)?;
                }
            }
            if matches!(key, GameKey::Slot(_)) {
                self.selected_role = None;
            }
            self.key(key.native())?;
            if key == GameKey::Skill
                && !self.underwater_route
                && !self.vehicle
                && self.selected_role == Some(Role::Mavuika)
            {
                self.skill_ready
                    .insert(Role::Mavuika, Instant::now() + Duration::from_secs(16));
            }
            Ok(())
        }
        fn key_state(&mut self, key: GameKey, down: bool) -> Result<()> {
            if key == GameKey::Skill && down {
                self.check_underwater_skill()?;
            }
            if key == GameKey::Skill
                && self.selected_role == Some(Role::Mavuika)
                && !self.underwater_route
            {
                if down && !self.vehicle {
                    self.wait_skill(Role::Mavuika)?;
                }
                if !down {
                    self.skill_ready
                        .insert(Role::Mavuika, Instant::now() + Duration::from_secs(16));
                }
            }
            if key == GameKey::Skill && self.selected_role == Some(Role::Zhongli) {
                if down {
                    self.wait_skill(Role::Zhongli)?;
                } else {
                    self.skill_ready
                        .insert(Role::Zhongli, Instant::now() + Duration::from_secs(12));
                }
            }
            if down {
                self.check()?;
                self.ctrl.system_control.key_down(key.native())
            } else {
                self.ctrl.system_control.key_up(key.native())
            }
        }
        fn button_state(&mut self, button: Button, down: bool) -> Result<()> {
            if down {
                self.check()?;
            }
            match button {
                Button::Left => self.ctrl.system_control.left_button(down),
                Button::Middle => self.ctrl.system_control.middle_button(down),
            }
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
            selected_role: None,
            script_deadline: None,
            loot_tracker: Default::default(),
            started: Instant::now(),
            skill_ready: Default::default(),
            motion: MotionDetector::new()?,
            vehicle: false,
            underwater_route: false,
        };
        game.check()?;
        if !game.ctrl.return_to_main_ui(3) {
            anyhow::bail!("Could not return to the main world");
        }
        if request.calibrate_crab {
            ensure!(
                game.motion()? == Motion::Underwater,
                "Crab skill calibration requires an underwater HUD"
            );
            let frame = game.frame()?;
            ensure!(
                game.motion.borrowed_skill_present(&frame),
                "A borrowed underwater E skill must be visible before calibration"
            );
            super::super::motion::save_crab_reference(&frame)?;
            report.route = "crabCalibration".into();
            report.completed_points = 1;
            return Ok(());
        }
        if request.stationary {
            game.underwater_route = game.motion()? == Motion::Underwater;
            ensure!(
                !game.underwater_route || request.strategy == MiningStrategy::Auto,
                "Underwater stationary mining requires automatic strategy"
            );
        }
        if request.stationary
            && !game.underwater_route
            && matches!(
                request.strategy,
                MiningStrategy::Auto | MiningStrategy::Linnea
            )
        {
            game.select_role(Role::Linnea)?;
        }
        if request.stationary {
            report.route = "stationary".into();
            report.waypoint = 1;
            match request.strategy {
                MiningStrategy::Auto if game.underwater_route => ground_mine(
                    &mut game,
                    &mut detector,
                    MiningStrategy::Auto,
                    true,
                    progress,
                    report,
                )?,
                MiningStrategy::Auto | MiningStrategy::Linnea => {
                    mine(&mut game, &mut detector, 1, 24, progress, report)?
                },
                method => ground_mine(&mut game, &mut detector, method, true, progress, report)?,
            }
            report.completed_points = 1;
            return Ok(());
        }
        let atlas = atlas.unwrap();
        let mini_locator = mini_locator.as_mut().unwrap();
        let teleports = TeleportCatalog::new()?;
        let mut position = None;
        for (route, route_id) in routes.into_iter().zip(&request.route_ids) {
            report.route = route.info.name.clone();
            mini_locator.set_floor(
                route
                    .info
                    .floor
                    .or_else(|| (route.environment() == "ground").then_some(0)),
            );
            let completed_before = report.completed_points;
            let loot_before = report.loot.clone();
            let result = (|| -> Result<()> {
                game.underwater_route = route.environment() == "underwater";
                if game.underwater_route {
                    game.select_role(Role::Dehya)?;
                }
                for (index, point) in route.positions.iter().enumerate() {
                    game.check()?;
                    report.waypoint = index + 1;
                    if point.kind != "teleport"
                        && point.action != "fight"
                        && !game.underwater_route
                        && super::super::vision::enemy_health_bar(&game.frame()?)
                    {
                        fight(&mut game, progress, report)?;
                        position = Some(
                            locate(&game, mini_locator, position, 60)
                                .or_else(|_| locate(&game, mini_locator, position, 250))?,
                        );
                    }
                    report.phase = if point.kind == "teleport" {
                        "teleporting"
                    } else {
                        "moving"
                    }
                    .into();
                    progress(report);
                    if point.kind == "teleport" || point.action == "force_tp" {
                        teleport(
                            &mut game,
                            &atlas,
                            mini_locator,
                            (point.x, point.y),
                            teleports.nearest((point.x, point.y))?,
                        )?;
                        game.selected_role = None;
                        position = Some(locate(&game, mini_locator, Some((point.x, point.y)), 50)?);
                        report.map_floor = mini_locator.last_floor;
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
                            &point.move_mode,
                        )?);
                        pickup_visible(&mut game, progress, report)?;
                    }
                    report.map_floor = mini_locator.last_floor;
                    match point.action.as_str() {
                        "linnea_mining" => {
                            let (shots, rounds) = point.shots()?;
                            mine(&mut game, &mut detector, shots, rounds, progress, report)?;
                        },
                        "combat_script" => {
                            let parsed = actions::parse_script(&point.action_params)?;
                            let initial_loot = report.loot.values().sum::<usize>();
                            report.phase = "executingAction".into();
                            progress(report);
                            game.script_deadline = Some(Instant::now() + Duration::from_secs(120));
                            let result = actions::execute_script(&mut game, &point.action_params);
                            game.script_deadline = None;
                            result?;
                            if parsed.iter().any(|a| {
                                matches!(
                                    a,
                                    actions::Action::Hold {
                                        key: GameKey::Forward
                                            | GameKey::Back
                                            | GameKey::Left
                                            | GameKey::Right,
                                        ..
                                    } | actions::Action::Down {
                                        key: GameKey::Forward
                                            | GameKey::Back
                                            | GameKey::Left
                                            | GameKey::Right
                                    }
                                )
                            }) {
                                position=Some(locate(&game,mini_locator,position,60)
                                    .or_else(|_|locate(&game,mini_locator,position,250))
                                    .context("Position after route movement script could not be confirmed")?);
                            }
                            let water_mining = game.underwater_route
                                && parsed.iter().any(|a| {
                                    matches!(
                                        a,
                                        actions::Action::Press {
                                            key: GameKey::Skill
                                        } | actions::Action::Hold {
                                            key: GameKey::Skill,
                                            ..
                                        } | actions::Action::Down {
                                            key: GameKey::Skill
                                        }
                                    )
                                });
                            if water_mining {
                                finish_ground_mine(
                                    &mut game,
                                    "armoredCrab",
                                    false,
                                    initial_loot,
                                    progress,
                                    report,
                                )?;
                            } else if game.underwater_route {
                                pickup_visible(&mut game, progress, report)?;
                                poll_loot(&mut game, report, progress, 800)?;
                            }
                            if actions::mining_role(&parsed).is_some() {
                                finish_ground_mine(
                                    &mut game,
                                    "zhongli",
                                    false,
                                    initial_loot,
                                    progress,
                                    report,
                                )?;
                            }
                        },
                        "mining" => ground_mine(
                            &mut game,
                            &mut detector,
                            request.strategy,
                            point.action_params != "disablePickupAround",
                            progress,
                            report,
                        )?,
                        "pick_around" => {
                            let rounds = point.action_params.parse::<usize>().unwrap_or(1);
                            for _ in 0..rounds {
                                collect_nearby(&mut game, true, progress, report)?;
                            }
                        },
                        "stop_flying" => land(&mut game, &point.action_params, progress, report)?,
                        "fight" => {
                            fight(&mut game, progress, report)?;
                            position = Some(
                                locate(&game, mini_locator, position, 60)
                                    .or_else(|_| locate(&game, mini_locator, position, 250))?,
                            );
                        },
                        "force_tp" => {}, // handled by the teleport branch below
                        "" => {},
                        other => anyhow::bail!("Unsupported route action: {other}"),
                    }
                    report.completed_points += 1;
                    progress(report);
                }
                Ok(())
            })();
            let loot = report
                .loot
                .iter()
                .filter_map(|(name, n)| {
                    let gain = n.saturating_sub(*loot_before.get(name).unwrap_or(&0));
                    (gain > 0).then(|| (name.clone(), gain))
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let validation = super::super::routes::RouteValidation {
                engine_revision: String::new(),
                route_fingerprint: String::new(),
                finished_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
                outcome: if token.is_cancelled() {
                    "stopped"
                } else if result.is_err() {
                    "failed"
                } else if loot.is_empty() {
                    "completedUnconfirmed"
                } else {
                    "completedWithLoot"
                }
                .into(),
                completed_points: report.completed_points - completed_before,
                total_points: route.positions.len(),
                loot,
                error: result.as_ref().err().map(|e| format!("{e:#}")),
            };
            if let Err(error) = RouteStore::default().record_run(route_id, validation) {
                yas::log_warn!(
                    "保存路线测试记录失败：{}",
                    "Could not save route validation: {}",
                    error
                );
            }
            result?;
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
        let mut last_error = None;
        for pixels in [160.0, 320.0] {
            let before = game.frame()?;
            game.rotate(pixels, 0.0)?;
            let after = game.frame()?;
            game.rotate(-pixels, 0.0)?;
            match super::super::vision::estimate_camera_rotation(&before, &after, pixels) {
                Ok(heading) => return Ok(heading),
                Err(error) => {
                    if cfg!(debug_assertions) {
                        before.save("data/mining/last-camera-before.png").ok();
                        after.save("data/mining/last-camera-after.png").ok();
                    }
                    last_error = Some(error);
                },
            }
        }
        Err(last_error.unwrap()).context("Camera calibration failed after two probes")
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
        mode: &str,
    ) -> Result<(f64, f64)> {
        let started = Instant::now();
        let mut movement = MovementProgress::new(current, target);
        let mut recovery_used = false;
        let mut last_heading: Option<f64> = None;
        let mut since_face = 0;
        let mut glide_finished = false;
        if mode == "fly" {
            for _ in 0..8 {
                if game.motion()? == Motion::Gliding {
                    break;
                }
                game.key(Key::Space)?;
                game.forward(180, false)?;
                game.wait(150)?;
            }
            ensure!(
                game.motion()? == Motion::Gliding,
                "Glider takeoff could not be confirmed"
            );
        }
        loop {
            let distance = (target.0 - current.0).hypot(target.1 - current.1);
            let motion = game.motion()?;
            if distance < 2.0 && (mode != "climb" || motion != Motion::Climbing) {
                return Ok(current);
            }
            ensure!(
                started.elapsed() < Duration::from_secs(120),
                "Waypoint timed out"
            );
            let heading = (current.1 - target.1).atan2(current.0 - target.0);
            if since_face >= 6
                || last_heading.is_none_or(|last| angle_difference(heading, last).abs() > 0.12)
            {
                face(game, current, target)?;
                last_heading = Some(heading);
                since_face = 0;
            }
            since_face += 1;
            if mode == "fly" && !glide_finished {
                if motion == Motion::Ground
                    && distance < 8.0
                    && game.motion.ground_skill_present(&game.frame()?)
                {
                    glide_finished = true;
                    yas::log_info!(
                        "已在目标附近落地，改为步行接近",
                        "Landed near target; finishing approach on foot"
                    );
                }
                ensure!(
                    glide_finished || motion == Motion::Gliding,
                    "Gliding ended before the waypoint"
                );
            }
            if mode == "jump" {
                game.key(Key::Space)?;
            }
            game.forward(
                if distance < 6.0 { 150 } else { 450 },
                matches!(mode, "run" | "dash" | "swim") && distance > 20.0,
            )?;
            game.wait(100)?;
            let next = locate(game, mini_locator, Some(current), 20)
                .or_else(|_| locate(game, mini_locator, Some(current), 60))?;
            ensure!(
                (current.0 - next.0).hypot(current.1 - next.1) < 24.0,
                "Mini-map position jumped unexpectedly"
            );
            // Climbing may change altitude without measurable map displacement.
            // Its independent waypoint deadline still bounds the attempt.
            if let Err(error) = if mode == "climb" && motion == Motion::Climbing {
                Ok(())
            } else {
                movement.observe(next)
            } {
                if recovery_used {
                    return Err(error)
                        .with_context(|| format!("Target {target:?}; observed {next:?}"));
                }
                recovery_used = true;
                if motion == Motion::Underwater {
                    yas::log_info!(
                        "水下行进受阻，尝试一次短距离上浮",
                        "Underwater path blocked; attempting one short ascent"
                    );
                    actions::execute_script(game, "keydown(SPACE),w(0.6),keyup(SPACE)")?;
                    game.wait(400)?;
                } else {
                    yas::log_info!(
                        "挖矿行进受阻，尝试一次短跳脱困",
                        "Mining path blocked; attempting one short jump"
                    );
                    game.key(Key::Space)?;
                    game.forward(450, false)?;
                    game.wait(900)?;
                }
                current = locate(game, mini_locator, Some(next), 20)
                    .or_else(|_| locate(game, mini_locator, Some(next), 60))?;
                ensure!(
                    (current.0 - next.0).hypot(current.1 - next.1) < 24.0,
                    "Mini-map position jumped unexpectedly during recovery"
                );
                movement = MovementProgress::new(current, target);
                last_heading = None;
                continue;
            }
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
            let (x, y) = super::super::vision::map_region_button(region, |x, y, w, h| {
                game.read(&frame, x, y, w, h)
            })?
            .with_context(|| format!("Map region {region} could not be selected"))?;
            game.click(x, y)?;
            game.wait(700)?;
        }
        if let Some((x, y)) = game.motion.ground_button(&game.frame()?) {
            game.click(x, y)?;
            game.wait(500)?;
        }
        // Require several terrain features to agree on a north-up transform.
        let mut pan_trace = Vec::new();
        let mut previous_map: Option<(f64, f64, f64)> = None;
        let matcher =
            super::super::features::FeatureAtlas::new(&atlas.image, map_target, Some(game.token))?;
        let map_started = Instant::now();
        for _ in 0..40 {
            ensure!(
                map_started.elapsed() < Duration::from_secs(120),
                "Map navigation exceeded 120 seconds"
            );
            let frame = game.frame()?;
            if cfg!(debug_assertions) {
                frame.save("data/mining/last-map-before-match.png").ok();
            }
            let patch = imageops::crop_imm(&frame, 660, 330, 600, 420).to_image();
            let (x, y, scale) = matcher.locate(&patch, Some(game.token))?;
            if let Some((px, py, ps)) = previous_map {
                ensure!(
                    (x - px).hypot(y - py) < 2000.0 && (scale - ps).abs() < 0.025,
                    "Map projection changed unexpectedly after panning"
                );
            }
            previous_map = Some((x, y, scale));
            let screen_x = 960.0 + (x - map_target.0) * 0.25 / scale;
            let screen_y = 540.0 + (y - map_target.1) * 0.25 / scale;
            pan_trace.push(serde_json::json!({"center":[x,y],"scale":scale,"target":map_target,"screen":[screen_x,screen_y]}));
            if cfg!(debug_assertions) {
                let _ = super::super::routes::write_cache(
                    std::path::Path::new("data/mining/last-map-projection.json"),
                    &serde_json::to_vec_pretty(&pan_trace)?,
                );
            }
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
            let (drag_x, drag_y) = super::super::vision::map_drag_origin(&frame);
            let sx = game.ctrl.scaler.scale_x(drag_x) as i32 + origin.x;
            let sy = game.ctrl.scaler.scale_y(drag_y) as i32 + origin.y;
            game.ctrl.system_control.mouse_drag(
                sx,
                sy,
                sx + game.ctrl.scaler.scale_x(dx) as i32,
                sy + game.ctrl.scaler.scale_y(dy) as i32,
            )?;
            game.wait(500)?;
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&pan_trace) {
            let _ = super::super::routes::write_cache(
                std::path::Path::new("data/mining/last-map-projection.json"),
                &bytes,
            );
        }
        anyhow::bail!("Target anchor could not be reached on the map")
    }

    fn land(
        game: &mut Game<'_>,
        params: &str,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        report.phase = "landing".into();
        progress(report);
        if game.motion()? == Motion::Gliding {
            if !params.is_empty() {
                game.key(Key::Space)?;
                game.wait(params.parse::<u64>()?)?;
                game.key(Key::Space)?;
                game.wait(300)?;
            }
            actions::execute_script(game, "attack")?;
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut grounded = None;
        while Instant::now() < deadline {
            game.wait(300)?;
            let frame = game.frame()?;
            if game.motion()? == Motion::Ground && game.motion.ground_skill_present(&frame) {
                let since = grounded.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(1500) {
                    return Ok(());
                }
            } else {
                grounded = None;
            }
        }
        anyhow::bail!("Landing HUD could not be confirmed within 15 seconds")
    }

    fn fight(
        game: &mut Game<'_>,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        report.phase = "fighting".into();
        progress(report);
        let started = Instant::now();
        let mut saw_enemy = false;
        let mut clear_since = None;
        let mut last_shield = None;
        let mut last_fire: Option<Instant> = None;
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(90),
                "Combat exceeded 90 seconds; enemy clearance unconfirmed"
            );
            let enemy = super::super::vision::enemy_health_bar(&game.frame()?);
            saw_enemy |= enemy;
            if enemy {
                clear_since = None;
            } else {
                let since = clear_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(4) {
                    let (_, sensitivity) = camera_heading(game)?;
                    let mut found = false;
                    for _ in 0..4 {
                        game.rotate(90.0 * sensitivity, 0.0)?;
                        if super::super::vision::enemy_health_bar(&game.frame()?) {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        clear_since = None;
                        saw_enemy = true;
                        continue;
                    }
                    report.points.push(MiningPointResult {
                        route: report.route.clone(),
                        waypoint: report.waypoint,
                        outcome: if saw_enemy {
                            "cleared"
                        } else {
                            "noEnemiesObserved"
                        }
                        .into(),
                        shots: 0,
                        method: "combat".into(),
                    });
                    return Ok(());
                }
            }
            if enemy {
                if game.underwater_route {
                    game.press(GameKey::Skill)?;
                    game.wait(500)?;
                    continue;
                }
                if last_shield.is_none_or(|time: Instant| time.elapsed() >= Duration::from_secs(13))
                {
                    actions::execute_script(game, "钟离 e(hold)")?;
                    last_shield = Some(Instant::now());
                }
                game.select_role(Role::Mavuika)?;
                if last_fire.is_none_or(|time| time.elapsed() >= Duration::from_secs(16)) {
                    game.press(GameKey::Skill)?;
                    last_fire = Some(Instant::now());
                }
                actions::execute_script(game, "attack(1.5)")?;
            } else if saw_enemy {
                // Finish a nearly depleted bar before the clearance window;
                // its final few pixels may be below the vision threshold.
                actions::execute_script(game, "attack(0.35)")?;
            } else {
                game.wait(300)?;
            }
            progress(report);
        }
    }

    fn pickup_visible(
        game: &mut Game<'_>,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<usize> {
        let mut picked = 0;
        let attribute_to_previous = report.phase == "moving";
        for _ in 0..12 {
            let frame = game.frame()?;
            if game.record_loot(&frame, report)? && attribute_to_previous {
                if let Some(last) = report.points.last_mut() {
                    if last.route == report.route
                        && last.method != "linnea"
                        && last.outcome == "unconfirmed"
                    {
                        last.outcome = "collected".into();
                    }
                }
            }
            let mut prompts = Vec::new();
            for (top, height) in super::super::vision::text_rows(&frame, 1070, 330, 530, 420) {
                let text = game.read(&frame, 1070, top, 530, height)?;
                if !text.trim().is_empty() {
                    prompts.push((top, text));
                }
            }
            if cfg!(debug_assertions) {
                if let Ok(bytes) = serde_json::to_vec_pretty(&prompts) {
                    let _ = super::super::routes::write_cache(
                        std::path::Path::new("data/mining/last-pickup-prompts.json"),
                        &bytes,
                    );
                }
            }
            // Only use an ore-labelled F prompt. Never select an unrelated
            // NPC, quest or object merely because an interaction is available.
            let is_ore = prompts.iter().any(|(top, text)| {
                super::super::loot::ore_name(text).is_some()
                    && game.motion.ore_prompt_key(&frame, *top, 42)
            });
            if !is_ore {
                break;
            }
            report.phase = "pickingUp".into();
            progress(report);
            game.key(Key::Layout('f'))?;
            picked += 1;
            game.wait(200)?;
        }
        progress(report);
        Ok(picked)
    }

    fn poll_loot(
        game: &mut Game<'_>,
        report: &mut MiningReport,
        progress: &MiningProgress<'_>,
        max_ms: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(max_ms);
        let mut last_gain = None;
        while Instant::now() < deadline {
            game.wait(200)?;
            let frame = game.frame()?;
            if game.record_loot(&frame, report)? {
                last_gain = Some(Instant::now());
            }
            progress(report);
            if last_gain
                .is_some_and(|time| Instant::now().duration_since(time) >= Duration::from_secs(4))
            {
                break;
            }
        }
        Ok(())
    }

    fn collect_nearby(
        game: &mut Game<'_>,
        sweep: bool,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        report.phase = "pickingUp".into();
        progress(report);
        pickup_visible(game, progress, report)?;
        if sweep {
            let deadline = Instant::now() + Duration::from_secs(20);
            for script in ["w(0.2)", "s(0.4)", "w(0.2)", "a(0.2)", "d(0.4)", "a(0.2)"] {
                if Instant::now() >= deadline {
                    break;
                }
                actions::execute_script(game, script)?;
                pickup_visible(game, progress, report)?;
            }
        }
        poll_loot(game, report, progress, 2000)
    }

    fn finish_ground_mine(
        game: &mut Game<'_>,
        method: &str,
        sweep: bool,
        initial_loot: usize,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        game.wait(500)?;
        let result = collect_nearby(game, sweep, progress, report);
        report.points.push(MiningPointResult {
            route: report.route.clone(),
            waypoint: report.waypoint,
            shots: 0,
            method: method.into(),
            outcome: if result.is_err() {
                "interrupted"
            } else if report.loot.values().sum::<usize>() > initial_loot {
                "collected"
            } else {
                "unconfirmed"
            }
            .into(),
        });
        result
    }

    fn ground_mine(
        game: &mut Game<'_>,
        detector: &mut OreDetector,
        strategy: MiningStrategy,
        sweep: bool,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        if matches!(strategy, MiningStrategy::Linnea) {
            return mine(game, detector, 1, 24, progress, report);
        }
        let initial_loot = report.loot.values().sum::<usize>();
        if game.underwater_route {
            game.press(GameKey::Skill)?;
            return finish_ground_mine(game, "armoredCrab", sweep, initial_loot, progress, report);
        }
        report.phase = "breakingOre".into();
        progress(report);
        let method = if matches!(strategy, MiningStrategy::Zhongli) {
            actions::execute_script(game, "钟离 e(hold)")?;
            "zhongli"
        } else {
            game.select_role(Role::Mavuika)?;
            for _ in 0..3 {
                actions::execute_script(game, "attack(1.4)")?;
                pickup_visible(game, progress, report)?;
            }
            "claymore"
        };
        finish_ground_mine(game, method, sweep, initial_loot, progress, report)
    }

    fn mine(
        game: &mut Game<'_>,
        detector: &mut OreDetector,
        shot_limit: usize,
        rounds: usize,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        game.select_role(Role::Linnea)?;
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
                poll_loot(game, report, progress, 15000)?;
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
            method: "linnea".into(),
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
        let mut moving = MovementProgress::new((0.0, 0.0), (100.0, 0.0));
        for step in 1..100 {
            assert!(moving.observe((step as f64 / 3.0, 0.0)).is_ok());
        }
        let mut blocked = MovementProgress::new((0.0, 0.0), (100.0, 0.0));
        for _ in 0..7 {
            assert!(blocked.observe((0.1, 0.0)).is_ok());
        }
        assert!(blocked.observe((0.1, 0.0)).is_err());
    }
    #[test]
    fn circling_does_not_count_as_navigation_progress() {
        let mut progress = MovementProgress::new((10.0, 0.0), (0.0, 0.0));
        for step in 1..10 {
            let angle = step as f64 * 0.3;
            assert!(progress
                .observe((10.0 * angle.cos(), 10.0 * angle.sin()))
                .is_ok());
        }
        assert!(progress
            .observe((10.0 * 3.0f64.cos(), 10.0 * 3.0f64.sin()))
            .is_err());
    }
}
