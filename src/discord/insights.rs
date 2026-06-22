//! `/insights [category]` — surfaces the learned ValueModel: which mods drive
//! price for a category, scoped to the active league. Read-only; open to
//! everyone (non-secret market data).

use super::{Context, Error};
use crate::trade::value::{canonical_category, MIN_CATEGORY_SAMPLE};
use futures::Stream;
use poise::serenity_prelude as serenity;

/// The active league name from the store snapshot, if the bot has warmed up.
async fn current_league(ctx: &Context<'_>) -> Option<String> {
    ctx.data()
        .store
        .snapshot()
        .await
        .map(|s| s.league.name.clone())
}

/// Autocomplete: canonical category names with data in the active league,
/// substring-matched.
pub async fn autocomplete_insights_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let p = partial.to_lowercase();
    let names: Vec<String> = match current_league(&ctx).await {
        Some(league) => {
            let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
            model
                .categories_sorted(&league)
                .into_iter()
                .map(|c| c.category.clone())
                .filter(|name| name.to_lowercase().contains(&p))
                .take(25)
                .collect()
        }
        None => Vec::new(),
    };
    futures::stream::iter(names)
}

/// Show learned value-drivers for a category (or list categories with no arg).
#[poise::command(slash_command)]
pub async fn insights(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff). Omit to list categories."]
    #[autocomplete = "autocomplete_insights_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let Some(league) = current_league(&ctx).await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    // Build the embed under the value-model lock, then drop the guard before .await.
    let embed: serenity::CreateEmbed = {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        match category.as_deref() {
            None => {
                // Menu: only categories trusted enough to give reliable insights.
                let trusted: Vec<_> = model
                    .categories_sorted(&league)
                    .into_iter()
                    .filter(|c| c.sample_size >= MIN_CATEGORY_SAMPLE)
                    .collect();
                if trusted.is_empty() {
                    serenity::CreateEmbed::default().title("Market insights").description(format!(
                        "No category has enough data yet for **{league}** (need ≥{MIN_CATEGORY_SAMPLE} listings). Run `/harvest <category>` to warm one up."
                    ))
                } else {
                    let mut lines = String::new();
                    for c in trusted.iter().take(25) {
                        lines.push_str(&format!(
                            "• **{}** — {} listings (median {:.1} div)\n",
                            c.category, c.sample_size, c.base_median
                        ));
                    }
                    lines.push_str("\nPass one, e.g. `/insights category:Staff`.");
                    serenity::CreateEmbed::default()
                        .title(format!("Market insights — {league}"))
                        .description(lines)
                }
            }
            Some(raw) => {
                let canon = canonical_category(raw);
                match model.category(&league, &canon) {
                    None => serenity::CreateEmbed::default()
                        .title(canon.clone())
                        .description(format!("No market data yet for **{canon}** in {league}.")),
                    Some(cat) if cat.sample_size < MIN_CATEGORY_SAMPLE => {
                        serenity::CreateEmbed::default().title(canon.clone()).description(format!(
                            "Only {} listings for **{canon}** so far (need ≥{MIN_CATEGORY_SAMPLE} for reliable insights). Harvest more.",
                            cat.sample_size
                        ))
                    }
                    Some(cat) => {
                        // Clone so we can drop the model read-guard before the catalog lookup.
                        let cat = cat.clone();
                        let (sample_size, base_median) = (cat.sample_size, cat.base_median);
                        drop(model);
                        let catalog = ctx.data().pricer.catalog();
                        // Resolve label: pre-stored label, else reverse-lookup via
                        // the catalog, else fall back to the raw stat id.
                        let label = |s_id: &str, s_label: &Option<String>| -> String {
                            s_label
                                .as_deref()
                                .or_else(|| catalog.label_for(s_id))
                                .unwrap_or(s_id)
                                .to_string()
                        };

                        let mut body = String::from("**Value drivers** (independent lift in parens):\n");
                        let mut any = false;
                        for s in cat.drivers().take(8) {
                            any = true;
                            let cond = match s.conditional_lift {
                                Some(c) => format!(" (independent {c:.1}×)"),
                                None => String::new(),
                            };
                            body.push_str(&format!(
                                "• **{}** — {:.1}× ({:.1} div){} · in {:.0}% of priciest · n={}\n",
                                label(&s.stat_id, &s.label),
                                s.lift,
                                s.median_with,
                                cond,
                                s.top_decile_freq * 100.0,
                                s.count
                            ));
                        }
                        if !any {
                            body.push_str("_(no mod clears the value-driver threshold yet)_\n");
                        }
                        if !cat.cooccurrences.is_empty() {
                            body.push_str("\n**Top combos on expensive items:**\n");
                            for p in cat.cooccurrences.iter().take(5) {
                                body.push_str(&format!(
                                    "• {} + {} (n={})\n",
                                    label(&p.a, &None),
                                    label(&p.b, &None),
                                    p.count
                                ));
                            }
                        }
                        serenity::CreateEmbed::default()
                            .title(format!("{canon} — value drivers"))
                            .description(body)
                            .footer(serenity::CreateEmbedFooter::new(format!(
                                "{sample_size} listings · median {base_median:.1} div · {league}"
                            )))
                    }
                }
            }
        }
    };
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
