//! `/harvest <category>` — price-banded trade2 market sweep.
//! Searches across PRICE_BANDS and logs every listing as a Harvest observation,
//! warming the corpus for the learning layer.

use super::{Context, Error};
use futures::Stream;

/// Discord caps an autocomplete choice's display name at 100 characters; exceeding
/// it makes the WHOLE autocomplete response fail (Discord returns no suggestions).
/// Gate labels come from the live stat catalog and can be long, so ellipsize the
/// display name to stay under the cap. The submitted value (the stat id) is never
/// touched — only what the operator sees is shortened.
const MAX_CHOICE_NAME: usize = 100;

fn truncate_choice_name(name: &str) -> String {
    if name.chars().count() <= MAX_CHOICE_NAME {
        return name.to_string();
    }
    let kept: String = name.chars().take(MAX_CHOICE_NAME - 1).collect();
    format!("{kept}…")
}

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

/// Autocomplete callback for `/harvest`'s optional `stat_mod` argument. Surfaces the
/// undersampled-gate candidates the value model has flagged for the active league
/// (across categories), so the operator picks a flagged mod from a labelled list
/// instead of pasting a raw `explicit.stat_…` id. Each choice shows
/// `<category> — <label> (n=<count>)` but submits the bare stat id (the value the
/// targeted sweep filters on). Gates are few per category, and the operator narrows
/// by typing, so listing the whole league rather than the in-progress category is fine.
pub async fn autocomplete_harvest_mod<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = poise::serenity_prelude::AutocompleteChoice> + 'a {
    use poise::serenity_prelude::AutocompleteChoice;
    let needle = partial.to_lowercase();
    let mut choices: Vec<AutocompleteChoice> = Vec::new();

    // Active league via the cheap accessor (avoids cloning the whole Snapshot every
    // keystroke); awaited BEFORE taking the model read lock, so no guard is held
    // across an await.
    if let Some(league) = ctx.data().store.league_name().await {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        let catalog = ctx.data().pricer.catalog();
        'outer: for cat in model.categories_sorted(&league) {
            // Category is constant across its gates: lowercase it once, and only when
            // there is a needle to match against.
            let cat_lower = if needle.is_empty() {
                String::new()
            } else {
                cat.category.to_lowercase()
            };
            for g in &cat.undersampled_gates {
                let label = g
                    .label
                    .as_deref()
                    .or_else(|| catalog.label_for(&g.stat_id))
                    .unwrap_or(&g.stat_id);
                let matches = needle.is_empty()
                    || label.to_lowercase().contains(&needle)
                    || g.stat_id.to_lowercase().contains(&needle)
                    || cat_lower.contains(&needle);
                if matches {
                    let name = truncate_choice_name(&format!(
                        "{} — {} (n={})",
                        cat.category, label, g.count
                    ));
                    choices.push(AutocompleteChoice::new(name, g.stat_id.clone()));
                    if choices.len() >= 25 {
                        break 'outer;
                    }
                }
            }
        }
    }
    futures::stream::iter(choices)
}

/// Harvest a category into the corpus. Optionally filter by a specific mod stat id.
#[poise::command(slash_command)]
pub async fn harvest(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff, Helmet)"]
    #[autocomplete = "autocomplete_harvest_category"]
    category: String,
    #[description = "Optional: target a flagged undersampled gate (pick from autocomplete) to deep-sample it."]
    #[autocomplete = "autocomplete_harvest_mod"]
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

#[cfg(test)]
mod tests {
    use super::{truncate_choice_name, MAX_CHOICE_NAME};

    #[test]
    fn short_names_pass_through_unchanged() {
        let s = "Staff — +1 to Level of all Spell Skills (n=12)";
        assert_eq!(truncate_choice_name(s), s);
    }

    #[test]
    fn long_names_are_ellipsized_within_the_discord_cap() {
        let long = format!("Body Armour — {} (n=3)", "very long stat label ".repeat(20));
        assert!(long.chars().count() > MAX_CHOICE_NAME);
        let out = truncate_choice_name(&long);
        assert!(
            out.chars().count() <= MAX_CHOICE_NAME,
            "must fit Discord's {MAX_CHOICE_NAME}-char cap (got {})",
            out.chars().count()
        );
        assert!(out.ends_with('…'), "ellipsis marks the truncation");
    }
}
