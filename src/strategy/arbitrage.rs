//! Mathematical Arbitrage Strategy
//!
//! Detects arbitrage opportunities where YES + NO < $1.00 - required_edge
//! and returns two OrderIntents to execute both legs atomically.
//!
//! ## Strategy Logic
//!
//! In a binary market, one of YES or NO will resolve to $1.00.
//! If we can buy both for less than $1.00 (minus fees/slippage),
//! we profit at resolution regardless of outcome.
//!
//! ## Execution
//!
//! Returns `Urgency::Immediate` intents → converted to FOK orders by TakerPolicy.
//! Both legs are grouped so partial fill handling knows they're linked.

use crate::api::types::{ConditionId, Side, TokenId};
use crate::ledger::Fill;
use crate::strategy::edge_calculator::{EdgeCalculator, EdgeConfig};
use crate::strategy::market_pair::{MarketPair, MarketPairRegistry};
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Diagnostic statistics for arbitrage strategy
#[derive(Debug, Clone)]
pub struct ArbStats {
    pub checks_total: u64,
    pub checks_not_two_sided: u64,
    pub checks_quick_fail: u64,
    pub checks_full_calc_fail: u64,
    pub checks_size_too_small: u64,
    pub checks_exposure_limit: u64,
    pub intents_generated: u64,
}

/// Configuration for the math arb strategy
#[derive(Debug, Clone)]
pub struct MathArbConfig {
    /// Minimum edge to execute (default: 3 cents for taker)
    pub min_edge: Decimal,

    /// Maximum position size per trade
    pub max_position_size: Decimal,

    /// Minimum position size per trade
    pub min_position_size: Decimal,

    /// Maximum total exposure across all positions
    pub max_total_exposure: Decimal,

    /// Cooldown between trades on same market (ms)
    pub cooldown_ms: u64,

    /// Whether to use maker execution (lower edge, GTC orders)
    pub use_maker_execution: bool,
}

impl Default for MathArbConfig {
    fn default() -> Self {
        Self {
            min_edge: dec!(0.03),       // 3 cents minimum edge
            max_position_size: dec!(500), // Max $500 per leg
            min_position_size: dec!(10),  // Min $10 per leg
            max_total_exposure: dec!(2000), // Max $2000 total
            cooldown_ms: 1000,           // 1 second cooldown
            use_maker_execution: false,
        }
    }
}

impl MathArbConfig {
    /// Create config for taker execution (FOK orders, higher edge)
    pub fn taker() -> Self {
        Self::default()
    }

    /// Create config for maker execution (GTC orders, lower edge)
    pub fn maker() -> Self {
        Self {
            min_edge: dec!(0.01), // 1 cent minimum (no fees)
            use_maker_execution: true,
            ..Self::default()
        }
    }
}

/// Mathematical arbitrage strategy
///
/// Implements the Strategy trait. On each book update, checks if
/// the market presents an arb opportunity and returns order intents.
pub struct MathArbStrategy {
    /// Strategy name
    name: String,

    /// Configuration
    config: MathArbConfig,

    /// Market pair registry (for YES/NO lookups)
    registry: Arc<MarketPairRegistry>,

    /// Edge calculator
    edge_calculator: EdgeCalculator,

    /// Is strategy enabled?
    enabled: AtomicBool,

    /// Last trade timestamp per market (for cooldown)
    last_trade: dashmap::DashMap<ConditionId, Instant>,

    /// Trade counter
    trade_count: AtomicU64,

    /// Current exposure (approximate, for quick checks)
    current_exposure: std::sync::RwLock<Decimal>,

    /// Diagnostic counters
    checks_total: AtomicU64,
    checks_not_two_sided: AtomicU64,
    checks_quick_fail: AtomicU64,
    checks_full_calc_fail: AtomicU64,
    checks_size_too_small: AtomicU64,
    checks_exposure_limit: AtomicU64,
}

impl MathArbStrategy {
    /// Create a new math arb strategy
    pub fn new(registry: Arc<MarketPairRegistry>) -> Self {
        Self::with_config(registry, MathArbConfig::default())
    }

