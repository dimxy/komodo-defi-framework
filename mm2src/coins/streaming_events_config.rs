#![allow(dead_code)]
// FIXME: Get rid of this.

use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub(crate) struct BalanceEventConfig {
    /// A map of coin tickers to their balance streaming configuration.
    ///
    /// The configuration doesn't have a specific structure at this point, every coin
    /// has its own configuration structure.
    coins: HashMap<String, Json>,
}

impl BalanceEventConfig {
    /// Returns the coin's configuration as a JSON object if it exists.
    ///
    /// Note that the `ticker` is looked up in a case-insensitive manner.
    pub(crate) fn find_coin(&self, ticker: &str) -> Option<Json> {
        // FIXME: Is this OK? are coin tickers like an identifier for the coin? Could two coins have the same ticker?
        let ticker = ticker.to_lowercase();
        self.coins
            .iter()
            .find(|(key, _)| key.to_lowercase() == ticker)
            .map(|(_, value)| value.clone())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// An empty configuration struct.
///
/// It's used mainly to make sure there is no config provided.
pub(crate) struct EmptySubConfig {}
