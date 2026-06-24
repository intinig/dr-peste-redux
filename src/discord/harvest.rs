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

/// Harvest a category into the corpus. Optionally filter by a specific mod stat id.
#[poise::command(slash_command)]
pub async fn harvest(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff, Helmet)"]
    #[autocomplete = "autocomplete_harvest_category"]
    category: String,
    #[description = "Optional: stat id to target (e.g. explicit.stat_…). Filters the sweep to items with this mod."]
    stat_mod: Option<String>,
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

    // Reuse the shared connect dialog: ensure_session fast-paths an existing
    // session and otherwise prompts for the POESESSID inline (same button + modal
    // as /paste), rather than dead-ending to a "go run /paste" message.
    let Some(session) = crate::discord::paste::ensure_session(&ctx).await? else {
        return Ok(()); // user dismissed / timed out / invalid (already messaged)
    };

    // Built once and reused in both the progress and completion messages.
    let suffix = stat_mod
        .as_deref()
        .map(|sid| format!(" (mod: `{sid}`)"))
        .unwrap_or_default();

    let reply = ctx
        .send(poise::CreateReply::default().content(format!(
            "⏳ Harvesting **{category}**{suffix} — this runs several searches against your account…"
        )))
        .await?;

    let result = if let Some(ref sid) = stat_mod {
        data.pricer
            .harvest_mod(&category_id, &category, &snap.league.name, sid, &session)
            .await
    } else {
        data.pricer
            .harvest(&category_id, &category, &snap.league.name, &session)
            .await
    };

    match result {
        Ok(n) => {
            // Rebuild the value model off the async executor — a whole-corpus
            // read + aggregation shouldn't block the runtime worker thread.
            let log_path = data.config.observation_log_path.clone();
            let value = data.value.clone();
            let catalog = data.pricer.catalog().clone();
            let _ = tokio::task::spawn_blocking(move || {
                crate::trade::value::rebuild_into(
                    &crate::observe::ObservationLog::new(&log_path),
                    &value,
                    &catalog,
                );
            })
            .await;
            reply
                .edit(
                    ctx,
                    poise::CreateReply::default().content(format!(
                        "Harvested **{category}**{suffix}: logged {n} market observations to the corpus."
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
