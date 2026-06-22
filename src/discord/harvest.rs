//! `/harvest <category>` — category-filtered trade2 market search.
//! Stub wired at startup so the category catalog is exercised in the binary;
//! full implementation in the next task.

use super::{Context, Error};
use crate::trade::categories::CategoryCatalog;
use futures::Stream;

/// Autocomplete callback for `/harvest`'s category argument. Returns up to 25
/// trade2 category labels that case-insensitively prefix-match `partial`.
pub async fn autocomplete_harvest_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let names: Vec<String> = ctx
        .data()
        .categories
        .matches(partial)
        .into_iter()
        .map(|c| c.text.clone())
        .take(25)
        .collect();
    futures::stream::iter(names)
}

/// Returns the trade2 category id for the given human-readable label, or `None`
/// if the label is not in the catalog. Used by the `/harvest` handler.
pub fn category_id_for<'a>(catalog: &'a CategoryCatalog, text: &str) -> Option<&'a str> {
    catalog.id_for_text(text)
}

/// List the most-listed items in a trade2 category.
#[poise::command(slash_command)]
pub async fn harvest(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff, Helmet)"]
    #[autocomplete = "autocomplete_harvest_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let cat_id = category
        .as_deref()
        .and_then(|t| category_id_for(&data.categories, t));
    ctx.say(format!(
        "Category catalog has {} entries. Resolved id: {:?} (full implementation coming soon).",
        data.categories.all().len(),
        cat_id
    ))
    .await?;
    Ok(())
}
