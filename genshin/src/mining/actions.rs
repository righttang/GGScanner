//! Bounded, typed subset of the community route action language.
//! Parsing never executes text; the native game controller handles each action.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Linnea,
    Zhongli,
    Mavuika,
    Dehya,
}
impl Role {
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim() {
            "莉奈娅" | "莉奈婭" | "Linnea" => Ok(Self::Linnea),
            "钟离" | "鍾離" | "Zhongli" => Ok(Self::Zhongli),
            "玛薇卡" | "瑪薇卡" | "Mavuika" => Ok(Self::Mavuika),
            "迪希雅" | "Dehya" => Ok(Self::Dehya),
            other => bail!("Unsupported mining party character: {other}"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Linnea => "莉奈娅",
            Self::Zhongli => "钟离",
            Self::Mavuika => "玛薇卡",
            Self::Dehya => "迪希雅",
        }
    }
    pub fn matches(self, text: &str) -> bool {
        match self {
            Self::Linnea => text.contains("莉奈") || text.contains("莉奈婭"),
            Self::Zhongli => text.contains("钟离") || text.contains("鍾離"),
            Self::Mavuika => {
                text.contains("玛薇")
                    || text.contains("瑪薇")
                    || text.contains("蚂薇卡")
                    || text.contains("码薇卡")
            },
            Self::Dehya => text.contains("迪希雅"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GameKey {
    Forward,
    Back,
    Left,
    Right,
    Skill,
    Burst,
    Interact,
    Jump,
    Descend,
    Detach,
    Sprint,
    Slot(u8),
}
impl GameKey {
    pub fn parse(input: &str) -> Result<Self> {
        let upper = input.trim().trim_matches(['\'', '"']).to_ascii_uppercase();
        let key = upper.strip_prefix("VK_").unwrap_or(&upper);
        match key {
            "W" => Ok(Self::Forward),
            "S" => Ok(Self::Back),
            "A" => Ok(Self::Left),
            "D" => Ok(Self::Right),
            "E" => Ok(Self::Skill),
            "Q" => Ok(Self::Burst),
            "F" => Ok(Self::Interact),
            "J" | "SPACE" => Ok(Self::Jump),
            "CTRL" | "CONTROL" | "LCONTROL" => Ok(Self::Descend),
            "X" => Ok(Self::Detach),
            "SHIFT" | "LSHIFT" => Ok(Self::Sprint),
            "1" | "2" | "3" | "4" => Ok(Self::Slot(key.parse()?)),
            _ => bail!("Unsupported game key: {input}"),
        }
    }
    #[cfg(windows)]
    pub(super) fn native(self) -> enigo::Key {
        match self {
            Self::Forward => enigo::Key::Layout('w'),
            Self::Back => enigo::Key::Layout('s'),
            Self::Left => enigo::Key::Layout('a'),
            Self::Right => enigo::Key::Layout('d'),
            Self::Skill => enigo::Key::Layout('e'),
            Self::Burst => enigo::Key::Layout('q'),
            Self::Interact => enigo::Key::Layout('f'),
            Self::Jump => enigo::Key::Space,
            Self::Descend => enigo::Key::Control,
            Self::Detach => enigo::Key::Layout('x'),
            Self::Sprint => enigo::Key::Shift,
            Self::Slot(slot) => enigo::Key::Layout(char::from(b'0' + slot)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Button {
    Left,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    Select { role: Role },
    Wait { milliseconds: u64 },
    MoveCamera { x: i32, y: i32 },
    Press { key: GameKey },
    Down { key: GameKey },
    Up { key: GameKey },
    Hold { key: GameKey, milliseconds: u64 },
    Attack { milliseconds: u64 },
    Charge { milliseconds: u64 },
    Click { button: Button },
}
impl Action {
    fn duration(&self) -> u64 {
        match self {
            Self::Wait { milliseconds }
            | Self::Hold { milliseconds, .. }
            | Self::Attack { milliseconds }
            | Self::Charge { milliseconds } => *milliseconds,
            _ => 0,
        }
    }
}

fn milliseconds(input: &str, default: u64) -> Result<u64> {
    if input.is_empty() {
        return Ok(default);
    }
    let seconds: f64 = input.parse().context("Expected seconds")?;
    ensure!(
        seconds.is_finite() && (0.0..=30.0).contains(&seconds),
        "Action duration must be 0–30 seconds"
    );
    Ok((seconds * 1000.0).round() as u64)
}

fn tokens(script: &str) -> Result<Vec<&str>> {
    ensure!(script.len() <= 16384, "Action script exceeds 16 KiB");
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    for (index, c) in script.char_indices() {
        match c {
            '(' => {
                depth += 1;
                ensure!(depth <= 1, "Nested action expressions are unsupported");
            },
            ')' => {
                depth -= 1;
                ensure!(depth >= 0, "Unmatched closing parenthesis");
            },
            ',' | ';' | '\n' | '\r' if depth == 0 => {
                let token = script[start..index].trim();
                if !token.is_empty() {
                    result.push(token);
                }
                start = index + c.len_utf8();
            },
            _ => {},
        }
    }
    ensure!(depth == 0, "Unclosed action parenthesis");
    let last = script[start..].trim();
    if !last.is_empty() {
        result.push(last);
    }
    ensure!(
        !result.is_empty() && result.len() <= 128,
        "Script requires 1–128 commands"
    );
    Ok(result)
}

pub fn parse_script(script: &str) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    for token in tokens(script)? {
        let mut command = token;
        if let Some((prefix, rest)) = token.split_once(char::is_whitespace) {
            if !prefix.contains('(') && !rest.trim_start().starts_with('(') {
                actions.push(Action::Select {
                    role: Role::parse(prefix)?,
                });
                command = rest.trim();
            }
        }
        let (name, args) = match command.split_once('(') {
            Some((name, tail)) => (
                name.trim().to_ascii_lowercase(),
                tail.strip_suffix(')')
                    .context("Invalid action call")?
                    .trim(),
            ),
            None => (command.to_ascii_lowercase(), ""),
        };
        let action = match name.as_str() {
            "wait" => Action::Wait {
                milliseconds: milliseconds(args, 0)?,
            },
            "moveby" => {
                let (x, y) = args.split_once(',').context("moveby needs x,y")?;
                let x: i32 = x.trim().parse()?;
                let y: i32 = y.trim().parse()?;
                ensure!(
                    x.unsigned_abs() <= 12000 && y.unsigned_abs() <= 12000,
                    "Camera movement exceeds 12000 pixels"
                );
                Action::MoveCamera { x, y }
            },
            "keypress" => Action::Press {
                key: GameKey::parse(args)?,
            },
            "keydown" => Action::Down {
                key: GameKey::parse(args)?,
            },
            "keyup" => Action::Up {
                key: GameKey::parse(args)?,
            },
            "click" => Action::Click {
                button: match args.to_ascii_lowercase().as_str() {
                    "left" => Button::Left,
                    "middle" => Button::Middle,
                    _ => bail!("Unsupported mouse button: {args}"),
                },
            },
            "e" | "q" => {
                let key = if name == "e" {
                    GameKey::Skill
                } else {
                    GameKey::Burst
                };
                if args.is_empty() {
                    Action::Press { key }
                } else {
                    Action::Hold {
                        key,
                        milliseconds: if args == "hold" {
                            1000
                        } else {
                            milliseconds(args, 1000)?
                        },
                    }
                }
            },
            "w" | "a" | "s" | "d" => Action::Hold {
                key: GameKey::parse(&name)?,
                milliseconds: milliseconds(args, 200)?,
            },
            "dash" => Action::Hold {
                key: GameKey::Sprint,
                milliseconds: milliseconds(args, 150)?,
            },
            "j" | "jump" => {
                ensure!(args.is_empty(), "Jump takes no arguments");
                Action::Press { key: GameKey::Jump }
            },
            "attack" => Action::Attack {
                milliseconds: milliseconds(args, 200)?,
            },
            "charge" => Action::Charge {
                milliseconds: milliseconds(args, 1000)?,
            },
            _ => bail!("Unsupported route command: {name}"),
        };
        actions.push(action);
    }
    ensure!(
        actions.len() <= 256 && actions.iter().map(Action::duration).sum::<u64>() <= 120000,
        "Action script exceeds 256 actions or 120 seconds"
    );
    Ok(actions)
}

/// Implemented by the production game controller, and by recording drivers in
/// tests. Cancellation/focus checks belong in driver waits and input presses.
pub trait ActionDriver {
    /// Native backends can verify a skill transition after its inputs release.
    fn after_action(&mut self, _action: &Action) -> Result<()> {
        Ok(())
    }
    fn select(&mut self, role: Role) -> Result<()>;
    fn wait(&mut self, milliseconds: u64) -> Result<()>;
    fn move_camera(&mut self, x: i32, y: i32) -> Result<()>;
    fn press(&mut self, key: GameKey) -> Result<()>;
    fn key_state(&mut self, key: GameKey, down: bool) -> Result<()>;
    fn button_state(&mut self, button: Button, down: bool) -> Result<()>;
}

pub fn mining_role(actions: &[Action]) -> Option<Role> {
    let mut role = None;
    for action in actions {
        match action {
            Action::Select { role: selected } => role = Some(*selected),
            Action::Hold {
                key: GameKey::Skill,
                ..
            } if role == Some(Role::Zhongli) => return role,
            _ => {},
        }
    }
    None
}

struct HeldInputs<'a, D: ActionDriver> {
    driver: &'a mut D,
    keys: std::collections::BTreeSet<GameKey>,
    buttons: Vec<Button>,
}
impl<D: ActionDriver> HeldInputs<'_, D> {
    fn down(&mut self, key: GameKey) -> Result<bool> {
        let fresh = self.keys.insert(key);
        if fresh {
            self.driver.key_state(key, true)?;
        }
        Ok(fresh)
    }
    fn up(&mut self, key: GameKey) -> Result<()> {
        self.driver.key_state(key, false)?;
        self.keys.remove(&key);
        Ok(())
    }
    fn button(&mut self, button: Button, down: bool) -> Result<()> {
        if down && !self.buttons.contains(&button) {
            self.buttons.push(button);
        }
        self.driver.button_state(button, down)?;
        if !down {
            self.buttons.retain(|&held| held != button);
        }
        Ok(())
    }
    fn click(&mut self, button: Button) -> Result<()> {
        self.button(button, true)?;
        self.driver.wait(20)?;
        self.button(button, false)
    }
    fn release(&mut self) -> Result<()> {
        let mut error = None;
        for key in self.keys.clone() {
            if let Err(e) = self.up(key) {
                error = Some(e);
            }
        }
        for button in self.buttons.clone() {
            if let Err(e) = self.button(button, false) {
                error = Some(e);
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
impl<D: ActionDriver> Drop for HeldInputs<'_, D> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub fn execute_script(driver: &mut impl ActionDriver, script: &str) -> Result<()> {
    // Validate the complete script before the first input event.
    let actions = parse_script(script)?;
    let mut held = HeldInputs {
        driver,
        keys: Default::default(),
        buttons: Vec::new(),
    };
    let result = (|| {
        for action in actions {
            let observed_action = action.clone();
            match action {
                Action::Select { role } => held.driver.select(role)?,
                Action::Wait { milliseconds } => held.driver.wait(milliseconds)?,
                Action::MoveCamera { x, y } => held.driver.move_camera(x, y)?,
                Action::Press { key } => held.driver.press(key)?,
                Action::Down { key } => {
                    held.down(key)?;
                },
                Action::Up { key } => held.up(key)?,
                Action::Hold { key, milliseconds } => {
                    let fresh = held.down(key)?;
                    held.driver.wait(milliseconds)?;
                    if fresh {
                        held.up(key)?;
                    }
                },
                Action::Click { button } => held.click(button)?,
                Action::Charge { milliseconds } => {
                    held.button(Button::Left, true)?;
                    held.driver.wait(milliseconds)?;
                    held.button(Button::Left, false)?;
                },
                Action::Attack { milliseconds } => {
                    let mut remaining = milliseconds.max(20);
                    while remaining > 0 {
                        held.click(Button::Left)?;
                        let pause = remaining.saturating_sub(20).min(180);
                        held.driver.wait(pause)?;
                        remaining = remaining.saturating_sub(20 + pause);
                    }
                },
            }
            held.driver.after_action(&observed_action)?;
        }
        Ok(())
    })();
    let release = held.release();
    result.and(release)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mavuika_sidebar_accepts_observed_ocr_variants_without_suffix_matching() {
        for text in ["玛薇卡", "瑪薇卡", "蚂薇卡", "//码薇卡、"] {
            assert!(Role::Mavuika.matches(text), "{text}");
            assert!(!Role::Linnea.matches(text));
            assert!(!Role::Zhongli.matches(text));
            assert!(!Role::Dehya.matches(text));
        }
        for text in ["薇卡", "蚂薇", "码薇", "莉奈娅", "钟离", "迪希雅"] {
            assert!(!Role::Mavuika.matches(text), "{text}");
        }
    }
    #[test]
    fn parses_ground_mining_and_camera_commands_without_splitting_arguments() {
        assert_eq!(
            parse_script("钟离 d(0.2),e(hold)").unwrap(),
            vec![
                Action::Select {
                    role: Role::Zhongli
                },
                Action::Hold {
                    key: GameKey::Right,
                    milliseconds: 200
                },
                Action::Hold {
                    key: GameKey::Skill,
                    milliseconds: 1000
                },
            ]
        );
        assert_eq!(
            parse_script("wait(0.2),moveby(0,-1000)").unwrap(),
            vec![
                Action::Wait { milliseconds: 200 },
                Action::MoveCamera { x: 0, y: -1000 },
            ]
        );
    }
    #[test]
    fn parses_real_underwater_and_vehicle_sequences() {
        let water = parse_script(
            "keydown(VK_LCONTROL);wait(4);keyup(VK_LCONTROL);click(middle);wait(0.5);moveby(0,571)",
        )
        .unwrap();
        assert_eq!(
            water[0],
            Action::Down {
                key: GameKey::Descend
            }
        );
        assert_eq!(
            water[2],
            Action::Up {
                key: GameKey::Descend
            }
        );
        let vehicle = parse_script("玛薇卡 w(0.1),e(hold),keydown(VK_W),dash(3),keyup(VK_W),wait(0.1),e,keypress(1),keypress(2),keypress(3),keypress(4)").unwrap();
        assert_eq!(
            vehicle[0],
            Action::Select {
                role: Role::Mavuika
            }
        );
        assert!(vehicle.contains(&Action::Hold {
            key: GameKey::Sprint,
            milliseconds: 3000
        }));
    }
    #[test]
    fn rejects_unbounded_or_non_game_operations_before_execution() {
        for script in [
            "wait(NaN)",
            "wait(-1)",
            "attack(31)",
            "moveby(0,99999)",
            "moveby(-2147483648,0)",
            "keypress(VK_LWIN)",
            "click(right)",
            "run(cmd)",
            "wait(1",
            "wait(1))",
            "eval(wait(1))",
            "陌生角色 e",
        ] {
            assert!(parse_script(script).is_err(), "{script}");
        }
        assert!(parse_script(&vec!["wait(30)"; 5].join(",")).is_err());
    }
    #[derive(Default)]
    struct Recording {
        keys: std::collections::BTreeSet<GameKey>,
        left: bool,
        middle: bool,
        elapsed: u64,
        cancel_after: Option<u64>,
        panic: bool,
        inputs: usize,
    }
    impl ActionDriver for Recording {
        fn select(&mut self, _: Role) -> Result<()> {
            self.inputs += 1;
            Ok(())
        }
        fn wait(&mut self, milliseconds: u64) -> Result<()> {
            self.elapsed += milliseconds;
            assert!(!self.panic, "simulated driver panic");
            ensure!(
                self.cancel_after.is_none_or(|n| self.elapsed < n),
                "cancelled"
            );
            Ok(())
        }
        fn move_camera(&mut self, _: i32, _: i32) -> Result<()> {
            self.inputs += 1;
            Ok(())
        }
        fn press(&mut self, _: GameKey) -> Result<()> {
            self.inputs += 1;
            Ok(())
        }
        fn key_state(&mut self, key: GameKey, down: bool) -> Result<()> {
            self.inputs += 1;
            if down {
                self.keys.insert(key);
            } else {
                self.keys.remove(&key);
            }
            Ok(())
        }
        fn button_state(&mut self, button: Button, down: bool) -> Result<()> {
            self.inputs += 1;
            match button {
                Button::Left => self.left = down,
                Button::Middle => self.middle = down,
            }
            Ok(())
        }
    }
    #[test]
    fn cancellation_and_panics_release_simultaneous_keys_and_mouse_buttons() {
        for (script, milliseconds) in [
            ("keydown(VK_W),dash(3),keyup(VK_W)", 100),
            ("keydown(VK_LCONTROL),wait(4),keyup(VK_LCONTROL)", 100),
            ("charge(2)", 100),
            ("click(middle)", 10),
        ] {
            for panic in [false, true] {
                let mut driver = Recording {
                    cancel_after: Some(milliseconds),
                    panic,
                    ..Default::default()
                };
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute_script(&mut driver, script)
                }));
                assert!(outcome.is_err() || outcome.unwrap().is_err());
                assert!(
                    driver.keys.is_empty() && !driver.left && !driver.middle,
                    "{script}"
                );
            }
        }
    }
    #[test]
    fn invalid_tail_causes_no_partial_execution_and_success_releases_unpaired_down() {
        let mut driver = Recording::default();
        assert!(execute_script(&mut driver, "钟离 e(hold),execute(system)").is_err());
        assert_eq!(driver.inputs, 0);
        execute_script(&mut driver, "keydown(W),wait(0.1)").unwrap();
        assert!(driver.keys.is_empty());
        assert_eq!(driver.elapsed, 100);
    }
}
