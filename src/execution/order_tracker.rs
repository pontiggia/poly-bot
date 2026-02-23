//! Order tracker for managing outstanding orders
//!
//! Tracks GTC orders that are posted but not yet filled.
//! Essential for maker strategy to:
//! - Prevent duplicate orders at same price
//! - Cancel stale orders after TTL
//! - Cancel all orders before market close
//! - Track partial fills

use crate::api::types::{ConditionId, OrderId, Side, TokenId};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info};

// ============================================================================
// TRACKED ORDER
// ============================================================================

/// Information about an active order
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    /// Order ID from exchange
    pub order_id: OrderId,

    /// Token being traded
    pub token_id: TokenId,

    /// Market/condition ID
    pub market_id: ConditionId,

    /// Buy or Sell
    pub side: Side,

    /// Limit price
    pub price: Decimal,

    /// Original order size
    pub original_size: Decimal,

    /// Amount filled so far
    pub filled_size: Decimal,

    /// When the order was created
    pub created_at: Instant,

    /// Strategy that created this order
    pub strategy_name: String,

    /// Group ID for linked orders (e.g., arb legs)
    pub group_id: Option<String>,

    /// When the order was fully filled (None if still active).
    /// We keep completed orders around so late WS fill notifications
    /// can still match back to the tracked order instead of being silently dropped.
    pub completed_at: Option<Instant>,
}

impl TrackedOrder {
    /// Remaining unfilled size
    pub fn remaining_size(&self) -> Decimal {
        self.original_size - self.filled_size
    }

    /// Is the order fully filled?
    pub fn is_filled(&self) -> bool {
        self.filled_size >= self.original_size
    }

    /// How long has this order been active?
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Is this order stale (older than threshold)?
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.age() > max_age
    }
}

// ============================================================================
// ORDER TRACKER
// ============================================================================

/// Tracks outstanding orders for maker strategy
///
/// Thread-safe using DashMap for concurrent access.
pub struct OrderTracker {
    /// Active orders by order_id
    orders: DashMap<OrderId, TrackedOrder>,

    /// Orders by token (for checking if we have orders at a price)
    by_token: DashMap<TokenId, HashSet<OrderId>>,

    /// Orders by market (for bulk cancellation)
    by_market: DashMap<ConditionId, HashSet<OrderId>>,

    /// Orders by group (for linked order tracking)
    by_group: DashMap<String, HashSet<OrderId>>,

    /// Total orders tracked (ever)
    total_tracked: AtomicU64,

    /// Total orders completed (filled or cancelled)
    total_completed: AtomicU64,
}

impl OrderTracker {
    /// Create a new order tracker
    pub fn new() -> Self {
        Self {
            orders: DashMap::new(),
            by_token: DashMap::new(),
            by_market: DashMap::new(),
            by_group: DashMap::new(),
            total_tracked: AtomicU64::new(0),
            total_completed: AtomicU64::new(0),
        }
    }

