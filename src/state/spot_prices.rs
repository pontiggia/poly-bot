//! Spot price state and history for external reference prices (Binance)
//!
//! Provides lock-free spot price storage and ring-buffer history
//! for momentum calculation and adverse selection defense.

use dashmap::DashMap;
use rust_decimal::Decimal;
use std::collections::VecDeque;
use std::sync::RwLock;

/// A single spot price observation
#[derive(Debug, Clone, Copy)]
pub struct SpotPrice {
    /// Price in USD
    pub price: Decimal,
    /// Unix timestamp in milliseconds
    pub timestamp_ms: i64,
}

/// Update from the Binance WebSocket
#[derive(Debug, Clone)]
pub struct SpotPriceUpdate {
    /// Lowercase asset symbol (e.g., "btc", "eth")
    pub symbol: String,
    /// Price in USD
    pub price: Decimal,
    /// Unix timestamp in milliseconds
    pub timestamp_ms: i64,
}

/// Lock-free spot price storage (latest price per asset)
pub struct SpotPriceState {
    /// Latest price per asset (key = lowercase asset name, e.g. "btc")
    prices: DashMap<String, SpotPrice>,
}

impl SpotPriceState {
    pub fn new() -> Self {
        Self {
            prices: DashMap::new(),
        }
    }

    /// Update the latest price for an asset
    pub fn update(&self, update: &SpotPriceUpdate) {
        self.prices.insert(
            update.symbol.clone(),
            SpotPrice {
                price: update.price,
                timestamp_ms: update.timestamp_ms,
            },
        );
    }

    /// Get the latest price for an asset
    pub fn get(&self, asset: &str) -> Option<SpotPrice> {
        self.prices.get(asset).map(|r| *r)
    }

    /// Get the latest price value for an asset
    pub fn price(&self, asset: &str) -> Option<Decimal> {
        self.get(asset).map(|sp| sp.price)
    }

    /// Number of tracked assets
    pub fn len(&self) -> usize {
        self.prices.len()
    }

    /// Whether any assets are tracked
    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

impl Default for SpotPriceState {
    fn default() -> Self {
        Self::new()
    }
}

/// Ring buffer history for momentum/trend calculation
///
/// Stores up to `max_entries` price observations per asset.
/// Thread-safe via per-asset RwLock.
pub struct PriceHistory {
    /// Per-asset history ring buffers
    histories: DashMap<String, RwLock<VecDeque<(i64, Decimal)>>>,
    /// Maximum entries per asset
    max_entries: usize,
}

impl PriceHistory {
    /// Create with a max history depth
    ///
    /// Default 600 entries = 10 minutes at ~1 update/sec
    pub fn new(max_entries: usize) -> Self {
        Self {
            histories: DashMap::new(),
            max_entries,
        }
    }

    /// Record a price observation
    pub fn record(&self, asset: &str, timestamp_ms: i64, price: Decimal) {
        let entry = self
            .histories
            .entry(asset.to_string())
            .or_insert_with(|| RwLock::new(VecDeque::with_capacity(self.max_entries)));

        let mut buf = entry.write().unwrap();
        buf.push_back((timestamp_ms, price));
        if buf.len() > self.max_entries {
            buf.pop_front();
        }
    }

    /// Calculate percentage price change over a time window
    ///
    /// Returns `Some(pct)` where pct is fractional (0.01 = 1% up).
    /// Returns `None` if insufficient history.
    pub fn price_change_pct(&self, asset: &str, window_ms: i64) -> Option<Decimal> {
        let entry = self.histories.get(asset)?;
        let buf = entry.read().unwrap();

        if buf.is_empty() {
            return None;
        }

        let (latest_ts, latest_price) = *buf.back()?;
        let cutoff = latest_ts - window_ms;

        // Find the oldest entry within (or closest to) the window
        let old_entry = buf.iter().find(|(ts, _)| *ts >= cutoff)?;
        let old_price = old_entry.1;

        if old_price.is_zero() {
            return None;
        }

        Some((latest_price - old_price) / old_price)
    }

    /// Rolling standard deviation of returns over a window (for volatility)
    ///
    /// Returns the standard deviation of price returns within the window.
    /// Returns None if insufficient data (need at least 3 observations).
    pub fn volatility(&self, asset: &str, window_ms: i64) -> Option<Decimal> {
        let entry = self.histories.get(asset)?;
        let buf = entry.read().unwrap();

        if buf.len() < 3 {
            return None;
        }

        let (latest_ts, _) = *buf.back()?;
        let cutoff = latest_ts - window_ms;

        // Collect prices within window
        let prices: Vec<Decimal> = buf.iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .map(|(_, p)| *p)
            .collect();

        if prices.len() < 3 {
            return None;
        }

        // Compute returns
        let returns: Vec<Decimal> = prices.windows(2)
            .filter_map(|w| {
                if w[0].is_zero() { None }
                else { Some((w[1] - w[0]) / w[0]) }
            })
            .collect();

        if returns.is_empty() {
            return None;
        }

        // Mean
        let n = Decimal::from(returns.len() as u64);
        let mean: Decimal = returns.iter().sum::<Decimal>() / n;

        // Variance
        let variance: Decimal = returns.iter()
            .map(|r| {
                let diff = *r - mean;
                diff * diff
            })
            .sum::<Decimal>() / n;

        // Standard deviation (approximate sqrt via Newton's method)
        Some(decimal_sqrt(variance))
    }

