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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<MiningRecovery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiningRecovery {
    pub outcome: String,
    pub destination: String,
    pub error: Option<String>,
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

/// Samples include a bounded wait in the live driver. Never send another menu
/// toggle while the previous transition is still settling.
#[cfg(any(windows, test))]
fn confirm_world_state(
    expected_world: bool,
    mut sample: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let mut stable = 0;
    for _ in 0..25 {
        if sample()? == expected_world {
            stable += 1;
            if stable == 2 {
                return Ok(());
            }
        } else {
            stable = 0;
        }
    }
    anyhow::bail!("World UI transition did not settle")
}

pub(super) struct MovementProgress {
    anchor: (f64, f64),
    unchanged: usize,
    target: (f64, f64),
    closest: f64,
    not_closer: usize,
}

impl MovementProgress {
    pub(super) fn new(position: (f64, f64), target: (f64, f64)) -> Self {
        Self {
            anchor: position,
            unchanged: 0,
            target,
            closest: (position.0 - target.0).hypot(position.1 - target.1),
            not_closer: 0,
        }
    }
    pub(super) fn observe(&mut self, position: (f64, f64)) -> Result<()> {
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
        selected_slot: Option<usize>,
        combat_active: bool,
        script_deadline: Option<Instant>,
        loot_tracker: super::super::loot::LootTracker,
        started: Instant,
        skill_ready: std::collections::BTreeMap<Role, Instant>,
        motion: MotionDetector,
        vehicle: bool,
        underwater_route: bool,
        map_locator: super::super::big_map::MapLocator,
        anchor_icons: super::super::big_map::AnchorIcons,
        camera_calibration: Option<(f64, f64, Instant)>,
        camera_probes: u64,
        has_mavuika: Option<bool>,
        navigation_trace: std::collections::VecDeque<serde_json::Value>,
        teleport_diagnostic: serde_json::Value,
        teleport_selection: Option<RgbImage>,
    }
    impl KeyDriver for Game<'_> {
        fn key_down(&mut self, key: Key) -> Result<()> {
            self.check()?;
            if matches!(key, Key::Layout('1'..='4')) {
                self.selected_role = None;
                self.selected_slot = None;
            }
            self.ctrl.system_control.key_down(key)
        }
        fn key_up(&mut self, key: Key) -> Result<()> {
            // Release even when cancelled or the foreground window has changed.
            self.ctrl.system_control.key_up(key)
        }
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
            let mut checked_health = Instant::now();
            while Instant::now() < deadline {
                self.check()?;
                if self.combat_active && checked_health.elapsed() >= Duration::from_millis(250) {
                    ensure!(
                        super::super::vision::player_health_fraction(&self.frame()?)
                            .is_some_and(|hp| hp >= 0.50),
                        "Health too low or unconfirmed during combat action"
                    );
                    checked_health = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }
        fn key(&mut self, key: Key) -> Result<()> {
            self.check()?;
            let hold = if matches!(key, Key::Layout('1'..='4')) {
                150
            } else {
                60
            };
            yas::system_control::key_hold::with_held_key_control(self, key, |game| game.wait(hold))
        }
        fn pause_world(&mut self) -> Result<()> {
            self.check()?;
            self.key_up(Key::Layout('w'))?;
            self.key_up(Key::Shift)?;
            ensure!(
                self.ctrl.is_likely_main_world(),
                "Main world is unavailable for pausing"
            );
            self.key(Key::Escape)?;
            self.wait(300)?;
            confirm_world_state(false, || {
                self.wait(100)?;
                Ok(self.ctrl.is_likely_main_world())
            })
            .context("Pause menu did not open")
        }
        fn resume_world(&mut self) -> Result<()> {
            self.check()?;
            if self.ctrl.is_likely_main_world() {
                return Ok(());
            }
            self.key(Key::Escape)?;
            self.wait(300)?;
            confirm_world_state(true, || {
                self.wait(100)?;
                Ok(self.ctrl.is_likely_main_world())
            })
            .context("Pause menu did not return to the world")
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
        fn set_map_zoom(&mut self, y: f64) -> Result<()> {
            ensure!(
                (490.0..=595.0).contains(&y),
                "Map zoom target is outside the navigation range"
            );
            let frame = self.frame()?;
            let (x, current_y) = self.map_locator.zoom_knob(&frame)?;
            if (current_y - y).abs() < 3.0 {
                return Ok(());
            }
            let origin = self.ctrl.game_info.window.origin();
            let sx = origin.x + self.ctrl.scaler.scale_x(x).round() as i32;
            let sy = origin.y + self.ctrl.scaler.scale_y(current_y).round() as i32;
            self.ctrl.system_control.mouse_drag(
                sx,
                sy,
                sx,
                origin.y + self.ctrl.scaler.scale_y(y).round() as i32,
            )?;
            self.wait(400)?;
            let after = self.frame()?;
            let (_, actual) = self.map_locator.zoom_knob(&after)?;
            ensure!(
                (actual - y).abs() < 8.0,
                "Map zoom slider did not reach its target"
            );
            Ok(())
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
            let before = self.frame()?;
            ensure!(
                super::super::vision::player_health_fraction(&before).is_some_and(|hp| hp >= 0.45),
                "Health too low or unconfirmed for ore aiming"
            );
            ensure!(
                !super::super::vision::enemy_health_bar(&before),
                "Enemy approached during ore aiming"
            );
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
        fn select_slot(&mut self, slot: usize) -> Result<()> {
            ensure!(slot < 4, "Party slot is outside the active party");
            let before = self.frame()?;
            if super::super::vision::active_party_slot(&before) == Some(slot) {
                self.selected_slot = Some(slot);
                return Ok(());
            }
            for _ in 0..3 {
                let mut before = self.frame()?;
                let ready_by = Instant::now() + Duration::from_secs(1);
                while !super::super::vision::party_slot_available(&before, slot)
                    && Instant::now() < ready_by
                {
                    // A previous switch can still be fading its HUD. Observe
                    // until it settles; do not send a key on an unknown frame.
                    self.wait(100)?;
                    before = self.frame()?;
                }
                if super::super::vision::active_party_slot(&before) == Some(slot) {
                    self.selected_slot = Some(slot);
                    return Ok(());
                }
                ensure!(
                    super::super::vision::party_slot_available(&before, slot),
                    "Party slot {} is defeated, too low on health or its HUD is unconfirmed",
                    slot + 1
                );
                let initial_hp = super::super::vision::player_health_fraction(&before).unwrap();
                self.key(Key::Layout(char::from(b'1' + slot as u8)))?;
                // Poll acknowledgement instead of standing exposed for the
                // full shared switch CD after every successful change.
                let deadline = Instant::now() + Duration::from_millis(1500);
                let mut confirmed = false;
                while Instant::now() < deadline {
                    self.wait(100)?;
                    let after = self.frame()?;
                    if super::super::vision::active_party_slot(&after) == Some(slot) {
                        if confirmed {
                            self.selected_slot = Some(slot);
                            return Ok(());
                        }
                        confirmed = true;
                        continue;
                    }
                    confirmed = false;
                    // The actual switch animation briefly hides the HP bars.
                    // It is neither acknowledgement nor permission for another
                    // key. The next attempt repeats the living-target preflight.
                    if let Some(hp) = super::super::vision::player_health_fraction(&after) {
                        ensure!(
                            hp >= initial_hp - 0.15
                                && (!super::super::vision::enemy_health_bar(&after) || hp >= 0.45),
                            "Health fell while party switching was blocked"
                        );
                    }
                }
                self.wait(250)?;
            }
            let after = self.frame()?;
            if cfg!(debug_assertions) {
                before.save("data/mining/last-party-switch-before.png").ok();
                after.save("data/mining/last-party-switch-after.png").ok();
            }
            anyhow::bail!(
                "Requested party slot {} did not become active (observed {:?})",
                slot + 1,
                super::super::vision::active_party_slot(&after)
            );
        }
        fn select_role(&mut self, role: Role) -> Result<()> {
            let mut names = Vec::new();
            for _ in 0..3 {
                self.wait(400)?;
                let frame = self.frame()?;
                names.clear();
                for slot in 0..4 {
                    let name = self.read(&frame, 1640, 259 + slot * 92, 130, 28)?;
                    if role.matches(&name) {
                        let active = super::super::vision::active_party_slot(&frame);
                        if active == Some(slot as usize) {
                            self.selected_role = Some(role);
                            self.selected_slot = Some(slot as usize);
                            return Ok(());
                        }
                        self.select_slot(slot as usize)
                            .with_context(|| format!("Select role {}", role.name()))?;
                        self.selected_role = Some(role);
                        self.camera_calibration = None;
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
        fn select_travel_role(&mut self) -> Result<()> {
            if self.underwater_route {
                return self.select_role(Role::Dehya);
            }
            if self.has_mavuika.is_none() {
                let frame = self.frame()?;
                let mut available = false;
                for slot in 0..4 {
                    available |= Role::Mavuika.matches(&self.read(
                        &frame,
                        1640,
                        259 + slot * 92,
                        130,
                        28,
                    )?);
                }
                self.has_mavuika = Some(available);
            }
            if self.has_mavuika == Some(true) {
                self.select_role(Role::Mavuika)?;
            }
            Ok(())
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
            let visible =
                super::super::loot::read_visible(frame, |x, y, w, h| self.read(frame, x, y, w, h))?;
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
                self.key_down(key.native())
            } else {
                self.key_up(key.native())
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
            selected_slot: None,
            combat_active: false,
            script_deadline: None,
            loot_tracker: Default::default(),
            started: Instant::now(),
            skill_ready: Default::default(),
            motion: MotionDetector::new()?,
            vehicle: false,
            underwater_route: false,
            map_locator: super::super::big_map::MapLocator::new(),
            anchor_icons: super::super::big_map::AnchorIcons::new(),
            camera_calibration: None,
            camera_probes: 0,
            has_mavuika: None,
            navigation_trace: Default::default(),
            teleport_diagnostic: serde_json::Value::Null,
            teleport_selection: None,
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
            game.navigation_trace.clear();
            game.teleport_diagnostic = serde_json::Value::Null;
            game.teleport_selection = None;
            mini_locator.set_floor(route.floor_hint());
            let mut requested_floor = route.floor_hint();
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
                    let waypoint_floor = route.waypoint_floor(index);
                    if waypoint_floor != requested_floor {
                        mini_locator.set_floor(waypoint_floor);
                        requested_floor = waypoint_floor;
                    }
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
                        let anchor = teleports.route_anchor((point.x, point.y))?;
                        let landing = anchor.landing_position();
                        teleport(
                            &mut game,
                            &atlas,
                            mini_locator,
                            landing,
                            anchor,
                            point.action == "force_tp",
                        )?;
                        game.selected_role = None;
                        game.selected_slot = None;
                        position = Some(locate(&game, mini_locator, Some(landing), 50)?);
                        report.map_floor = mini_locator.last_floor;
                        let landed = position.unwrap();
                        ensure!(
                            (landed.0 - landing.0).hypot(landed.1 - landing.1) <= 16.0,
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
                            progress,
                            report,
                        )?);
                        pickup_visible(&mut game, progress, report)?;
                    }
                    // A clear scene before walking does not establish safety
                    // at the destination. Do not enter a long aiming loop with
                    // enemies that came into view during the movement leg.
                    if !game.underwater_route
                        && point.action != "fight"
                        && matches!(point.action.as_str(), "mining" | "linnea_mining")
                        && super::super::vision::enemy_health_bar(&game.frame()?)
                    {
                        fight(&mut game, progress, report)?;
                        position = Some(
                            locate(&game, mini_locator, position, 60)
                                .or_else(|_| locate(&game, mini_locator, position, 250))?,
                        );
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
            if result.is_err() && game.check().is_ok() {
                // Recovery moves away from the failed waypoint. Keep one
                // local frame of the obstacle/HUD before that evidence is lost.
                if let Ok(frame) = game.frame() {
                    frame.save("data/mining/last-route-failure.png").ok();
                }
                if let Some(frame) = &game.teleport_selection {
                    frame.save("data/mining/last-teleport-selection.png").ok();
                }
                if let Ok(bytes) = serde_json::to_vec_pretty(&serde_json::json!({
                    "engineRevision":super::super::routes::ENGINE_REVISION,
                    "route":report.route,"waypoint":report.waypoint,
                    "error":result.as_ref().err().map(|error| format!("{error:#}")),
                    "lastNavigation":game.navigation_trace,
                    "lastTeleport":game.teleport_diagnostic,
                })) {
                    let _ = super::super::routes::write_cache(
                        std::path::Path::new("data/mining/last-route-failure.json"),
                        &bytes,
                    );
                }
                if let Err(error) = recover_if_needed(
                    &mut game,
                    &atlas,
                    mini_locator,
                    &teleports,
                    progress,
                    report,
                ) {
                    let paused = game.check().is_ok()
                        && game.ctrl.return_to_main_ui(2)
                        && game.pause_world().is_ok();
                    report.recovery = Some(MiningRecovery {
                        outcome: if paused { "paused" } else { "failed" }.into(),
                        destination: if paused {
                            "current"
                        } else {
                            "StarfellLakeStatue"
                        }
                        .into(),
                        error: Some(format!("{error:#}")),
                    });
                    progress(report);
                }
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
        if let Some((sensitivity, offset, at)) = game.camera_calibration {
            if at.elapsed() < Duration::from_secs(15) {
                if let Ok(angle) = super::super::vision::camera_cone_heading(&game.frame()?) {
                    return Ok((angle + offset, sensitivity));
                }
            }
            game.camera_calibration = None;
        }
        // Camera rotation changes the mini-map's viewing cone, while terrain
        // stays still. Difference the two frames to remove terrain brightness.
        game.key_up(Key::Layout('w'))?;
        game.key_up(Key::Shift)?;
        game.camera_probes += 1;
        let mut last_error = None;
        for pixels in [160.0, 320.0] {
            let before = game.frame()?;
            game.rotate(pixels, 0.0)?;
            let after = game.frame()?;
            game.rotate(-pixels, 0.0)?;
            if cfg!(debug_assertions) && game.camera_probes == 1 {
                before.save("data/mining/last-camera-probe-before.png").ok();
                after.save("data/mining/last-camera-probe-after.png").ok();
            }
            match super::super::vision::estimate_camera_rotation(&before, &after, pixels) {
                Ok(heading) => {
                    if let (Ok(a), Ok(b)) = (
                        super::super::vision::camera_cone_heading(&before),
                        super::super::vision::camera_cone_heading(&after),
                    ) {
                        let measured = (pixels / heading.1).to_radians();
                        if angle_difference(b, a + measured).abs() < 8.0_f64.to_radians() {
                            game.camera_calibration =
                                Some((heading.1, angle_difference(heading.0, a), Instant::now()));
                        }
                    }
                    return Ok(heading);
                },
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

    struct Travel<'a, 'g, 'p> {
        game: &'a mut Game<'g>,
        mini: &'a mut MiniMapLocator,
        sensitivity: f64,
        started: Instant,
        phase: Option<&'p mut dyn FnMut(&str)>,
        target: (f64, f64),
        mode: String,
    }
    impl KeyDriver for Travel<'_, '_, '_> {
        fn key_down(&mut self, key: Key) -> Result<()> {
            self.game.key_down(key)
        }
        fn key_up(&mut self, key: Key) -> Result<()> {
            self.game.key_up(key)
        }
    }
    impl super::super::travel::TravelDriver for Travel<'_, '_, '_> {
        fn observe(
            &mut self,
            expected: (f64, f64),
            pause: bool,
        ) -> Result<super::super::travel::Observation> {
            let began = Instant::now();
            let mut paused = pause
                || self
                    .game
                    .camera_calibration
                    .is_none_or(|(_, _, at)| at.elapsed() >= Duration::from_secs(15));
            if paused {
                self.game.key_up(Key::Layout('w'))?;
                self.game.key_up(Key::Shift)?;
            }
            let frame = self.game.frame()?;
            ensure!(
                super::super::vision::player_health_fraction(&frame).is_some_and(|hp| hp >= 0.45),
                "Health too low or unconfirmed for navigation"
            );
            ensure!(
                !matches!(
                    (self.game.selected_slot, super::super::vision::active_party_slot(&frame)),
                    (Some(expected), Some(active)) if active != expected
                ),
                "Active party member changed unexpectedly during navigation"
            );
            let mut position = self
                .mini
                .locate(&frame, expected, 20.0, Some(self.game.token));
            if position
                .as_ref()
                .map_or(true, |p| (p.0 - expected.0).hypot(p.1 - expected.1) > 24.0)
            {
                self.game.check()?;
                self.game.key_up(Key::Layout('w'))?;
                self.game.key_up(Key::Shift)?;
                paused = true;
                if let Some(update) = self.phase.as_mut() {
                    update("relocalizing");
                }
                // Match the captured world image while Paimon's menu pauses
                // combat. Expanded/global searches must not leave an idle
                // character exposed for the duration of asset loading.
                let recovery_frame = self.game.frame()?;
                self.game.pause_world()?;
                let recovered = self
                    .mini
                    .locate(&recovery_frame, expected, 60.0, Some(self.game.token))
                    .or_else(|_| {
                        self.mini
                            .locate(&recovery_frame, expected, 250.0, Some(self.game.token))
                    })
                    .or_else(|_| self.mini.relocalize(&recovery_frame, Some(self.game.token)))?;
                self.game.resume_world()?;
                self.game.wait(100)?;
                let confirmed = self.mini.locate(
                    &self.game.frame()?,
                    recovered,
                    12.0,
                    Some(self.game.token),
                )?;
                ensure!(
                    (confirmed.0 - recovered.0).hypot(confirmed.1 - recovered.1) < 3.0,
                    "Global mini-map recovery was not stable across fresh frames"
                );
                position = Ok(confirmed);
                if let Some(update) = self.phase.as_mut() {
                    update("moving");
                }
            }
            if began.elapsed() > Duration::from_millis(250) {
                self.game.key_up(Key::Layout('w'))?;
                paused = true;
            }
            let probes_before = self.game.camera_probes;
            let fast = self.game.camera_calibration.and_then(|(s, offset, at)| {
                if at.elapsed() >= Duration::from_secs(15) {
                    return None;
                }
                super::super::vision::camera_cone_heading(&frame)
                    .ok()
                    .map(|angle| (angle + offset, s))
            });
            let (heading, sensitivity) = match fast {
                Some(value) => value,
                None => camera_heading(self.game)?,
            };
            paused |= probes_before != self.game.camera_probes;
            self.sensitivity = sensitivity;
            let labels = format!(
                "{} {}",
                self.game.read(&frame, 1560, 1024, 100, 34)?,
                self.game.read(&frame, 1660, 1024, 100, 34)?
            );
            let motion = self.game.motion.observe(&frame, &labels);
            if began.elapsed() > Duration::from_millis(250) || paused {
                self.game.key_up(Key::Layout('w'))?;
                self.game.key_up(Key::Shift)?;
                paused = true;
            }
            let navigation = serde_json::json!({
                "elapsedMs":self.started.elapsed().as_millis(),"position":position.as_ref().ok(),
                "headingDegrees":heading.to_degrees(),"motion":format!("{motion:?}"),"paused":paused,
                "target":self.target,"mode":self.mode,
            });
            self.game.navigation_trace.push_back(navigation.clone());
            if self.game.navigation_trace.len() > 64 {
                self.game.navigation_trace.pop_front();
            }
            if cfg!(debug_assertions) {
                if let Ok(bytes) = serde_json::to_vec(&navigation) {
                    let _ = super::super::routes::write_cache(
                        std::path::Path::new("data/mining/last-navigation.json"),
                        &bytes,
                    );
                }
            }
            Ok(super::super::travel::Observation {
                position: position?,
                heading,
                motion,
                grounded: motion == Motion::Ground && self.game.motion.ground_skill_present(&frame),
                paused,
            })
        }
        fn turn(&mut self, radians: f64) -> Result<()> {
            self.game
                .rotate(radians.to_degrees() * self.sensitivity, 0.0)
        }
        fn wait(&mut self, ms: u64) -> Result<()> {
            self.game.wait(ms)
        }
        fn time_ms(&self) -> u64 {
            self.started.elapsed().as_millis() as u64
        }
    }
    fn move_to(
        game: &mut Game<'_>,
        mini_locator: &mut MiniMapLocator,
        current: (f64, f64),
        target: (f64, f64),
        mode: &str,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<(f64, f64)> {
        if !matches!(game.motion()?, Motion::Gliding | Motion::Climbing) {
            game.select_travel_role()?;
        }
        let mut update = |phase: &str| {
            report.phase = phase.into();
            progress(report);
        };
        move_to_current(game, mini_locator, current, target, mode, Some(&mut update))
    }
    fn move_to_current(
        game: &mut Game<'_>,
        mini_locator: &mut MiniMapLocator,
        current: (f64, f64),
        target: (f64, f64),
        mode: &str,
        phase: Option<&mut dyn FnMut(&str)>,
    ) -> Result<(f64, f64)> {
        game.navigation_trace.clear();
        let mut driver = Travel {
            game,
            mini: mini_locator,
            sensitivity: 1.0,
            started: Instant::now(),
            phase,
            target,
            mode: mode.into(),
        };
        super::super::travel::follow(&mut driver, current, target, mode)
            .with_context(|| format!("Target {target:?}; mode {mode}"))
    }
    fn recover_if_needed(
        game: &mut Game<'_>,
        atlas: &Atlas,
        mini: &mut MiniMapLocator,
        teleports: &TeleportCatalog,
        progress: &MiningProgress<'_>,
        report: &mut MiningReport,
    ) -> Result<()> {
        game.check()?;
        game.key_up(Key::Layout('w'))?;
        game.key_up(Key::Shift)?;
        game.ctrl.system_control.left_button(false)?;
        game.ctrl.system_control.middle_button(false)?;
        game.leave_aim();
        if !game.ctrl.return_to_main_ui(3) {
            let frame = game.frame()?;
            ensure!(
                game.read(&frame, 650, 395, 650, 100)?.contains("全部队员")
                    && game.read(&frame, 820, 950, 300, 75)?.contains("复苏"),
                "Recovery cannot enter the main world"
            );
            game.click(960.0, 985.0)?;
            game.wait(5000)?;
            ensure!(
                game.ctrl.return_to_main_ui(3),
                "Free revival did not return to the world"
            );
        }
        let frame = game.frame()?;
        let motion = game.motion()?;
        if super::super::vision::party_ready_for_next_route(&frame, motion == Motion::Ground) {
            report.recovery = Some(MiningRecovery {
                outcome: "ready".into(),
                destination: "current".into(),
                error: None,
            });
            progress(report);
            return Ok(());
        }
        report.phase = "recovering".into();
        progress(report);
        let statue = teleports.recovery_statue()?;
        mini.set_floor(Some(0));
        teleport(game, atlas, mini, statue.landing_position(), statue, false)?;
        report.map_floor = mini.last_floor;
        game.vehicle = false;
        game.wait(10000)?;
        // Statue healing can already be complete at the confirmed teleport
        // landing. Do not risk another localization/approach when all four
        // members can be verified restored here.
        if matches!(party_restored(game), Ok(true)) {
            report.recovery = Some(MiningRecovery {
                outcome: "healed".into(),
                destination: "StarfellLakeStatue".into(),
                error: None,
            });
            progress(report);
            return Ok(());
        }
        game.check()?;
        ensure!(
            game.ctrl.return_to_main_ui(3),
            "Recovery cannot resume the world"
        );
        let position = locate(game, mini, Some(statue.landing_position()), 30)?;
        move_to_current(game, mini, position, statue.map_position(), "walk", None)?;
        land(game, "", progress, report)?;
        report.phase = "recovering".into();
        progress(report);
        game.wait(10000)?;
        ensure!(
            party_restored(game)?,
            "Statue did not restore every party member"
        );
        report.recovery = Some(MiningRecovery {
            outcome: "healed".into(),
            destination: "StarfellLakeStatue".into(),
            error: None,
        });
        progress(report);
        Ok(())
    }

    fn party_restored(game: &mut Game<'_>) -> Result<bool> {
        for slot in 0..4 {
            game.select_slot(slot)?;
            if !super::super::vision::player_health_fraction(&game.frame()?)
                .is_some_and(|hp| hp >= 0.97)
            {
                return Ok(false);
            }
        }
        game.selected_role = None;
        Ok(true)
    }

    fn teleport(
        game: &mut Game<'_>,
        atlas: &Atlas,
        mini_locator: &mut MiniMapLocator,
        target: (f64, f64),
        anchor: &TeleportAnchor,
        force: bool,
    ) -> Result<()> {
        let result = teleport_inner(game, atlas, mini_locator, target, anchor, force);
        if result.is_err() && game.check().is_ok() {
            // Own menu recovery instead of requiring the next route/test to
            // close the failed map. return_to_main_ui verifies its destination.
            game.ctrl.return_to_main_ui(3);
        }
        result
    }

    fn teleport_inner(
        game: &mut Game<'_>,
        atlas: &Atlas,
        mini_locator: &mut MiniMapLocator,
        target: (f64, f64),
        anchor: &TeleportAnchor,
        force: bool,
    ) -> Result<()> {
        use super::super::big_map::Projection;
        game.teleport_diagnostic = serde_json::json!({
            "anchor": anchor.diagnostic(), "target": target, "force": force,
            "phase": "preparing", "hasSelectionFrame": false,
        });
        game.teleport_selection = None;
        game.camera_calibration = None;
        let map_target = anchor.map_position();
        ensure!(
            game.ctrl.return_to_main_ui(3),
            "Could not open the map from the main world"
        );
        for key in [Key::Layout('w'), Key::Shift, Key::Space] {
            game.ctrl.system_control.key_up(key)?;
        }
        if let Ok(position) = locate(game, mini_locator, Some(anchor.landing_position()), 20) {
            let landing = anchor.landing_position();
            if !force
                && (position.0 - landing.0).hypot(position.1 - landing.1) < 12.0
                && (position.0 - target.0).hypot(position.1 - target.1) < 16.0
            {
                game.teleport_diagnostic["phase"] = "alreadyNearAnchor".into();
                return Ok(());
            }
        }
        game.key(Key::Layout('m'))?;
        game.wait(1000)?;
        let map_frame = game.frame()?;
        ensure!(
            game.map_locator.is_open(&map_frame)?,
            "World map did not open"
        );
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
        game.set_map_zoom(570.0)?;
        // Require several terrain features to agree on a north-up transform.
        let mut pan_trace = Vec::new();
        let mut predicted: Option<Projection> = None;
        let mut before_pan: Option<Projection> = None;
        let mut recognition_failures = 0;
        let mut zoom_recoveries = 0;
        let mut click_retries = 0;
        let mut target_ground_checked = false;
        let mut selection_zoom_steps = 0;
        let map_started = Instant::now();
        for _ in 0..40 {
            ensure!(
                map_started.elapsed() < Duration::from_secs(180),
                "Map navigation exceeded 180 seconds"
            );
            let frame = game.frame()?;
            ensure!(
                game.map_locator.is_open(&frame)?,
                "World map closed during navigation"
            );
            if cfg!(debug_assertions) {
                frame.save("data/mining/last-map-before-match.png").ok();
            }
            let hint = predicted.map(|p| p.center).unwrap_or(map_target);
            let observation = game
                .map_locator
                .observe(&frame, atlas, hint, game.token)
                .and_then(|p| {
                    ensure!(
                        predicted.is_none_or(|old| p.confirms_pan(old)
                            && before_pan.is_none_or(|before| p.confirms_direction(before, old))),
                        "Map projection disagrees with the sent drag"
                    );
                    Ok(p)
                });
            let projection = match observation {
                Ok(p) => {
                    recognition_failures = 0;
                    p
                },
                Err(error) => {
                    game.check()?;
                    recognition_failures += 1;
                    // No click is permitted on a predicted-only location.
                    // Retry one fresh observation, then change zoom to expose
                    // terrain at a different scale and perform global recovery.
                    if recognition_failures < 2 {
                        game.wait(300)?;
                        continue;
                    }
                    ensure!(
                        zoom_recoveries < 4,
                        "Map localization failed after zoom recovery: {error:#}"
                    );
                    game.set_map_zoom([536.0, 570.0, 505.0, 590.0][zoom_recoveries])?;
                    zoom_recoveries += 1;
                    predicted = None;
                    recognition_failures = 0;
                    game.wait(400)?;
                    continue;
                },
            };
            let (x, y) = projection.center;
            let scale = projection.scale;
            let (screen_x, screen_y) = projection.screen(map_target);
            game.teleport_diagnostic["phase"] = "mapNavigation".into();
            game.teleport_diagnostic["projection"] = serde_json::json!({
                "center": [x,y], "scale": scale, "screen": [screen_x,screen_y],
            });
            pan_trace.push(serde_json::json!({"center":[x,y],"scale":scale,"target":map_target,"screen":[screen_x,screen_y]}));
            if cfg!(debug_assertions) {
                let _ = super::super::routes::write_cache(
                    std::path::Path::new("data/mining/last-map-projection.json"),
                    &serde_json::to_vec_pretty(&pan_trace)?,
                );
            }
            if Projection::clickable((screen_x, screen_y)) {
                // Layer controls can appear only after panning into a cave's
                // region. The region-menu check alone cannot establish the
                // layer of the target viewport. Re-observe after this click.
                if !target_ground_checked {
                    target_ground_checked = true;
                    if let Some((x, y)) = game.motion.ground_button(&frame) {
                        game.click(x, y)?;
                        game.wait(500)?;
                        predicted = None;
                        before_pan = None;
                        continue;
                    }
                }
                if anchor.selection_needs_zoom(scale) && selection_zoom_steps < 3 {
                    game.set_map_zoom([536.0, 505.0, 490.0][selection_zoom_steps])?;
                    selection_zoom_steps += 1;
                    predicted = None;
                    before_pan = None;
                    continue;
                }
                let point =
                    match game
                        .anchor_icons
                        .refine(&frame, anchor, (screen_x, screen_y), scale)
                    {
                        Ok(p) => p,
                        Err(error) => {
                            ensure!(
                                click_retries < 3,
                                "Teleport icon refinement failed: {error:#}"
                            );
                            click_retries += 1;
                            // Restore a larger icon before trying to select it.
                            game.click(48.0, 442.0)?;
                            game.wait(400)?;
                            predicted = None;
                            continue;
                        },
                    };
                game.teleport_diagnostic["iconClick"] = serde_json::json!(point);
                game.click(point.0, point.1)?;
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
                game.teleport_diagnostic["buttonText"] = label.clone().into();
                game.teleport_diagnostic["hasSelectionFrame"] = true.into();
                game.teleport_selection = Some(panel);
                ensure!(
                    label.contains("传送"),
                    "Teleport button absent; anchor may be locked or obscured"
                );
                game.click(1710.0, 1010.0)?;
                game.teleport_diagnostic["phase"] = "landing".into();
                game.wait(1500)?;
                for observation in 1..=60 {
                    game.teleport_diagnostic["landingObservations"] = observation.into();
                    match locate(game, mini_locator, Some(anchor.landing_position()), 50) {
                        Ok(landed) => {
                            let landing = anchor.landing_position();
                            game.teleport_diagnostic["lastLandingError"] = serde_json::Value::Null;
                            game.teleport_diagnostic["lastLanding"] = serde_json::json!({
                                "position": landed, "floor": mini_locator.last_floor,
                                "anchorDistance": (landed.0-landing.0).hypot(landed.1-landing.1),
                                "targetDistance": (landed.0-target.0).hypot(landed.1-target.1),
                            });
                            if (landed.0 - target.0).hypot(landed.1 - target.1) <= 40.0
                                && (landed.0 - landing.0).hypot(landed.1 - landing.1) <= 16.0
                            {
                                game.teleport_diagnostic["phase"] = "confirmed".into();
                                return Ok(());
                            }
                        },
                        Err(error) => {
                            // Retain only the latest error, not sixty copies of
                            // a potentially large layered-map diagnostic.
                            game.teleport_diagnostic["lastLandingError"] =
                                format!("{error:#}").into();
                            game.teleport_diagnostic["lastLanding"] = serde_json::Value::Null;
                        },
                    }
                    game.wait(500)?;
                }
                anyhow::bail!("Teleport landing could not be confirmed");
            }
            if (screen_x - 960.0).hypot(screen_y - 540.0) > 1600.0
                && scale < 0.38
                && zoom_recoveries < 4
            {
                game.click(48.0, 635.0)?;
                game.wait(400)?;
                predicted = None;
                zoom_recoveries += 1;
                continue;
            }
            let origin = game.ctrl.game_info.window.origin();
            let (drag_x, drag_y) = super::super::vision::map_drag_origin(&frame);
            let dx =
                ((drag_x + (960.0 - screen_x).clamp(-600.0, 600.0)).clamp(380.0, 1680.0)) - drag_x;
            let dy =
                ((drag_y + (540.0 - screen_y).clamp(-300.0, 300.0)).clamp(150.0, 900.0)) - drag_y;
            let sx = game.ctrl.scaler.scale_x(drag_x) as i32 + origin.x;
            let sy = game.ctrl.scaler.scale_y(drag_y) as i32 + origin.y;
            game.ctrl.system_control.mouse_drag(
                sx,
                sy,
                sx + game.ctrl.scaler.scale_x(dx) as i32,
                sy + game.ctrl.scaler.scale_y(dy) as i32,
            )?;
            before_pan = Some(projection);
            predicted = Some(projection.panned((
                game.ctrl.scaler.scale_x(dx) as i32 as f64 / game.ctrl.scaler.scale_x(1.0),
                game.ctrl.scaler.scale_y(dy) as i32 as f64 / game.ctrl.scaler.scale_y(1.0),
            )));
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
        let motion = game.motion()?;
        if motion == Motion::Climbing {
            game.key(Key::Layout('x'))?;
            game.wait(300)?;
        } else if motion == Motion::Gliding {
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
            if game.motion()? == Motion::Ground
                && game.motion.ground_skill_present(&frame)
                && super::super::vision::active_party_slot(&frame).is_some()
            {
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
        game.combat_active = true;
        let result = fight_inner(game, progress, report);
        game.combat_active = false;
        result
    }

    fn fight_inner(
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
            let frame = game.frame()?;
            ensure!(
                super::super::vision::player_health_fraction(&frame).is_some_and(|hp| hp >= 0.50),
                "Health too low or unconfirmed for automatic combat"
            );
            let enemy = super::super::vision::enemy_health_bar(&frame);
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
                ensure!(
                    super::super::vision::player_health_fraction(&game.frame()?)
                        .is_some_and(|hp| hp >= 0.50),
                    "Health too low or unconfirmed after shielding"
                );
                if game.selected_role != Some(Role::Mavuika) && game.motion()? == Motion::Ground {
                    // A short dash can leave hit-stun before switching from
                    // the shield character. The caller relocalizes after combat.
                    game.key(Key::Shift)?;
                    game.wait(150)?;
                }
                game.select_role(Role::Mavuika)?;
                if last_fire.is_none_or(|time| time.elapsed() >= Duration::from_secs(16)) {
                    game.press(GameKey::Skill)?;
                    last_fire = Some(Instant::now());
                }
                actions::execute_script(game, "attack(0.45)")?;
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
            let prompts = super::super::loot::pickup_prompts(
                &frame,
                |x, y, w, h| game.read(&frame, x, y, w, h),
                &game.motion,
            )?;
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
            let is_ore = super::super::loot::has_ore_pickup(&prompts);
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
            let before = report.loot.values().sum::<usize>();
            // Break/charge animations can delay the F prompt beyond the
            // initial pickup pass. Continue observing it while loot settles;
            // an ore label and its selected-row key are still both required.
            pickup_visible(game, progress, report)?;
            let frame = game.frame()?;
            if game.record_loot(&frame, report)? || report.loot.values().sum::<usize>() > before {
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
            let scripts: &[&str] = if game.underwater_route {
                &["w(0.2)", "s(0.4)", "w(0.2)", "a(0.2)", "d(0.4)", "a(0.2)"]
            } else {
                // Blasted ore can land beyond the short cardinal sweep.
                // Visit the four corners, then finish near the starting point.
                &[
                    "w(0.35)", "d(0.35)", "s(0.7)", "a(0.7)", "w(0.7)", "d(0.35)", "s(0.35)",
                ]
            };
            for &script in scripts {
                if Instant::now() >= deadline {
                    break;
                }
                actions::execute_script(game, script)?;
                pickup_visible(game, progress, report)?;
            }
        }
        poll_loot(game, report, progress, 3000)
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
        let initial_loot = report.loot.values().sum::<usize>();
        if game.underwater_route {
            game.press(GameKey::Skill)?;
            return finish_ground_mine(game, "armoredCrab", sweep, initial_loot, progress, report);
        }
        if matches!(strategy, MiningStrategy::Auto | MiningStrategy::Linnea) {
            return mine(game, detector, 1, 24, progress, report);
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
    fn delayed_menu_transition_waits_for_a_stable_world_hud() {
        // The D07 failure checked at 200ms, before the menu animation ended.
        // A transient HUD frame during that animation is not an acknowledgement.
        let states = [false, false, false, false, false, true, false, true, true];
        let mut samples = 0;
        confirm_world_state(true, || {
            let world = states[samples];
            samples += 1;
            Ok(world)
        })
        .unwrap();
        assert_eq!(samples, states.len());
    }
    #[test]
    fn menu_transition_timeout_and_cancellation_do_not_retry_input() {
        let mut samples = 0;
        assert!(confirm_world_state(true, || {
            samples += 1;
            Ok(false)
        })
        .is_err());
        assert_eq!(samples, 25);
        let mut samples = 0;
        let error = confirm_world_state(false, || {
            samples += 1;
            anyhow::bail!("Game window lost focus")
        })
        .unwrap_err();
        assert_eq!(samples, 1);
        assert!(error.to_string().contains("lost focus"));
    }
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
