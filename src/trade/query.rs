//! Builds a `TradeQuery` from a parsed item (pseudo-preferred for fungible
//! groups) and serializes it to the trade2 search payload.

use serde_json::{json, Value};

use crate::itemtext::ParsedItem;
use crate::trade::model::{EquipFilter, MiscFilters, StatFilter, TradeQuery};
use crate::trade::pseudo::PseudoMap;
use crate::trade::stats::{StatCatalog, StatGroup};

/// Band width as a fraction of the roll.
const BAND_K: f64 = 0.5;
/// Your roll sits at this percentile of the band (0.2 = bottom 20%).
const BAND_PCTL: f64 = 0.2;

/// Search band `[min, max]` around roll `v`, with `v` at the BAND_PCTL position.
/// With BAND_K=0.5, BAND_PCTL=0.2 → `[0.9·v, 1.4·v]`.
fn band(v: f64) -> (Option<f64>, Option<f64>) {
    let lo = (v * (1.0 - BAND_PCTL * BAND_K)).round();
    let hi = (v * (1.0 + (1.0 - BAND_PCTL) * BAND_K)).round();
    (Some(lo), Some(hi))
}

pub fn build_baseline(
    item: &ParsedItem,
    pseudo: &PseudoMap,
    catalog: &StatCatalog,
    league: &str,
) -> TradeQuery {
    // Only the item's explicit affixes drive value/comparable filters. Runes are
    // buyer-added sockets, implicits are base-inherent, enchants are added — none
    // should constrain the comparable search (they over-collapse it; see the
    // marginal-pricing design).
    let all_stats: Vec<_> = item.explicits.to_vec();

    let mut stats: Vec<StatFilter> = Vec::new();

    // Pseudo aggregates with a positive total become min-bounded filters.
    let resolved: Vec<_> = pseudo.resolve(&all_stats);

    // When both pseudo_total_elemental_resistance and pseudo_total_resistance
    // resolve to positive totals they describe overlapping lines — emitting both
    // makes each single-drop delta ≈ 0 and breaks the ablation breakdown.
    // Keep only one: prefer pseudo_total_resistance when its total is strictly
    // greater (i.e. chaos resistance contributes), otherwise keep
    // pseudo_total_elemental_resistance and drop the total-resistance one.
    let ele_total = resolved
        .iter()
        .find(|ps| ps.id == "pseudo.pseudo_total_elemental_resistance")
        .map(|ps| ps.total);
    let all_total = resolved
        .iter()
        .find(|ps| ps.id == "pseudo.pseudo_total_resistance")
        .map(|ps| ps.total);
    let drop_total_resistance = match (ele_total, all_total) {
        (Some(ele), Some(all)) => all <= ele, // no chaos contribution → keep elemental
        _ => false,
    };

    for ps in &resolved {
        if ps.total > 0.0 {
            if ps.id == "pseudo.pseudo_total_resistance" && drop_total_resistance {
                continue;
            }
            if ps.id == "pseudo.pseudo_total_elemental_resistance"
                && !drop_total_resistance
                && ele_total.is_some()
                && all_total.is_some()
            {
                continue;
            }
            let (min, max) = band(ps.total);
            stats.push(StatFilter {
                id: ps.id.clone(),
                label: ps.label.clone(),
                min,
                max,
            });
        }
    }

    // Individual (non-fungible) mods: map each to its trade2 stat id and add a
    // banded filter. Mods already covered by a pseudo aggregate are skipped to
    // avoid double-constraining; unmatched mods are logged and skipped.
    // Only explicit affixes are considered — runes/implicits/enchants are
    // excluded for the same reason as above (they over-collapse comparables).
    let buckets = [(&item.explicits, StatGroup::Explicit)];
    for (mods, group) in buckets {
        for m in mods {
            if pseudo.covers(&m.raw) {
                continue;
            }
            match catalog.match_stat(&m.raw, group) {
                Some(id) => {
                    let (min, max) = m.value.map(band).unwrap_or((None, None));
                    stats.push(StatFilter {
                        id,
                        label: m.raw.clone(),
                        min,
                        max,
                    });
                }
                None => tracing::debug!(item_mod = %m.raw, "no trade2 stat match; skipping filter"),
            }
        }
    }

    let mut equipment = Vec::new();
    for (key, val) in [
        ("es", item.energy_shield),
        ("ar", item.armour),
        ("ev", item.evasion),
    ] {
        if let Some(v) = val {
            let (min, max) = band(v as f64);
            equipment.push(EquipFilter {
                key: key.to_string(),
                min,
                max,
            });
        }
    }

    TradeQuery {
        league: league.to_string(),
        category: None, // category inference deferred (needs a base→category table)
        type_line: item.base_type.clone(),
        stats,
        misc: MiscFilters {
            item_level_min: item.item_level,
            quality_min: item.quality,
            corrupted: Some(item.corrupted),
        },
        equipment,
    }
}

