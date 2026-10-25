//! Paired key events: release on success, error, failed press, and unwinding.
use anyhow::Result;
use enigo::Key;

pub trait KeyDriver {
    fn key_down(&mut self, key: Key) -> Result<()>;
    fn key_up(&mut self, key: Key) -> Result<()>;
}

pub fn with_held_key(
    driver: &mut impl KeyDriver,
    key: Key,
    run: impl FnOnce() -> Result<()>,
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
    let result = run();
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
}
