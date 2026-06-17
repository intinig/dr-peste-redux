use super::{embeds, AppContext, Context, Error};
use crate::store::{self, MatchOutcome};
use crate::{itemtext, poeninja::League};

#[derive(Debug, poise::Modal)]
#[name = "Paste an item"]
struct PasteModal {
    #[name = "Paste your item"]
    #[placeholder = "Ctrl+C an item in-game, then paste it here"]
    #[paragraph]
    item_text: String,
}

/// Paste a copied in-game item to price it.
#[poise::command(slash_command)]
pub async fn paste(app_ctx: AppContext<'_>) -> Result<(), Error> {
    use poise::Modal as _;

    let Some(modal) = PasteModal::execute(app_ctx).await? else {
        return Ok(());
    };
    let ctx = Context::Application(app_ctx);

    let Some(parsed) = itemtext::parse(&modal.item_text) else {
        ctx.say("Couldn't read that — paste the full item text copied with Ctrl+C.")
            .await?;
        return Ok(());
    };

    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    match store::route(&snap.items, &parsed) {
        MatchOutcome::Found(it) => {
            ctx.send(poise::CreateReply::default().embed(embeds::item_embed(it, &snap.league)))
                .await?;
        }
        MatchOutcome::Suggestions(s) => {
            let names = s
                .iter()
                .map(|i| format!("• {}", i.name))
                .collect::<Vec<_>>()
                .join("\n");
            ctx.say(format!(
                "No exact match for **{}**. Did you mean:\n{names}",
                parsed.name
            ))
            .await?;
        }
        MatchOutcome::Rare => {
            ctx.say("That looks like rare/magic gear, which poe.ninja doesn't price. Try a unique or currency item.")
                .await?;
        }
        MatchOutcome::NotFound => {
            ctx.say(format_not_found(&parsed.name, &snap.league))
                .await?;
        }
    }
    Ok(())
}

fn format_not_found(name: &str, league: &League) -> String {
    format!("Couldn't find **{name}** in {} data.", league.name)
}
