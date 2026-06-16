use super::{autocomplete_category, embeds, Context, Error};
use crate::poeninja::categories::by_slug;
use crate::store::{self, FarmSort};

#[derive(Debug, poise::ChoiceParameter)]
pub enum SortChoice {
    #[name = "Value"]
    Value,
    #[name = "Trending"]
    Trending,
}

/// Show the most valuable or fastest-rising items to farm right now.
#[poise::command(slash_command)]
pub async fn farm(
    ctx: Context<'_>,
    #[description = "Sort by value (default) or trending"] sort: Option<SortChoice>,
    #[description = "Restrict to one category slug (optional)"]
    #[autocomplete = "autocomplete_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    if let Some(slug) = &category {
        if by_slug(slug).is_none() {
            ctx.say(format!("Unknown category `{slug}`. Try autocomplete."))
                .await?;
            return Ok(());
        }
    }

    let sort = match sort {
        Some(SortChoice::Trending) => FarmSort::Trending,
        _ => FarmSort::Value,
    };
    let min_volume = ctx.data().config.min_volume;
    let top = store::farm(&snap.items, sort, min_volume, category.as_deref(), 10);

    let title = match sort {
        FarmSort::Value => "💰 Most valuable right now",
        FarmSort::Trending => "📈 Heating up right now",
    };
    ctx.send(poise::CreateReply::default().embed(embeds::farm_embed(title, &top, &snap.league)))
        .await?;
    Ok(())
}
