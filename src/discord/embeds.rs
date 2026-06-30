use poise::serenity_prelude as serenity;

use crate::arb::model::Opportunity;
use crate::itemtext::ParsedItem;
use crate::poeninja::model::PricedItem;
use crate::poeninja::League;
use crate::trade::model::{Breakdown, Confidence, Contribution, Currency, PriceEstimate};

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

pub fn div_str(v: f64) -> String {
    // Finer precision below 1 div so cheap items don't render as "0.0 div".
    if v >= 1.0 {
        format!("{v:.1} div")
    } else {
        format!("{v:.2} div")
    }
}

/// Formats a divine value, and when the market prices this item in a non-divine
/// currency, appends the equivalent on a second line (e.g. "0.10 div\n≈ 20 ex").
/// `div_per_unit` is the divine value of one unit of `modal` (from the live rate
/// table); a missing/non-positive rate or a Divine modal omits the second line.
pub fn value_lines(div: f64, modal: &Currency, div_per_unit: Option<f64>) -> String {
    let main = div_str(div);
    match div_per_unit {
        Some(rate) if rate > 0.0 && div > 0.0 && !matches!(modal, Currency::Divine) => {
            format!("{main}\n≈ {:.0} {}", div / rate, modal.short())
        }
        _ => main,
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

/// Secondary `/paste` fallback line shown when live ablation has no price: a corpus
/// range labelled with its confidence. Takes the item name (not `ParsedItem`) so it is
/// trivially testable.
pub fn range_fallback_line(
    item_name: &str,
    r: &crate::trade::value::estimate::RangeEstimate,
) -> String {
    use crate::trade::value::estimate::Confidence;
    let conf = match r.confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    };
    format!(
        "📊 **{}** — No live trade price · corpus estimate **{}–{}** (fair ~{}) · {} confidence",
        item_name,
        div_str(r.floor),
        div_str(r.ask),
        div_str(r.fair),
        conf
    )
}

/// One-line description of which comparable set the estimate used.
fn tier_note(parsed: &ParsedItem, est: &PriceEstimate) -> String {
    use crate::trade::model::EstimateBasis::*;
    match est.basis {
        CraftTier => {
            let c = parsed.craftability();
            let open_p = c.map(|c| c.open_prefixes).unwrap_or(0);
            let open_s = c.map(|c| c.open_suffixes).unwrap_or(0);
            format!(
                "clean base · {open_p} open prefix(es), {open_s} open suffix(es) · {} comparable listings",
                est.listing_count
            )
        }
        BroadMarket => "broad-market estimate — no comparable open-base listings".to_string(),
        AffixesOnly => {
            "affixes present; craftability not detected — paste in Advanced Mode for a sharper estimate"
                .to_string()
        }
    }
}

pub fn estimate_embed(
    parsed: &ParsedItem,
    est: &PriceEstimate,
    league: &League,
    secondary_rate: Option<f64>,
) -> serenity::CreateEmbed {
    let title = parsed
        .base_type
        .clone()
        .unwrap_or_else(|| parsed.name.clone());
    let mut embed = serenity::CreateEmbed::default()
        .title(title)
        .description(format!("**{}**", parsed.name));

    if est.listing_count == 0 {
        embed = embed.field("Estimated value", "No comparable listings found", false);
    } else {
        embed = embed
            .field(
                "Quick sale",
                value_lines(est.low, &est.modal_currency, secondary_rate),
                true,
            )
            .field(
                "Fair",
                value_lines(est.typical, &est.modal_currency, secondary_rate),
                true,
            )
            .field(
                "Patient",
                value_lines(est.high, &est.modal_currency, secondary_rate),
                true,
            )
            .field(
                "Confidence",
                format!(
                    "{} ({} listings)",
                    confidence_string(&est.confidence),
                    est.listing_count
                ),
                false,
            )
            .field("Priced as", tier_note(parsed, est), false);
    }

    embed.footer(serenity::CreateEmbedFooter::new(format!(
        "live trade • {} • not affiliated with GGG",
        league.name
    )))
}

/// Message shown on `/paste` when an item prices under 1 divine: report it's cheap
/// rather than estimating a precise sub-1-div value. Takes the item name (not the
/// whole `ParsedItem`) so it is trivially testable.
pub fn sub_one_div_message(item_name: &str) -> String {
    format!("💸 **{item_name}** — worth under 1 divine. Not worth pricing precisely.")
}

pub fn breakdown_embed(
    parsed: &ParsedItem,
    bd: &Breakdown,
    league: &League,
) -> serenity::CreateEmbed {
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

pub fn arb_embed(opps: &[Opportunity], league: &str) -> serenity::CreateEmbed {
    let mut lines = String::new();
    for (i, o) in opps.iter().enumerate() {
        match o {
            Opportunity::Triangulation {
                legs,
                multiplier,
                feasible_volume,
                ..
            } => {
                let path = std::iter::once(legs[0].from.as_str())
                    .chain(legs.iter().map(|l| l.to.as_str()))
                    .collect::<Vec<_>>()
                    .join(" → ");
                lines.push_str(&format!(
                    "**{}. Cycle** `{}`  +{:.1}%  (~{:.0} vol)\n",
                    i + 1,
                    path,
                    (multiplier - 1.0) * 100.0,
                    feasible_volume
                ));
            }
            Opportunity::Flip {
                market,
                spread_pct,
                volume,
                ..
            } => {
                lines.push_str(&format!(
                    "**{}. Flip** `{} / {}`  {:.1}% spread  (~{:.0} vol)\n",
                    i + 1,
                    market.0,
                    market.1,
                    spread_pct * 100.0,
                    volume
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push_str("Nothing above thresholds.");
    }
    // Discord's embed description cap is 4096 chars; truncate to avoid a silent send failure.
    const DESC_CAP: usize = 4000;
    if lines.len() > DESC_CAP {
        lines.truncate(DESC_CAP);
        lines.push_str("\n… (truncated)");
    }
    serenity::CreateEmbed::new()
        .title("⚖️ Currency arbitrage")
        .description(lines)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "{league} • execute manually in-game; ratios move fast"
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::parse;
    use crate::trade::model::{Confidence, Currency, EstimateBasis, PriceEstimate};

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

    fn est(basis: EstimateBasis, n: usize) -> PriceEstimate {
        PriceEstimate {
            low: 1.0,
            typical: 2.0,
            high: 3.0,
            listing_count: n,
            confidence: Confidence::from_count(n),
            modal_currency: Currency::Divine,
            basis,
        }
    }

    const BOOTS: &str = "Item Class: Boots\nRarity: Rare\nKraken Slippers\nSandsworn Sandals\n--------\nItem Level: 83\n--------\n{ Prefix Modifier \"Hellion's\" (Tier: 1) }\n35% increased Movement Speed\n{ Suffix Modifier \"of Archaeology\" (Tier: 1) }\n16% increased Rarity of Items found\n";

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

    use crate::trade::model::{AblationKind, Contribution};

    #[test]
    fn div_str_formats_by_magnitude() {
        assert_eq!(div_str(8.0), "8.0 div");
        assert_eq!(div_str(1.0), "1.0 div");
        // sub-1-div values get finer precision so cheap items aren't "0.0 div"
        assert_eq!(div_str(0.04), "0.04 div");
    }

    #[test]
    fn value_lines_appends_modal_currency_when_not_divine() {
        use crate::trade::model::Currency;
        // 1 ex = 0.005 div → 0.1 div = 20 ex
        let s = value_lines(0.1, &Currency::Exalted, Some(0.005));
        assert!(s.contains("0.10 div"));
        assert!(s.contains("≈ 20 ex"));
        // divine modal → no second line
        assert_eq!(value_lines(0.1, &Currency::Divine, Some(0.005)), "0.10 div");
        // no rate → just divine
        assert_eq!(value_lines(0.1, &Currency::Exalted, None), "0.10 div");
    }

    #[test]
    fn confidence_string_high() {
        assert_eq!(confidence_string(&Confidence::High), "High");
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

    #[test]
    fn tier_note_describes_craft_tier() {
        let p = parse(BOOTS).unwrap();
        let note = tier_note(&p, &est(EstimateBasis::CraftTier, 7));
        assert!(note.contains("open prefix"), "{note}");
        assert!(note.contains("7"), "{note}");
    }

    #[test]
    fn tier_note_flags_broad_market() {
        let p = parse(BOOTS).unwrap();
        let note = tier_note(&p, &est(EstimateBasis::BroadMarket, 30));
        assert!(note.to_lowercase().contains("broad-market"), "{note}");
    }

    fn league() -> crate::poeninja::League {
        crate::poeninja::League {
            name: "Standard".into(),
            url: "standard".into(),
        }
    }

    #[test]
    fn estimate_embed_shows_price_fields() {
        let p = parse(BOOTS).unwrap();
        let e = est(EstimateBasis::CraftTier, 15);
        let embed = estimate_embed(&p, &e, &league(), None);
        let json = serde_json::to_string(&embed).unwrap();
        assert!(json.contains("Quick sale"), "field missing: {json}");
        assert!(
            !json.contains("Learned (corpus)"),
            "dead field present: {json}"
        );
    }

    #[test]
    fn estimate_embed_zero_listings_shows_no_comparable() {
        let p = parse(BOOTS).unwrap();
        let e = est(EstimateBasis::CraftTier, 0);
        let embed = estimate_embed(&p, &e, &league(), None);
        let json = serde_json::to_string(&embed).unwrap();
        assert!(
            json.contains("No comparable listings found"),
            "expected fallback field: {json}"
        );
    }

    #[test]
    fn sub_one_div_message_names_the_item_and_says_under_one_div() {
        let m = super::sub_one_div_message("Chiming Staff");
        assert!(m.contains("Chiming Staff"));
        assert!(m.contains("under 1 divine"));
    }

    #[test]
    fn range_fallback_line_shows_floor_fair_ask_and_confidence() {
        use crate::trade::value::estimate::{Confidence, RangeEstimate};
        let r = RangeEstimate {
            floor: 5.0,
            fair: 12.0,
            ask: 30.0,
            confidence: Confidence::Low,
            pool: 14,
        };
        let line = super::range_fallback_line("Chiming Staff", &r);
        assert!(line.contains("Chiming Staff"), "{line}");
        assert!(line.contains("No live trade price"), "{line}");
        assert!(
            line.contains('5') && line.contains("30") && line.contains("12"),
            "floor/ask/fair: {line}"
        );
        assert!(
            line.to_lowercase().contains("low"),
            "confidence label: {line}"
        );
    }
}
