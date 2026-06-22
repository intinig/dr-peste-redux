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
                    Some(cat) => format!(
                        "**{canon}** — {} listings, median {:.1} div. Driver insights coming online.",
                        cat.sample_size, cat.base_median
                    ),
                }
            }
        }
        // model guard dropped here, before .await below
    };
    ctx.say(reply).await?;
    Ok(())
}
