//! Strategy traits and core abstractions
//!
//! Strategies output WHAT they want (OrderIntent), not HOW to execute.
//! The execution layer (policies + executor) handles the HOW.

use crate::api::types::{ConditionId, Side, TokenId};
use crate::execution::OrderTracker;
use crate::ledger::{Fill, Ledger, Position};
use crate::state::{OrderBookState, PriceHistory, SpotPriceState};
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Instant;

// ============================================================================
// URGENCY - How quickly does the strategy need execution?
// ============================================================================

/// Execution urgency - affects order type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Must execute immediately or not at all (FOK for takers)
    /// Use for: arbitrage where both legs must fill atomically
    Immediate,

    /// Execute soon, accept partial fills (FAK for takers)
    /// Use for: directional bets where partial is acceptable
    Normal,

    /// Willing to wait for better price (GTC for makers)
    /// Use for: capturing maker rebates, passive liquidity
    Passive,
}

impl Default for Urgency {
    fn default() -> Self {
        Urgency::Normal
    }
}

// ============================================================================
// ORDER INTENT - What the strategy wants to do
// ============================================================================

/// An order intent - what the strategy wants, not how to execute
///
/// Strategies return OrderIntents, which the execution layer converts
/// to actual OrderParams based on the active ExecutionPolicy.
#[derive(Debug, Clone)]
pub struct OrderIntent {
    /// Market/condition ID
    pub market_id: ConditionId,

    /// Token to trade (YES or NO token)
    pub token_id: TokenId,

    /// Buy or sell
    pub side: Side,

    /// Target price (limit price)
    pub price: Decimal,

    /// Desired size in shares
    pub size: Decimal,

    /// How urgently this needs to execute
    pub urgency: Urgency,

    /// Human-readable reason for logging/debugging
    pub reason: String,

    /// Strategy that generated this intent
    pub strategy_name: String,

    /// Optional: linked intent ID for multi-leg orders (e.g., arb)
    /// Both legs of an arb should share the same group_id
    pub group_id: Option<String>,

    /// Priority (higher = more important, for conflict resolution)
    pub priority: u8,

    /// Created timestamp
    pub created_at: Instant,

    /// Fee rate in basis points (1000 = 10% for 15-min crypto markets)
    pub fee_rate_bps: u32,
}

impl OrderIntent {
    /// Create a new order intent
    pub fn new(
        market_id: ConditionId,
        token_id: TokenId,
        side: Side,
        price: Decimal,
        size: Decimal,
        urgency: Urgency,
        reason: impl Into<String>,
        strategy_name: impl Into<String>,
    ) -> Self {
        Self {
            market_id,
            token_id,
            side,
            price,
            size,
            urgency,
            reason: reason.into(),
            strategy_name: strategy_name.into(),
            group_id: None,
            priority: 50, // Default middle priority
            created_at: Instant::now(),
            fee_rate_bps: 0, // Default to 0, strategies should set this
        }
    }

    /// Set fee rate in basis points
    pub fn with_fee_rate(mut self, fee_rate_bps: u32) -> Self {
        self.fee_rate_bps = fee_rate_bps;
        self
    }

    /// Set group ID for linked orders (e.g., arb legs)
    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    /// Set priority (0-255, higher = more important)
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Notional value of this intent
    pub fn notional(&self) -> Decimal {
        self.price * self.size
    }

    /// Is this part of a multi-leg group?
    pub fn is_grouped(&self) -> bool {
        self.group_id.is_some()
    }
}

// ============================================================================
// STRATEGY CONTEXT - Read-only view of system state
// ============================================================================

/// Read-only context provided to strategies
///
/// Strategies use this to observe the world without modifying it.
/// All state access is immutable - strategies cannot directly change anything.
pub struct StrategyContext<'a> {
    /// Order book state for all markets
    pub books: &'a OrderBookState,

    /// Ledger (orders, positions, cash)
    pub ledger: &'a Ledger,

    /// Current timestamp for timing decisions
    pub now: Instant,

    /// Current UTC timestamp for expiration calculations
    pub utc_now: chrono::DateTime<chrono::Utc>,

    /// Spot prices from Binance (optional — None before Binance WS connects)
    pub spot_prices: Option<&'a SpotPriceState>,

    /// Price history for momentum calculation (optional)
    pub price_history: Option<&'a PriceHistory>,

    /// Order tracker for looking up active orders by token (optional)
    pub order_tracker: Option<Arc<OrderTracker>>,
}

