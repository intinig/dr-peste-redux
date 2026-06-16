use super::{Context, Error};
use poise::serenity_prelude as serenity;

/// Show what this bot can do.
#[poise::command(slash_command)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let embed = serenity::CreateEmbed::default()
        .title("dr-peste-redux — PoE2 price bot")
        .description("Live prices and farming hints from poe.ninja, for the current league.")
        .field(
            "/price `item`",
            "Look up an item's value, with autocomplete.",
            false,
        )
        .field(
            "/paste",
            "Open a box, paste a copied in-game item (Ctrl+C), and get its price.",
            false,
        )
        .field(
            "/farm `[category] [sort]`",
            "Most valuable items, or the biggest movers (sort: value or trending).",
            false,
        )
        .field("/help", "Show this message.", false)
        .footer(serenity::CreateEmbedFooter::new(
            "Data from poe.ninja • prices update periodically",
        ));
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}
