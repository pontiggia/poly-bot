//! Exchange trait definition
//!
//! The Exchange trait provides an abstraction over exchange operations,
//! allowing strategies and the executor to remain decoupled from the
//! underlying SDK implementation.

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::api::types::Side;
use crate::exchange::errors::ExchangeError;
use crate::exchange::types::{DomainOrder, OrderId, OrderStatus};

/// Result type for exchange operations
pub type ExchangeResult<T> = Result<T, ExchangeError>;

/// Order type for exchange submission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeOrderType {
    /// Good 'til Cancelled - rests on book until filled or cancelled
    GTC,
    /// Fill or Kill - fill entire order immediately or cancel
    FOK,
    /// Fill and Kill - fill what's possible immediately, cancel rest
    FAK,
    /// Good 'til Date - expires at specified timestamp
    GTD { expiration: u64 },
}

impl ExchangeOrderType {
    /// Returns true if this order type rests on the book
    pub fn is_maker(&self) -> bool {
        matches!(self, Self::GTC | Self::GTD { .. })
    }

    /// Returns true if this order type requires immediate fill
    pub fn is_immediate(&self) -> bool {
        matches!(self, Self::FOK | Self::FAK)
    }
}

/// Parameters for placing an order
///
/// This struct contains all information needed to place an order.
/// The Exchange implementation handles amount calculation using the SDK.
#[derive(Debug, Clone)]
pub struct ExchangeOrderParams {
    /// Token ID to trade
    pub token_id: String,

    /// Order side (Buy/Sell)
    pub side: Side,

    /// Limit price
    /// IMPORTANT: Pass full precision Decimal - SDK handles truncation
    pub price: Decimal,

    /// Order size in shares
    /// IMPORTANT: Pass full precision Decimal - SDK handles truncation
    pub size: Decimal,

    /// Order type
    pub order_type: ExchangeOrderType,

    /// Fee rate in basis points (usually from market metadata)
    pub fee_rate_bps: u32,

    /// Minimum tick size for this market (required for amount calculation)
    pub minimum_tick_size: Decimal,

    /// Whether to use post-only mode (maker orders only)
    /// Only valid for GTC and GTD orders
    pub post_only: bool,
}

impl ExchangeOrderParams {
    /// Create new order params with sensible defaults
    pub fn new(token_id: String, side: Side, price: Decimal, size: Decimal) -> Self {
        Self {
            token_id,
            side,
            price,
            size,
            order_type: ExchangeOrderType::GTC,
            fee_rate_bps: 0,
            minimum_tick_size: Decimal::new(1, 2), // 0.01 default
            post_only: false,
        }
    }

    /// Set order type
    pub fn with_order_type(mut self, order_type: ExchangeOrderType) -> Self {
        self.order_type = order_type;
        self
    }

    /// Set fee rate
    pub fn with_fee_rate_bps(mut self, fee_rate_bps: u32) -> Self {
        self.fee_rate_bps = fee_rate_bps;
        self
    }

    /// Set minimum tick size
    pub fn with_tick_size(mut self, tick_size: Decimal) -> Self {
        self.minimum_tick_size = tick_size;
        self
    }

    /// Set post-only mode
    pub fn with_post_only(mut self, post_only: bool) -> Self {
        self.post_only = post_only;
        self
    }
}

/// Exchange abstraction trait
///
/// This trait defines the interface for exchange operations. Strategies
/// and the executor interact with the exchange through this trait,
/// never directly with the SDK.
///
/// ## Key Design Decisions
///
/// 1. **Amount calculation is internal**: The implementation handles
///    converting price/size to maker_amount/taker_amount using the SDK's
///    correct truncation logic. Callers just pass Decimal values.
///
/// 2. **Signing is internal**: The implementation handles EIP-712 signing.
///
/// 3. **Error classification**: Errors are typed to help with retry logic
///    and circuit breaker integration.
#[async_trait]
pub trait Exchange: Send + Sync {
    /// Place an order on the exchange
    ///
    /// The implementation handles:
    /// - Amount calculation (using SDK's truncation)
    /// - Order signing (EIP-712)
    /// - Submission and response parsing
    ///
    /// # Arguments
    /// * `params` - Order parameters (price/size as Decimal, SDK handles amounts)
    ///
    /// # Returns
    /// * `Ok(DomainOrder)` - Order was accepted (may or may not be filled)
    /// * `Err(ExchangeError)` - Order was rejected or submission failed
    async fn place_order(&self, params: ExchangeOrderParams) -> ExchangeResult<DomainOrder>;

