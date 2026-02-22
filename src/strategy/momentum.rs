//! 5-Minute Momentum Sniper Strategy
//!
//! Near the close of 5-minute crypto markets, this strategy:
//! 1. Checks the Binance spot price direction over the last 5 minutes
//! 2. If direction is clear (> min confidence), buys the winning outcome via FAK
//! 3. Pays ~93c for a token that resolves to $1 if correct
//!
//! Risk/reward: ~7% return per correct prediction, small size, high frequency.

use crate::api::types::{ConditionId, Side};
use crate::strategy::market_pair::MarketPairRegistry;
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info};

/// Momentum sniper configuration
#[derive(Debug, Clone)]
pub struct MomentumConfig {
    /// Seconds before close to trigger (default: 10)
    pub trigger_secs_before_close: i64,
    /// Minimum spot price change to act on (fractional, e.g. 0.0005 = 0.05%)
    pub min_direction_confidence: Decimal,
    /// Price to buy at (e.g. 0.93 for 7% expected return)
    pub buy_price: Decimal,
    /// Maximum size per trade in shares
    pub max_size: Decimal,
    /// Assets to trade (lowercase, e.g. ["btc", "eth"])
    pub assets: Vec<String>,
}

impl MomentumConfig {
    /// Conservative defaults for initial testing
    pub fn default_test() -> Self {
        Self {
            trigger_secs_before_close: 10,
            min_direction_confidence: dec!(0.0005), // 0.05%
            buy_price: dec!(0.93),
            max_size: dec!(50),
            assets: vec![
                "btc".to_string(),
                "eth".to_string(),
                "sol".to_string(),
                "xrp".to_string(),
            ],
        }
    }
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self::default_test()
    }
}

/// 5-minute momentum sniper strategy
pub struct MomentumStrategy {
    /// Configuration
    config: MomentumConfig,
    /// Market pair registry
    registry: Arc<MarketPairRegistry>,
    /// Markets we've already acted on (condition_id -> when)
    acted_on: DashMap<String, Instant>,
    /// Enabled flag
    enabled: bool,
}

impl MomentumStrategy {
    /// Create with config
    pub fn new(registry: Arc<MarketPairRegistry>, config: MomentumConfig) -> Self {
        Self {
            config,
            registry,
            acted_on: DashMap::new(),
            enabled: true,
        }
    }

    /// Extract asset name from event slug (e.g. "btc-updown-5m-1740000000" -> "btc")
    fn extract_asset(slug: &str) -> Option<String> {
        let first_part = slug.split('-').next()?;
        let asset = first_part.to_lowercase();
        // Must be a known crypto asset name
        match asset.as_str() {
            "btc" | "bitcoin" => Some("btc".to_string()),
            "eth" | "ethereum" => Some("eth".to_string()),
            "sol" | "solana" => Some("sol".to_string()),
            "xrp" => Some("xrp".to_string()),
            _ => None,
        }
    }

    /// Check if this is a 5-min market
    fn is_5min_market(slug: &str) -> bool {
        slug.contains("-5m-")
    }

    /// Clean up old acted_on entries (older than 10 minutes)
    fn cleanup_acted(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(600);
        self.acted_on.retain(|_, v| *v > cutoff);
    }
}

impl Strategy for MomentumStrategy {
    fn name(&self) -> &str {
        "MomentumSniper"
    }

    fn priority(&self) -> u8 {
        60 // Higher than default, below arb
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        // Subscribe to all markets — we filter by slug in on_tick
        Vec::new()
    }

