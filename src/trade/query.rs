//! Builds a `TradeQuery` from a parsed item (pseudo-preferred for fungible
//! groups) and serializes it to the trade2 search payload.

use serde_json::{json, Value};

use crate::itemtext::ParsedItem;
use crate::trade::model::{EquipFilter, MiscFilters, StatFilter, TradeQuery};
use crate::trade::pseudo::PseudoMap;
use crate::trade::stats::{StatCatalog, StatGroup};

/// Band width as a fraction of the roll.
const BAND_K: f64 = 0.5;
/// Tighter band width for learned value-drivers.
const BAND_K_DRIVER: f64 = 0.25;
/// Your roll sits at this percentile of the band (0.2 = bottom 20%).
const BAND_PCTL: f64 = 0.2;

/// Search band `[min, max]` around roll `v`, with `v` at the BAND_PCTL position.
/// With BAND_K=0.5, BAND_PCTL=0.2 → `[0.9·v, 1.4·v]`.
fn band(v: f64) -> (Option<f64>, Option<f64>) {
    let lo = (v * (1.0 - BAND_PCTL * BAND_K)).round();
    let hi = (v * (1.0 + (1.0 - BAND_PCTL) * BAND_K)).round();
    (Some(lo), Some(hi))
}

/// Tighter band for learned value-drivers: keeps the price-defining combo
/// constrained. With BAND_K_DRIVER=0.25, BAND_PCTL=0.2 → [0.95·v, 1.2·v].
fn driver_band(v: f64) -> (Option<f64>, Option<f64>) {
    let lo = (v * (1.0 - BAND_PCTL * BAND_K_DRIVER)).round();
    let hi = (v * (1.0 + (1.0 - BAND_PCTL) * BAND_K_DRIVER)).round();
    (Some(lo), Some(hi))
}

/// Relaxation strength for a normal (non-cornerstone) mod. Higher = kept longer.
/// Trusted lift when known; otherwise a small cold-start score from the tier
/// (stronger tier = higher), kept below trusted lifts so unknown mods relax
/// first. Drivers (lift >= DRIVER_LIFT) naturally sort to the front of normals.
fn mod_strength(trusted_lift: Option<f64>, tier: Option<u8>) -> f64 {
    match trusted_lift {
        Some(lift) => lift,
        None => -1.0 - (tier.unwrap_or(u8::MAX) as f64) / 1000.0,
    }
}

/// Cornerstone affixes are searched *exact* (min = roll, no max) because a worse
/// roll is a materially different item: `+X to skill levels` and movement speed.
/// This is the one hand-coded value-known; everything else is banded/relaxed.
fn is_cornerstone(raw: &str) -> bool {
    let l = raw.to_lowercase();
    l.contains("movement speed") || (l.contains("to level of") && l.contains("skill"))
}