impl<'a> StrategyContext<'a> {
    /// Create a new strategy context
    pub fn new(books: &'a OrderBookState, ledger: &'a Ledger) -> Self {
        Self {
            books,
            ledger,
            now: Instant::now(),
            utc_now: chrono::Utc::now(),
            spot_prices: None,
            price_history: None,
            order_tracker: None,
        }
    }

    /// Add spot price references
    pub fn with_spot(
        mut self,
        spot_prices: &'a SpotPriceState,
        price_history: &'a PriceHistory,
    ) -> Self {
        self.spot_prices = Some(spot_prices);
        self.price_history = Some(price_history);
        self
    }

    /// Add order tracker reference
    pub fn with_order_tracker(mut self, tracker: Arc<OrderTracker>) -> Self {
        self.order_tracker = Some(tracker);
        self
    }

    /// Get the latest spot price for an asset (e.g., "btc")
    pub fn spot_price(&self, asset: &str) -> Option<Decimal> {
        self.spot_prices?.price(asset)
    }

    /// Get spot price percentage change over a window (in milliseconds)
    pub fn spot_change_pct(&self, asset: &str, window_ms: i64) -> Option<Decimal> {
        self.price_history?.price_change_pct(asset, window_ms)
    }

    /// Get best bid for a token
    pub fn best_bid(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books.best_bid(token_id)
    }

    /// Get best ask for a token
    pub fn best_ask(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books.best_ask(token_id)
    }

    /// Get mid price for a token
    pub fn mid_price(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books.mid_price(token_id)
    }

    /// Get spread in basis points for a token
    pub fn spread_bps(&self, token_id: &TokenId) -> Option<u32> {
        self.books.spread_bps(token_id)
    }

    /// Get current position for a token
    pub fn position(&self, token_id: &TokenId) -> Position {
        self.ledger.get_position(token_id)
    }

    /// Get available cash
    pub fn available_cash(&self) -> Decimal {
        self.ledger.cash.available()
    }

    /// Get total cash (including reserved)
    pub fn total_cash(&self) -> Decimal {
        self.ledger.cash.total()
    }

    /// Count of open orders
    pub fn open_orders_count(&self) -> u32 {
        self.ledger.open_orders_count()
    }

    /// Get total exposure across all positions (sum of abs(shares * avg_cost))
    pub fn total_exposure(&self) -> Decimal {
        self.ledger.total_exposure()
    }

    /// Check if we hold any shares of a specific token
    pub fn holds_position(&self, token_id: &TokenId) -> bool {
        !self.ledger.get_position(token_id).is_flat()
    }

    /// Get the first active order ID for a token (e.g., for cancel/replace)
    pub fn first_order_for_token(&self, token_id: &TokenId) -> Option<String> {
        self.order_tracker.as_ref()?.orders_for_token(token_id).into_iter().next()
    }
}

// ============================================================================
// STRATEGY TRAIT - Core abstraction for trading strategies
// ============================================================================

/// Core strategy trait
///
/// Strategies implement this trait to receive market events and
/// return order intents. The execution layer handles actual order submission.
///
/// # Design Philosophy
///
/// Strategies output WHAT they want, not HOW to execute:
/// - Return `OrderIntent` with desired price, size, urgency
/// - Execution policy converts to actual order type (FOK/FAK/GTC)
/// - Executor handles submission, retries, partial fills
///
/// This separation allows:
/// - Same strategy to run as taker or maker (just change policy)
/// - Easy backtesting (mock the executor)
/// - Clean strategy code focused on opportunity detection
pub trait Strategy: Send + Sync {
    /// Strategy name (for logging and identification)
    fn name(&self) -> &str;

    /// Which markets does this strategy subscribe to?
    /// Return empty to subscribe to all markets.
    fn subscribed_markets(&self) -> Vec<ConditionId> {
        Vec::new() // Default: subscribe to all
    }

    /// Called when a subscribed market's order book updates
    ///
    /// This is the primary entry point for most strategies.
    /// Return order intents for any opportunities detected.
    fn on_book_update(
        &self,
        market_id: &ConditionId,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called when one of our orders is filled
    ///
    /// Use this to react to fills (e.g., hedge, update state).
    fn on_fill(&self, fill: &Fill, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Default: no action
        let _ = (fill, ctx);
        Vec::new()
    }

    /// Called periodically (e.g., every second)
    ///
    /// Use for time-based logic (e.g., expiring stale orders).
    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Default: no action
        let _ = ctx;
        Vec::new()
    }