    /// Create with custom config
    pub fn with_config(registry: Arc<MarketPairRegistry>, config: MathArbConfig) -> Self {
        let edge_config = if config.use_maker_execution {
            EdgeConfig::maker()
        } else {
            EdgeConfig::taker()
        };

        Self {
            name: "MathArbStrategy".to_string(),
            config,
            registry,
            edge_calculator: EdgeCalculator::with_config(edge_config),
            enabled: AtomicBool::new(true),
            last_trade: dashmap::DashMap::new(),
            trade_count: AtomicU64::new(0),
            current_exposure: std::sync::RwLock::new(Decimal::ZERO),
            checks_total: AtomicU64::new(0),
            checks_not_two_sided: AtomicU64::new(0),
            checks_quick_fail: AtomicU64::new(0),
            checks_full_calc_fail: AtomicU64::new(0),
            checks_size_too_small: AtomicU64::new(0),
            checks_exposure_limit: AtomicU64::new(0),
        }
    }

    /// Set enabled state
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Get trade count
    pub fn trade_count(&self) -> u64 {
        self.trade_count.load(Ordering::Relaxed)
    }

    /// Get diagnostic stats
    pub fn get_stats(&self) -> ArbStats {
        ArbStats {
            checks_total: self.checks_total.load(Ordering::Relaxed),
            checks_not_two_sided: self.checks_not_two_sided.load(Ordering::Relaxed),
            checks_quick_fail: self.checks_quick_fail.load(Ordering::Relaxed),
            checks_full_calc_fail: self.checks_full_calc_fail.load(Ordering::Relaxed),
            checks_size_too_small: self.checks_size_too_small.load(Ordering::Relaxed),
            checks_exposure_limit: self.checks_exposure_limit.load(Ordering::Relaxed),
            intents_generated: self.trade_count.load(Ordering::Relaxed),
        }
    }

    /// Check if market is on cooldown
    fn is_on_cooldown(&self, condition_id: &ConditionId) -> bool {
        if let Some(last) = self.last_trade.get(condition_id) {
            last.elapsed().as_millis() < self.config.cooldown_ms as u128
        } else {
            false
        }
    }

