//! Conservative character identity resolution for actions that change equipment.

use std::collections::{HashMap, HashSet};

use super::fuzzy_match::{normalize_ocr_text, ocr_confusion_variants};

/// Strip the element/header prefix and decorative OCR noise, preserving Latin
/// custom names as well as Chinese names.
pub fn clean_char_name(text: &str) -> String {
    let text = text.trim();
    let name = text.rsplit(['/', '／', '丨', '|']).next().unwrap_or(text);
    let name = name.rsplit_once("元素").map_or(name, |(_, after)| after);
    normalize_ocr_text(name)
}

/// Resolve against the complete catalog before the caller filters pending
/// targets. An exact non-target name must never become a fuzzy target match.
/// Short names require exact recognition or a cataloged OCR correction; longer
/// names permit one substitution only when it identifies one canonical key.
pub fn resolve_character_name(text: &str, names: &HashMap<String, String>) -> Option<String> {
    let cleaned = clean_char_name(text);
    if cleaned.is_empty() {
        return None;
    }

    let exact: HashSet<&str> = names
        .iter()
        .filter(|(name, key)| normalize_ocr_text(name) == cleaned || key.as_str() == cleaned)
        .map(|(_, key)| key.as_str())
        .collect();
    if !exact.is_empty() {
        return unique_key(exact);
    }

    let variants = ocr_confusion_variants(&cleaned);
    let corrected: HashSet<&str> = names
        .iter()
        .filter(|(name, _)| variants.contains(&normalize_ocr_text(name)))
        .map(|(_, key)| key.as_str())
        .collect();
    if !corrected.is_empty() {
        return unique_key(corrected);
    }

    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() < 3 {
        return None;
    }
    unique_key(
        names
            .iter()
            .filter(|(name, _)| {
                let expected = normalize_ocr_text(name);
                expected.chars().count() == chars.len()
                    && chars
                        .iter()
                        .copied()
                        .zip(expected.chars())
                        .filter(|(a, b)| a != b)
                        .count()
                        <= 1
            })
            .map(|(_, key)| key.as_str())
            .collect(),
    )
}

fn unique_key(keys: HashSet<&str>) -> Option<String> {
    if keys.len() == 1 {
        keys.into_iter().next().map(str::to_owned)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> HashMap<String, String> {
        HashMap::from([
            ("闲云".into(), "Xianyun".into()),
            ("重云".into(), "Chongyun".into()),
            ("神里绫华".into(), "KamisatoAyaka".into()),
            ("神里绫人".into(), "KamisatoAyato".into()),
            ("茜特菈莉".into(), "Citlali".into()),
            ("菈乌玛".into(), "Lauma".into()),
        ])
    }

    #[test]
    fn exact_character_identity_wins_across_the_entire_catalog() {
        let entries: Vec<_> = catalog().into_iter().collect();
        for offset in 0..entries.len() {
            let names = entries
                .iter()
                .cycle()
                .skip(offset)
                .take(entries.len())
                .cloned()
                .collect();
            assert_eq!(
                resolve_character_name("风元素 / 闲云。", &names).as_deref(),
                Some("Xianyun")
            );
            assert_eq!(
                resolve_character_name("冰元素 / 重云", &names).as_deref(),
                Some("Chongyun")
            );
        }
    }

    #[test]
    fn short_names_and_truncated_names_cannot_select_a_similar_character() {
        for text in ["闲芸", "重芸", "云", "神里绫", "莉"] {
            assert_eq!(resolve_character_name(text, &catalog()), None, "{text}");
        }
    }

    #[test]
    fn ambiguous_long_name_is_rejected_instead_of_using_hashmap_order() {
        assert_eq!(resolve_character_name("神里绫花", &catalog()), None);
    }

    #[test]
    fn known_ocr_corrections_are_shared_with_the_scanner() {
        assert_eq!(
            resolve_character_name("西特莱莉", &catalog()).as_deref(),
            Some("Citlali")
        );
        assert_eq!(
            resolve_character_name("拉鸟玛", &catalog()).as_deref(),
            Some("Lauma")
        );
    }

    #[test]
    fn aliases_of_one_character_do_not_create_false_ambiguity() {
        let names = HashMap::from([
            ("测试甲".into(), "Traveler".into()),
            ("测试乙".into(), "Traveler".into()),
        ]);
        assert_eq!(
            resolve_character_name("测试丙", &names).as_deref(),
            Some("Traveler")
        );
    }

    #[test]
    fn latin_custom_names_and_good_keys_are_preserved() {
        let mut names = catalog();
        names.insert("a".into(), "Manekin".into());
        names.insert("西特莱莉".into(), "Manekina".into());
        assert_eq!(
            resolve_character_name("风元素 ／ ａ。", &names).as_deref(),
            Some("Manekin")
        );
        assert_eq!(
            resolve_character_name("Xianyun", &names).as_deref(),
            Some("Xianyun")
        );
        assert_eq!(
            resolve_character_name("西特莱莉", &names).as_deref(),
            Some("Manekina")
        );
    }
}
