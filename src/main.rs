mod config;
mod discord;
mod itemtext;
mod poeninja;
mod store;

use std::time::Duration;

use anyhow::Result;
use poise::serenity_prelude as serenity;
use tracing_subscriber::EnvFilter;

use discord::Data;
use poeninja::NinjaClient;
use store::{PriceStore, Snapshot};

async fn refresh_once(client: &NinjaClient, store: &PriceStore) -> Result<()> {
    let league = client.current_league().await?;
    let items = client.fetch_all(&league.name).await;
    if items.is_empty() {
        tracing::warn!(league = %league.name, "all categories returned no items; keeping last snapshot");
        return Ok(());
    }
    tracing::info!(league = %league.name, count = items.len(), "snapshot refreshed");
    store.replace(Snapshot { league, items }).await;
    Ok(())
}

fn spawn_refresher(client: NinjaClient, store: PriceStore, interval: Duration) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = refresh_once(&client, &store).await {
                tracing::error!(error = %e, "refresh failed; keeping last snapshot");
            }
            tokio::time::sleep(interval).await;
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

    // Best-effort initial refresh so commands have data quickly.
    if let Err(e) = refresh_once(&client, &store).await {
        tracing::warn!(error = %e, "initial refresh failed; will retry in background");
    }

    let interval = Duration::from_secs(config.poll_interval_mins * 60);
    spawn_refresher(client, store.clone(), interval);

    let token = config.discord_token.clone();
    let guild_id = serenity::GuildId::new(config.guild_id);
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                discord::price::price(),
                discord::farm::farm(),
                discord::pricecheck::pricecheck(),
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                tracing::info!("commands registered; bot ready");
                Ok(Data { store, config })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;
    client.start().await?;
    Ok(())
}
