use poise::serenity_prelude as serenity;

use crate::poeninja::model::PricedItem;
use crate::poeninja::League;

/// Picks a human-friendly value string: divine if ≥1 divine, else exalted if
/// ≥1 exalted, else chaos.
pub fn best_price_string(it: &PricedItem) -> String {
    if it.value_divine >= 1.0 {
        format!("{:.2} divine", it.value_divine)
    } else if it.value_exalted >= 1.0 {
        format!("{:.1} exalted", it.value_exalted)
    } else {
        format!("{:.1} chaos", it.value_chaos)
    }
}

pub fn trend_string(change: f64) -> String {
    let arrow = if change > 0.5 {
        "📈"
    } else if change < -0.5 {
        "📉"
    } else {
        "➡️"
    };
    format!("{arrow} {change:+.1}%")
}

fn ninja_url(it: &PricedItem, league: &League) -> String {
    format!(
        "https://poe.ninja/poe2/economy/{}/{}/{}",
        league.url, it.slug, it.details_id
    )
}

pub fn item_embed(it: &PricedItem, league: &League) -> serenity::CreateEmbed {
    let mut e = serenity::CreateEmbed::default()
        .title(it.name.clone())
        .url(ninja_url(it, league))
        .field("Value", best_price_string(it), true)
        .field("Trend", trend_string(it.change_pct), true)
        .field("Category", it.category.clone(), true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "poe.ninja • {}",
            league.name
        )));
    if let Some(base) = &it.base_type {
        e = e.description(base.clone());
    }
    if let Some(icon) = &it.icon_url {
        e = e.thumbnail(icon.clone());
    }
    e
}

pub fn farm_embed(title: &str, items: &[&PricedItem], league: &League) -> serenity::CreateEmbed {
    let body = if items.is_empty() {
        "No items matched the current filter.".to_string()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                format!(
                    "**{}. {}** — {} ({})",
                    i + 1,
                    it.name,
                    best_price_string(it),
                    trend_string(it.change_pct)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    serenity::CreateEmbed::default()
        .title(title)
        .description(body)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "poe.ninja • {} • ranked from live data",
            league.name
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(divine: f64, exalted: f64, chaos: f64) -> PricedItem {
        PricedItem {
            name: "X".into(),
            base_type: None,
            category: "Currency".into(),
            slug: "currency".into(),
            details_id: "x".into(),
            value_chaos: chaos,
            value_exalted: exalted,
            value_divine: divine,
            change_pct: 0.0,
            volume: 0.0,
            icon_url: None,
        }
    }

    #[test]
    fn price_string_picks_largest_sensible_unit() {
        assert_eq!(best_price_string(&item(2.0, 200.0, 2000.0)), "2.00 divine");
        assert_eq!(best_price_string(&item(0.5, 90.0, 900.0)), "90.0 exalted");
        assert_eq!(best_price_string(&item(0.001, 0.2, 2.5)), "2.5 chaos");
    }

    #[test]
    fn trend_string_has_direction() {
        assert!(trend_string(5.0).contains("+5.0%"));
        assert!(trend_string(-5.0).contains("-5.0%"));
    }
}