    /// Cancel an order by ID
    ///
    /// # Arguments
    /// * `order_id` - The order ID to cancel
    ///
    /// # Returns
    /// * `Ok(())` - Cancellation request accepted
    /// * `Err(OrderNotFound)` - Order doesn't exist or already terminal
    /// * `Err(...)` - Other failure
    async fn cancel_order(&self, order_id: &OrderId) -> ExchangeResult<()>;

    /// Get current status of an order
    ///
    /// # Arguments
    /// * `order_id` - The order ID to query
    async fn get_order(&self, order_id: &OrderId) -> ExchangeResult<OrderStatus>;

    /// Get all open orders
    ///
    /// Returns orders that are currently active (pending, open, partially filled)
    async fn get_open_orders(&self) -> ExchangeResult<Vec<DomainOrder>>;

    /// Cancel all orders for a specific token
    ///
    /// # Arguments
    /// * `token_id` - Token ID to cancel orders for
    ///
    /// # Returns
    /// List of order IDs that were cancelled
    async fn cancel_orders_for_token(&self, token_id: &str) -> ExchangeResult<Vec<OrderId>>;

    /// Cancel all open orders
    ///
    /// # Returns
    /// List of order IDs that were cancelled
    async fn cancel_all_orders(&self) -> ExchangeResult<Vec<OrderId>>;

    /// Get minimum tick size for a market
    ///
    /// The tick size is needed for correct amount calculation.
    /// Implementations should cache this value.
    ///
    /// # Arguments
    /// * `token_id` - Token ID to query
    async fn get_minimum_tick_size(&self, token_id: &str) -> ExchangeResult<Decimal>;

    /// Get cached tick size without network call. Returns 0.01 default if not cached.
    /// Use after warm_caches() has been called.
    fn get_minimum_tick_size_cached(&self, _token_id: &str) -> Decimal {
        Decimal::new(1, 2) // 0.01 default
    }

    /// Get current USDC balance from exchange
    ///
    /// Returns the available USDC balance for trading.
    async fn get_balance(&self) -> ExchangeResult<Decimal>;

    /// Place multiple orders in a single API call (batch endpoint).
    /// Both legs are built+signed concurrently, then submitted in one HTTP request.
    /// Default: falls back to sequential place_order calls.
    async fn place_orders_batch(
        &self,
        params_list: Vec<ExchangeOrderParams>,
    ) -> ExchangeResult<Vec<ExchangeResult<DomainOrder>>> {
        let mut results = Vec::with_capacity(params_list.len());
        for params in params_list {
            results.push(self.place_order(params).await);
        }
        Ok(results)
    }

    /// Pre-warm internal caches for all known tokens.
    /// Eliminates HTTP calls during first order build/sign per token.
    /// Default: no-op.
    async fn warm_caches(&self, _token_ids: &[String]) {}

    /// Check if the exchange connection is healthy
    fn is_healthy(&self) -> bool;

    /// Get the maker address (proxy wallet)
    fn maker_address(&self) -> &str;

    /// Get the signer address (EOA)
    fn signer_address(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_type_classification() {
        assert!(ExchangeOrderType::GTC.is_maker());
        assert!(ExchangeOrderType::GTD { expiration: 1000 }.is_maker());
        assert!(!ExchangeOrderType::FOK.is_maker());
        assert!(!ExchangeOrderType::FAK.is_maker());

        assert!(ExchangeOrderType::FOK.is_immediate());
        assert!(ExchangeOrderType::FAK.is_immediate());
        assert!(!ExchangeOrderType::GTC.is_immediate());
    }

    #[test]
    fn test_order_params_builder() {
        let params = ExchangeOrderParams::new(
            "token123".into(),
            Side::Buy,
            dec!(0.55),
            dec!(100),
        )
        .with_order_type(ExchangeOrderType::FOK)
        .with_fee_rate_bps(100)
        .with_tick_size(dec!(0.001))
        .with_post_only(true);

        assert_eq!(params.token_id, "token123");
        assert_eq!(params.side, Side::Buy);
        assert_eq!(params.price, dec!(0.55));
        assert_eq!(params.size, dec!(100));
        assert_eq!(params.order_type, ExchangeOrderType::FOK);
        assert_eq!(params.fee_rate_bps, 100);
        assert_eq!(params.minimum_tick_size, dec!(0.001));
        assert!(params.post_only);
    }
}
