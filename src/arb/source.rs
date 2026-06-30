//! Candidate edge sources. Phase 1: WatchlistSource (live trade2). The engine
//! depends only on the `CandidateSource` trait, so Phase 2's cxapi source slots
//! in behind the same interface.
//!
//! **Cost note:** N currencies → N·(N-1) exchange queries, all paced by the
//! rate limiter. Keep `ARB_WATCHLIST` small (≤ ~6) in Phase 1; whole-market
//! coverage is exactly what the cxapi source (Phase 2) replaces this with.

use crate::arb::model::{Currency, Edge, Freshness, RatioQuote};
use crate::trade::client::{ExchangeOffer, TradeClient};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait CandidateSource: Send + Sync {
    async fn edges(&self, league: &str) -> Result<Vec<Edge>>;
}

pub struct WatchlistSource {
    client: Arc<TradeClient>,
    watchlist: Vec<Currency>,
}

impl WatchlistSource {
    pub fn new(client: Arc<TradeClient>, watchlist: Vec<Currency>) -> Self {
        WatchlistSource { client, watchlist }
    }
}

#[async_trait]
impl CandidateSource for WatchlistSource {
    async fn edges(&self, league: &str) -> Result<Vec<Edge>> {
        let mut edges = Vec::new();
        for have in &self.watchlist {
            for want in &self.watchlist {
                if have == want {
                    continue;
                }
                match self.client.exchange(have, want, league).await {
                    Ok(offers) => {
                        if let Some(best) = offers.into_iter().next() {
                            let ExchangeOffer {
                                pay_amount,
                                get_amount,
                                stock,
                                ..
                            } = best;
                            edges.push(Edge {
                                from: have.clone(),
                                to: want.clone(),
                                quote: RatioQuote {
                                    pay: pay_amount,
                                    get: get_amount,
                                    stock,
                                    freshness: Freshness::Live,
                                },
                            });
                        } else {
                            tracing::debug!(%have, %want, "no offers for pair; skipping");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%have, %want, error = %e, "exchange pair failed; skipping");
                    }
                }
            }
        }
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource(Vec<Edge>);

    #[async_trait]
    impl CandidateSource for FakeSource {
        async fn edges(&self, _league: &str) -> Result<Vec<Edge>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn trait_seam_returns_edges() {
        let src = FakeSource(vec![Edge {
            from: "A".into(),
            to: "B".into(),
            quote: RatioQuote {
                pay: 1,
                get: 2,
                stock: 10,
                freshness: Freshness::Live,
            },
        }]);
        let edges = src.edges("X").await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "B");
    }
}
