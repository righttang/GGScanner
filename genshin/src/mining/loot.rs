//! Ore acquisition parsing and conservative de-duplication of the HUD feed.
use std::collections::BTreeMap;

pub const ORE_NAMES: &[&str] = &[
    "铁块",
    "白铁块",
    "水晶块",
    "魔晶块",
    "紫晶块",
    "萃凝晶",
    "虹滴晶",
    "星银矿石",
];

pub fn ore_name(text: &str) -> Option<&'static str> {
    ORE_NAMES
        .iter()
        .copied()
        .filter(|name| text.contains(name))
        .max_by_key(|name| name.len())
}

pub fn parse_line(text: &str) -> Option<(&'static str, usize)> {
    let name = ore_name(text)?;
    let (_, tail) = text.split_once(name)?;
    let count = tail
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '×' | 'x' | 'X' | '*'))
        .trim()
        .parse::<usize>()
        .ok()?;
    (1..=99).contains(&count).then_some((name, count))
}

#[derive(Default)]
pub struct LootTracker {
    maximum: BTreeMap<String, usize>,
    empty_since: Option<u64>,
    last_nonempty: Option<u64>,
}
impl LootTracker {
    /// Toasts can flicker during OCR. Require a sustained empty interval before
    /// accepting a new notification of the same size. Counts are lower bounds.
    pub fn observe(
        &mut self,
        now_ms: u64,
        visible: BTreeMap<String, usize>,
    ) -> BTreeMap<String, usize> {
        if visible.is_empty() {
            let since = *self.empty_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= 1500 {
                self.maximum.clear();
            }
            return BTreeMap::new();
        }
        if self
            .last_nonempty
            .is_some_and(|last| now_ms.saturating_sub(last) >= 8000)
            || self
                .empty_since
                .is_some_and(|since| now_ms.saturating_sub(since) >= 1500)
        {
            self.maximum.clear();
        }
        self.empty_since = None;
        self.last_nonempty = Some(now_ms);
        let mut gain = BTreeMap::new();
        for (name, count) in visible {
            let previous = self.maximum.entry(name.clone()).or_default();
            if count > *previous {
                gain.insert(name, count - *previous);
                *previous = count;
            }
        }
        gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feed(n: usize) -> BTreeMap<String, usize> {
        BTreeMap::from([("萃凝晶".into(), n)])
    }
    #[test]
    fn distinguishes_long_ore_names_and_rejects_unreadable_quantities() {
        assert_eq!(parse_line("白铁块 × 3"), Some(("白铁块", 3)));
        assert_eq!(parse_line("萃凝晶1"), Some(("萃凝晶", 1)));
        assert_eq!(parse_line("虹滴晶 x 12"), Some(("虹滴晶", 12)));
        for line in ["虹滴晶", "虹滴晶 0", "虹滴晶 100", "角色对话"] {
            assert_eq!(parse_line(line), None);
        }
    }
    #[test]
    fn flicker_does_not_duplicate_loot_but_new_notifications_can_count() {
        let mut tracker = LootTracker::default();
        assert_eq!(tracker.observe(0, feed(1)), feed(1));
        assert!(tracker.observe(200, feed(1)).is_empty());
        tracker.observe(400, BTreeMap::new());
        assert!(tracker.observe(600, feed(1)).is_empty());
        assert_eq!(tracker.observe(800, feed(3)), feed(2));
        tracker.observe(1000, BTreeMap::new());
        tracker.observe(2600, BTreeMap::new());
        assert_eq!(tracker.observe(2800, feed(1)), feed(1));
        assert_eq!(tracker.observe(12000, feed(1)), feed(1));
    }
}