    /// Record a trade for cooldown purposes
    fn record_trade(&self, condition_id: &ConditionId) {
        self.last_trade.insert(condition_id.clone(), Instant::now());
        self.trade_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Check exposure limit
    fn check_exposure_limit(&self, additional: Decimal) -> bool {
        let current = *self.current_exposure.read().unwrap();
        current + additional <= self.config.max_total_exposure
    }

    /// Update exposure tracking
    fn add_exposure(&self, amount: Decimal) {
        let mut exposure = self.current_exposure.write().unwrap();
        *exposure += amount;
    }

    /// Try to find arb opportunity for a market pair
    fn check_arb_opportunity(
        &self,
        pair: &MarketPair,
        ctx: &StrategyContext,
    ) -> Option<Vec<OrderIntent>> {
        self.checks_total.fetch_add(1, Ordering::Relaxed);

        // Get books for both tokens
        let yes_book = ctx.books.get_book(&pair.yes_token_id)?;
        let no_book = ctx.books.get_book(&pair.no_token_id)?;

        // Both books need to be two-sided
        if !yes_book.is_two_sided() || !no_book.is_two_sided() {
            self.checks_not_two_sided.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        // Quick check first
        let quick_result = self.edge_calculator.quick_check(ctx.books, &pair.yes_token_id, &pair.no_token_id);
        
        if quick_result.is_none() {
            self.checks_quick_fail.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        
        let (yes_ask, no_ask, edge) = quick_result.unwrap();

        debug!(
            market = %pair.condition_id,
            yes_ask = %yes_ask,
            no_ask = %no_ask,
            edge = %edge,
            "Arb opportunity detected (quick check)"
        );

        // Full edge calculation
        let calc = self.edge_calculator.calculate(
            &yes_book,
            &no_book,
            pair.fee_rate_bps,
            self.config.min_position_size,
        );

        if !calc.is_profitable {
            self.checks_full_calc_fail.fetch_add(1, Ordering::Relaxed);
            debug!(
                market = %pair.condition_id,
                actual_edge = %calc.actual_edge,
                required_edge = %calc.required_edge,
                "Not profitable after full calculation"
            );
            return None;
        }

        // Determine trade size
        let max_by_book = calc.max_size;
        let max_by_config = self.config.max_position_size;
        let max_by_exposure = {
            let current = *self.current_exposure.read().unwrap();
            (self.config.max_total_exposure - current) / dec!(2) // Divided by 2 since we're buying both sides
        };

        let trade_size = max_by_book
            .min(max_by_config)
            .min(max_by_exposure)
            .max(Decimal::ZERO);

        if trade_size < self.config.min_position_size {
            self.checks_size_too_small.fetch_add(1, Ordering::Relaxed);
            debug!(
                market = %pair.condition_id,
                trade_size = %trade_size,
                min_size = %self.config.min_position_size,
                "Trade size below minimum"
            );
            return None;
        }

        // Check exposure limit
        let total_notional = (yes_ask + no_ask) * trade_size;
        if !self.check_exposure_limit(total_notional) {
            self.checks_exposure_limit.fetch_add(1, Ordering::Relaxed);
            debug!(
                market = %pair.condition_id,
                "Exposure limit would be exceeded"
            );
            return None;
        }

        info!(
            market = %pair.condition_id,
            yes_ask = %yes_ask,
            no_ask = %no_ask,
            edge_cents = %((calc.actual_edge * dec!(100)).round()),
            trade_size = %trade_size,
            "🎯 Arb opportunity! Executing..."
        );

        // Generate group ID for linked orders
        let group_id = format!("arb-{}", Uuid::new_v4());

        // Determine urgency based on config
        let urgency = if self.config.use_maker_execution {
            Urgency::Passive
        } else {
            Urgency::Immediate
        };

        // Create order intents for both legs
        let yes_intent = OrderIntent::new(
            pair.condition_id.clone(),
            pair.yes_token_id.clone(),
            Side::Buy,
            yes_ask, // Buy at ask
            trade_size,
            urgency,
            format!("Arb YES leg, edge: {:.1}%", calc.actual_edge * dec!(100)),
            self.name.clone(),
        )
        .with_group(group_id.clone())
        .with_priority(100); // High priority for arb

        let no_intent = OrderIntent::new(
            pair.condition_id.clone(),
            pair.no_token_id.clone(),
            Side::Buy,
            no_ask, // Buy at ask
            trade_size,
            urgency,
            format!("Arb NO leg, edge: {:.1}%", calc.actual_edge * dec!(100)),
            self.name.clone(),
        )
        .with_group(group_id)
        .with_priority(100);

        // Record trade for cooldown
        self.record_trade(&pair.condition_id);

        // Update exposure tracking
        self.add_exposure(total_notional);

        Some(vec![yes_intent, no_intent])
    }
}

impl Strategy for MathArbStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        100 // High priority - arb opportunities are time-sensitive
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        // Subscribe to all registered markets
        self.registry.all_condition_ids()
    }

    fn on_book_update(
        &self,
        market_id: &ConditionId,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        // Skip if disabled
        if !self.is_enabled() {
            return vec![];
        }

        // Look up the market pair for this token
        let pair = match self.registry.get_by_token(token_id) {
            Some(p) => p,
            None => {
                debug!(token = %token_id, "Token not in registry");
                return vec![];
            }
        };

        // Verify market_id matches
        if &pair.condition_id != market_id {
            warn!(
                token = %token_id,
                expected_market = %pair.condition_id,
                actual_market = %market_id,
                "Market ID mismatch"
            );
            return vec![];
        }

        // Check cooldown
        if self.is_on_cooldown(market_id) {
            return vec![];
        }

        // Check for arb opportunity
        self.check_arb_opportunity(&pair, ctx).unwrap_or_default()
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Track fills to update exposure
        // Reduce exposure when positions are closed
        let notional = fill.notional();

        match fill.side {
            Side::Sell => {
                // Selling reduces exposure
                let mut exposure = self.current_exposure.write().unwrap();
                *exposure = (*exposure - notional).max(Decimal::ZERO);
            }
            Side::Buy => {
                // Buying increases exposure (already tracked in check_arb_opportunity)
                // But we may need to reconcile if fill amount differs
            }
        }

        vec![]
    }

    fn on_tick(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Could scan all markets here, but we rely on book updates instead
        vec![]
    }

    fn on_shutdown(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // TODO: Could generate intents to close all positions
        // For now, just log
        info!(
            trades = self.trade_count(),
            "MathArbStrategy shutting down"
        );
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::PriceLevel;
    use crate::ledger::Ledger;
    use crate::state::OrderBookState;

    fn setup_registry() -> Arc<MarketPairRegistry> {
        let registry = Arc::new(MarketPairRegistry::new());

        registry.register(
            MarketPair::new(
                "0xmarket123".to_string(),
                "yes_token_123".to_string(),
                "no_token_456".to_string(),
            )
            .with_fee_rate(0)
            .with_description("Test market"),
        );

        registry
    }

    fn setup_books_with_arb() -> OrderBookState {
        let books = OrderBookState::new();

        // YES ask = 0.48
        books.update_book(
            "yes_token_123".to_string(),
            "0xmarket123".to_string(),
            vec![PriceLevel {
                price: "0.47".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.48".to_string(),
                size: "1000".to_string(),
            }],
            Some(1234567890),
            None,
        );

        // NO ask = 0.49
        books.update_book(
            "no_token_456".to_string(),
            "0xmarket123".to_string(),
            vec![PriceLevel {
                price: "0.48".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.49".to_string(),
                size: "1000".to_string(),
            }],
            Some(1234567890),
            None,
        );

        books
    }

    fn setup_books_no_arb() -> OrderBookState {
        let books = OrderBookState::new();

        // YES ask = 0.51, NO ask = 0.51 (combined > 1.0)
        books.update_book(
            "yes_token_123".to_string(),
            "0xmarket123".to_string(),
            vec![PriceLevel {
                price: "0.50".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.51".to_string(),
                size: "1000".to_string(),
            }],
            Some(1234567890),
            None,
        );

        books.update_book(
            "no_token_456".to_string(),
            "0xmarket123".to_string(),
            vec![PriceLevel {
                price: "0.50".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.51".to_string(),
                size: "1000".to_string(),
            }],
            Some(1234567890),
            None,
        );

        books
    }

    #[test]
    fn test_detects_arb_opportunity() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::new(registry);
        let books = setup_books_with_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );

        assert_eq!(intents.len(), 2);

        // Verify both intents are linked
        assert!(intents[0].group_id.is_some());
        assert_eq!(intents[0].group_id, intents[1].group_id);

        // Verify sides
        assert_eq!(intents[0].side, Side::Buy);
        assert_eq!(intents[1].side, Side::Buy);

        // Verify urgency (taker = immediate)
        assert_eq!(intents[0].urgency, Urgency::Immediate);
    }

