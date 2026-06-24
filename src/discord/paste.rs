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

#[derive(poise::Modal)]
#[name = "Connect your PoE account"]
struct ConnectModal {
    #[name = "POESESSID (from your pathofexile.com cookies)"]
    #[placeholder = "32-character hex value"]
    poesessid: String,
}

impl std::fmt::Debug for ConnectModal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectModal(***)")
    }
}

/// A POESESSID is a 32-character hex string. Light pre-check so we don't burn a
/// trade call on an obvious paste error.
fn valid_poesessid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
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
    let Some(session) = ensure_session(ctx).await? else {
        return Ok(()); // user dismissed / timed out / invalid (already messaged)
    };
    run_pricing(ctx, parsed, league, &session).await
}

async fn run_pricing(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
    session: &crate::trade::session::TradeSession,
) -> Result<(), Error> {
    use poise::serenity_prelude as serenity;

    let pricer = ctx.data().pricer.clone();
    let reply = ctx
        .send(poise::CreateReply::default().content("⏳ Pricing…"))
        .await?;
    let est = match pricer.price(parsed, &league.name, session).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            reply
                .edit(
                    *ctx,
                    poise::CreateReply::default()
                        .content("Couldn't reach trade right now — try again shortly."),
                )
                .await?;
            return Ok(());
        }
    };

    // Sub-1-div items: report "too cheap to price" and stop — skip the precise
    // breakdown and the learned estimate (we don't care how cheap, just that it is).
    if est.is_sub_priceable() {
        reply
            .edit(
                *ctx,
                poise::CreateReply::default()
                    .content(embeds::sub_one_div_message(&parsed.name))
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

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

    let learned = pricer.learned_estimate(parsed, &league.name);

    let button = serenity::CreateButton::new("drp_breakdown")
        .label("Break it down")
        .style(serenity::ButtonStyle::Secondary);
    let row = serenity::CreateActionRow::Buttons(vec![button]);

    let author = ctx.author().id;
    reply
        .edit(
            *ctx,
            poise::CreateReply::default()
                .embed(embeds::estimate_embed(
                    parsed,
                    &est,
                    league,
                    secondary_rate,
                    learned.as_ref(),
                ))
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
            match pricer.breakdown(parsed, &league.name, session).await {
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
                        .embed(embeds::estimate_embed(
                            parsed,
                            &est,
                            league,
                            secondary_rate,
                            learned.as_ref(),
                        ))
                        .components(vec![]),
                )
                .await?;
        }
        None => {
            reply
                .edit(
                    *ctx,
                    poise::CreateReply::default()
                        .embed(embeds::estimate_embed(
                            parsed,
                            &est,
                            league,
                            secondary_rate,
                            learned.as_ref(),
                        ))
                        .components(vec![]),
                )
                .await?;
        }
    }
    Ok(())
}

/// Ensures the member has a captured POESESSID, prompting inline (the connect
/// button + POESESSID modal) if not. Returns the live session, or `None` if the
/// member dismissed/timed out or the cookie was invalid/unreachable (those cases
/// are messaged to the member). Shared by `/paste` and `/harvest`.
pub(crate) async fn ensure_session(
    ctx: &Context<'_>,
) -> Result<Option<crate::trade::session::TradeSession>, Error> {
    use poise::serenity_prelude as serenity;

    let uid = ctx.author().id.get();
    if let Some(session) = ctx.data().sessions.session_for(uid) {
        return Ok(Some(session));
    }

    let button = serenity::CreateButton::new("drp_connect")
        .label("🔑 Connect your PoE account")
        .style(serenity::ButtonStyle::Primary);
    let row = serenity::CreateActionRow::Buttons(vec![button]);
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(
                    "To search the trade site as **you**, connect your account. \n\
                     Click below and paste your **POESESSID** (pathofexile.com cookie). \n\
                     It's kept in memory only, used solely for your searches, and you can remove it any time with `/logout`. \n\
                     Privacy: https://drp.pme.it/privacy",
                )
                .components(vec![row]),
        )
        .await?;

    let msg = reply.message().await?;
    let interaction =
        serenity::ComponentInteractionCollector::new(ctx.serenity_context().shard.clone())
            .message_id(msg.id)
            .custom_ids(vec!["drp_connect".to_string()])
            .filter(move |mci| mci.user.id.get() == uid)
            .timeout(Duration::from_secs(120))
            .await;

    let Some(mci) = interaction else {
        reply
            .edit(
                *ctx,
                poise::CreateReply::default()
                    .content("Connect timed out — run the command again when ready.")
                    .components(vec![]),
            )
            .await?;
        return Ok(None);
    };

    // Open the POESESSID modal off the component interaction.
    let submitted = poise::execute_modal_on_component_interaction::<ConnectModal>(
        ctx,
        mci,
        None,
        Some(Duration::from_secs(300)),
    )
    .await?;

    // Helper: clear the now-stale connect button and show a terminal status on
    // the original ephemeral message, so a dead button isn't left behind.
    let finish = |content: &'static str| {
        reply.edit(
            *ctx,
            poise::CreateReply::default()
                .content(content)
                .components(vec![]),
        )
    };

    let Some(modal) = submitted else {
        finish("Connect cancelled — run the command again when ready.").await?;
        return Ok(None);
    };

    if !valid_poesessid(&modal.poesessid) {
        finish("That doesn't look like a POESESSID (expected 32 hex chars). Run the command again and try once more.").await?;
        return Ok(None);
    }

    let cookie = secrecy::SecretString::new(modal.poesessid.trim().to_string());
    if let Err(e) = ctx.data().sessions.store(uid, cookie).await {
        tracing::warn!(error = %e, "session store/validation failed"); // never logs the cookie
        finish("Couldn't reach the trade site — please try again in a moment.").await?;
        return Ok(None);
    }

    Ok(ctx.data().sessions.session_for(uid))
}

fn format_not_found(name: &str, league: &League) -> String {
    format!("Couldn't find **{name}** in {} data.", league.name)
}

#[cfg(test)]
mod tests {
    use super::valid_poesessid;

    #[test]
    fn accepts_32_hex_rejects_otherwise() {
        assert!(valid_poesessid("0123456789abcdef0123456789ABCDEF"));
        assert!(valid_poesessid("  0123456789abcdef0123456789abcdef  ")); // trimmed
        assert!(!valid_poesessid(""));
        assert!(!valid_poesessid("tooshort"));
        assert!(!valid_poesessid("zzzz567890abcdef0123456789abcdef")); // non-hex
    }
}