/// The same query with all stat filters removed (type + misc + equipment kept).
/// Used by the marginal-contribution sampler to fetch the base population.
pub fn base_query(q: &TradeQuery) -> TradeQuery {
    TradeQuery {
        stats: Vec::new(),
        ..q.clone()
    }
}

/// Serializes a `TradeQuery` to the trade2 search request body.
///
/// Assumption (confirmed by the live smoke test in Task 7): trade2 expects
/// `{ query: { status, type, filters: { type_filters, misc_filters }, stats }, sort }`.
pub fn to_payload(q: &TradeQuery) -> Value {
    let stat_filters: Vec<Value> = q
        .stats
        .iter()
        .map(|s| {
            let mut value = json!({});
            if let Some(m) = s.min {
                value["min"] = json!(m);
            }
            if let Some(m) = s.max {
                value["max"] = json!(m);
            }
            json!({ "id": s.id, "value": value })
        })
        .collect();

    let mut type_filters = json!({});
    if let Some(c) = &q.category {
        type_filters["category"] = json!({ "option": c });
    }
    if let Some(min) = q.misc.item_level_min {
        type_filters["ilvl"] = json!({ "min": min });
    }
    if let Some(min) = q.misc.quality_min {
        type_filters["quality"] = json!({ "min": min });
    }

    let mut misc_filters = json!({});
    if let Some(c) = q.misc.corrupted {
        misc_filters["corrupted"] = json!({ "option": c });
    }

    let mut equip = serde_json::Map::new();
    for e in &q.equipment {
        let mut value = json!({});
        if let Some(m) = e.min {
            value["min"] = json!(m);
        }
        if let Some(m) = e.max {
            value["max"] = json!(m);
        }
        equip.insert(e.key.clone(), value);
    }

    let mut query = json!({
        "status": { "option": "online" },
        "stats": [ { "type": "and", "filters": stat_filters } ],
        "filters": {
            "type_filters": { "filters": type_filters },
            "misc_filters": { "filters": misc_filters },
            "equipment_filters": { "filters": equip },
        }
    });
    if let Some(t) = &q.type_line {
        query["type"] = json!(t);
    }

    json!({ "query": query, "sort": { "price": "asc" } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::{ItemStat, ParsedItem, Rarity};
    use crate::trade::pseudo::PseudoMap;
    fn ring() -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare,
            name: "Woe Coil".into(),
            base_type: Some("Sapphire Ring".into()),
            item_class: Some("Rings".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat {
                    raw: "+40 to maximum Life".into(),
                    value: Some(40.0),
                    affix: None,
                },
                ItemStat {
                    raw: "+32% to Fire Resistance".into(),
                    value: Some(32.0),
                    affix: None,
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
                    affix: None,
                },
            ],
        }
    }

    #[test]
    fn fire_and_lightning_only_emits_elemental_not_total_resistance() {
        // A ring with only fire + lightning resistance (no chaos) should produce
        // exactly ONE resistance stat filter — the elemental one — not both
        // pseudo_total_elemental_resistance AND pseudo_total_resistance.
        let item = ParsedItem {
            rarity: Rarity::Rare,
            name: "Woe Coil".into(),
            base_type: Some("Sapphire Ring".into()),
            item_class: Some("Rings".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat {
                    raw: "+32% to Fire Resistance".into(),
                    value: Some(32.0),
                    affix: None,
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
                    affix: None,
                },
            ],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        let res_filters: Vec<_> = q
            .stats
            .iter()
            .filter(|s| {
                s.id == "pseudo.pseudo_total_elemental_resistance"
                    || s.id == "pseudo.pseudo_total_resistance"
            })
            .collect();
        assert_eq!(
            res_filters.len(),
            1,
            "expected exactly one resistance filter, got: {:?}",
            res_filters.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        assert_eq!(
            res_filters[0].id, "pseudo.pseudo_total_elemental_resistance",
            "no chaos → should keep elemental filter"
        );
    }

    #[test]
    fn baseline_prefers_pseudo_resistance_over_individual_lines() {
        let q = build_baseline(
            &ring(),
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        assert_eq!(q.league, "Standard");
        assert_eq!(q.type_line.as_deref(), Some("Sapphire Ring"));
        let ele = q
            .stats
            .iter()
            .find(|s| s.id == "pseudo.pseudo_total_elemental_resistance")
            .unwrap();
        assert_eq!(ele.min, Some(45.0)); // round(0.9 * 50) = 45
        assert_eq!(ele.max, Some(70.0)); // round(1.4 * 50) = 70
        assert!(!q.stats.iter().any(|s| s.label.contains("Fire Resistance")));
        assert!(q
            .stats
            .iter()
            .any(|s| s.id == "pseudo.pseudo_total_life" && s.min == Some(36.0)));
        // round(0.9 * 40) = 36
    }

    #[test]
    fn payload_has_status_type_and_sort() {
        let q = build_baseline(
            &ring(),
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        let payload = to_payload(&q);
        assert_eq!(payload["query"]["status"]["option"], "online");
        assert_eq!(payload["query"]["type"], "Sapphire Ring");
        assert_eq!(payload["sort"]["price"], "asc");
    }

    #[test]
    fn baseline_emits_individual_filter_for_matched_nonpseudo_mod() {
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let mut item = ring(); // life + fire res + lightning res
        item.explicits.push(ItemStat {
            raw: "80% increased Spell Damage".into(),
            value: Some(80.0),
            affix: None,
        });
        let q = build_baseline(&item, &PseudoMap::load(), &catalog, "Standard");
        // spell damage is non-pseudo → individual banded filter (round(0.9*80)=72, round(1.4*80)=112)
        let sd = q
            .stats
            .iter()
            .find(|s| s.id == "explicit.stat_spell_dmg")
            .unwrap();
        assert_eq!(sd.min, Some(72.0));
        assert_eq!(sd.max, Some(112.0));
        // resists stay collapsed into the pseudo, NOT individual filters
        assert!(q
            .stats
            .iter()
            .any(|s| s.id == "pseudo.pseudo_total_elemental_resistance"));
        assert!(!q.stats.iter().any(|s| s.id == "explicit.stat_3372524247"));
    }

    #[test]
    fn energy_shield_produces_banded_equip_filter() {
        let item = ParsedItem {
            rarity: Rarity::Rare,
            name: "Kraken Slippers".into(),
            base_type: Some("Sandsworn Sandals".into()),
            item_class: Some("Boots".into()),
            item_level: Some(83),
            quality: None,
            corrupted: false,
            energy_shield: Some(78),
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        let es = q.equipment.iter().find(|e| e.key == "es").unwrap();
        // band(78): lo = round(0.9 * 78) = round(70.2) = 70
        //           hi = round(1.4 * 78) = round(109.2) = 109
        assert_eq!(es.min, Some(70.0));
        assert_eq!(es.max, Some(109.0));
        assert!(q.equipment.iter().all(|e| e.key != "ar"));
        assert!(q.equipment.iter().all(|e| e.key != "ev"));
    }

    #[test]
    fn payload_includes_equipment_filters_for_es() {
        let item = ParsedItem {
            rarity: Rarity::Rare,
            name: "Kraken Slippers".into(),
            base_type: Some("Sandsworn Sandals".into()),
            item_class: Some("Boots".into()),
            item_level: Some(83),
            quality: None,
            corrupted: false,
            energy_shield: Some(78),
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        let payload = to_payload(&q);
        let es_min = &payload["query"]["filters"]["equipment_filters"]["filters"]["es"]["min"];
        assert_eq!(*es_min, serde_json::json!(70.0));
    }

    #[test]
    fn rune_resistance_excluded_from_pseudo_total_elemental_resistance() {
        // Boots: cold-rune (+18), lightning explicit (+34), fire explicit (+39).
        // After the affix-explicits-only change, the rune's +18% is NOT counted.
        // Total elemental = 34+39 = 73; band lo = round(0.9*73) = round(65.7) = 66.
        //
        // Concern surfaced: the previous test ("rune_resistance_folds_into_pseudo")
        // verified the OLD behaviour where runes contributed to the pseudo aggregate
        // (total 91, lo=82). That test has been updated here to document the new
        // contract: runes are buyer-added and must not constrain comparables.
        let item = ParsedItem {
            rarity: Rarity::Rare,
            name: "Kraken Slippers".into(),
            base_type: Some("Sandsworn Sandals".into()),
            item_class: Some("Boots".into()),
            item_level: Some(83),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![ItemStat {
                raw: "+18% to Cold Resistance".into(),
                value: Some(18.0),
                affix: None,
            }],
            explicits: vec![
                ItemStat {
                    raw: "+34% to Lightning Resistance".into(),
                    value: Some(34.0),
                    affix: None,
                },
                ItemStat {
                    raw: "+39% to Fire Resistance".into(),
                    value: Some(39.0),
                    affix: None,
                },
            ],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            "Standard",
        );
        let ele = q
            .stats
            .iter()
            .find(|s| s.id == "pseudo.pseudo_total_elemental_resistance")
            .expect("elemental resistance pseudo filter present");
        // round(0.9 * 73) = round(65.7) = 66  (rune +18 excluded)
        assert_eq!(
            ele.min,
            Some(66.0),
            "rune resistance must NOT fold into pseudo"
        );
    }

    #[test]
    fn build_baseline_ignores_runes_and_implicits() {
        let item = ParsedItem {
            rarity: crate::itemtext::Rarity::Rare,
            name: "Onslaught Spell".into(),
            base_type: Some("Chiming Staff".into()),
            item_class: Some("Staves".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![ItemStat {
                raw: "10% increased Cast Speed".into(),
                value: Some(10.0),
                affix: None,
            }],
            enchants: vec![],
            runes: vec![ItemStat {
                raw: "+1 to Level of all Spell Skills".into(),
                value: Some(1.0),
                affix: None,
            }],
            explicits: vec![ItemStat {
                raw: "201% increased Spell Physical Damage".into(),
                value: Some(201.0),
                affix: Some(crate::itemtext::Affix::Prefix),
            }],
        };
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let q = build_baseline(&item, &PseudoMap::load(), &catalog, "Standard");
        // Only the explicit affix yields a filter (if the sample catalog matches it);
        // the rune and implicit never do.
        assert!(q
            .stats
            .iter()
            .all(|s| !s.label.contains("all Spell Skills")));
        assert!(q
            .stats
            .iter()
            .all(|s| !s.label.contains("increased Cast Speed") || s.label.contains("Spell")));
        // no implicit cast-speed filter
    }

    #[test]
    fn base_query_clears_stats_keeps_type_and_misc() {
        use crate::trade::model::{MiscFilters, StatFilter};
        let q = TradeQuery {
            league: "L".into(),
            category: None,
            type_line: Some("Chiming Staff".into()),
            stats: vec![StatFilter {
                id: "explicit.stat_1".into(),
                label: "x".into(),
                min: Some(1.0),
                max: Some(2.0),
            }],
            misc: MiscFilters {
                item_level_min: Some(80),
                quality_min: None,
                corrupted: Some(false),
            },
            equipment: vec![],
        };
        let b = base_query(&q);
        assert!(b.stats.is_empty());
        assert_eq!(b.type_line, q.type_line);
        assert_eq!(b.misc.item_level_min, Some(80));
    }
}
