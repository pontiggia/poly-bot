//! Execution policies - convert OrderIntent to actual order parameters
//!
//! Policies determine HOW to execute what strategies want.
//! This separation allows the same strategy to run as taker or maker
//! by simply changing the policy.

use crate::api::types::OrderType;
use crate::strategy::Urgency;
use rust_decimal::Decimal;

// ============================================================================
// PARTIAL FILL ACTION - What to do when order partially fills
// ============================================================================

/// Action to take when an order partially fills
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialFillAction {
    /// Unwind the filled portion immediately
    /// Use for: arbitrage where we need balanced positions
    UnwindFilled,

    /// Cancel any remaining unfilled portion
    /// Use for: taker orders where we don't want to wait
    CancelRemainder,

    /// Keep remainder on the book (for GTC orders)
    /// Use for: maker strategies willing to wait for fills
    KeepRemainder,
}

// ============================================================================
// ORDER PARAMS - Parameters for actual order submission
// ============================================================================

/// Order parameters for submission to the exchange
///
/// This is what the executor uses to build and sign orders.
#[derive(Debug, Clone)]
pub struct OrderParams {
    /// Token to trade
    pub token_id: String,

    /// Buy or sell
    pub side: crate::api::types::Side,

    /// Limit price
    pub price: Decimal,

    /// Size in shares
    pub size: Decimal,

    /// Order type (FOK, FAK, GTC)
    pub order_type: OrderType,

    /// Expiration timestamp (Unix seconds, 0 = never)
    pub expiration: u64,

    /// What to do if partially filled
    pub on_partial_fill: PartialFillAction,

    /// Source intent's strategy name (for logging)
    pub strategy_name: String,

    /// Source intent's group ID (for linked orders)
    pub group_id: Option<String>,

    /// Fee rate in basis points (1000 = 10% for 15-min crypto markets)
    pub fee_rate_bps: u32,
}

// ============================================================================
// EXECUTION POLICY TRAIT
// ============================================================================

/// Execution policy converts order intents to actual order parameters
///
/// Different policies handle the same intent differently:
/// - TakerPolicy: Optimizes for immediate execution (FOK/FAK)
/// - MakerPolicy: Optimizes for maker rebates (GTC)
pub trait ExecutionPolicy: Send + Sync {
    /// Policy name for logging
    fn name(&self) -> &str;

    /// Convert an order intent to order parameters
    ///
    /// This is where Urgency gets mapped to OrderType
    fn to_order_params(&self, intent: &IntentRef) -> OrderParams;

    /// What to do when an order partially fills
    ///
    /// Called by the executor to determine next action
    fn on_partial_fill(&self, intent: &IntentRef, filled_size: Decimal) -> PartialFillAction;
}

/// Reference to an order intent (avoids cloning)
#[derive(Debug, Clone)]
pub struct IntentRef {
    pub token_id: String,
    pub side: crate::api::types::Side,
    pub price: Decimal,
    pub size: Decimal,
    pub urgency: Urgency,
    pub strategy_name: String,
    pub group_id: Option<String>,
    pub fee_rate_bps: u32,
}

impl IntentRef {
    /// Create from an OrderIntent
    pub fn from_intent(intent: &crate::strategy::OrderIntent) -> Self {
        Self {
            token_id: intent.token_id.clone(),
            side: intent.side,
            price: intent.price,
            size: intent.size,
            urgency: intent.urgency,
            strategy_name: intent.strategy_name.clone(),
            group_id: intent.group_id.clone(),
            fee_rate_bps: intent.fee_rate_bps,
        }
    }
}

// ============================================================================
// TAKER POLICY - Optimize for immediate execution
// ============================================================================

/// Taker execution policy - optimizes for immediate fills
///
/// Urgency mapping:
/// - Immediate → FOK (Fill Or Kill - must fill entirely or cancel)
/// - Normal → FAK (Fill And Kill - fill what you can, cancel rest)
/// - Passive → FAK (even passive urgency uses FAK in taker mode)
///
/// Partial fill handling:
/// - UnwindFilled for grouped orders (e.g., arb legs need to be balanced)
/// - CancelRemainder for single orders
pub struct TakerPolicy {
    /// Default expiration offset in seconds (0 = no expiration)
    pub expiration_secs: u64,

    /// Whether to unwind on partial fills for grouped orders
    pub unwind_grouped_partials: bool,
}

impl Default for TakerPolicy {
    fn default() -> Self {
        Self {
            expiration_secs: 60, // 1 minute default
            unwind_grouped_partials: true,
        }
    }
}

impl TakerPolicy {
    /// Create a new taker policy
    pub fn new() -> Self {
        Self::default()
    }

