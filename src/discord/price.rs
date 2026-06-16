use super::{autocomplete_item, embeds, Context, Error};
use crate::store;

/// Look up the value of a tracked PoE2 item.
#[poise::command(slash_command)]
pub async fn price(
    ctx: Context<'_>,
    #[description = "Item name"]
    #[autocomplete = "autocomplete_item"]
    item: String,
) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    if let Some(found) = store::find_exact(&snap.items, &item) {
        ctx.send(poise::CreateReply::default().embed(embeds::item_embed(found, &snap.league)))
            .await?;
        return Ok(());
    }

    let suggestions = store::search(&snap.items, &item, 3);
    if suggestions.is_empty() {
        ctx.say(format!("No match for **{item}** in {}.", snap.league.name))
            .await?;
    } else {
        let names = suggestions
            .iter()
            .map(|i| format!("• {}", i.name))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.say(format!(
            "No exact match for **{item}**. Did you mean:\n{names}"
        ))
        .await?;
    }
    Ok(())
}