pub fn build_baseline(
    item: &ParsedItem,
    pseudo: &PseudoMap,
    catalog: &StatCatalog,
    value: &crate::trade::value::ValueModel,
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

    // Learned value for this item's canonical category, only if trusted.
    let cat_model = item
        .item_class
        .as_deref()
        .map(crate::trade::value::canonical_category)
        .and_then(|c| value.category(league, &c))
        .filter(|m| m.sample_size >= crate::trade::value::MIN_CATEGORY_SAMPLE);

    // Per-mod explicit filters, tagged for ordering. Cornerstones are searched
    // exact (min = roll, no max); everything else uses the loose band unless the
    // category model identifies the stat as a value-driver (tighter band).
    let mut mod_filters: Vec<(bool, f64, StatFilter)> = Vec::new();
    for m in &item.explicits {
        if pseudo.covers(&m.raw) {
            continue;
        }
        if let Some(id) = catalog.match_stat(&m.raw, StatGroup::Explicit) {
            let corner = is_cornerstone(&m.raw);
            let stat_lift = cat_model.as_ref().and_then(|cm| {
                cm.stats
                    .iter()
                    .find(|s| s.stat_id == id && s.count >= crate::trade::value::MIN_STAT_SAMPLE)
                    .map(|s| s.lift)
            });
            let is_driver = stat_lift.is_some_and(|l| l >= crate::trade::value::DRIVER_LIFT);
            let (min, max) = if corner {
                (m.value, None) // exact: at least this roll, no upper bound
            } else if is_driver {
                m.value.map(driver_band).unwrap_or((None, None))
            } else {
                m.value.map(band).unwrap_or((None, None))
            };
            let strength = if corner {
                f64::INFINITY
            } else {
                mod_strength(stat_lift, m.tier)
            };
            mod_filters.push((
                corner,
                strength,
                StatFilter {
                    id,
                    label: m.raw.clone(),
                    min,
                    max,
                },
            ));
        } else {
            tracing::debug!(item_mod = %m.raw, "no trade2 stat match; skipping filter");
        }
    }
    // Order: cornerstones first (dropped last in relaxation). Among normals,
    // strongest first (highest learned lift / tier score) so the weakest relaxes
    // first; drivers, having the highest lift, sit at the front of the normals and
    // survive longest before cornerstones. With an empty/untrusted model every
    // normal falls to the tier-based cold-start score, reproducing today's order.
    mod_filters.sort_by(|(ca, sa, _), (cb, sb, _)| {
        cb.cmp(ca) // cornerstones (true) before normals (false)
            .then(sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal))
    });
    // `stats` already holds the pseudo-aggregate filters; append the ordered mods
    // after them, but keep cornerstones ahead of pseudo so they're dropped last.
    let (corners, normals): (Vec<_>, Vec<_>) = mod_filters.into_iter().partition(|(c, _, _)| *c);
    let mut ordered: Vec<StatFilter> = corners.into_iter().map(|(_, _, f)| f).collect();
    ordered.append(&mut stats); // pseudo aggregates in the middle
    ordered.extend(normals.into_iter().map(|(_, _, f)| f));
    let stats = ordered;

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
        min_price_divine: None,
        max_price_divine: None,
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
    if q.min_price_divine.is_some() || q.max_price_divine.is_some() {
        let mut price = json!({ "option": "divine" });
        if let Some(min) = q.min_price_divine {
            price["min"] = json!(min);
        }
        if let Some(max) = q.max_price_divine {
            price["max"] = json!(max);
        }
        query["filters"]["trade_filters"] = json!({ "filters": { "price": price } });
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
                    tier: None,
                },
                ItemStat {
                    raw: "+32% to Fire Resistance".into(),
                    value: Some(32.0),
                    affix: None,
                    tier: None,
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
                    affix: None,
                    tier: None,
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
                    tier: None,
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
                    affix: None,
                    tier: None,
                },
            ],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            &crate::trade::value::ValueModel::default(),
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
            &crate::trade::value::ValueModel::default(),
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
            &crate::trade::value::ValueModel::default(),
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
            tier: None,
        });
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &catalog,
            &crate::trade::value::ValueModel::default(),
            "Standard",
        );
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
            &crate::trade::value::ValueModel::default(),
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
            &crate::trade::value::ValueModel::default(),
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
                tier: None,
            }],
            explicits: vec![
                ItemStat {
                    raw: "+34% to Lightning Resistance".into(),
                    value: Some(34.0),
                    affix: None,
                    tier: None,
                },
                ItemStat {
                    raw: "+39% to Fire Resistance".into(),
                    value: Some(39.0),
                    affix: None,
                    tier: None,
                },
            ],
        };
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &StatCatalog::default(),
            &crate::trade::value::ValueModel::default(),
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
                tier: None,
            }],
            enchants: vec![],
            runes: vec![ItemStat {
                raw: "+1 to Level of all Spell Skills".into(),
                value: Some(1.0),
                affix: None,
                tier: None,
            }],
            explicits: vec![ItemStat {
                raw: "201% increased Spell Physical Damage".into(),
                value: Some(201.0),
                affix: Some(crate::itemtext::Affix::Prefix),
                tier: None,
            }],
        };
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &catalog,
            &crate::trade::value::ValueModel::default(),
            "Standard",
        );
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
    fn to_payload_emits_min_price_band() {
        let q = TradeQuery {
            league: "L".into(),
            category: Some("weapon.staff".into()),
            type_line: None,
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
            min_price_divine: Some(20.0),
            max_price_divine: None,
        };
        let p = to_payload(&q);
        assert_eq!(
            p["query"]["filters"]["trade_filters"]["filters"]["price"]["min"],
            20.0
        );
        assert_eq!(
            p["query"]["filters"]["trade_filters"]["filters"]["price"]["option"],
            "divine"
        );
    }

    #[test]
    fn to_payload_emits_min_and_max_price_band() {
        let q = TradeQuery {
            league: "L".into(),
            category: Some("weapon.staff".into()),
            type_line: None,
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
            min_price_divine: Some(20.0),
            max_price_divine: Some(50.0),
        };
        let p = to_payload(&q);
        let price = &p["query"]["filters"]["trade_filters"]["filters"]["price"];
        assert_eq!(price["min"], 20.0);
        assert_eq!(price["max"], 50.0);
        assert_eq!(price["option"], "divine");
    }

    #[test]
    fn to_payload_emits_max_only_price_band() {
        // Harvest's first band is lo=0 (no min) with hi=Some(..) → a max-only filter.
        let q = TradeQuery {
            league: "L".into(),
            category: Some("weapon.staff".into()),
            type_line: None,
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
            min_price_divine: None,
            max_price_divine: Some(5.0),
        };
        let p = to_payload(&q);
        let price = &p["query"]["filters"]["trade_filters"]["filters"]["price"];
        assert_eq!(price["max"], 5.0);
        assert_eq!(price["option"], "divine");
        assert!(price.get("min").is_none());
    }

    #[test]
    fn to_payload_omits_price_when_none() {
        let q = TradeQuery {
            league: "L".into(),
            category: None,
            type_line: Some("Chiming Staff".into()),
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
            min_price_divine: None,
            max_price_divine: None,
        };
        let p = to_payload(&q);
        assert!(p["query"]["filters"].get("trade_filters").is_none());
    }

    #[test]
    fn cornerstone_detects_skill_levels_and_movement_speed() {
        assert!(is_cornerstone("+6 to Level of all Physical Spell Skills"));
        assert!(is_cornerstone("+1 to Level of all Spell Skills"));
        assert!(is_cornerstone("35% increased Movement Speed"));
        // Not cornerstones:
        assert!(!is_cornerstone("201% increased Spell Physical Damage"));
        assert!(!is_cornerstone("+298 to maximum Mana"));
        assert!(!is_cornerstone("52% increased Cast Speed"));
    }

    #[test]
    fn cornerstone_searched_exact_and_weakest_last() {
        let item = ParsedItem {
            rarity: crate::itemtext::Rarity::Rare,
            name: "Test".into(),
            base_type: Some("Sandsworn Sandals".into()),
            item_class: Some("Boots".into()),
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
                    raw: "35% increased Movement Speed".into(),
                    value: Some(35.0),
                    affix: Some(crate::itemtext::Affix::Prefix),
                    tier: Some(1),
                },
                ItemStat {
                    raw: "+34% to Lightning Resistance".into(),
                    value: Some(34.0),
                    affix: Some(crate::itemtext::Affix::Suffix),
                    tier: Some(3),
                },
                ItemStat {
                    raw: "+39% to Fire Resistance".into(),
                    value: Some(39.0),
                    affix: Some(crate::itemtext::Affix::Suffix),
                    tier: Some(2),
                },
            ],
        };
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let q = build_baseline(
            &item,
            &PseudoMap::load(),
            &catalog,
            &crate::trade::value::ValueModel::default(),
            "Standard",
        );

        // Cornerstone (movement speed) is searched exact: min set, no max.
        let ms = q
            .stats
            .iter()
            .find(|s| s.label.contains("Movement Speed"))
            .expect("movement speed filter present");
        assert_eq!(ms.min, Some(35.0));
        assert_eq!(ms.max, None);

        // Resistances pseudo-group into total elemental resistance (not per-mod),
        // so assert ordering via the cornerstone being before any non-cornerstone.
        let ms_idx = q
            .stats
            .iter()
            .position(|s| s.label.contains("Movement Speed"))
            .unwrap();
        assert!(
            q.stats
                .iter()
                .enumerate()
                .all(|(i, s)| s.label.contains("Movement Speed") || i > ms_idx),
            "cornerstone must precede all non-cornerstone filters"
        );
    }

    /// Helper: returns the exact stat-id order build_baseline produces with an
    /// empty model for `staff_item()` — a cornerstone (movement speed, tier 1)
    /// followed by two normals (spell damage tier 2, rarity tier 3). With an empty
    /// model, normals sort strongest→weakest by tier (lower tier number first),
    /// so spell damage precedes rarity. Captured from the cold-start run before
    /// value feedback was added; serves as the regression oracle for
    /// `empty_model_reproduces_cold_start_query`.
    fn expected_cold_start_order() -> Vec<&'static str> {
        // cornerstone first (dropped last), then normals by tier ascending:
        // movement speed (tier 1, cornerstone) → spell damage (tier 2) → rarity (tier 3).
        vec![
            "explicit.stat_movespeed",
            "explicit.stat_spell_dmg",
            "explicit.stat_rarity",
        ]
    }

    /// Staff fixture with one cornerstone (movement speed) and two non-cornerstone
    /// explicits — spell damage and increased rarity — all matched by the sample
    /// stat catalog. The two normals let tests distinguish driver-vs-plain
    /// relaxation order: spell damage is the seeded driver, rarity stays a plain
    /// normal.
    fn staff_item() -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare,
            name: "Onslaught Spell".into(),
            base_type: Some("Chiming Staff".into()),
            item_class: Some("Staves".into()),
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
                    raw: "35% increased Movement Speed".into(),
                    value: Some(35.0),
                    affix: Some(crate::itemtext::Affix::Prefix),
                    tier: Some(1),
                },
                ItemStat {
                    raw: "80% increased Spell Damage".into(),
                    value: Some(80.0),
                    affix: Some(crate::itemtext::Affix::Prefix),
                    tier: Some(2),
                },
                ItemStat {
                    raw: "20% increased Rarity of Items found".into(),
                    value: Some(20.0),
                    affix: Some(crate::itemtext::Affix::Suffix),
                    tier: Some(3),
                },
            ],
        }
    }

    /// Build a ValueModel where `driver_stat_id` is a strong value driver for the
    /// staff category: ≥MIN_CATEGORY_SAMPLE observations, the driver appears in
    /// ≥MIN_STAT_SAMPLE of them at high price, giving lift ≥DRIVER_LIFT. Items
    /// without the driver stat have a low base price; items with it have a high
    /// price. Observations log the clipboard-plural category text ("Staves"), so
    /// they round-trip through `canonical_category` ("Staves" → "Staff") exactly
    /// like real harvest/paste data and like the item under test (item_class
    /// "Staves") — a future divergence in that mapping would break this test.
    fn seed_staff_model_with_driver(driver_stat_id: &str) -> crate::trade::value::ValueModel {
        use crate::observe::{Observation, Source};
        use crate::trade::model::ListingMod;
        use crate::trade::value::MIN_CATEGORY_SAMPLE;

        let mut obs: Vec<Observation> = Vec::new();

        // Category logged as the clipboard-plural "Staves"; `ValueModel::build`
        // folds it through `canonical_category` to "Staff".
        // Background: 35 cheap staves without the driver (price 1.0 each).
        for i in 0..35 {
            obs.push(Observation {
                timestamp_unix: i,
                league: "Standard".into(),
                base_type: Some("Chiming Staff".into()),
                category: Some("Staves".into()),
                mods: vec![ListingMod {
                    stat_id: "explicit.stat_cast_speed".into(),
                    tier: None,
                    roll: None,
                }],
                price_divine: 1.0,
                source: Source::Harvest,
                indexed: None,
            });
        }

        // Driver group: 20 expensive staves WITH the driver stat (price 10.0 each).
        // This gives: count=20 >= MIN_STAT_SAMPLE(15),
        //   median_with ≈ 10.0, median_without ≈ 1.0,
        //   lift = 10.0 / 1.0 = 10.0 >= DRIVER_LIFT(1.5). ✓
        for i in 35..55 {
            obs.push(Observation {
                timestamp_unix: i,
                league: "Standard".into(),
                base_type: Some("Chiming Staff".into()),
                category: Some("Staves".into()),
                mods: vec![ListingMod {
                    stat_id: driver_stat_id.into(),
                    tier: None,
                    roll: None,
                }],
                price_divine: 10.0,
                source: Source::Harvest,
                indexed: None,
            });
        }

        // sample_size = 55 >= MIN_CATEGORY_SAMPLE(50). ✓
        assert!(obs.len() >= MIN_CATEGORY_SAMPLE);
        crate::trade::value::ValueModel::build(&obs)
    }

    #[test]
    fn empty_model_reproduces_cold_start_query() {
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let item = staff_item();
        let pseudo = PseudoMap::load();
        let empty = crate::trade::value::ValueModel::default();

        let q = build_baseline(&item, &pseudo, &catalog, &empty, "Standard");

        // Same stat ids, same order, same bands as the pre-feedback baseline.
        // Cornerstone first (dropped last), then normal strongest→weakest by tier.
        let ids: Vec<&str> = q.stats.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, expected_cold_start_order());

        // Bands are the wide defaults (not driver-tightened).
        let spell = q
            .stats
            .iter()
            .find(|s| s.id == "explicit.stat_spell_dmg")
            .unwrap();
        // band(80.0): lo = round(0.9 * 80) = 72, hi = round(1.4 * 80) = 112
        assert_eq!(spell.min, Some(72.0));
        assert_eq!(spell.max, Some(112.0));
    }

    #[test]
    fn trusted_model_makes_drivers_survive_and_tighten() {
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let item = staff_item();
        let pseudo = PseudoMap::load();

        // Build a model where spell_dmg is a strong driver for the Staff category.
        let model = seed_staff_model_with_driver("explicit.stat_spell_dmg");

        let q = build_baseline(&item, &pseudo, &catalog, &model, "Standard");

        let driver = q
            .stats
            .iter()
            .find(|s| s.id == "explicit.stat_spell_dmg")
            .expect("driver stat must be present in query");

        // Driver uses the tight band: driver_band(80.0): lo = round(0.95 * 80) = 76, hi = round(1.2 * 80) = 96
        assert_eq!(driver.min, Some(76.0), "driver should use tight lo band");
        assert_eq!(driver.max, Some(96.0), "driver should use tight hi band");

        // Ratio check: tight band gives hi/lo < 1.35
        let ratio = driver.max.unwrap() / driver.min.unwrap();
        assert!(
            ratio < 1.35,
            "driver should use the tight band, got ratio {ratio}"
        );

        // The plain (non-driver) normal — increased rarity — is absent from the
        // seeded model, so it keeps the LOOSE default band, distinguishing the two
        // band tiers.
        let plain = q
            .stats
            .iter()
            .find(|s| s.id == "explicit.stat_rarity")
            .expect("plain normal stat must be present in query");
        // band(20.0): lo = round(0.9 * 20) = 18, hi = round(1.4 * 20) = 28
        assert_eq!(
            plain.min,
            Some(18.0),
            "plain normal should use loose lo band"
        );
        assert_eq!(
            plain.max,
            Some(28.0),
            "plain normal should use loose hi band"
        );
        let plain_ratio = plain.max.unwrap() / plain.min.unwrap();
        assert!(
            plain_ratio > 1.4,
            "plain normal should use the loose band, got ratio {plain_ratio}"
        );

        // Ordering: cornerstone first (dropped last), then among normals the driver
        // outranks the plain normal — proving the value-driver survives relaxation
        // LONGER than a plain mod (gather_comparables drops the last stat first).
        let corner_pos = q
            .stats
            .iter()
            .position(|s| s.id == "explicit.stat_movespeed")
            .unwrap();
        let driver_pos = q
            .stats
            .iter()
            .position(|s| s.id == "explicit.stat_spell_dmg")
            .unwrap();
        let plain_pos = q
            .stats
            .iter()
            .position(|s| s.id == "explicit.stat_rarity")
            .unwrap();
        assert!(driver_pos > corner_pos, "cornerstone must precede driver");
        assert!(
            driver_pos < plain_pos,
            "driver (pos {driver_pos}) must outrank plain normal (pos {plain_pos}) \
             so it relaxes later"
        );
    }
}