    /// Set expiration in seconds
    pub fn with_expiration(mut self, secs: u64) -> Self {
        self.expiration_secs = secs;
        self
    }
}

impl ExecutionPolicy for TakerPolicy {
    fn name(&self) -> &str {
        "TakerPolicy"
    }

    fn to_order_params(&self, intent: &IntentRef) -> OrderParams {
        let order_type = match intent.urgency {
            Urgency::Immediate => OrderType::FOK,
            Urgency::Normal => OrderType::FAK,
            Urgency::Passive => OrderType::FAK, // Even passive uses FAK in taker mode
        };

        let expiration = if self.expiration_secs > 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() + self.expiration_secs)
                .unwrap_or(0)
        } else {
            0
        };

        OrderParams {
            token_id: intent.token_id.clone(),
            side: intent.side,
            price: intent.price,
            size: intent.size,
            order_type,
            expiration,
            on_partial_fill: self.on_partial_fill(intent, Decimal::ZERO),
            strategy_name: intent.strategy_name.clone(),
            group_id: intent.group_id.clone(),
            fee_rate_bps: intent.fee_rate_bps,
        }
    }

    fn on_partial_fill(&self, intent: &IntentRef, _filled_size: Decimal) -> PartialFillAction {
        // For grouped orders (like arb legs), unwind to stay balanced
        if intent.group_id.is_some() && self.unwind_grouped_partials {
            PartialFillAction::UnwindFilled
        } else {
            // For single orders, just cancel remainder (FAK already does this)
            PartialFillAction::CancelRemainder
        }
    }
}

// ============================================================================
// MAKER POLICY - Optimize for maker rebates
// ============================================================================

/// Maker execution policy - optimizes for capturing maker rebates
///
/// All orders are GTC (Good Till Cancel) - sit on the book until filled.
///
/// Features:
/// - post_only: Ensure we're always the maker (reject if would match)
/// - price_offset: Post inside the spread for higher fill probability
///
/// Partial fill handling:
/// - Always KeepRemainder - let the order keep working
pub struct MakerPolicy {
    /// Require post-only orders (reject if would immediately match)
    pub post_only: bool,

    /// Price offset in cents to post inside spread
    /// Positive = more aggressive (higher bid, lower ask)
    pub price_offset_cents: Decimal,

    /// Default expiration offset in seconds (0 = no expiration)
    pub expiration_secs: u64,
}

impl Default for MakerPolicy {
    fn default() -> Self {
        Self {
            post_only: false, // Polymarket doesn't support post-only yet
            price_offset_cents: Decimal::ZERO,
            expiration_secs: 0, // No expiration for GTC
        }
    }
}

impl MakerPolicy {
    /// Create a new maker policy
    pub fn new() -> Self {
        Self::default()
    }

    /// Set post-only mode
    pub fn with_post_only(mut self, post_only: bool) -> Self {
        self.post_only = post_only;
        self
    }

    /// Set price offset to post inside spread
    pub fn with_price_offset(mut self, cents: Decimal) -> Self {
        self.price_offset_cents = cents;
        self
    }

    /// Set expiration in seconds
    pub fn with_expiration(mut self, secs: u64) -> Self {
        self.expiration_secs = secs;
        self
    }

    /// Apply price offset to intent price
    fn apply_price_offset(&self, price: Decimal, side: crate::api::types::Side) -> Decimal {
        use crate::api::types::Side;

        if self.price_offset_cents == Decimal::ZERO {
            return price;
        }

        let offset = self.price_offset_cents / Decimal::from(100); // Convert cents to price

        let adjusted = match side {
            Side::Buy => price + offset, // More aggressive bid
            Side::Sell => price - offset, // More aggressive ask
        };

        // Round to tick size (2 decimal places for 0.01 tick)
        adjusted.round_dp(2)
    }
}

impl ExecutionPolicy for MakerPolicy {
    fn name(&self) -> &str {
        "MakerPolicy"
    }

    fn to_order_params(&self, intent: &IntentRef) -> OrderParams {
        // Maker always uses GTC, regardless of urgency
        let order_type = OrderType::GTC;

        // Apply price offset for more aggressive posting
        let adjusted_price = self.apply_price_offset(intent.price, intent.side);

        let expiration = if self.expiration_secs > 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() + self.expiration_secs)
                .unwrap_or(0)
        } else {
            0
        };

        OrderParams {
            token_id: intent.token_id.clone(),
            side: intent.side,
            price: adjusted_price,
            size: intent.size,
            order_type,
            expiration,
            on_partial_fill: PartialFillAction::KeepRemainder,
            strategy_name: intent.strategy_name.clone(),
            group_id: intent.group_id.clone(),
            fee_rate_bps: intent.fee_rate_bps,
        }
    }

    fn on_partial_fill(&self, _intent: &IntentRef, _filled_size: Decimal) -> PartialFillAction {
        // Makers always keep remainder on the book
        PartialFillAction::KeepRemainder
    }
}

