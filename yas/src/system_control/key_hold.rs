//! Paired key events: release on success, error, failed press, and unwinding.
use anyhow::Result;
use enigo::Key;

pub trait KeyDriver {
    fn key_down(&mut self, key: Key) -> Result<()>;
    fn key_up(&mut self, key: Key) -> Result<()>;
}

/// A movement loop may stop and resume several keys. Arrange cleanup without
/// issuing an initial press (pressing Shift merely to create a guard can dash).
pub fn with_key_cleanup<D: KeyDriver, T>(
    driver: &mut D,
    keys: &[Key],
    run: impl FnOnce(&mut D) -> Result<T>,
) -> Result<T> {
    struct Cleanup<'a, D: KeyDriver> {
        driver: &'a mut D,
        keys: &'a [Key],
        released: bool,
    }
    impl<D: KeyDriver> Drop for Cleanup<'_, D> {
        fn drop(&mut self) {
            if !self.released {
                for &key in self.keys {
                    let _ = self.driver.key_up(key);
                }
            }
        }
    }
    let mut guard = Cleanup {
        driver,
        keys,
        released: false,
    };
    let result = run(guard.driver);
    let mut release = Ok(());
    for &key in guard.keys {
        let r = guard.driver.key_up(key);
        if release.is_ok() {
            release = r;
        }
    }
    guard.released = release.is_ok();
    result.and_then(|value| release.map(|_| value))
}

pub fn with_held_key(
    driver: &mut impl KeyDriver,
    key: Key,
    run: impl FnOnce() -> Result<()>,
) -> Result<()> {
    with_held_key_control(driver, key, |_| run())
}

/// Also permits nested keys while retaining modifier release on every exit.
pub fn with_held_key_control<D: KeyDriver>(
    driver: &mut D,
    key: Key,
    run: impl FnOnce(&mut D) -> Result<()>,
) -> Result<()> {
    struct Release<'a, D: KeyDriver> {
        driver: &'a mut D,
        key: Key,
        released: bool,
    }
    impl<D: KeyDriver> Drop for Release<'_, D> {
        fn drop(&mut self) {
            if !self.released {
                let _ = self.driver.key_up(self.key);
            }
        }
    }
    let mut guard = Release {
        driver,
        key,
        released: false,
    };
    guard.driver.key_down(key)?;
    let result = run(guard.driver);
    let release = guard.driver.key_up(key);
    guard.released = release.is_ok();
    result.and(release)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Driver {
        held: bool,
        fail_press: bool,
        releases: usize,
    }
    impl KeyDriver for Driver {
        fn key_down(&mut self, _: Key) -> Result<()> {
            self.held = true;
            anyhow::ensure!(!self.fail_press, "partial press failed");
            Ok(())
        }
        fn key_up(&mut self, _: Key) -> Result<()> {
            self.held = false;
            self.releases += 1;
            Ok(())
        }
    }
    #[test]
    fn cleanup_guard_does_not_press_keys_and_releases_after_unwinding() {
        let mut driver = Driver::default();
        let value = with_key_cleanup(&mut driver, &[Key::Layout('w'), Key::Shift], |driver| {
            assert!(!driver.held);
            driver.key_down(Key::Layout('w'))?;
            Ok(42)
        })
        .unwrap();
        assert_eq!(value, 42);
        assert!(!driver.held);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = with_key_cleanup(&mut driver, &[Key::Layout('w')], |driver| {
                driver.key_down(Key::Layout('w'))?;
                panic!("movement unwound");
            });
        }))
        .is_err());
        assert!(!driver.held);
    }
    #[test]
    fn held_key_releases_after_success_error_and_partial_press() {
        for (fail_press, fail_run) in [(false, false), (false, true), (true, false)] {
            let mut driver = Driver {
                fail_press,
                ..Default::default()
            };
            let result = with_held_key(&mut driver, Key::Layout('w'), || {
                anyhow::ensure!(!fail_run, "cancelled or focus lost");
                Ok(())
            });
            assert_eq!(result.is_err(), fail_press || fail_run);
            assert!(!driver.held);
            assert_eq!(driver.releases, 1);
        }
    }
    #[test]
    fn held_key_releases_during_unwinding() {
        let mut driver = Driver::default();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = with_held_key(&mut driver, Key::Layout('w'), || {
                panic!("input worker panicked")
            });
        }))
        .is_err());
        assert!(!driver.held);
        assert_eq!(driver.releases, 1);
    }
    #[test]
    fn nested_chord_releases_both_keys_on_error_and_panic() {
        for panic in [false, true] {
            let mut driver = Driver::default();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_held_key_control(&mut driver, Key::Alt, |control| {
                    with_held_key(control, Key::Return, || {
                        if panic {
                            panic!("input failed");
                        }
                        anyhow::bail!("focus lost");
                    })
                })
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert!(!driver.held);
            assert_eq!(driver.releases, 2);
        }
    }
}
