//! `/harvest <category>` — price-banded trade2 market sweep.
//! Searches across PRICE_BANDS and logs every listing as a Harvest observation,
//! warming the corpus for the learning layer.

use super::{Context, Error};
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

/// Harvest a whole item category into the observation corpus, warming pricing data.
#[poise::command(slash_command)]
pub async fn harvest(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff, Helmet)"]
    #[autocomplete = "autocomplete_harvest_category"]
    category: String,
) -> Result<(), Error> {
    let data = ctx.data();

    let Some(category_id) = data.categories.id_for_text(&category).map(str::to_string) else {
        ctx.say(format!(
            "Unknown category `{category}` — pick one from the autocomplete."
        ))
        .await?;
        return Ok(());
    };

    let Some(snap) = data.store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    // Reuse the shared connect dialog: if the member has no session, prompt for
    // their POESESSID inline (same button + modal as /paste) rather than
    // dead-ending to a "go run /paste" message.
    let uid = ctx.author().id.get();
    let session = match data.sessions.session_for(uid) {
        Some(s) => s,
        None => match crate::discord::paste::ensure_session(&ctx).await? {
            Some(s) => s,
            None => return Ok(()), // user dismissed / timed out / invalid (already messaged)
        },
    };

    let reply = ctx
        .send(poise::CreateReply::default().content(format!(
            "⏳ Harvesting **{category}** — this runs several searches against your account…"
        )))
        .await?;

    match data
        .pricer
        .harvest(&category_id, &category, &snap.league.name, &session)
        .await
    {
        Ok(n) => {
            // Rebuild the value model off the async executor — a whole-corpus
            // read + aggregation shouldn't block the runtime worker thread.
            let log_path = data.config.observation_log_path.clone();
            let value = data.value.clone();
            let _ = tokio::task::spawn_blocking(move || {
                crate::trade::value::rebuild_into(
                    &crate::observe::ObservationLog::new(&log_path),
                    &value,
                );
            })
            .await;
            reply
                .edit(
                    ctx,
                    poise::CreateReply::default().content(format!(
                        "Harvested **{category}**: logged {n} market observations to the corpus."
                    )),
                )
                .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "harvest failed");
            reply
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Harvest hit an error — try again shortly."),
                )
                .await?;
        }
    }

    Ok(())
}