// ============================================================================
// DUAL POLICY - Selects between taker and maker based on urgency
// ============================================================================

/// Dual execution policy that routes to taker or maker based on urgency
///
/// This allows the bot to use both execution styles:
/// - Urgency::Immediate/Normal → TakerPolicy (FOK/FAK for immediate fills)
/// - Urgency::Passive → MakerPolicy (GTC for maker rebates)
pub struct DualPolicy {
    /// Taker policy for immediate execution
    taker: TakerPolicy,

    /// Maker policy for passive execution
    maker: MakerPolicy,
}

impl DualPolicy {
    /// Create a new dual policy with default taker and maker policies
    pub fn new() -> Self {
        Self {
            taker: TakerPolicy::new(),
            maker: MakerPolicy::new(),
        }
    }

    /// Create with custom maker price offset (cents inside spread)
    pub fn with_maker_offset(mut self, price_offset_cents: Decimal) -> Self {
        self.maker = self.maker.with_price_offset(price_offset_cents);
        self
    }

    /// Create with custom taker expiration
    pub fn with_taker_expiration(mut self, secs: u64) -> Self {
        self.taker = self.taker.with_expiration(secs);
        self
    }

    /// Create with custom maker expiration
    pub fn with_maker_expiration(mut self, secs: u64) -> Self {
        self.maker = self.maker.with_expiration(secs);
        self
    }
}

impl Default for DualPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionPolicy for DualPolicy {
    fn name(&self) -> &str {
        "DualPolicy"
    }

    fn to_order_params(&self, intent: &IntentRef) -> OrderParams {
        match intent.urgency {
            // Immediate/Normal → use taker for fast execution
            Urgency::Immediate | Urgency::Normal => self.taker.to_order_params(intent),

            // Passive → use maker for zero fees
            Urgency::Passive => self.maker.to_order_params(intent),
        }
    }