    /// Get the raw price N milliseconds ago (for precise delta computation)
    ///
    /// Finds the closest observation to the target time.
    pub fn price_at(&self, asset: &str, ms_ago: i64) -> Option<Decimal> {
        let entry = self.histories.get(asset)?;
        let buf = entry.read().unwrap();

        if buf.is_empty() {
            return None;
        }

        let (latest_ts, _) = *buf.back()?;
        let target_ts = latest_ts - ms_ago;

        // Find the observation closest to target_ts
        buf.iter()
            .min_by_key(|(ts, _)| (*ts - target_ts).unsigned_abs())
            .map(|(_, p)| *p)
    }

    /// Number of observations in the last N milliseconds (data quality check)
    pub fn observation_count(&self, asset: &str, window_ms: i64) -> usize {
        let Some(entry) = self.histories.get(asset) else { return 0 };
        let buf = entry.read().unwrap();

        if buf.is_empty() {
            return 0;
        }

        let (latest_ts, _) = match buf.back() {
            Some(v) => *v,
            None => return 0,
        };
        let cutoff = latest_ts - window_ms;

        buf.iter().filter(|(ts, _)| *ts >= cutoff).count()
    }

    /// Number of tracked assets
    pub fn len(&self) -> usize {
        self.histories.len()
    }

    /// Whether any assets are tracked
    pub fn is_empty(&self) -> bool {
        self.histories.is_empty()
    }
}

/// Approximate square root for Decimal using Newton's method
fn decimal_sqrt(x: Decimal) -> Decimal {
    if x.is_zero() || x < Decimal::ZERO {
        return Decimal::ZERO;
    }

    let mut guess = x / Decimal::from(2);
    if guess.is_zero() {
        guess = Decimal::new(1, 10); // very small positive
    }

    // 20 iterations of Newton's method is plenty for our precision needs
    for _ in 0..20 {
        let new_guess = (guess + x / guess) / Decimal::from(2);
        if (new_guess - guess).abs() < Decimal::new(1, 18) {
            break;
        }
        guess = new_guess;
    }

    guess
}

impl Default for PriceHistory {
    fn default() -> Self {
        Self::new(600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_spot_price_state() {
        let state = SpotPriceState::new();

        assert!(state.get("btc").is_none());

        state.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50000),
            timestamp_ms: 1000,
        });

        assert_eq!(state.price("btc"), Some(dec!(50000)));
        assert_eq!(state.len(), 1);

        // Update overwrites
        state.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(51000),
            timestamp_ms: 2000,
        });

        assert_eq!(state.price("btc"), Some(dec!(51000)));
    }

    #[test]
    fn test_price_history_change() {
        let history = PriceHistory::new(100);

        // Record prices over a 5-second window
        history.record("btc", 1000, dec!(50000));
        history.record("btc", 2000, dec!(50100));
        history.record("btc", 3000, dec!(50200));
        history.record("btc", 4000, dec!(50300));
        history.record("btc", 5000, dec!(50500));

        // 5-second window: 50000 -> 50500 = +1%
        let change = history.price_change_pct("btc", 5000).unwrap();
        assert_eq!(change, dec!(0.01));

        // 2-second window: cutoff=3000, oldest >= 3000 is (3000, 50200)
        let change_2s = history.price_change_pct("btc", 2000).unwrap();
        // (50500 - 50200) / 50200 ≈ 0.005976
        assert!(change_2s > dec!(0.005) && change_2s < dec!(0.007));
    }

    #[test]
    fn test_price_history_ring_buffer() {
        let history = PriceHistory::new(3);

        history.record("eth", 1000, dec!(3000));
        history.record("eth", 2000, dec!(3100));
        history.record("eth", 3000, dec!(3200));
        history.record("eth", 4000, dec!(3300));

        // Should have dropped the first entry (3000 at ts=1000)
        // Oldest is now 3100 at ts=2000
        let change = history.price_change_pct("eth", 5000).unwrap();
        // (3300 - 3100) / 3100
        assert!(change > dec!(0.06) && change < dec!(0.07));
    }

    #[test]
    fn test_price_history_no_data() {
        let history = PriceHistory::new(100);
        assert!(history.price_change_pct("btc", 5000).is_none());
    }
}
