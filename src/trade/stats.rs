//! trade2 stat catalog + a matcher that maps a clipboard mod line to its stat id.
//! Fetched from `trade2/data/stats` at startup; a committed fixture drives tests.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

/// Which stat namespace a parsed mod belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatGroup {
    Explicit,
    Implicit,
    Enchant,
    Rune,
    Pseudo,
}

impl StatGroup {
    fn json_id(self) -> &'static str {
        match self {
            StatGroup::Explicit => "explicit",
            StatGroup::Implicit => "implicit",
            StatGroup::Enchant => "enchant",
            StatGroup::Rune => "rune",
            StatGroup::Pseudo => "pseudo",
        }
    }
}

/// Replaces each signed number token (e.g. "+40", "-10", "12.5") with `#` and
/// collapses whitespace, so a clipboard line matches a catalog template.
///
/// Advanced Mode shows rolls as `current(min-max)` (e.g. "16(15-18)%"); the
/// `(min-max)` range is dropped so it normalizes to `#%` (matching the catalog)
/// rather than `#(#-#)%` (which would never match).
pub fn normalize(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Drop a value-range annotation like "(15-18)" entirely.
        if c == '(' {
            if let Some(rel) = chars[i + 1..].iter().position(|&ch| ch == ')') {
                let inner = &chars[i + 1..i + 1 + rel];
                if !inner.is_empty()
                    && inner
                        .iter()
                        .all(|&ch| ch.is_ascii_digit() || matches!(ch, '-' | '–' | '.' | '+' | ' '))
                {
                    i += rel + 2;
                    continue;
                }
            }
        }
        let starts_num = c.is_ascii_digit()
            || ((c == '+' || c == '-') && i + 1 < chars.len() && chars[i + 1].is_ascii_digit());
        if starts_num {
            out.push('#');
            if c == '+' || c == '-' {
                i += 1;
            }
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Deserialize)]
struct RawStats {
    result: Vec<RawGroup>,
}
#[derive(Deserialize)]
struct RawGroup {
    id: String,
    #[serde(default)]
    entries: Vec<RawEntry>,
}
#[derive(Deserialize)]
struct RawEntry {
    id: String,
    text: String,
}

/// Maps normalized mod text → stat id, per group.
#[derive(Debug, Default)]
pub struct StatCatalog {
    groups: HashMap<StatGroup, HashMap<String, String>>,
}

impl StatCatalog {
    /// Builds the catalog from a `data/stats` JSON body (only the groups we
    /// price are retained). On a normalized-text collision the first id wins
    /// (rare local/global duplicates; acceptable for v1).
    pub fn from_json(json: &str) -> Result<Self> {
        let raw: RawStats = serde_json::from_str(json)?;
        let want = [
            StatGroup::Explicit,
            StatGroup::Implicit,
            StatGroup::Enchant,
            StatGroup::Rune,
            StatGroup::Pseudo,
        ];
        let mut groups: HashMap<StatGroup, HashMap<String, String>> = HashMap::new();
        for g in &raw.result {
            if let Some(sg) = want.iter().copied().find(|s| s.json_id() == g.id) {
                let map = groups.entry(sg).or_default();
                for e in &g.entries {
                    map.entry(normalize(&e.text))
                        .or_insert_with(|| e.id.clone());
                }
            }
        }
        Ok(StatCatalog { groups })
    }

    /// Looks up the stat id for a clipboard mod line within a group.
    pub fn match_stat(&self, raw_line: &str, group: StatGroup) -> Option<String> {
        self.groups.get(&group)?.get(&normalize(raw_line)).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.values().all(|m| m.is_empty())
    }

    /// Fetches the live catalog from the trade2 API.
    pub async fn fetch(client: &crate::trade::client::TradeClient) -> Result<Self> {
        Self::from_json(&client.fetch_stats_raw().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> StatCatalog {
        StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap()
    }

    #[test]
    fn matches_common_single_number_mods() {
        let c = cat();
        assert_eq!(
            c.match_stat("+40 to maximum Life", StatGroup::Explicit)
                .as_deref(),
            Some("explicit.stat_3299347043")
        );
        assert_eq!(
            c.match_stat("+32% to Fire Resistance", StatGroup::Explicit)
                .as_deref(),
            Some("explicit.stat_3372524247")
        );
        assert_eq!(
            c.match_stat("12.5% increased Spell Damage", StatGroup::Explicit)
                .as_deref(),
            Some("explicit.stat_spell_dmg")
        );
    }

    #[test]
    fn matches_multi_number_and_other_groups() {
        let c = cat();
        assert_eq!(
            c.match_stat("Adds 5 to 12 Fire Damage", StatGroup::Explicit)
                .as_deref(),
            Some("explicit.stat_adds_fire")
        );
        assert_eq!(
            c.match_stat("+25 to maximum Mana", StatGroup::Implicit)
                .as_deref(),
            Some("implicit.stat_mana")
        );
        assert_eq!(
            c.match_stat("12% increased Rarity of Items found", StatGroup::Enchant)
                .as_deref(),
            Some("enchant.stat_rarity")
        );
    }

    #[test]
    fn unmatched_and_wrong_group_return_none() {
        let c = cat();
        assert_eq!(
            c.match_stat("Some Totally Unknown Mod", StatGroup::Explicit),
            None
        );
        // maximum Life is an explicit, not an implicit:
        assert_eq!(
            c.match_stat("+40 to maximum Life", StatGroup::Implicit),
            None
        );
    }

    #[test]
    fn normalize_collapses_numbers_and_signs() {
        assert_eq!(normalize("+40 to maximum Life"), "# to maximum Life");
        assert_eq!(
            normalize("-10% to Chaos Resistance"),
            "#% to Chaos Resistance"
        );
        assert_eq!(
            normalize("Adds 5 to 12 Fire Damage"),
            "Adds # to # Fire Damage"
        );
    }

    #[test]
    fn normalize_strips_value_ranges() {
        // Advanced Mode shows "current(min-max)"; the range must drop so the
        // line matches the catalog "#" template (not "#(#-#)").
        assert_eq!(
            normalize("16(15-18)% increased Rarity of Items found"),
            "#% increased Rarity of Items found"
        );
        assert_eq!(
            normalize("+34(31-35)% to Lightning Resistance"),
            "#% to Lightning Resistance"
        );
    }

    #[test]
    fn matches_ranged_advanced_mode_mod() {
        let c = cat();
        assert_eq!(
            c.match_stat(
                "16(15-18)% increased Rarity of Items found",
                StatGroup::Explicit
            )
            .as_deref(),
            Some("explicit.stat_rarity")
        );
    }

    #[tokio::test]
    #[ignore = "hits the live trade2 API"]
    async fn live_catalog_matches_a_common_mod() {
        let rates = std::sync::Arc::new(std::sync::RwLock::new(
            crate::trade::rates::RateTable::default(),
        ));
        let client = crate::trade::client::TradeClient::new(None, rates).unwrap();
        let catalog = StatCatalog::fetch(&client).await.unwrap();
        assert!(!catalog.is_empty());
        // a ubiquitous explicit mod should resolve to some stat id
        assert!(catalog
            .match_stat("+40 to maximum Life", StatGroup::Explicit)
            .is_some());
        // spell-skills is a top spellcaster driver and is NOT a trade2 pseudo —
        // it must resolve as an explicit (regression guard for the dropped rule).
        assert!(catalog
            .match_stat("+7 to Level of all Spell Skills", StatGroup::Explicit)
            .is_some());
    }
}