    fn on_book_update(
        &self,
        _market_id: &ConditionId,
        _token_id: &crate::api::types::TokenId,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        // Momentum strategy acts on tick, not on book updates
        Vec::new()
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Need spot prices to function
        if ctx.spot_prices.is_none() || ctx.price_history.is_none() {
            return Vec::new();
        }

        // Periodic cleanup
        self.cleanup_acted();

        let now_unix = ctx.utc_now.timestamp();
        let mut intents = Vec::new();

        // Iterate all registered 5-min markets
        let pairs = self.registry.filter(|pair| {
            Self::is_5min_market(&pair.event_slug) && pair.close_time.is_some()
        });

        for pair in pairs {
            let condition_id = &pair.condition_id;

            // Skip if already acted on
            if self.acted_on.contains_key(condition_id) {
                continue;
            }

            let close_time = match pair.close_time {
                Some(t) => t,
                None => continue,
            };

            // Check if within trigger window
            let secs_until_close = close_time - now_unix;
            if secs_until_close < 0 || secs_until_close > self.config.trigger_secs_before_close {
                continue;
            }

            // Extract asset
            let asset = match Self::extract_asset(&pair.event_slug) {
                Some(a) => a,
                None => continue,
            };

            // Only trade configured assets
            if !self.config.assets.contains(&asset) {
                continue;
            }

            // Check spot price direction over 5-min window (300,000 ms)
            let change_pct = match ctx.spot_change_pct(&asset, 300_000) {
                Some(c) => c,
                None => {
                    debug!(
                        "MomentumSniper: no 5-min price data for {} (market {})",
                        asset, condition_id
                    );
                    continue;
                }
            };

            // Check if direction is clear enough
            if change_pct.abs() < self.config.min_direction_confidence {
                debug!(
                    "MomentumSniper: {} change {:.4}% too small (need {:.4}%)",
                    asset,
                    change_pct * dec!(100),
                    self.config.min_direction_confidence * dec!(100)
                );
                continue;
            }

            // Determine direction: positive change = UP, negative = DOWN
            let (winning_token, direction) = if change_pct > Decimal::ZERO {
                (pair.up_token_id().clone(), "UP")
            } else {
                (pair.down_token_id().clone(), "DOWN")
            };

            // Cap size to available cash (truncate to 2dp — Polymarket max lot precision)
            let max_affordable = (ctx.available_cash() / self.config.buy_price).floor();
            let trade_size = self.config.max_size.min(max_affordable)
                .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero);

            // Need at least $1 order value (Polymarket minimum)
            let min_shares = (dec!(1) / self.config.buy_price).ceil();
            if trade_size < min_shares {
                debug!(
                    "MomentumSniper: insufficient cash ${} for {} trade (need {} shares min)",
                    ctx.available_cash(),
                    asset,
                    min_shares
                );
                continue;
            }

            // Check that there's ask liquidity at our target price
            if let Some(best_ask) = ctx.best_ask(&winning_token) {
                if best_ask > self.config.buy_price {
                    debug!(
                        "MomentumSniper: {} {} best ask {} > target {}",
                        asset, direction, best_ask, self.config.buy_price
                    );
                    continue;
                }
            } else {
                debug!("MomentumSniper: no ask data for {} {}", asset, direction);
                continue;
            }

            info!(
                "5m Momentum: {} {} (change: {:.4}%, {}s to close) → BUY {} @ {}",
                asset,
                direction,
                change_pct * dec!(100),
                secs_until_close,
                &winning_token[..winning_token.len().min(12)],
                self.config.buy_price
            );

            let intent = OrderIntent::new(
                condition_id.clone(),
                winning_token,
                Side::Buy,
                self.config.buy_price,
                trade_size,
                Urgency::Normal, // FAK
                format!(
                    "5m momentum {} {} ({:+.3}%)",
                    asset,
                    direction,
                    change_pct * dec!(100)
                ),
                self.name().to_string(),
            )
            .with_fee_rate(pair.fee_rate_bps)
            .with_priority(60);

            intents.push(intent);

            // Mark as acted
            self.acted_on.insert(condition_id.clone(), Instant::now());
        }

        intents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::state::{OrderBookState, PriceHistory, SpotPriceState, SpotPriceUpdate};

