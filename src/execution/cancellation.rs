//! Order cancellation manager
//!
//! Handles cancelling orders:
//! - Stale orders (older than TTL)
//! - Orders for a specific market (before market close)
//! - All orders (emergency shutdown)

use crate::api::types::{ConditionId, OrderId};
use crate::exchange::Exchange;
use crate::execution::OrderTracker;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Manages order cancellations
pub struct CancellationManager {
    /// Exchange for cancellation requests
    exchange: Arc<dyn Exchange>,

    /// Order tracker to know which orders exist
    order_tracker: Arc<OrderTracker>,

    /// Maximum age before an order is considered stale
    stale_order_ttl: Duration,
}

impl CancellationManager {
    /// Create a new cancellation manager
    pub fn new(
        exchange: Arc<dyn Exchange>,
        order_tracker: Arc<OrderTracker>,
        stale_order_ttl: Duration,
    ) -> Self {
        Self {
            exchange,
            order_tracker,
            stale_order_ttl,
        }
    }

    /// Cancel a single order by ID
    pub async fn cancel(&self, order_id: &OrderId) -> Result<(), String> {
        debug!(order_id = %order_id, "Cancelling order");

        match self.exchange.cancel_order(order_id).await {
            Ok(_) => {
                self.order_tracker.remove(order_id);
                info!(order_id = %order_id, "Order cancelled");
                Ok(())
            }
            Err(e) => {
                error!(order_id = %order_id, error = %e, "Failed to cancel order");
                Err(format!("Failed to cancel {}: {}", order_id, e))
            }
        }
    }

    /// Cancel all orders for a specific market
    ///
    /// Returns the number of orders cancelled
    pub async fn cancel_market(&self, market_id: &ConditionId) -> usize {
        let order_ids = self.order_tracker.orders_for_market(market_id);

        if order_ids.is_empty() {
            return 0;
        }

        info!(
            market_id = %market_id,
            order_count = order_ids.len(),
            "Cancelling all orders for market"
        );

        let mut cancelled = 0;
        for order_id in order_ids {
            if self.cancel(&order_id).await.is_ok() {
                cancelled += 1;
            }
        }

        info!(
            market_id = %market_id,
            cancelled = cancelled,
            "Market orders cancelled"
        );

        cancelled
    }

    /// Cancel all stale orders (older than TTL)
    ///
    /// Returns the number of orders cancelled
    pub async fn cancel_stale(&self) -> usize {
        let stale_orders = self.order_tracker.stale_orders(self.stale_order_ttl);

        if stale_orders.is_empty() {
            return 0;
        }

        info!(
            stale_count = stale_orders.len(),
            ttl_secs = self.stale_order_ttl.as_secs(),
            "Cancelling stale orders"
        );

        let mut cancelled = 0;
        for order_id in stale_orders {
            if self.cancel(&order_id).await.is_ok() {
                cancelled += 1;
            }
        }

        info!(cancelled = cancelled, "Stale orders cancelled");

        cancelled
    }

    /// Cancel all outstanding orders (emergency)
    ///
    /// Returns the number of orders cancelled
    pub async fn cancel_all(&self) -> usize {
        let all_orders = self.order_tracker.all_orders();

        if all_orders.is_empty() {
            info!("No orders to cancel");
            return 0;
        }

        warn!(
            order_count = all_orders.len(),
            "EMERGENCY: Cancelling all orders"
        );

        let mut cancelled = 0;
        for order_id in all_orders {
            if self.cancel(&order_id).await.is_ok() {
                cancelled += 1;
            }
        }

        warn!(cancelled = cancelled, "Emergency cancellation complete");

        cancelled
    }

    /// Cancel all orders in a group (e.g., unwind arb legs)
    pub async fn cancel_group(&self, group_id: &str) -> usize {
        let group_orders = self.order_tracker.orders_in_group(group_id);

        if group_orders.is_empty() {
            return 0;
        }

        debug!(
            group_id = %group_id,
            order_count = group_orders.len(),
            "Cancelling group orders"
        );

        let mut cancelled = 0;
        for order_id in group_orders {
            if self.cancel(&order_id).await.is_ok() {
                cancelled += 1;
            }
        }

        cancelled
    }
}

#[cfg(test)]
mod tests {
    // Note: Full tests would require mocking Exchange trait
    // These are placeholder tests for the structure

    #[test]
    fn test_cancellation_manager_creation() {
        // This test would require mocking, skipped for now
        // Just verifying the struct compiles correctly
    }
}
