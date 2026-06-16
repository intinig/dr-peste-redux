pub mod categories;
pub mod model;

use anyhow::{Context, Result};
use reqwest::Client;

use categories::{Category, Family, CATEGORIES};
use model::{normalize_exchange, normalize_item, ExchangeOverview, ItemOverview, PricedItem};

const BASE: &str = "https://poe.ninja/poe2/api";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct League {
    pub name: String,
    pub url: String,
}

/// Picks the active softcore challenge league: first indexed, non-hardcore
/// league that is not the permanent "Standard" league. Falls back to the first.
pub fn select_current_league(v: &serde_json::Value) -> Option<League> {
    let leagues = v.get("economyLeagues")?.as_array()?;
    let pick = leagues
        .iter()
        .find(|l| {
            let indexed = l.get("indexed").and_then(|x| x.as_bool()).unwrap_or(false);
            let hardcore = l.get("hardcore").and_then(|x| x.as_bool()).unwrap_or(false);
            let name = l.get("name").and_then(|x| x.as_str()).unwrap_or("");
            indexed && !hardcore && name != "Standard"
        })
        .or_else(|| leagues.first())?;
    Some(League {
        name: pick.get("name")?.as_str()?.to_string(),
        url: pick
            .get("url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

pub struct NinjaClient {
    http: Client,
}

impl NinjaClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent("dr-peste-redux/0.1 (Discord guild price bot)")
            .build()?;
        Ok(Self { http })
    }

    pub async fn current_league(&self) -> Result<League> {
        let url = format!("{BASE}/data/index-state");
        let v: serde_json::Value = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        select_current_league(&v).context("no current league found in index-state")
    }

    pub async fn fetch_category(&self, league: &str, cat: &Category) -> Result<Vec<PricedItem>> {
        let url = format!("{BASE}/economy/{}", cat.family.path());
        let resp = self
            .http
            .get(url)
            .query(&[("league", league), ("type", cat.type_param)])
            .send()
            .await?
            .error_for_status()?;
        match cat.family {
            Family::Exchange => {
                let ov: ExchangeOverview = resp.json().await?;
                Ok(normalize_exchange(cat, ov))
            }
            Family::StashItem => {
                let ov: ItemOverview = resp.json().await?;
                Ok(normalize_item(cat, ov))
            }
        }
    }

    /// Fetches every category sequentially (polite). A failing category is
    /// logged and skipped, never fatal.
    pub async fn fetch_all(&self, league: &str) -> Vec<PricedItem> {
        let mut all = Vec::new();
        for cat in CATEGORIES {
            match self.fetch_category(league, cat).await {
                Ok(mut items) => all.append(&mut items),
                Err(e) => {
                    tracing::warn!(category = cat.slug, error = %e, "failed to fetch category")
                }
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_challenge_league_over_standard_and_hc() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/index_state.json")).unwrap();
        let league = select_current_league(&v).unwrap();
        assert_eq!(league.name, "Runes of Aldur");
        assert_eq!(league.url, "runesofaldur");
    }

    #[test]
    fn falls_back_to_first_when_none_match() {
        let v = serde_json::json!({
            "economyLeagues": [{ "name": "Standard", "url": "standard", "hardcore": false, "indexed": true }]
        });
        assert_eq!(select_current_league(&v).unwrap().name, "Standard");
    }

    #[test]
    fn returns_none_without_leagues() {
        let v = serde_json::json!({});
        assert!(select_current_league(&v).is_none());
    }

    #[tokio::test]
    #[ignore = "hits the live poe.ninja API"]
    async fn live_fetch_currency_has_divine() {
        let client = NinjaClient::new().unwrap();
        let league = client.current_league().await.unwrap();
        let cat = super::categories::by_slug("currency").unwrap();
        let items = client.fetch_category(&league.name, cat).await.unwrap();
        assert!(
            items.iter().any(|i| i.name == "Divine Orb"),
            "expected Divine Orb in currency"
        );
    }
}