    #[test]
    fn test_extract_asset() {
        assert_eq!(
            MomentumStrategy::extract_asset("btc-updown-5m-1740000000"),
            Some("btc".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("eth-updown-5m-1740000000"),
            Some("eth".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("sol-updown-5m-1740000000"),
            Some("sol".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("unknown-updown-5m-1740000000"),
            None
        );
    }

    #[test]
    fn test_is_5min_market() {
        assert!(MomentumStrategy::is_5min_market("btc-updown-5m-1740000000"));
        assert!(!MomentumStrategy::is_5min_market(
            "btc-updown-15m-1740000000"
        ));
        assert!(!MomentumStrategy::is_5min_market("some-other-slug"));
    }

    #[test]
    fn test_momentum_triggers_on_up_move() {
        let registry = Arc::new(MarketPairRegistry::new());

        // Create a 5-min market that closes 5 seconds from now
        let now = chrono::Utc::now().timestamp();
        let pair = crate::strategy::MarketPair::new_up_down(
            "0x5m_test".to_string(),
            "up_token_123".to_string(),
            "down_token_456".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 5); // closes in 5 seconds

        registry.register(pair);

        let config = MomentumConfig {
            trigger_secs_before_close: 10,
            min_direction_confidence: dec!(0.0005),
            buy_price: dec!(0.93),
            max_size: dec!(50),
            assets: vec!["btc".to_string()],
        };

        let strategy = MomentumStrategy::new(registry, config);

        // Set up spot prices with BTC going UP
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(100);

        let now_ms = chrono::Utc::now().timestamp_millis();
        // 5 minutes ago: $50000
        history.record("btc", now_ms - 300_000, dec!(50000));
        // Now: $50100 (+0.2%)
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50100),
            timestamp_ms: now_ms,
        });
        history.record("btc", now_ms, dec!(50100));

        // Set up order book with ask at 0.90 (below our buy_price of 0.93)
        let books = OrderBookState::new();
        books.update_book(
            "up_token_123".to_string(),
            "0x5m_test".to_string(),
            vec![],
            vec![crate::api::types::PriceLevel {
                price: "0.90".to_string(),
                size: "100".to_string(),
            }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let intents = strategy.on_tick(&ctx);

        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].token_id, "up_token_123"); // BTC going UP → buy UP token
        assert_eq!(intents[0].price, dec!(0.93));
        assert_eq!(intents[0].urgency, Urgency::Normal); // FAK
        assert_eq!(intents[0].side, Side::Buy);
    }

    #[test]
    fn test_momentum_no_double_act() {
        let registry = Arc::new(MarketPairRegistry::new());

        let now = chrono::Utc::now().timestamp();
        let pair = crate::strategy::MarketPair::new_up_down(
            "0x5m_test".to_string(),
            "up_token".to_string(),
            "down_token".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 5);

        registry.register(pair);

        let strategy = MomentumStrategy::new(registry, MomentumConfig::default_test());

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(100);
        let now_ms = chrono::Utc::now().timestamp_millis();
        history.record("btc", now_ms - 300_000, dec!(50000));
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50100),
            timestamp_ms: now_ms,
        });
        history.record("btc", now_ms, dec!(50100));

        let books = OrderBookState::new();
        books.update_book(
            "up_token".to_string(),
            "0x5m_test".to_string(),
            vec![],
            vec![crate::api::types::PriceLevel {
                price: "0.90".to_string(),
                size: "100".to_string(),
            }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        // First tick: should generate intent
        let intents1 = strategy.on_tick(&ctx);
        assert_eq!(intents1.len(), 1);

        // Second tick: should NOT generate (already acted)
        let intents2 = strategy.on_tick(&ctx);
        assert_eq!(intents2.len(), 0);
    }

    #[test]
    fn test_momentum_ignores_15min_markets() {
        let registry = Arc::new(MarketPairRegistry::new());

        let now = chrono::Utc::now().timestamp();
        let pair = crate::strategy::MarketPair::new_up_down(
            "0x15m_test".to_string(),
            "up_token".to_string(),
            "down_token".to_string(),
        )
        .with_event_slug("btc-updown-15m-1740000000") // 15-min, not 5-min
        .with_close_time(now + 5);

        registry.register(pair);

        let strategy = MomentumStrategy::new(registry, MomentumConfig::default_test());

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(100);
        let now_ms = chrono::Utc::now().timestamp_millis();
        history.record("btc", now_ms - 300_000, dec!(50000));
        history.record("btc", now_ms, dec!(50100));

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let intents = strategy.on_tick(&ctx);
        assert_eq!(intents.len(), 0); // Should not act on 15-min markets
    }
}