    #[test]
    fn test_no_arb_when_unprofitable() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::new(registry);
        let books = setup_books_no_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );

        assert!(intents.is_empty());
    }

    #[test]
    fn test_cooldown() {
        let registry = setup_registry();
        let mut config = MathArbConfig::default();
        config.cooldown_ms = 10000; // 10 second cooldown
        let strategy = MathArbStrategy::with_config(registry, config);

        let books = setup_books_with_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        // First call should generate intents
        let intents1 = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );
        assert_eq!(intents1.len(), 2);

        // Second call should be on cooldown
        let intents2 = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );
        assert!(intents2.is_empty());
    }

    #[test]
    fn test_disabled_strategy() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::new(registry);
        strategy.set_enabled(false);

        let books = setup_books_with_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );

        assert!(intents.is_empty());
    }

    #[test]
    fn test_unknown_token() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::new(registry);
        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xunknown".to_string(),
            &"unknown_token".to_string(),
            &ctx,
        );

        assert!(intents.is_empty());
    }

    #[test]
    fn test_maker_config() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::with_config(registry, MathArbConfig::maker());

        let books = setup_books_with_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );

        // Should still detect opportunity
        assert_eq!(intents.len(), 2);

        // But with Passive urgency
        assert_eq!(intents[0].urgency, Urgency::Passive);
    }

    #[test]
    fn test_exposure_limit() {
        let registry = setup_registry();
        let mut config = MathArbConfig::default();
        config.max_total_exposure = dec!(10); // Very low limit
        let strategy = MathArbStrategy::with_config(registry, config);

        let books = setup_books_with_arb();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        // Should fail due to exposure limit
        let intents = strategy.on_book_update(
            &"0xmarket123".to_string(),
            &"yes_token_123".to_string(),
            &ctx,
        );

        // Trade size would be ~$9.70 which is below min of $10
        assert!(intents.is_empty());
    }
}
