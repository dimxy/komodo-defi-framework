use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub(crate) struct BalanceEventConfig {
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
