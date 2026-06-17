use poise::serenity_prelude as serenity;

use crate::itemtext::ParsedItem;
use crate::poeninja::model::PricedItem;
use crate::poeninja::League;
use crate::trade::model::{Breakdown, Confidence, Contribution, PriceEstimate};

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
        .field("Volume", format!("{:.0}", it.volume), true)
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

pub fn estimate_value_string(est: &PriceEstimate) -> String {
    if est.listing_count == 0 {
        return "No comparable listings".to_string();
    }
    if (est.high - est.low).abs() < f64::EPSILON {
        format!("~{:.1} div", est.typical)
    } else {
        format!("{:.1}–{:.1} div", est.low, est.high)
    }
}

pub fn confidence_string(c: &Confidence) -> String {
    match c {
        Confidence::High => "High",
        Confidence::Medium => "Medium",
        Confidence::Low => "Low",
    }
    .to_string()
}

pub fn contribution_line(c: &Contribution) -> String {
    format!("• {} — ~{:.1} div", c.characteristic, c.delta_divine)
}

pub fn estimate_embed(parsed: &ParsedItem, est: &PriceEstimate, league: &League) -> serenity::CreateEmbed {
    let title = parsed.base_type.clone().unwrap_or_else(|| parsed.name.clone());
    serenity::CreateEmbed::default()
        .title(title)
        .description(format!("**{}**", parsed.name))
        .field("Estimated value", estimate_value_string(est), true)
        .field(
            "Confidence",
            format!("{} ({} listings)", confidence_string(&est.confidence), est.listing_count),
            true,
        )
        .footer(serenity::CreateEmbedFooter::new(format!(
            "live trade • {} • not affiliated with GGG",
            league.name
        )))
}

pub fn breakdown_embed(parsed: &ParsedItem, bd: &Breakdown, league: &League) -> serenity::CreateEmbed {
    let mut lines: Vec<String> = bd.ranked.iter().map(contribution_line).collect();
    if let Some(syn) = &bd.synergy {
        lines.push(format!(
            "✨ synergy: **{}** + **{}** add ~{:.1} div together",
            syn.a, syn.b, syn.extra_divine
        ));
    }
    let body = if lines.is_empty() {
        "No drivers identified.".to_string()
    } else {
        lines.join("\n")
    };
    serenity::CreateEmbed::default()
        .title(format!("What drives the price — {}", parsed.name))
        .description(body)
        .url(bd.trade_url.clone())
        .footer(serenity::CreateEmbedFooter::new(format!(
            "live trade • {} • not affiliated with GGG",
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

    #[test]
    fn ninja_url_uses_poe2_economy_path() {
        let mut it = item(0.0, 0.0, 5.0);
        it.slug = "currency".into();
        it.details_id = "divine-orb".into();
        let league = League {
            name: "Runes of Aldur".into(),
            url: "runesofaldur".into(),
        };
        assert_eq!(
            ninja_url(&it, &league),
            "https://poe.ninja/poe2/economy/runesofaldur/currency/divine-orb"
        );
    }

    use crate::trade::model::{AblationKind, Confidence, Contribution, PriceEstimate};

    #[test]
    fn estimate_value_string_formats_range_and_confidence() {
        let est = PriceEstimate {
            low: 8.0,
            typical: 8.0,
            high: 15.0,
            listing_count: 12,
            confidence: Confidence::High,
        };
        let s = estimate_value_string(&est);
        assert!(s.contains("8"));
        assert!(s.contains("15"));
        assert_eq!(confidence_string(&est.confidence), "High");
    }

    #[test]
    fn contribution_line_shows_label_and_delta() {
        let c = Contribution {
            characteristic: "+to all Spell Skills".into(),
            kind: AblationKind::Drop,
            delta_divine: 16.0,
        };
        let line = contribution_line(&c);
        assert!(line.contains("+to all Spell Skills"));
        assert!(line.contains("16"));
    }
}
