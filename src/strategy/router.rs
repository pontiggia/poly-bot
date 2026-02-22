//! Strategy Router - dispatches market events to subscribed strategies
//!
//! Routes book updates, fills, and tick events to the appropriate strategies.
//! Handles conflict resolution when multiple strategies want to trade the same market.

use crate::api::types::{ConditionId, TokenId};
use crate::ledger::Fill;
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

/// Strategy registration with metadata
struct RegisteredStrategy {
    /// The strategy implementation
    strategy: Arc<dyn Strategy>,
    /// Is this strategy currently enabled?
    enabled: bool,
}

/// Routes market events to subscribed strategies
///
/// # Design
///
/// - Strategies register with the router
/// - Router dispatches events to subscribed strategies only
/// - Conflict resolution: higher priority strategy wins
/// - Can enable/disable strategies at runtime
pub struct StrategyRouter {
    /// Registered strategies by name
    strategies: RwLock<HashMap<String, RegisteredStrategy>>,
}

impl StrategyRouter {
    /// Create a new router
    pub fn new() -> Self {
        Self {
            strategies: RwLock::new(HashMap::new()),
        }
    }

    /// Register a strategy
    ///
    /// Returns error if a strategy with the same name already exists.
    pub fn register(&self, strategy: Arc<dyn Strategy>) -> Result<(), RouterError> {
        let name = strategy.name().to_string();
        let subscriptions = strategy.subscribed_markets().len();

        let mut strategies = self.strategies.write().unwrap();

        if strategies.contains_key(&name) {
            return Err(RouterError::StrategyAlreadyRegistered(name));
        }

        let mode = if subscriptions == 0 { "all markets (tick-driven)" } else { "subscription-based" };
        info!(
            strategy = %name,
            subscriptions,
            mode,
            "Registered strategy"
        );

        strategies.insert(
            name,
            RegisteredStrategy {
                strategy,
                enabled: true,
            },
        );

        Ok(())
    }

    /// Unregister a strategy
    pub fn unregister(&self, name: &str) -> Result<(), RouterError> {
        let mut strategies = self.strategies.write().unwrap();

        if strategies.remove(name).is_some() {
            info!(strategy = %name, "Unregistered strategy");
            Ok(())
        } else {
            Err(RouterError::StrategyNotFound(name.to_string()))
        }
    }

    /// Enable a strategy
    pub fn enable(&self, name: &str) -> Result<(), RouterError> {
        let mut strategies = self.strategies.write().unwrap();

        if let Some(reg) = strategies.get_mut(name) {
            reg.enabled = true;
            info!(strategy = %name, "Enabled strategy");
            Ok(())
        } else {
            Err(RouterError::StrategyNotFound(name.to_string()))
        }
    }

    /// Disable a strategy
    pub fn disable(&self, name: &str) -> Result<(), RouterError> {
        let mut strategies = self.strategies.write().unwrap();

        if let Some(reg) = strategies.get_mut(name) {
            reg.enabled = false;
            info!(strategy = %name, "Disabled strategy");
            Ok(())
        } else {
            Err(RouterError::StrategyNotFound(name.to_string()))
        }
    }

    /// Check if a strategy is enabled
    pub fn is_enabled(&self, name: &str) -> bool {
        let strategies = self.strategies.read().unwrap();
        strategies
            .get(name)
            .map(|r| r.enabled && r.strategy.is_enabled())
            .unwrap_or(false)
    }

    /// Get list of registered strategy names
    pub fn strategy_names(&self) -> Vec<String> {
        self.strategies.read().unwrap().keys().cloned().collect()
    }

    /// Route a book update to subscribed strategies
    ///
    /// Returns all order intents from all strategies, sorted by priority.
    pub fn on_book_update(
        &self,
        market_id: &ConditionId,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        let strategies = self.strategies.read().unwrap();
        let mut all_intents = Vec::new();

        for (name, reg) in strategies.iter() {
            // Skip disabled strategies
            if !reg.enabled || !reg.strategy.is_enabled() {
                continue;
            }

            // Skip if not subscribed to this market
            // Query the strategy's live subscribed_markets() to pick up
            // dynamically-registered markets (not just those cached at registration)
            let live_subs = reg.strategy.subscribed_markets();
            if !live_subs.is_empty() && !live_subs.contains(market_id) {
                continue;
            }

            // Get intents from this strategy
            let intents = reg.strategy.on_book_update(market_id, token_id, ctx);

            if !intents.is_empty() {
                debug!(
                    strategy = %name,
                    market = %market_id,
                    intents = intents.len(),
                    "Strategy generated intents"
                );
            }

            all_intents.extend(intents);
        }

        // Resolve conflicts and sort by priority
        self.resolve_conflicts(all_intents)
    }

    /// Route a fill notification to relevant strategies
    pub fn on_fill(&self, fill: &Fill, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let strategies = self.strategies.read().unwrap();
        let mut all_intents = Vec::new();

        for (name, reg) in strategies.iter() {
            if !reg.enabled || !reg.strategy.is_enabled() {
                continue;
            }

            let intents = reg.strategy.on_fill(fill, ctx);

            if !intents.is_empty() {
                debug!(
                    strategy = %name,
                    intents = intents.len(),
                    "Strategy reacted to fill"
                );
            }

            all_intents.extend(intents);
        }

        self.resolve_conflicts(all_intents)
    }

