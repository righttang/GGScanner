//! Ore acquisition parsing and de-duplication of the HUD feed.
use anyhow::Result;
use image::RgbImage;
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

pub(super) fn pickup_prompts(
    frame: &RgbImage,
    mut read: impl FnMut(u32, u32, u32, u32) -> Result<String>,
    motion: &super::motion::MotionDetector,
) -> Result<Vec<(u32, String, bool)>> {
    let mut prompts = Vec::new();
    for (top, height) in super::vision::text_rows(frame, 1070, 330, 530, 420) {
        let text = read(1070, top, 530, height)?;
        if !text.trim().is_empty() {
            prompts.push((top, text, motion.ore_prompt_key(frame, top, 42)));
        }
    }
    Ok(prompts)
}

pub(super) fn has_ore_pickup(prompts: &[(u32, String, bool)]) -> bool {
    prompts
        .iter()
        .any(|(_, text, key)| *key && ore_name(text).is_some())
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

pub(super) fn read_visible(
    frame: &RgbImage,
    mut read: impl FnMut(u32, u32, u32, u32) -> Result<String>,
) -> Result<BTreeMap<String, usize>> {
    let mut visible = BTreeMap::new();
    // The aimed character can fill the right half of the old wide crop. Hair
    // merges separate toast rows or is OCR'd as extra quantity digits. Keep
    // only the icon/name/count columns and include the whole stacked feed.
    for (top, height) in super::vision::text_rows(frame, 90, 300, 260, 600) {
        if let Some((name, count)) = parse_line(&read(90, top, 260, height)?) {
            *visible.entry(name.to_string()).or_default() += count;
        }
    }
    Ok(visible)
}

#[derive(Default)]
pub struct LootTracker {
    maximum: BTreeMap<String, usize>,
    empty_since: Option<u64>,
    last_nonempty: Option<u64>,
}
impl LootTracker {
    /// Toasts can flicker during OCR. Require a sustained empty interval before
    /// accepting a new notification of the same size. Counts remain OCR estimates.
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
        for (name, &count) in &visible {
            // A fading/obscured 白铁块 can lose its leading glyph and be
            // read as 铁块. When that longer label is simultaneously losing
            // count, do not introduce its suffix as newly acquired ore.
            if self.maximum.iter().any(|(longer, &previous)| {
                longer != name
                    && longer.contains(name.as_str())
                    && visible.get(longer).copied().unwrap_or(0) < previous
            }) {
                continue;
            }
            let previous = self.maximum.entry(name.clone()).or_default();
            if count > *previous {
                gain.insert(name.clone(), count - *previous);
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
    #[test]
    fn fading_long_labels_do_not_invent_suffix_ore_but_fresh_mixed_loot_counts() {
        let fresh = BTreeMap::from([("白铁块".into(), 2), ("虹滴晶".into(), 3)]);
        let faded = BTreeMap::from([
            ("白铁块".into(), 1),
            ("虹滴晶".into(), 2),
            ("铁块".into(), 1),
        ]);
        let mut tracker = LootTracker::default();
        assert_eq!(tracker.observe(0, fresh.clone()), fresh);
        assert!(tracker.observe(400, faded).is_empty());
        tracker.observe(1000, BTreeMap::new());
        tracker.observe(2600, BTreeMap::new());
        let mixed = BTreeMap::from([("白铁块".into(), 1), ("铁块".into(), 1)]);
        assert_eq!(tracker.observe(2800, mixed.clone()), mixed);
    }
    #[test]
    fn pickup_requires_the_selected_ore_row() {
        assert!(has_ore_pickup(&[(448, "日萃凝晶".into(), true)]));
        assert!(!has_ore_pickup(&[
            (448, "调查".into(), true),
            (523, "萃凝晶".into(), false),
        ]));
        assert!(!has_ore_pickup(&[(448, "萃凝晶".into(), false)]));
        assert!(!has_ore_pickup(&[(448, "对话".into(), true)]));
    }
    #[test]
    #[ignore = "requires a local ore interaction frame and OCR model"]
    fn reads_local_ore_pickup_prompt() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let ocr = crate::scanner::common::ocr_factory::create_ocr_model("ppocrv4").unwrap();
        let prompts = pickup_prompts(
            &frame,
            |x, y, w, h| {
                ocr.image_to_text(
                    &image::imageops::crop_imm(&frame, x, y, w, h).to_image(),
                    false,
                )
            },
            &super::super::motion::MotionDetector::new().unwrap(),
        )
        .unwrap();
        println!("Pickup prompts: {prompts:?}");
        assert_eq!(
            has_ore_pickup(&prompts),
            std::env::var("GGSCANNER_EXPECT_PICKUP").unwrap() == "true"
        );
    }
    #[test]
    #[ignore = "requires a local acquisition feed frame and OCR model"]
    fn reads_local_stacked_feed_without_aimed_character_pixels() {
        let frame = image::open(std::env::var("GGSCANNER_MINING_FRAME").unwrap())
            .unwrap()
            .to_rgb8();
        let ocr = crate::scanner::common::ocr_factory::create_ocr_model("ppocrv4").unwrap();
        let actual = read_visible(&frame, |x, y, w, h| {
            ocr.image_to_text(
                &image::imageops::crop_imm(&frame, x, y, w, h).to_image(),
                false,
            )
        })
        .unwrap();
        let expected: BTreeMap<String, usize> =
            serde_json::from_str(&std::env::var("GGSCANNER_EXPECTED_LOOT").unwrap()).unwrap();
        println!("Acquisition feed: {actual:?}");
        if std::env::var_os("GGSCANNER_LOOT_ALLOW_FADE").is_some() {
            // Replay the fading frame after the fully visible notification.
            // Partial glyphs must not add newly acquired ore to the report.
            let mut tracker = LootTracker::default();
            tracker.observe(0, expected);
            assert!(tracker.observe(400, actual).is_empty());
        } else {
            assert_eq!(actual, expected);
        }
    }
}
