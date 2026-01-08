//! Mathematical Arbitrage Strategy
//!
//! Detects arbitrage opportunities where YES + NO < $1.00 - required_edge
//! and returns two OrderIntents to execute both legs.
//!
//! ## Strategy Logic
//!
//! In a binary market, one of YES or NO will resolve to $1.00.
//! If we can buy both for less than $1.00 (minus fees/slippage),
//! we profit at resolution regardless of outcome.
//!
//! ## Execution Modes
//!
//! - **Maker mode (default for live_test):** Returns `Urgency::Passive` intents
//!   → converted to GTC orders by MakerPolicy. Zero fees on 15-min crypto markets.
//!   Both legs submitted in parallel via `tokio::join!`.
//!
//! - **Taker mode:** Returns `Urgency::Immediate` intents → converted to FOK orders.
//!   Sequential execution with rollback if second leg fails.
//!
//! ## Dynamic Share Sizing
//!
//! Polymarket requires minimum $1.00 order value. The strategy calculates
//! `min_shares = ceil($1.00 / min_leg_price)` and ensures both legs exceed this.

use crate::api::types::{ConditionId, Side, TokenId};
use crate::ledger::Fill;
use crate::strategy::edge_calculator::{EdgeCalculator, EdgeConfig};
use crate::strategy::market_pair::{MarketPair, MarketPairRegistry};
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Minimum order value required by Polymarket API
/// Orders with notional value (price × size) below this will be rejected
const MIN_ORDER_VALUE: Decimal = dec!(1.00);
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

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

    /// Config for Phase 9 live testing with $100 capital
    ///
    /// Settings:
    /// - 1% min edge for safer arbitrage
    /// - Position sizes $5-$15 per leg to allow dynamic sizing for $1 min order value
    /// - $50 max exposure (aggressive risk tolerance)
    /// - Maker execution for zero fees + rebates
    /// - Share count auto-adjusts to meet $1.00 minimum order value
    pub fn live_test() -> Self {
        Self {
            min_edge: dec!(0.01),         // 1% - safer edge requirement
            max_position_size: dec!(15),  // Allow up to 15 shares for low-priced legs
            min_position_size: dec!(5),   // $5 per leg min (market min is 5)
            max_total_exposure: dec!(50), // $50 max exposure
            cooldown_ms: 3000,            // 3 second cooldown
            use_maker_execution: true,    // Maker mode for zero fees
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

    /// Best edge observed (even if not profitable) - for diagnostics
    best_edge_seen: std::sync::RwLock<Decimal>,

    /// Count of near-miss opportunities (edge > 0 but < threshold)
    near_misses: AtomicU64,

    /// Count of opportunities that passed quick check
    quick_check_passes: AtomicU64,
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
            best_edge_seen: std::sync::RwLock::new(Decimal::ZERO),
            near_misses: AtomicU64::new(0),
            quick_check_passes: AtomicU64::new(0),
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

    /// Get near-miss count (edge > 0 but below threshold)
    pub fn near_miss_count(&self) -> u64 {
        self.near_misses.load(Ordering::Relaxed)
    }

    /// Get quick check pass count
    pub fn quick_check_pass_count(&self) -> u64 {
        self.quick_check_passes.load(Ordering::Relaxed)
    }

    /// Get best edge seen (for diagnostics)
    pub fn best_edge_seen(&self) -> Decimal {
        *self.best_edge_seen.read().unwrap()
    }

    /// Update best edge tracking
    fn update_best_edge(&self, edge: Decimal) {
        let mut best = self.best_edge_seen.write().unwrap();
        if edge > *best {
            *best = edge;
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
        // Get books for both tokens
        let yes_book = ctx.books.get_book(&pair.yes_token_id)?;
        let no_book = ctx.books.get_book(&pair.no_token_id)?;

        // Both books need to be two-sided
        if !yes_book.is_two_sided() || !no_book.is_two_sided() {
            return None;
        }

        // Quick check first
        let (yes_ask, no_ask, edge) =
            self.edge_calculator
                .quick_check(ctx.books, &pair.yes_token_id, &pair.no_token_id)?;

        // Track that we passed quick check
        self.quick_check_passes.fetch_add(1, Ordering::Relaxed);
        self.update_best_edge(edge);

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
            // Track near-miss: edge > 0 but below required threshold
            if calc.actual_edge > Decimal::ZERO && calc.actual_edge < calc.required_edge {
                self.near_misses.fetch_add(1, Ordering::Relaxed);
                info!(
                    market = %pair.condition_id,
                    actual_edge = %calc.actual_edge,
                    required_edge = %calc.required_edge,
                    yes_ask = %yes_ask,
                    no_ask = %no_ask,
                    "📊 Near-miss: edge positive but below threshold"
                );
            }
            debug!(
                market = %pair.condition_id,
                actual_edge = %calc.actual_edge,
                required_edge = %calc.required_edge,
                "Not profitable after full calculation"
            );
            return None;
        }

        // Calculate maker prices (1 tick below ask) - need these for min size calculation
        let tick = dec!(0.01);
        let yes_maker_price = yes_ask - tick;
        let no_maker_price = no_ask - tick;

        // Get prices for orders (maker or taker)
        let (yes_price, no_price) = if self.config.use_maker_execution {
            (yes_maker_price, no_maker_price)
        } else {
            (yes_ask, no_ask)
        };

        // Calculate minimum shares needed for each leg to meet $1.00 order value
        // Formula: min_shares = ceil($1.00 / price)
        // We use the lower of the two prices to ensure BOTH legs meet the minimum
        let min_price = yes_price.min(no_price);
        let min_shares_for_order_value = if min_price > Decimal::ZERO {
            (MIN_ORDER_VALUE / min_price).ceil()
        } else {
            return None; // Can't divide by zero price
        };

        // Determine trade size - use the HIGHER of config minimum and order value minimum
        let effective_min_size = self.config.min_position_size.max(min_shares_for_order_value);
        
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

        // Check if trade size meets the effective minimum (includes $1 order value requirement)
        if trade_size < effective_min_size {
            debug!(
                market = %pair.condition_id,
                trade_size = %trade_size,
                effective_min_size = %effective_min_size,
                config_min = %self.config.min_position_size,
                min_for_order_value = %min_shares_for_order_value,
                min_price = %min_price,
                "Trade size below effective minimum (includes $1 order value requirement)"
            );
            return None;
        }

        // Check exposure limit
        let total_notional = (yes_ask + no_ask) * trade_size;
        if !self.check_exposure_limit(total_notional) {
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
            yes_price = %yes_price,
            no_price = %no_price,
            edge_cents = %((calc.actual_edge * dec!(100)).round()),
            trade_size = %trade_size,
            min_shares_required = %min_shares_for_order_value,
            maker_mode = %self.config.use_maker_execution,
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
            yes_price, // Bid for maker, ask for taker
            trade_size,
            urgency,
            format!("Arb YES leg, edge: {:.1}%", calc.actual_edge * dec!(100)),
            self.name.clone(),
        )
        .with_group(group_id.clone())
        .with_priority(100) // High priority for arb
        .with_fee_rate(pair.fee_rate_bps);

        let no_intent = OrderIntent::new(
            pair.condition_id.clone(),
            pair.no_token_id.clone(),
            Side::Buy,
            no_price, // Bid for maker, ask for taker
            trade_size,
            urgency,
            format!("Arb NO leg, edge: {:.1}%", calc.actual_edge * dec!(100)),
            self.name.clone(),
        )
        .with_group(group_id)
        .with_priority(100)
        .with_fee_rate(pair.fee_rate_bps);

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
        // Log comprehensive diagnostics on shutdown
        let best_edge = self.best_edge_seen();
        info!(
            trades = self.trade_count(),
            quick_check_passes = self.quick_check_pass_count(),
            near_misses = self.near_miss_count(),
            best_edge_pct = %((best_edge * dec!(100)).round_dp(2)),
            "📊 MathArbStrategy shutting down - Final stats"
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

    #[test]
    fn test_live_test_config() {
        let config = MathArbConfig::live_test();

        // Verify Phase 9 live test settings (1% edge for better fills)
        assert_eq!(config.min_edge, dec!(0.01));          // 1% edge (more conservative)
        assert_eq!(config.max_position_size, dec!(15));  // Up to 15 shares for low-priced legs
        assert_eq!(config.min_position_size, dec!(5));   // $5 per leg (market min)
        assert_eq!(config.max_total_exposure, dec!(50)); // $50 max exposure
        assert_eq!(config.cooldown_ms, 3000);            // 3 second cooldown
        assert!(config.use_maker_execution);             // Maker mode enabled
    }

    #[test]
    fn test_near_miss_tracking() {
        let registry = setup_registry();
        let strategy = MathArbStrategy::with_config(registry, MathArbConfig::live_test());

        // Initial state
        assert_eq!(strategy.near_miss_count(), 0);
        assert_eq!(strategy.quick_check_pass_count(), 0);
        assert_eq!(strategy.best_edge_seen(), Decimal::ZERO);
    }
}
