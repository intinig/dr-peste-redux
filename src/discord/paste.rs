use std::time::Duration;

use super::{embeds, AppContext, Context, Error};
use crate::itemtext;
use crate::poeninja::League;
use crate::store::{self, MatchOutcome};

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
            price_rare(&ctx, &parsed, &snap.league).await?;
        }
        MatchOutcome::NotFound => {
            ctx.say(format_not_found(&parsed.name, &snap.league))
                .await?;
        }
    }
    Ok(())
}

async fn price_rare(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
) -> Result<(), Error> {
    use poise::serenity_prelude as serenity;

    let uid = ctx.author().id.get();
    let Some(session) = ctx.data().sessions.session_for(uid) else {
        ctx.say("🔑 You need to connect your PoE account first — coming in the next step.")
            .await?;
        return Ok(());
    };

    let pricer = ctx.data().pricer.clone();
    let est = match pricer.price(parsed, &league.name, &session).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            ctx.say("Couldn't reach trade right now — try again shortly.")
                .await?;
            return Ok(());
        }
    };

    let secondary_rate = if matches!(est.modal_currency, crate::trade::model::Currency::Divine) {
        None
    } else {
        let code = est.modal_currency.code().to_string();
        ctx.data()
            .rates
            .read()
            .ok()
            .and_then(|r| r.to_divine(1.0, &code))
    };

    let button = serenity::CreateButton::new("drp_breakdown")
        .label("Break it down")
        .style(serenity::ButtonStyle::Secondary);
    let row = serenity::CreateActionRow::Buttons(vec![button]);

    let author = ctx.author().id;
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .embed(embeds::estimate_embed(parsed, &est, league, secondary_rate))
                .components(vec![row]),
        )
        .await?;

    let msg = reply.message().await?;
    let interaction =
        serenity::ComponentInteractionCollector::new(ctx.serenity_context().shard.clone())
            .message_id(msg.id)
            .custom_ids(vec!["drp_breakdown".to_string()])
            .filter(move |mci| mci.user.id == author)
            .timeout(Duration::from_secs(120))
            .await;

    match interaction {
        Some(mci) => {
            mci.defer(ctx.serenity_context()).await?;
            match pricer.breakdown(parsed, &league.name, &session).await {
                Ok(bd) => {
                    mci.create_followup(
                        ctx.serenity_context(),
                        serenity::CreateInteractionResponseFollowup::default()
                            .embed(embeds::breakdown_embed(parsed, &bd, league)),
                    )
                    .await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "trade breakdown failed");
                    mci.create_followup(
                        ctx.serenity_context(),
                        serenity::CreateInteractionResponseFollowup::default()
                            .content("Couldn't break that down right now."),
                    )
                    .await?;
                }
            }
            reply
                .edit(
                    *ctx,
                    poise::CreateReply::default()
                        .embed(embeds::estimate_embed(parsed, &est, league, secondary_rate))
                        .components(vec![]),
                )
                .await?;
        }
        None => {
            reply
                .edit(
                    *ctx,
                    poise::CreateReply::default()
                        .embed(embeds::estimate_embed(parsed, &est, league, secondary_rate))
                        .components(vec![]),
                )
                .await?;
        }
    }
    Ok(())
}

fn format_not_found(name: &str, league: &League) -> String {
    format!("Couldn't find **{name}** in {} data.", league.name)
}
