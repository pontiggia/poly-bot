//! Domain types for exchange operations
//!
//! These types provide a stable interface independent of SDK internals.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::api::types::Side;

/// Order ID type alias
pub type OrderId = String;

/// Domain order representation
///
/// This is our internal representation of an order, independent of the SDK's
/// response format. It contains all information needed for tracking and reconciliation.
#[derive(Debug, Clone)]
pub struct DomainOrder {
    /// Unique order identifier from exchange
    pub order_id: OrderId,

    /// Token being traded
    pub token_id: String,

    /// Order side (Buy/Sell)
    pub side: Side,

    /// Limit price
    pub price: Decimal,

    /// Original order size
    pub original_size: Decimal,

    /// Remaining unfilled size
    pub remaining_size: Decimal,

    /// Total filled size
    pub filled_size: Decimal,

    /// Current order status
    pub status: OrderStatus,

    /// When the order was created
    pub created_at: DateTime<Utc>,

    /// Maker amount in base units (USDC × 10^6)
    /// This is the SDK-calculated amount for verification
    pub maker_amount: Decimal,

    /// Taker amount in base units
    pub taker_amount: Decimal,
}

impl DomainOrder {
    /// Returns true if the order is fully filled
    pub fn is_filled(&self) -> bool {
        self.status == OrderStatus::Filled
    }

    /// Returns true if the order is still active (can be filled)
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }

    /// Returns true if the order is in a terminal state
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// Returns the fill ratio (0.0 to 1.0)
    pub fn fill_ratio(&self) -> Decimal {
        if self.original_size.is_zero() {
            Decimal::ZERO
        } else {
            self.filled_size / self.original_size
        }
    }
}

/// Order status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// Order submitted, awaiting confirmation
    Pending,

    /// Order accepted and resting on order book
    Open,

    /// Order has some fills but not complete
    PartiallyFilled,

    /// Order completely filled
    Filled,

    /// Order was cancelled (by user or system)
    Cancelled,

    /// Order expired (GTD orders)
    Expired,

    /// Order was rejected by exchange
    Rejected,
}

impl OrderStatus {
    /// Returns true if order is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Cancelled | Self::Expired | Self::Rejected
        )
    }

    /// Returns true if order is still active (can be filled or cancelled)
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Open | Self::PartiallyFilled)
    }

    /// Returns true if order has any fills
    pub fn has_fills(&self) -> bool {
        matches!(self, Self::PartiallyFilled | Self::Filled)
    }
}

impl std::fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "PENDING"),
            Self::Open => write!(f, "OPEN"),
            Self::PartiallyFilled => write!(f, "PARTIAL"),
            Self::Filled => write!(f, "FILLED"),
            Self::Cancelled => write!(f, "CANCELLED"),
            Self::Expired => write!(f, "EXPIRED"),
            Self::Rejected => write!(f, "REJECTED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_status_terminal() {
        assert!(OrderStatus::Filled.is_terminal());
        assert!(OrderStatus::Cancelled.is_terminal());
        assert!(OrderStatus::Expired.is_terminal());
        assert!(OrderStatus::Rejected.is_terminal());
        assert!(!OrderStatus::Open.is_terminal());
        assert!(!OrderStatus::Pending.is_terminal());
    }

    #[test]
    fn test_order_status_active() {
        assert!(OrderStatus::Open.is_active());
        assert!(OrderStatus::Pending.is_active());
        assert!(OrderStatus::PartiallyFilled.is_active());
        assert!(!OrderStatus::Filled.is_active());
        assert!(!OrderStatus::Cancelled.is_active());
    }

    #[test]
    fn test_domain_order_fill_ratio() {
        let order = DomainOrder {
            order_id: "test".into(),
            token_id: "token".into(),
            side: Side::Buy,
            price: dec!(0.5),
            original_size: dec!(100),
            remaining_size: dec!(50),
            filled_size: dec!(50),
            status: OrderStatus::PartiallyFilled,
            created_at: Utc::now(),
            maker_amount: dec!(50000000),
            taker_amount: dec!(100000000),
        };

        assert_eq!(order.fill_ratio(), dec!(0.5));
    }
}