    /// Number of active (non-completed) orders
    pub fn active_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|e| e.completed_at.is_none())
            .count()
    }

    /// Total orders in tracker (including completed awaiting cleanup)
    pub fn total_in_tracker(&self) -> usize {
        self.orders.len()
    }

    /// Total orders ever tracked
    pub fn total_tracked(&self) -> u64 {
        self.total_tracked.load(Ordering::Relaxed)
    }

    /// Total orders completed
    pub fn total_completed(&self) -> u64 {
        self.total_completed.load(Ordering::Relaxed)
    }

    /// Check if we're tracking this order ID
    pub fn contains(&self, order_id: &str) -> bool {
        self.orders.contains_key(order_id)
    }

    /// Track a new order
    pub fn track(&self, order: TrackedOrder) {
        let order_id = order.order_id.clone();
        let token_id = order.token_id.clone();
        let market_id = order.market_id.clone();
        let group_id = order.group_id.clone();

        debug!(
            order_id = %order_id,
            token = %token_id,
            side = ?order.side,
            price = %order.price,
            size = %order.original_size,
            "Tracking new order"
        );

        // Add to main map
        self.orders.insert(order_id.clone(), order);

        // Add to token index
        self.by_token
            .entry(token_id)
            .or_default()
            .insert(order_id.clone());

        // Add to market index
        self.by_market
            .entry(market_id)
            .or_default()
            .insert(order_id.clone());

        // Add to group index if grouped
        if let Some(gid) = group_id {
            self.by_group.entry(gid).or_default().insert(order_id);
        }

        self.total_tracked.fetch_add(1, Ordering::Relaxed);
    }

    /// Update an order with a fill
    ///
    /// Returns the updated remaining size, or None if order not found.
    /// When fully filled, marks the order as completed but does NOT remove it
    /// immediately — we keep it around so late WS fill notifications can still
    /// match back to this order. Call `cleanup_completed()` periodically to purge.
    pub fn on_fill(&self, order_id: &OrderId, fill_size: Decimal) -> Option<Decimal> {
        let mut order = self.orders.get_mut(order_id)?;
        order.filled_size += fill_size;

        let remaining = order.remaining_size();

        debug!(
            order_id = %order_id,
            fill_size = %fill_size,
            total_filled = %order.filled_size,
            remaining = %remaining,
            "Order fill update"
        );

        // If fully filled, mark as completed but keep in map for late-fill matching
        if order.is_filled() && order.completed_at.is_none() {
            order.completed_at = Some(Instant::now());
            debug!(order_id = %order_id, "Order marked completed (kept for late-fill matching)");
        }

        Some(remaining)
    }

    /// Remove completed (fully-filled) orders that have been sitting for longer
    /// than `retention`. This prevents the tracker from growing unbounded while
    /// still allowing late WS fill notifications to match for a grace period.
    ///
    /// Returns the number of orders purged.
    pub fn cleanup_completed(&self, retention: Duration) -> usize {
        let to_purge: Vec<OrderId> = self
            .orders
            .iter()
            .filter_map(|entry| {
                if let Some(completed_at) = entry.completed_at {
                    if completed_at.elapsed() > retention {
                        return Some(entry.order_id.clone());
                    }
                }
                None
            })
            .collect();

        let count = to_purge.len();
        for order_id in &to_purge {
            self.remove(order_id);
        }
        if count > 0 {
            debug!(purged = count, "Cleaned up completed orders from tracker");
        }
        count
    }

    /// Remove an order from tracking
    pub fn remove(&self, order_id: &OrderId) {
        if let Some((_, order)) = self.orders.remove(order_id) {
            // Remove from token index
            if let Some(mut ids) = self.by_token.get_mut(&order.token_id) {
                ids.remove(order_id);
            }

            // Remove from market index
            if let Some(mut ids) = self.by_market.get_mut(&order.market_id) {
                ids.remove(order_id);
            }

            // Remove from group index
            if let Some(ref gid) = order.group_id {
                if let Some(mut ids) = self.by_group.get_mut(gid) {
                    ids.remove(order_id);
                }
            }

            self.total_completed.fetch_add(1, Ordering::Relaxed);

            debug!(order_id = %order_id, "Order removed from tracking");
        }
    }

    /// Check if we have an active (non-completed) order at a specific price for a token
    pub fn has_order_at(&self, token_id: &TokenId, side: Side, price: Decimal) -> bool {
        if let Some(order_ids) = self.by_token.get(token_id) {
            for order_id in order_ids.iter() {
                if let Some(order) = self.orders.get(order_id) {
                    if order.completed_at.is_none() && order.side == side && order.price == price {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if we have any active (non-completed) order for a token on a specific side
    pub fn has_order_for(&self, token_id: &TokenId, side: Side) -> bool {
        if let Some(order_ids) = self.by_token.get(token_id) {
            for order_id in order_ids.iter() {
                if let Some(order) = self.orders.get(order_id) {
                    if order.completed_at.is_none() && order.side == side {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Get all orders for a market
    pub fn orders_for_market(&self, market_id: &ConditionId) -> Vec<OrderId> {
        self.by_market
            .get(market_id)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all orders for a token
    pub fn orders_for_token(&self, token_id: &TokenId) -> Vec<OrderId> {
        self.by_token
            .get(token_id)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all orders in a group
    pub fn orders_in_group(&self, group_id: &str) -> Vec<OrderId> {
        self.by_group
            .get(group_id)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all stale active orders (older than max_age, not already completed)
    pub fn stale_orders(&self, max_age: Duration) -> Vec<OrderId> {
        self.orders
            .iter()
            .filter(|entry| entry.completed_at.is_none() && entry.is_stale(max_age))
            .map(|entry| entry.order_id.clone())
            .collect()
    }

    /// Get all active order IDs
    pub fn all_orders(&self) -> Vec<OrderId> {
        self.orders.iter().map(|e| e.order_id.clone()).collect()
    }

    /// Get order details
    pub fn get(&self, order_id: &OrderId) -> Option<TrackedOrder> {
        self.orders.get(order_id).map(|e| e.clone())
    }

    /// Log current state
    pub fn log_status(&self) {
        let total_in_tracker = self.total_in_tracker();
        let active = self.active_count();
        let completed_pending = total_in_tracker - active;
        info!(
            active = active,
            completed_pending = completed_pending,
            total_tracked = self.total_tracked(),
            total_completed = self.total_completed(),
            "OrderTracker status"
        );
    }
}

impl Default for OrderTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_order(order_id: &str, token_id: &str, price: Decimal) -> TrackedOrder {
        TrackedOrder {
            order_id: order_id.to_string(),
            token_id: token_id.to_string(),
            market_id: "market-1".to_string(),
            side: Side::Buy,
            price,
            original_size: dec!(100),
            filled_size: Decimal::ZERO,
            created_at: Instant::now(),
            strategy_name: "TestStrategy".to_string(),
            group_id: None,
            completed_at: None,
        }
    }

    #[test]
    fn test_track_order() {
        let tracker = OrderTracker::new();
        let order = sample_order("order-1", "token-a", dec!(0.55));

        tracker.track(order);

        assert_eq!(tracker.active_count(), 1);
        assert_eq!(tracker.total_tracked(), 1);
        assert!(tracker.get(&"order-1".to_string()).is_some());
    }

    #[test]
    fn test_has_order_at() {
        let tracker = OrderTracker::new();
        let order = sample_order("order-1", "token-a", dec!(0.55));

        tracker.track(order);

        assert!(tracker.has_order_at(&"token-a".to_string(), Side::Buy, dec!(0.55)));
        assert!(!tracker.has_order_at(&"token-a".to_string(), Side::Buy, dec!(0.56)));
        assert!(!tracker.has_order_at(&"token-a".to_string(), Side::Sell, dec!(0.55)));
        assert!(!tracker.has_order_at(&"token-b".to_string(), Side::Buy, dec!(0.55)));
    }

    #[test]
    fn test_on_fill_partial() {
        let tracker = OrderTracker::new();
        let order = sample_order("order-1", "token-a", dec!(0.55));

        tracker.track(order);

        let remaining = tracker.on_fill(&"order-1".to_string(), dec!(30));

        assert_eq!(remaining, Some(dec!(70)));
        assert_eq!(tracker.active_count(), 1); // Still active (not fully filled)
    }

    #[test]
    fn test_on_fill_complete() {
        let tracker = OrderTracker::new();
        let order = sample_order("order-1", "token-a", dec!(0.55));

        tracker.track(order);

        let remaining = tracker.on_fill(&"order-1".to_string(), dec!(100));

        assert_eq!(remaining, Some(Decimal::ZERO));
        assert_eq!(tracker.active_count(), 0); // Not active (completed)
        // But still accessible for late-fill matching
        assert!(tracker.get(&"order-1".to_string()).is_some());
        assert!(tracker.get(&"order-1".to_string()).unwrap().completed_at.is_some());
        assert_eq!(tracker.total_in_tracker(), 1); // Still in tracker

        // Cleanup with 0 retention removes it
        let purged = tracker.cleanup_completed(Duration::ZERO);
        assert_eq!(purged, 1);
        assert!(tracker.get(&"order-1".to_string()).is_none());
        assert_eq!(tracker.total_completed(), 1);
    }

    #[test]
    fn test_remove_order() {
        let tracker = OrderTracker::new();
        let order = sample_order("order-1", "token-a", dec!(0.55));

        tracker.track(order);
        tracker.remove(&"order-1".to_string());

        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.total_completed(), 1);
        assert!(!tracker.has_order_at(&"token-a".to_string(), Side::Buy, dec!(0.55)));
    }

    #[test]
    fn test_orders_for_market() {
        let tracker = OrderTracker::new();

        let order1 = TrackedOrder {
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            market_id: "market-1".to_string(),
            side: Side::Buy,
            price: dec!(0.55),
            original_size: dec!(100),
            filled_size: Decimal::ZERO,
            created_at: Instant::now(),
            strategy_name: "Test".to_string(),
            group_id: None,
            completed_at: None,
        };

        let order2 = TrackedOrder {
            order_id: "order-2".to_string(),
            market_id: "market-1".to_string(),
            ..order1.clone()
        };

        let order3 = TrackedOrder {
            order_id: "order-3".to_string(),
            market_id: "market-2".to_string(), // Different market
            ..order1.clone()
        };

        tracker.track(order1);
        tracker.track(order2);
        tracker.track(order3);

        let market1_orders = tracker.orders_for_market(&"market-1".to_string());
        assert_eq!(market1_orders.len(), 2);

        let market2_orders = tracker.orders_for_market(&"market-2".to_string());
        assert_eq!(market2_orders.len(), 1);
    }

    #[test]
    fn test_stale_orders() {
        let tracker = OrderTracker::new();

        // Create an order that's "old" (created in the past)
        let mut order = sample_order("order-1", "token-a", dec!(0.55));
        order.created_at = Instant::now() - Duration::from_secs(120);

        tracker.track(order);

        // Fresh order
        tracker.track(sample_order("order-2", "token-b", dec!(0.45)));

        // Check stale with 60 second threshold
        let stale = tracker.stale_orders(Duration::from_secs(60));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], "order-1");
    }

    #[test]
    fn test_grouped_orders() {
        let tracker = OrderTracker::new();

        let order1 = TrackedOrder {
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            market_id: "market-1".to_string(),
            side: Side::Buy,
            price: dec!(0.55),
            original_size: dec!(100),
            filled_size: Decimal::ZERO,
            created_at: Instant::now(),
            strategy_name: "Test".to_string(),
            group_id: Some("arb-001".to_string()),
            completed_at: None,
        };

        let order2 = TrackedOrder {
            order_id: "order-2".to_string(),
            token_id: "token-b".to_string(),
            group_id: Some("arb-001".to_string()), // Same group
            ..order1.clone()
        };

        tracker.track(order1);
        tracker.track(order2);

        let group_orders = tracker.orders_in_group("arb-001");
        assert_eq!(group_orders.len(), 2);
    }
}