    /// Route tick event to all enabled strategies
    pub fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let strategies = self.strategies.read().unwrap();
        let mut all_intents = Vec::new();

        for (_name, reg) in strategies.iter() {
            if !reg.enabled || !reg.strategy.is_enabled() {
                continue;
            }

            all_intents.extend(reg.strategy.on_tick(ctx));
        }

        self.resolve_conflicts(all_intents)
    }

    /// Route shutdown event to all strategies
    pub fn on_shutdown(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let strategies = self.strategies.read().unwrap();
        let mut all_intents = Vec::new();

        for (name, reg) in strategies.iter() {
            let intents = reg.strategy.on_shutdown(ctx);

            if !intents.is_empty() {
                info!(
                    strategy = %name,
                    intents = intents.len(),
                    "Strategy shutdown intents"
                );
            }

            all_intents.extend(intents);
        }

        // Don't resolve conflicts on shutdown - execute everything
        all_intents
    }

    /// Resolve conflicts between intents from different strategies
    ///
    /// Currently: higher priority wins. In the future, could implement
    /// more sophisticated conflict resolution (capital allocation, etc.)
    fn resolve_conflicts(&self, mut intents: Vec<OrderIntent>) -> Vec<OrderIntent> {
        if intents.is_empty() {
            return intents;
        }

        // Sort by priority (descending)
        intents.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Track which markets/tokens have intents
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        let mut resolved = Vec::new();

        for intent in intents {
            let key = (
                intent.market_id.clone(),
                intent.token_id.clone(),
                format!("{:?}", intent.side),
            );

            // If same market/token/side already has higher-priority intent, skip
            // Exception: grouped intents (e.g., arb legs) are not in conflict
            if intent.group_id.is_none() && seen.contains(&key) {
                debug!(
                    strategy = %intent.strategy_name,
                    market = %intent.market_id,
                    "Intent dropped due to conflict with higher priority"
                );
                continue;
            }

            seen.insert(key);
            resolved.push(intent);
        }

        resolved
    }
}

impl Default for StrategyRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Router errors
#[derive(Debug, Clone)]
pub enum RouterError {
    /// Strategy with this name already registered
    StrategyAlreadyRegistered(String),
    /// Strategy not found
    StrategyNotFound(String),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::StrategyAlreadyRegistered(name) => {
                write!(f, "Strategy already registered: {}", name)
            }
            RouterError::StrategyNotFound(name) => {
                write!(f, "Strategy not found: {}", name)
            }
        }
    }
}

impl std::error::Error for RouterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Side;
    use crate::ledger::Ledger;
    use crate::state::OrderBookState;
    use rust_decimal_macros::dec;

    /// Test strategy that always returns a fixed intent
    struct TestStrategy {
        name: String,
        priority: u8,
        enabled: bool,
    }

    impl TestStrategy {
        fn new(name: &str, priority: u8) -> Self {
            Self {
                name: name.to_string(),
                priority,
                enabled: true,
            }
        }
    }

    impl Strategy for TestStrategy {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> u8 {
            self.priority
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn on_book_update(
            &self,
            market_id: &ConditionId,
            token_id: &TokenId,
            _ctx: &StrategyContext,
        ) -> Vec<OrderIntent> {
            vec![OrderIntent::new(
                market_id.clone(),
                token_id.clone(),
                Side::Buy,
                dec!(0.50),
                dec!(100),
                crate::strategy::Urgency::Normal,
                format!("Test intent from {}", self.name),
                self.name.clone(),
            )
            .with_priority(self.priority)]
        }
    }

    #[test]
    fn test_register_strategy() {
        let router = StrategyRouter::new();
        let strategy = Arc::new(TestStrategy::new("test", 50));

        assert!(router.register(strategy.clone()).is_ok());
        assert!(router.register(strategy).is_err()); // Duplicate
    }

    #[test]
    fn test_enable_disable() {
        let router = StrategyRouter::new();
        let strategy = Arc::new(TestStrategy::new("test", 50));

        router.register(strategy).unwrap();

        assert!(router.is_enabled("test"));

        router.disable("test").unwrap();
        assert!(!router.is_enabled("test"));

        router.enable("test").unwrap();
        assert!(router.is_enabled("test"));
    }

    #[test]
    fn test_conflict_resolution() {
        let router = StrategyRouter::new();

        // Register two strategies with different priorities
        let high_priority = Arc::new(TestStrategy::new("high", 100));
        let low_priority = Arc::new(TestStrategy::new("low", 10));

        router.register(high_priority).unwrap();
        router.register(low_priority).unwrap();

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = router.on_book_update(&"market1".to_string(), &"token1".to_string(), &ctx);

        // Only one intent should survive (higher priority)
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].strategy_name, "high");
    }
}
