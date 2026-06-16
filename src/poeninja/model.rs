use std::collections::HashMap;

use serde::Deserialize;

use super::categories::Category;

/// Normalized, Discord-ready representation of a priced item.
#[derive(Clone, Debug, PartialEq)]
pub struct PricedItem {
    pub name: String,
    pub base_type: Option<String>,
    pub category: String,
    pub slug: String,
    pub details_id: String,
    pub value_chaos: f64,
    pub value_exalted: f64,
    pub value_divine: f64,
    pub change_pct: f64,
    pub volume: f64,
    pub icon_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Sparkline {
    #[serde(rename = "totalChange", default)]
    pub total_change: f64,
}

#[derive(Debug, Deserialize)]
pub struct Core {
    #[serde(default = "default_primary")]
    pub primary: String,
    #[serde(default)]
    pub rates: HashMap<String, f64>,
}

fn default_primary() -> String {
    "divine".to_string()
}

// ---- Exchange family ----

#[derive(Debug, Deserialize)]
pub struct ExchangeOverview {
    pub core: Core,
    #[serde(default)]
    pub lines: Vec<ExchangeLine>,
    #[serde(default)]
    pub items: Vec<ExchangeItem>,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(rename = "detailsId", default)]
    pub details_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeLine {
    pub id: String,
    #[serde(rename = "primaryValue")]
    pub primary_value: f64,
    #[serde(rename = "volumePrimaryValue", default)]
    pub volume_primary_value: f64,
    #[serde(default)]
    pub sparkline: Sparkline,
}

// ---- Stash item family ----

#[derive(Debug, Deserialize)]
pub struct ItemOverview {
    pub core: Core,
    #[serde(default)]
    pub lines: Vec<ItemLine>,
}

#[derive(Debug, Deserialize)]
pub struct ItemLine {
    pub name: String,
    #[serde(rename = "baseType", default)]
    pub base_type: Option<String>,
    #[serde(rename = "detailsId", default)]
    pub details_id: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(rename = "primaryValue")]
    pub primary_value: f64,
    #[serde(rename = "listingCount", default)]
    pub listing_count: f64,
    #[serde(rename = "sparkLine", default)]
    pub spark_line: Sparkline,
}

// ---- Conversion ----

/// Returns (chaos, exalted, divine) values for a primary-denominated price.
fn convert(core: &Core, primary_value: f64) -> (f64, f64, f64) {
    let to = |target: &str| -> f64 {
        if core.primary == target {
            primary_value
        } else {
            primary_value * core.rates.get(target).copied().unwrap_or(0.0)
        }
    };
    (to("chaos"), to("exalted"), to("divine"))
}

fn absolute_icon(path: String) -> String {
    if path.starts_with("http") {
        path
    } else {
        format!("https://poe.ninja{path}")
    }
}

pub fn normalize_exchange(cat: &Category, ov: ExchangeOverview) -> Vec<PricedItem> {
    let meta: HashMap<&str, &ExchangeItem> = ov.items.iter().map(|i| (i.id.as_str(), i)).collect();
    ov.lines
        .iter()
        .filter_map(|line| {
            let item = meta.get(line.id.as_str())?;
            let (chaos, exalted, divine) = convert(&ov.core, line.primary_value);
            Some(PricedItem {
                name: item.name.clone(),
                base_type: None,
                category: cat.display.to_string(),
                slug: cat.slug.to_string(),
                details_id: item.details_id.clone(),
                value_chaos: chaos,
                value_exalted: exalted,
                value_divine: divine,
                change_pct: line.sparkline.total_change,
                volume: line.volume_primary_value,
                icon_url: item.image.clone().map(absolute_icon),
            })
        })
        .collect()
}

pub fn normalize_item(cat: &Category, ov: ItemOverview) -> Vec<PricedItem> {
    ov.lines
        .iter()
        .map(|line| {
            let (chaos, exalted, divine) = convert(&ov.core, line.primary_value);
            PricedItem {
                name: line.name.clone(),
                base_type: line.base_type.clone(),
                category: cat.display.to_string(),
                slug: cat.slug.to_string(),
                details_id: line.details_id.clone(),
                value_chaos: chaos,
                value_exalted: exalted,
                value_divine: divine,
                change_pct: line.spark_line.total_change,
                volume: line.listing_count,
                icon_url: line.icon.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poeninja::categories::by_slug;

    #[test]
    fn normalizes_exchange_with_join_and_conversion() {
        let ov: ExchangeOverview =
            serde_json::from_str(include_str!("fixtures/exchange_currency.json")).unwrap();
        let items = normalize_exchange(by_slug("currency").unwrap(), ov);
        assert_eq!(items.len(), 2);

        let divine = items.iter().find(|i| i.name == "Divine Orb").unwrap();
        assert_eq!(divine.value_divine, 1.0);
        assert!((divine.value_chaos - 11.01).abs() < 1e-6);
        assert!((divine.value_exalted - 184.7).abs() < 1e-6);
        assert_eq!(divine.change_pct, 74.02);
        assert_eq!(divine.category, "Currency");
        assert_eq!(
            divine.icon_url.as_deref(),
            Some("https://poe.ninja/gen/image/divine.png")
        );
    }

    #[test]
    fn normalizes_stash_item() {
        let ov: ItemOverview =
            serde_json::from_str(include_str!("fixtures/item_uniqueweapons.json")).unwrap();
        let items = normalize_item(by_slug("unique-weapons").unwrap(), ov);
        assert_eq!(items.len(), 1);

        let d = &items[0];
        assert_eq!(d.name, "The Dancing Dervish");
        assert_eq!(d.base_type.as_deref(), Some("Scimitar"));
        assert_eq!(d.value_divine, 5822.0);
        assert!((d.value_chaos - 5822.0 * 11.27).abs() < 1e-3);
        assert_eq!(d.volume, 2.0);
        assert_eq!(
            d.icon_url.as_deref(),
            Some("https://web.poecdn.com/gen/image/dervish.png")
        );
    }
}
