use crate::discord::{embeds, Context, Error};

/// Find currency flip and triangulation opportunities right now.
#[poise::command(slash_command)]
pub async fn arb(ctx: Context<'_>) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };
    let league = snap.league.name.clone();
    // Live trade2 queries take seconds; defer so Discord doesn't time out.
    ctx.defer().await?;

    match ctx.data().arb.opportunities(&league).await {
        Ok(opps) if opps.is_empty() => {
            ctx.say(format!(
                "No currency arbitrage above the configured thresholds right now ({league})."
            ))
            .await?;
        }
        Ok(opps) => {
            ctx.send(poise::CreateReply::default().embed(embeds::arb_embed(&opps, &league)))
                .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "arb scan failed");
            ctx.say("Couldn't scan the exchange just now — try again shortly.")
                .await?;
        }
    }
    Ok(())
}
