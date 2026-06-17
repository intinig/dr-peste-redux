//! Builds a `TradeQuery` from a parsed item (pseudo-preferred for fungible
//! groups) and serializes it to the trade2 search payload.

use serde_json::{json, Value};

use crate::itemtext::ParsedItem;
use crate::trade::model::{MiscFilters, StatFilter, TradeQuery};
use crate::trade::pseudo::PseudoMap;

pub fn build_baseline(item: &ParsedItem, pseudo: &PseudoMap, league: &str) -> TradeQuery {
    let all_stats: Vec<_> = item
        .implicits
        .iter()
        .chain(&item.enchants)
        .chain(&item.runes)
        .chain(&item.explicits)
        .cloned()
        .collect();

    let mut stats: Vec<StatFilter> = Vec::new();

    // v1 note: only pseudo-mapped stats are emitted as filters here.
    // Non-pseudo explicit mods (e.g. flat "increased Spell Damage") are not yet
    // constrained — mapping explicit mod ids to trade2 stat ids is a documented
    // follow-up.

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
            stats.push(StatFilter {
                id: ps.id.clone(),
                label: ps.label.clone(),
                min: Some(ps.total),
                max: None,
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

    let mut query = json!({
        "status": { "option": "online" },
        "stats": [ { "type": "and", "filters": stat_filters } ],
        "filters": {
            "type_filters": { "filters": type_filters },
            "misc_filters": { "filters": misc_filters },
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
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat {
                    raw: "+40 to maximum Life".into(),
                    value: Some(40.0),
                },
                ItemStat {
                    raw: "+32% to Fire Resistance".into(),
                    value: Some(32.0),
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
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
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat {
                    raw: "+32% to Fire Resistance".into(),
                    value: Some(32.0),
                },
                ItemStat {
                    raw: "+18% to Lightning Resistance".into(),
                    value: Some(18.0),
                },
            ],
        };
        let q = build_baseline(&item, &PseudoMap::load(), "Standard");
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
        let q = build_baseline(&ring(), &PseudoMap::load(), "Standard");
        assert_eq!(q.league, "Standard");
        assert_eq!(q.type_line.as_deref(), Some("Sapphire Ring"));
        let ele = q
            .stats
            .iter()
            .find(|s| s.id == "pseudo.pseudo_total_elemental_resistance")
            .unwrap();
        assert_eq!(ele.min, Some(50.0));
        assert!(!q.stats.iter().any(|s| s.label.contains("Fire Resistance")));
        assert!(q
            .stats
            .iter()
            .any(|s| s.id == "pseudo.pseudo_total_life" && s.min == Some(40.0)));
    }

    #[test]
    fn payload_has_status_type_and_sort() {
        let q = build_baseline(&ring(), &PseudoMap::load(), "Standard");
        let payload = to_payload(&q);
        assert_eq!(payload["query"]["status"]["option"], "online");
        assert_eq!(payload["query"]["type"], "Sapphire Ring");
        assert_eq!(payload["sort"]["price"], "asc");
    }
}