    fn on_partial_fill(&self, intent: &IntentRef, filled_size: Decimal) -> PartialFillAction {
        match intent.urgency {
            Urgency::Immediate | Urgency::Normal => self.taker.on_partial_fill(intent, filled_size),
            Urgency::Passive => self.maker.on_partial_fill(intent, filled_size),
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Side;
    use rust_decimal_macros::dec;

    fn test_intent(urgency: Urgency, group_id: Option<String>) -> IntentRef {
        IntentRef {
            token_id: "token123".to_string(),
            side: Side::Buy,
            price: dec!(0.55),
            size: dec!(100),
            urgency,
            strategy_name: "TestStrategy".to_string(),
            group_id,
            fee_rate_bps: 0,
        }
    }

    // ========================================================================
    // Taker Policy Tests
    // ========================================================================

    #[test]
    fn test_taker_immediate_uses_fok() {
        let policy = TakerPolicy::new();
        let intent = test_intent(Urgency::Immediate, None);

        let params = policy.to_order_params(&intent);

        assert_eq!(params.order_type, OrderType::FOK);
        assert_eq!(params.price, dec!(0.55));
        assert_eq!(params.size, dec!(100));
    }

    #[test]
    fn test_taker_normal_uses_fak() {
        let policy = TakerPolicy::new();
        let intent = test_intent(Urgency::Normal, None);

        let params = policy.to_order_params(&intent);

        assert_eq!(params.order_type, OrderType::FAK);
    }

    #[test]
    fn test_taker_passive_uses_fak() {
        let policy = TakerPolicy::new();
        let intent = test_intent(Urgency::Passive, None);

        let params = policy.to_order_params(&intent);

        // Even passive urgency uses FAK in taker mode (we're taking, not making)
        assert_eq!(params.order_type, OrderType::FAK);
    }

    #[test]
    fn test_taker_grouped_unwinds_on_partial() {
        let policy = TakerPolicy::new();
        let intent = test_intent(Urgency::Immediate, Some("arb-001".to_string()));

        let action = policy.on_partial_fill(&intent, dec!(50));

        assert_eq!(action, PartialFillAction::UnwindFilled);
    }

    #[test]
    fn test_taker_single_cancels_remainder() {
        let policy = TakerPolicy::new();
        let intent = test_intent(Urgency::Immediate, None);

        let action = policy.on_partial_fill(&intent, dec!(50));

        assert_eq!(action, PartialFillAction::CancelRemainder);
    }

    #[test]
    fn test_taker_with_expiration() {
        let policy = TakerPolicy::new().with_expiration(120);
        let intent = test_intent(Urgency::Normal, None);

        let params = policy.to_order_params(&intent);

        // Expiration should be set (current time + 120 seconds)
        assert!(params.expiration > 0);
    }

    // ========================================================================
    // Maker Policy Tests
    // ========================================================================

    #[test]
    fn test_maker_always_uses_gtc() {
        let policy = MakerPolicy::new();

        // Test all urgency levels - maker always uses GTC
        for urgency in [Urgency::Immediate, Urgency::Normal, Urgency::Passive] {
            let intent = test_intent(urgency, None);
            let params = policy.to_order_params(&intent);
            assert_eq!(params.order_type, OrderType::GTC);
        }
    }

    #[test]
    fn test_maker_keeps_remainder() {
        let policy = MakerPolicy::new();
        let intent = test_intent(Urgency::Passive, None);

        let action = policy.on_partial_fill(&intent, dec!(50));

        assert_eq!(action, PartialFillAction::KeepRemainder);
    }

    #[test]
    fn test_maker_price_offset_buy() {
        let policy = MakerPolicy::new().with_price_offset(dec!(1)); // 1 cent
        let intent = test_intent(Urgency::Passive, None); // Buy at 0.55

        let params = policy.to_order_params(&intent);

        // Buy price should be higher (more aggressive)
        assert_eq!(params.price, dec!(0.56));
    }

    #[test]
    fn test_maker_price_offset_sell() {
        let policy = MakerPolicy::new().with_price_offset(dec!(1)); // 1 cent
        let intent = IntentRef {
            token_id: "token".to_string(),
            side: Side::Sell,
            price: dec!(0.55),
            size: dec!(100),
            urgency: Urgency::Passive,
            strategy_name: "Test".to_string(),
            group_id: None,
            fee_rate_bps: 0,
        };

        let params = policy.to_order_params(&intent);

        // Sell price should be lower (more aggressive)
        assert_eq!(params.price, dec!(0.54));
    }

    #[test]
    fn test_maker_no_expiration_by_default() {
        let policy = MakerPolicy::new();
        let intent = test_intent(Urgency::Passive, None);

        let params = policy.to_order_params(&intent);

        assert_eq!(params.expiration, 0);
    }

    #[test]
    fn test_maker_with_expiration() {
        let policy = MakerPolicy::new().with_expiration(3600); // 1 hour
        let intent = test_intent(Urgency::Passive, None);

        let params = policy.to_order_params(&intent);

        assert!(params.expiration > 0);
    }

    // ========================================================================
    // Dual Policy Tests
    // ========================================================================

    #[test]
    fn test_dual_immediate_uses_taker() {
        let policy = DualPolicy::new();
        let intent = test_intent(Urgency::Immediate, None);

        let params = policy.to_order_params(&intent);

        // Immediate should use FOK (taker)
        assert_eq!(params.order_type, OrderType::FOK);
    }

    #[test]
    fn test_dual_normal_uses_taker() {
        let policy = DualPolicy::new();
        let intent = test_intent(Urgency::Normal, None);

        let params = policy.to_order_params(&intent);

        // Normal should use FAK (taker)
        assert_eq!(params.order_type, OrderType::FAK);
    }

    #[test]
    fn test_dual_passive_uses_maker() {
        let policy = DualPolicy::new();
        let intent = test_intent(Urgency::Passive, None);

        let params = policy.to_order_params(&intent);

        // Passive should use GTC (maker)
        assert_eq!(params.order_type, OrderType::GTC);
    }

    #[test]
    fn test_dual_passive_with_offset() {
        let policy = DualPolicy::new().with_maker_offset(dec!(0.5)); // 0.5 cents
        let intent = test_intent(Urgency::Passive, None); // Buy at 0.55

        let params = policy.to_order_params(&intent);

        // Buy price adjusted: 0.55 + 0.005 = 0.555, rounded to tick size → 0.56
        assert_eq!(params.price, dec!(0.56));
        assert_eq!(params.order_type, OrderType::GTC);
    }

    #[test]
    fn test_dual_partial_fill_routes_correctly() {
        let policy = DualPolicy::new();

        // Immediate (taker) - should cancel or unwind
        let taker_intent = test_intent(Urgency::Immediate, None);
        let taker_action = policy.on_partial_fill(&taker_intent, dec!(50));
        assert_eq!(taker_action, PartialFillAction::CancelRemainder);

        // Passive (maker) - should keep remainder
        let maker_intent = test_intent(Urgency::Passive, None);
        let maker_action = policy.on_partial_fill(&maker_intent, dec!(50));
        assert_eq!(maker_action, PartialFillAction::KeepRemainder);
    }
}