    /// Called every tick for active order lifecycle management.
    /// Returns low-latency order actions (cancel, replace) that bypass
    /// the normal intent → policy → executor pipeline.
    fn on_order_management(&self, ctx: &StrategyContext) -> Vec<OrderAction> {
        let _ = ctx;
        Vec::new()
    }

    /// Called on graceful shutdown
    ///
    /// Return intents to close positions, cancel orders, etc.
    fn on_shutdown(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Default: no action
        let _ = ctx;
        Vec::new()
    }

    /// Strategy priority (higher = evaluated first, wins conflicts)
    fn priority(&self) -> u8 {
        50 // Default middle priority
    }

    /// Is this strategy currently enabled?
    fn is_enabled(&self) -> bool {
        true
    }
}

// ============================================================================
// ORDER ACTION - Low-latency order management
// ============================================================================

/// Low-latency order management action.
/// Processed directly by the executor, not through the policy pipeline.
#[derive(Debug, Clone)]
pub enum OrderAction {
    /// Cancel an outstanding order
    Cancel { order_id: String },
    /// Cancel ALL outstanding orders for a specific token
    CancelAllForToken { token_id: String },
    /// Cancel and replace with a new order (atomic cancel+post)
    Replace {
        old_order_id: String,
        new_intent: OrderIntent,
    },
    /// Post a taker fallback order (bypass maker pipeline)
    /// If cancel_token_id is set, cancel ALL outstanding orders for that token first
    /// and wait for collateral release. This is safer than cancelling by order_id because
    /// the strategy doesn't track individual order IDs from async execution.
    PostTakerFallback {
        cancel_token_id: Option<String>,
        intent: OrderIntent,
    },
}

// ============================================================================
// STRATEGY RESULT - For strategies that need to return errors
// ============================================================================

/// Result type for strategy operations that can fail
pub type StrategyResult<T> = Result<T, StrategyError>;

/// Errors that can occur in strategy operations
#[derive(Debug, Clone)]
pub enum StrategyError {
    /// Market data not available
    NoMarketData { market_id: String },

    /// Insufficient liquidity for desired size
    InsufficientLiquidity {
        token_id: String,
        needed: Decimal,
        available: Decimal,
    },

    /// Risk limit would be exceeded
    RiskLimitExceeded { limit: String, value: Decimal },

    /// Strategy-specific error
    Custom(String),
}

impl std::fmt::Display for StrategyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StrategyError::NoMarketData { market_id } => {
                write!(f, "No market data for {}", market_id)
            }
            StrategyError::InsufficientLiquidity {
                token_id,
                needed,
                available,
            } => {
                write!(
                    f,
                    "Insufficient liquidity for {}: need {}, have {}",
                    token_id, needed, available
                )
            }
            StrategyError::RiskLimitExceeded { limit, value } => {
                write!(f, "Risk limit exceeded: {} ({})", limit, value)
            }
            StrategyError::Custom(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for StrategyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_intent_creation() {
        let intent = OrderIntent::new(
            "market123".to_string(),
            "token456".to_string(),
            Side::Buy,
            dec!(0.55),
            dec!(100),
            Urgency::Immediate,
            "Arb opportunity",
            "MathArbStrategy",
        );

        assert_eq!(intent.market_id, "market123");
        assert_eq!(intent.token_id, "token456");
        assert_eq!(intent.side, Side::Buy);
        assert_eq!(intent.price, dec!(0.55));
        assert_eq!(intent.size, dec!(100));
        assert_eq!(intent.urgency, Urgency::Immediate);
        assert_eq!(intent.notional(), dec!(55));
        assert!(!intent.is_grouped());
    }

    #[test]
    fn test_order_intent_with_group() {
        let intent = OrderIntent::new(
            "market".to_string(),
            "token".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(50),
            Urgency::Immediate,
            "Arb leg 1",
            "Strategy",
        )
        .with_group("arb-001")
        .with_priority(100);

        assert!(intent.is_grouped());
        assert_eq!(intent.group_id, Some("arb-001".to_string()));
        assert_eq!(intent.priority, 100);
    }

    #[test]
    fn test_urgency_default() {
        assert_eq!(Urgency::default(), Urgency::Normal);
    }
}
