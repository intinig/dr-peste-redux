pub mod embeds;
pub mod farm;
pub mod help;
pub mod logout;
pub mod paste;
pub mod price;

use std::sync::{Arc, RwLock};

use futures::Stream;

use crate::config::Config;
use crate::store::{self, PriceStore};
use crate::trade::client::TradeClient;
use crate::trade::TradePricer;

pub struct Data {
    pub store: PriceStore,
    pub config: Config,
    pub pricer: Arc<TradePricer<TradeClient>>,
    pub rates: Arc<RwLock<crate::trade::rates::RateTable>>,
    pub sessions: Arc<crate::trade::session::MemberSessions>,
    pub pending:
        RwLock<std::collections::HashMap<u64, (crate::itemtext::ParsedItem, std::time::Instant)>>,
}

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;
pub type AppContext<'a> = poise::ApplicationContext<'a, Data, Error>;

/// Autocomplete callback shared by `/price`. Returns up to 25 item names that
/// fuzzy-match the partial input.
pub async fn autocomplete_item<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let names: Vec<String> = match ctx.data().store.snapshot().await {
        Some(snap) => store::search(&snap.items, partial, 25)
            .into_iter()
            .map(|it| it.name.clone())
            .collect(),
        None => Vec::new(),
    };
    futures::stream::iter(names)
}

/// Autocomplete callback for `/farm`'s category argument. Returns matching slugs.
pub async fn autocomplete_category<'a>(
    _ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let partial = partial.to_lowercase();
    let slugs: Vec<String> = crate::poeninja::categories::CATEGORIES
        .iter()
        .filter(|c| c.slug.contains(&partial) || c.display.to_lowercase().contains(&partial))
        .map(|c| c.slug.to_string())
        .take(25)
        .collect();
    futures::stream::iter(slugs)
}
