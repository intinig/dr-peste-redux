mod config;
mod discord;
mod itemtext;
mod observe;
mod poeninja;
mod store;
mod trade;

use std::time::Duration;

use anyhow::Result;
use poise::serenity_prelude as serenity;
use tracing_subscriber::EnvFilter;

use discord::Data;
use observe::ObservationLog;
use poeninja::NinjaClient;
use store::{PriceStore, Snapshot};
use trade::client::TradeClient;
use trade::pseudo::PseudoMap;
use trade::value::{rebuild_into, ValueModel, VALUE_REFRESH_MINS};
use trade::TradePricer;

async fn refresh_once(
    client: &NinjaClient,
    store: &PriceStore,
    rates: &std::sync::Arc<std::sync::RwLock<trade::rates::RateTable>>,
) -> Result<()> {
    let league = client.current_league().await?;
    match client.currency_rates(&league.name).await {
        Ok(map) => *rates.write().unwrap() = trade::rates::RateTable::new(map),
        Err(e) => tracing::warn!(error = %e, "currency rate refresh failed; keeping last rates"),
    }
    let items = client.fetch_all(&league.name).await;
    if items.is_empty() {
        tracing::warn!(league = %league.name, "all categories returned no items; keeping last snapshot");
        return Ok(());
    }
    tracing::info!(league = %league.name, count = items.len(), "snapshot refreshed");
    store.replace(Snapshot { league, items }).await;
    Ok(())
}

fn spawn_refresher(
    client: NinjaClient,
    store: PriceStore,
    rates: std::sync::Arc<std::sync::RwLock<trade::rates::RateTable>>,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = refresh_once(&client, &store, &rates).await {
                tracing::error!(error = %e, "refresh failed; keeping last snapshot");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

fn spawn_value_refresher(
    log: ObservationLog,
    value: std::sync::Arc<std::sync::RwLock<ValueModel>>,
    catalog: std::sync::Arc<trade::stats::StatCatalog>,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            rebuild_into(&log, &value, &catalog);
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env()?;
    let store = PriceStore::new();
    let client = NinjaClient::new()?;
    let rates = std::sync::Arc::new(std::sync::RwLock::new(trade::rates::RateTable::default()));
    let trade_client = TradeClient::new(config.poesessid.clone(), rates.clone())?;
    let catalog = match trade::stats::StatCatalog::fetch(&trade_client).await {
        Ok(c) if !c.is_empty() => {
            tracing::info!("loaded trade2 stat catalog");
            c
        }
        Ok(c) => {
            tracing::warn!("trade2 stat catalog is empty; pricing falls back to pseudo-only");
            c
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch trade2 stat catalog; pricing falls back to pseudo-only");
            trade::stats::StatCatalog::default()
        }
    };
    let category_catalog = match trade::categories::CategoryCatalog::fetch(&trade_client).await {
        Ok(c) => {
            tracing::info!(categories = c.all().len(), "loaded trade2 category catalog");
            c
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch category catalog; /harvest autocomplete empty");
            trade::categories::CategoryCatalog::default()
        }
    };
    let value = std::sync::Arc::new(std::sync::RwLock::new(ValueModel::default()));
    let catalog_arc = std::sync::Arc::new(catalog);
    rebuild_into(
        &ObservationLog::new(&config.observation_log_path),
        &value,
        &catalog_arc,
    ); // startup build
    spawn_value_refresher(
        ObservationLog::new(&config.observation_log_path),
        value.clone(),
        catalog_arc.clone(),
        Duration::from_secs(VALUE_REFRESH_MINS * 60),
    );

    let pricer = std::sync::Arc::new(TradePricer::new(
        trade_client,
        PseudoMap::load(),
        (*catalog_arc).clone(),
        ObservationLog::new(&config.observation_log_path),
        value.clone(),
    ));
    let sessions = std::sync::Arc::new(crate::trade::session::MemberSessions::new(
        config.proxy.clone(),
        std::time::Duration::from_secs(config.session_ttl_mins * 60),
    ));

    // Best-effort initial refresh so commands have data quickly.
    if let Err(e) = refresh_once(&client, &store, &rates).await {
        tracing::warn!(error = %e, "initial refresh failed; will retry in background");
    }

    let interval = Duration::from_secs(config.poll_interval_mins * 60);
    spawn_refresher(client, store.clone(), rates.clone(), interval);

    let token = config.discord_token.clone();
    let guild_id = serenity::GuildId::new(config.guild_id);
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                discord::price::price(),
                discord::farm::farm(),
                discord::harvest::harvest(),
                discord::insights::insights(),
                discord::paste::paste(),
                discord::logout::logout(),
                discord::help::help(),
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                tracing::info!("commands registered; bot ready");
                Ok(Data {
                    store,
                    config,
                    pricer,
                    rates,
                    sessions,
                    categories: category_catalog,
                    value: value.clone(),
                })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;
    client.start().await?;
    Ok(())
}
