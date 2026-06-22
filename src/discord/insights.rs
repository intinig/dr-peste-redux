//! `/insights [category]` — surfaces the learned ValueModel: which mods drive
//! price for a category. Read-only; open to everyone (non-secret market data).

use super::{Context, Error};
use crate::trade::value::{canonical_category, MIN_CATEGORY_SAMPLE};
use futures::Stream;

/// Autocomplete: canonical category names present in the model, prefix-matched.
pub async fn autocomplete_insights_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let p = partial.to_lowercase();
    let names: Vec<String> = {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        model
            .categories_sorted()
            .into_iter()
            .map(|c| c.category.clone())
            .filter(|name| name.to_lowercase().contains(&p))
            .take(25)
            .collect()
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
    // Build the reply text under the lock, then drop the guard before .await.
    let reply: String = {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        match category.as_deref() {
            None => {
                let cats = model.categories_sorted();
                if cats.is_empty() {
                    String::from(
                        "No market data yet — run `/harvest <category>` or price some rares first.",
                    )
                } else {
                    let mut lines = String::from("**Categories with market data:**\n");
                    for c in cats.iter().take(25) {
                        lines.push_str(&format!(
                            "• **{}** — {} listings (median {:.1} div)\n",
                            c.category, c.sample_size, c.base_median
                        ));
                    }
                    lines.push_str("\nPass one, e.g. `/insights category:Staff`.");
                    lines
                }
            }
            Some(raw) => {
                let canon = canonical_category(raw);
                match model.category(&canon) {
                    None => format!("No market data yet for **{canon}**."),
                    Some(cat) if cat.sample_size < MIN_CATEGORY_SAMPLE => format!(
                        "Only {} listings for **{canon}** so far (need ≥{MIN_CATEGORY_SAMPLE} for reliable insights). Harvest more.",
                        cat.sample_size
                    ),
                    Some(cat) => {
                        // Clone so we can drop the model read-guard before the catalog lookup.
                        let cat = cat.clone();
                        // model guard is dropped at end of this block (before .await below).
                        drop(model);
                        let catalog = ctx.data().pricer.catalog();
                        // Resolve label: use the pre-stored label if available, else
                        // reverse-look up via the catalog, else fall back to the raw id.
                        let label = |s_id: &str, s_label: &Option<String>| -> String {
                            s_label
                                .as_deref()
                                .or_else(|| catalog.label_for(s_id))
                                .unwrap_or(s_id)
                                .to_string()
                        };

                        let mut body = format!(
                            "**{canon}** — {} listings · median {:.1} div\n\n**Value drivers** (independent lift in parens):\n",
                            cat.sample_size, cat.base_median
                        );
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
                                    "• {} + {} ({}×)\n",
                                    label(&p.a, &None),
                                    label(&p.b, &None),
                                    p.count
                                ));
                            }
                        }
                        body
                    }
                }
            }
        }
        // model guard dropped here (or earlier in the Some(cat) arm), before .await below
    };
    ctx.say(reply).await?;
    Ok(())
}
