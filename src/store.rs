use std::sync::Arc;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use tokio::sync::RwLock;

use crate::itemtext::{ParsedItem, Rarity};
use crate::poeninja::model::PricedItem;
use crate::poeninja::League;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub league: League,
    pub items: Vec<PricedItem>,
}

/// Thread-safe holder for the latest snapshot. `None` until the first refresh.
#[derive(Clone, Default)]
pub struct PriceStore {
    inner: Arc<RwLock<Option<Snapshot>>>,
}

impl PriceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn replace(&self, snap: Snapshot) {
        *self.inner.write().await = Some(snap);
    }

    pub async fn snapshot(&self) -> Option<Snapshot> {
        self.inner.read().await.clone()
    }
}

pub fn find_exact<'a>(items: &'a [PricedItem], name: &str) -> Option<&'a PricedItem> {
    let q = name.trim().to_lowercase();
    items.iter().find(|it| it.name.to_lowercase() == q)
}

pub fn search<'a>(items: &'a [PricedItem], query: &str, limit: usize) -> Vec<&'a PricedItem> {
    let query = query.trim();
    if query.is_empty() {
        return items.iter().take(limit).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, &PricedItem)> = items
        .iter()
        .filter_map(|it| matcher.fuzzy_match(&it.name, query).map(|s| (s, it)))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.name.len().cmp(&b.1.name.len()))
    });
    scored.into_iter().take(limit).map(|(_, it)| it).collect()
}

#[derive(Clone, Copy, Debug)]
pub enum FarmSort {
    Value,
    Trending,
}

pub fn farm<'a>(
    items: &'a [PricedItem],
    sort: FarmSort,
    min_volume: f64,
    slug: Option<&str>,
    limit: usize,
) -> Vec<&'a PricedItem> {
    let mut filtered: Vec<&PricedItem> = items
        .iter()
        .filter(|it| it.volume >= min_volume)
        .filter(|it| slug.is_none_or(|s| it.slug == s))
        .collect();
    let key = |it: &&PricedItem| match sort {
        FarmSort::Value => it.value_chaos,
        FarmSort::Trending => it.change_pct,
    };
    filtered.sort_by(|a, b| {
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    filtered.into_iter().take(limit).collect()
}

#[derive(Debug)]
pub enum MatchOutcome<'a> {
    Found(&'a PricedItem),
    Suggestions(Vec<&'a PricedItem>),
    NotTracked,
    NotFound,
}

/// Routes a parsed pasted item to a price match. Magic/Rare gear is not priced
/// by poe.ninja and returns NotTracked.
pub fn route<'a>(items: &'a [PricedItem], parsed: &ParsedItem) -> MatchOutcome<'a> {
    if matches!(parsed.rarity, Rarity::Magic | Rarity::Rare) {
        return MatchOutcome::NotTracked;
    }
    if let Some(found) = find_exact(items, &parsed.name) {
        return MatchOutcome::Found(found);
    }
    let suggestions = search(items, &parsed.name, 3);
    if suggestions.is_empty() {
        MatchOutcome::NotFound
    } else {
        MatchOutcome::Suggestions(suggestions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, slug: &str, chaos: f64, change: f64, volume: f64) -> PricedItem {
        PricedItem {
            name: name.to_string(),
            base_type: None,
            category: slug.to_string(),
            slug: slug.to_string(),
            details_id: name.to_lowercase().replace(' ', "-"),
            value_chaos: chaos,
            value_exalted: chaos,
            value_divine: chaos / 100.0,
            change_pct: change,
            volume,
            icon_url: None,
        }
    }

    fn parsed(rarity: Rarity, name: &str, base: Option<&str>) -> ParsedItem {
        ParsedItem {
            rarity,
            name: name.into(),
            base_type: base.map(Into::into),
            item_class: None,
            item_level: None,
            quality: None,
            corrupted: false,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![],
        }
    }

    fn sample() -> Vec<PricedItem> {
        vec![
            item("Divine Orb", "currency", 11.0, 74.0, 1000.0),
            item("Exalted Orb", "currency", 0.06, -42.0, 5000.0),
            item("Mirror of Kalandra", "currency", 50000.0, 2.0, 1.0),
            item("The Dancing Dervish", "unique-weapons", 65000.0, 16.0, 2.0),
        ]
    }

    #[test]
    fn exact_match_is_case_insensitive() {
        let items = sample();
        assert_eq!(find_exact(&items, "divine orb").unwrap().name, "Divine Orb");
    }

    #[test]
    fn fuzzy_search_finds_typos() {
        let items = sample();
        let hits = search(&items, "dancing", 5);
        assert_eq!(hits[0].name, "The Dancing Dervish");
    }

    #[test]
    fn farm_by_value_respects_min_volume() {
        let items = sample();
        let top = farm(&items, FarmSort::Value, 10.0, None, 10);
        assert_eq!(top[0].name, "Divine Orb");
        assert!(top.iter().all(|i| i.name != "Mirror of Kalandra"));
    }

    #[test]
    fn farm_by_trending_sorts_by_change() {
        let items = sample();
        let top = farm(&items, FarmSort::Trending, 0.0, None, 2);
        assert_eq!(top[0].name, "Divine Orb");
    }

    #[test]
    fn farm_filters_by_category_slug() {
        let items = sample();
        let top = farm(&items, FarmSort::Value, 0.0, Some("unique-weapons"), 10);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].name, "The Dancing Dervish");
    }

    #[test]
    fn route_rejects_rare_gear() {
        let items = sample();
        let parsed = parsed(Rarity::Rare, "Corpse Bramble", Some("Vaal Regalia"));
        assert!(matches!(route(&items, &parsed), MatchOutcome::NotTracked));
    }

    #[test]
    fn route_finds_unique_by_name() {
        let items = sample();
        let parsed = parsed(Rarity::Unique, "The Dancing Dervish", Some("Scimitar"));
        assert!(matches!(route(&items, &parsed), MatchOutcome::Found(_)));
    }

    #[test]
    fn route_suggests_when_no_exact_match() {
        let items = sample();
        let parsed = parsed(Rarity::Currency, "Divine", None);
        assert!(matches!(
            route(&items, &parsed),
            MatchOutcome::Suggestions(_)
        ));
    }
}
