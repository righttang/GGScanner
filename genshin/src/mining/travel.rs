//! Closed-loop waypoint execution. All input goes through the same driver in
//! live play and replay tests; the driver owns capture and geometric validation.
use super::motion::Motion;
use anyhow::{ensure, Result};
use enigo::Key;
use yas::system_control::key_hold::{with_held_key_control, with_key_cleanup, KeyDriver};

pub(super) struct Observation {
    pub position: (f64, f64),
    pub heading: f64,
    pub motion: Motion,
    pub grounded: bool,
    pub paused: bool,
}
pub(super) trait TravelDriver: KeyDriver {
    fn observe(&mut self, expected: (f64, f64), pause: bool) -> Result<Observation>;
    fn turn(&mut self, radians: f64) -> Result<()>;
    fn wait(&mut self, ms: u64) -> Result<()>;
    fn time_ms(&self) -> u64;
}
pub(super) fn angle_difference(target: f64, current: f64) -> f64 {
    (target - current + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
        - std::f64::consts::PI
}
fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
fn pulse(d: &mut impl TravelDriver, key: Key, ms: u64) -> Result<()> {
    with_held_key_control(d, key, |d| d.wait(ms))
}
fn stop(d: &mut impl TravelDriver) -> Result<()> {
    let forward = d.key_up(Key::Layout('w'));
    let sprint = d.key_up(Key::Shift);
    forward.and(sprint)
}
fn escape(d: &mut impl TravelDriver, attempt: usize, motion: Motion) -> Result<()> {
    stop(d)?;
    if motion == Motion::Climbing {
        pulse(d, Key::Layout('x'), 60)?;
        d.wait(150)?;
    }
    pulse(d, Key::Layout('s'), 500 + attempt as u64 * 100)?;
    if motion == Motion::Underwater {
        pulse(
            d,
            if attempt % 2 == 0 {
                Key::Space
            } else {
                Key::Control
            },
            650,
        )?;
    }
    let side = if attempt % 2 == 0 { 'a' } else { 'd' };
    with_held_key_control(d, Key::Layout(side), |d| {
        if motion == Motion::Ground {
            pulse(d, Key::Space, 80)?;
        }
        d.wait(650 + attempt as u64 * 150)
    })?;
    // Briefly skirt the obstacle before returning to the actual target.
    d.turn(if attempt % 2 == 0 { -0.6 } else { 0.6 })?;
    pulse(d, Key::Layout('w'), 500)?;
    d.wait(100)
}

pub(super) fn follow(
    d: &mut impl TravelDriver,
    current: (f64, f64),
    target: (f64, f64),
    mode: &str,
) -> Result<(f64, f64)> {
    with_key_cleanup(d, &[Key::Layout('w'), Key::Shift], |d| {
        let initial = d.observe(current, true)?;
        if distance(initial.position, target) < 2.0 && initial.motion != Motion::Climbing {
            return Ok(initial.position);
        }
        follow_inner(d, initial, target, mode)
    })
}

fn follow_inner(
    d: &mut impl TravelDriver,
    mut observation: Observation,
    target: (f64, f64),
    mode: &str,
) -> Result<(f64, f64)> {
    let started = d.time_ms();
    let mut current = observation.position;
    let mut progress = super::runtime::MovementProgress::new(current, target);
    let mut progress_at = started;
    let mut turn_since = None;
    let mut turn_frames = 0;
    let mut forward = false;
    let mut sprint = false;
    let mut escapes = 0;
    let mut takeoffs = 0;
    let mut flying = mode == "fly" && observation.motion == Motion::Gliding;
    let mut flight_finished = false;
    let mut jump_at = 0;
    loop {
        let now = d.time_ms();
        ensure!(
            now - started < 240_000,
            "Waypoint timed out after 240 seconds"
        );
        current = observation.position;
        if observation.paused {
            forward = false;
            sprint = false;
        }
        let remaining = distance(current, target);
        if remaining < 2.0 && (mode != "climb" || observation.motion != Motion::Climbing) {
            ensure!(
                mode != "fly" || flying || flight_finished,
                "Reached glide waypoint without confirming gliding"
            );
            stop(d)?;
            return Ok(current);
        }
        if mode == "fly" {
            flying |= observation.motion == Motion::Gliding;
            if flying
                && observation.motion == Motion::Ground
                && observation.grounded
                && remaining < 8.0
            {
                flight_finished = true;
            }
            ensure!(
                !flying || flight_finished || observation.motion == Motion::Gliding,
                "Gliding ended before the waypoint"
            );
        }
        // Turning in place must not bypass the no-progress watchdog. A noisy
        // close-range heading used to stay in that branch for all 240 seconds.
        let unintended_climb =
            observation.motion == Motion::Climbing && !matches!(mode, "climb" | "fly");
        let stuck = if now - progress_at >= 1000 {
            progress_at = now;
            mode != "climb" && progress.observe(current).is_err()
        } else {
            false
        };
        if stuck || unintended_climb {
            ensure!(
                escapes < 3,
                "Waypoint remains blocked after three escape attempts"
            );
            escape(d, escapes, observation.motion)?;
            escapes += 1;
            forward = false;
            sprint = false;
            observation = d.observe(current, true)?;
            progress = super::runtime::MovementProgress::new(observation.position, target);
            progress_at = d.time_ms();
            continue;
        }
        let desired = (current.1 - target.1).atan2(current.0 - target.0);
        let error = angle_difference(desired, observation.heading);
        // A roughly 1.5-unit localization shift can change the apparent target
        // bearing by tens of degrees at close range. Keep the 2-unit arrival
        // check; avoid chasing a more precise bearing than that position allows.
        let heading_tolerance = if remaining < 6.0 {
            (1.5 / remaining.max(2.0))
                .atan()
                .clamp(5.0_f64.to_radians(), 35.0_f64.to_radians())
        } else {
            5.0_f64.to_radians()
        };
        let mut stationary_turn = false;
        if error.abs() > heading_tolerance {
            turn_frames += 1;
            let since = *turn_since.get_or_insert(now);
            stationary_turn = remaining < 6.0
                || error.abs() > 30.0_f64.to_radians()
                || (now - since > 2000 && turn_frames >= 3);
            if stationary_turn {
                stop(d)?;
                forward = false;
                sprint = false;
            }
            d.turn(error.clamp(-1.2, 1.2))?;
        } else {
            turn_since = None;
            turn_frames = 0;
        }
        if stationary_turn {
            d.wait(50)?;
            observation = d.observe(current, true)?;
            continue;
        }
        if mode == "fly" && !flying && !flight_finished {
            ensure!(takeoffs < 24, "Glider takeoff could not be confirmed");
            takeoffs += 1;
            // Keep moving toward the ledge. Two controlled presses within a
            // jump avoid spending the whole airborne window processing frames.
            if !forward {
                d.key_down(Key::Layout('w'))?;
                forward = true;
            }
            pulse(d, Key::Space, 60)?;
            d.wait(200)?;
            observation = d.observe(current, false)?;
            continue;
        }
        if mode == "jump" && now - jump_at >= 1000 {
            pulse(d, Key::Space, 60)?;
            jump_at = now;
        }
        if remaining < 6.0 {
            stop(d)?;
            forward = false;
            sprint = false;
            pulse(d, Key::Layout('w'), if remaining < 3.5 { 60 } else { 100 })?;
            d.wait(40)?;
        } else {
            if !forward {
                d.key_down(Key::Layout('w'))?;
                forward = true;
            }
            let want_sprint = matches!(mode, "run" | "dash" | "swim") && remaining > 20.0;
            if want_sprint != sprint {
                if want_sprint {
                    d.key_down(Key::Shift)?;
                } else {
                    d.key_up(Key::Shift)?;
                }
                sprint = want_sprint;
            }
            d.wait(100)?;
        }
        observation = d.observe(current, remaining < 6.0)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Replay {
        p: (f64, f64),
        heading: f64,
        now: u64,
        held: Vec<Key>,
        events: Vec<Key>,
        mode: Motion,
        blocked: bool,
        permanent: bool,
        cancel_at: u64,
        fail: bool,
        direction_coupled_jitter: bool,
        ignore_turns: bool,
    }
    impl KeyDriver for Replay {
        fn key_down(&mut self, k: Key) -> Result<()> {
            self.events.push(k);
            if !self.held.contains(&k) {
                self.held.push(k);
            }
            if k == Key::Layout('a') || k == Key::Layout('d') {
                self.blocked = self.permanent;
            }
            if k == Key::Space && self.mode == Motion::Ground {
                self.mode = Motion::Gliding;
            }
            Ok(())
        }
        fn key_up(&mut self, k: Key) -> Result<()> {
            self.held.retain(|v| *v != k);
            Ok(())
        }
    }
    impl TravelDriver for Replay {
        fn observe(&mut self, _: (f64, f64), pause: bool) -> Result<Observation> {
            if pause {
                self.key_up(Key::Layout('w'))?;
            }
            ensure!(!self.fail, "capture failed");
            let position = if self.direction_coupled_jitter {
                // The actual A03 trace alternated these +/- 0.5 offsets as
                // the viewing cone rotated across the mini-map terrain.
                let offset = if self.heading.rem_euclid(std::f64::consts::TAU).to_degrees() > 147.0
                {
                    0.5
                } else {
                    -0.5
                };
                (self.p.0 + offset, self.p.1 + offset)
            } else {
                self.p
            };
            Ok(Observation {
                position,
                heading: self.heading,
                motion: self.mode,
                grounded: self.mode == Motion::Ground,
                paused: pause,
            })
        }
        fn turn(&mut self, r: f64) -> Result<()> {
            if !self.ignore_turns {
                self.heading += r;
            }
            self.wait(30)
        }
        fn wait(&mut self, ms: u64) -> Result<()> {
            self.now += ms;
            ensure!(self.now < self.cancel_at, "cancelled");
            let speed = ms as f64 / 1000.0 * 6.0;
            if !self.blocked && self.held.contains(&Key::Layout('w')) {
                self.p.0 -= speed * self.heading.cos();
                self.p.1 -= speed * self.heading.sin();
            }
            Ok(())
        }
        fn time_ms(&self) -> u64 {
            self.now
        }
    }
    fn replay(mode: Motion) -> Replay {
        Replay {
            p: (40.0, 0.0),
            heading: std::f64::consts::PI,
            now: 0,
            held: vec![],
            events: vec![],
            mode,
            blocked: false,
            permanent: false,
            cancel_at: u64::MAX,
            fail: false,
            direction_coupled_jitter: false,
            ignore_turns: false,
        }
    }
    #[test]
    fn regression_near_waypoint_jitter_does_not_spin_in_place() {
        let mut d = replay(Motion::Underwater);
        d.p = (4358.0, 3951.0);
        d.heading = 138.0_f64.to_radians();
        d.direction_coupled_jitter = true;
        let target = (4361.1631, 3948.9119);
        let start = d.p;
        let position = follow(&mut d, start, target, "walk").unwrap();
        assert!(distance(position, target) < 2.0);
        assert!(distance(d.p, target) < 3.0);
        assert!(d.now < 15_000);
        assert!(!d.events.contains(&Key::Layout('s')));
        assert!(d.held.is_empty());
    }
    #[test]
    fn regression_stationary_turns_still_trigger_bounded_escape() {
        let mut d = replay(Motion::Underwater);
        d.heading = std::f64::consts::FRAC_PI_2;
        d.ignore_turns = true;
        d.blocked = true;
        d.permanent = true;
        let result = follow(&mut d, (40.0, 0.0), (0.0, 0.0), "walk");
        assert!(result.is_err());
        assert!(d.events.contains(&Key::Layout('s')));
        assert!(d.events.contains(&Key::Layout('a')));
        assert!(d.events.contains(&Key::Layout('d')));
        assert!(d.now < 60_000);
        assert!(d.held.is_empty());
    }
    #[test]
    fn follows_ground_glide_and_underwater_with_turns_and_fine_approach() {
        for (mode, motion) in [
            ("walk", Motion::Ground),
            ("run", Motion::Ground),
            ("fly", Motion::Ground),
            ("swim", Motion::Underwater),
        ] {
            let mut d = replay(motion);
            let p = follow(&mut d, (40.0, 0.0), (0.0, 0.0), mode).unwrap();
            assert!(distance(p, (0.0, 0.0)) < 2.0);
            assert!(d.held.is_empty());
            assert!(d.now < 30_000);
        }
    }
    #[test]
    fn escapes_sideways_and_bounds_permanent_obstacles() {
        for permanent in [false, true] {
            let mut d = replay(Motion::Underwater);
            d.blocked = true;
            d.permanent = permanent;
            let r = follow(&mut d, (40.0, 0.0), (0.0, 0.0), "swim");
            assert_eq!(r.is_err(), permanent);
            assert!(d.events.contains(&Key::Layout('s')) && d.events.contains(&Key::Layout('a')));
            if permanent {
                assert!(d.events.contains(&Key::Layout('d')));
            }
            assert!(d.held.is_empty());
        }
    }
    #[test]
    fn cancellation_and_capture_failure_release_forward_and_sprint() {
        for capture_failure in [false, true] {
            let mut d = replay(Motion::Ground);
            d.cancel_at = 700;
            d.fail = capture_failure;
            assert!(follow(&mut d, (40.0, 0.0), (0.0, 0.0), "run").is_err());
            assert!(d.held.is_empty());
        }
    }
}
