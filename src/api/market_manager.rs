//! Market Manager - dynamic discovery and lifecycle management
//!
//! Polls Gamma API every 60 seconds to discover new markets and
//! remove expired ones. Dynamically subscribes the market WebSocket
//! to new tokens as they appear.

use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, info};

use crate::api::discovery::MarketDiscovery;
use crate::kill_switch::KillSwitch;
use crate::state::OrderBookState;
use crate::strategy::MarketPairRegistry;

/// Manages dynamic market discovery and lifecycle
pub struct MarketManager {
    /// Discovery service
    discovery: MarketDiscovery,
    /// Market pair registry (shared with bot)
    registry: Arc<MarketPairRegistry>,
    /// Order book state (shared with bot)
    order_book_state: Arc<OrderBookState>,
    /// Channel to send new token IDs for WS subscription
    subscription_tx: mpsc::UnboundedSender<Vec<String>>,
    /// Known condition IDs (to detect new vs existing)
    known_conditions: HashSet<String>,
    /// Kill switch for shutdown
    kill_switch: Arc<KillSwitch>,
}

impl MarketManager {
    /// Create a new market manager
    pub fn new(
        discovery: MarketDiscovery,
        registry: Arc<MarketPairRegistry>,
        order_book_state: Arc<OrderBookState>,
        subscription_tx: mpsc::UnboundedSender<Vec<String>>,
        kill_switch: Arc<KillSwitch>,
    ) -> Self {
        // Populate known conditions from current registry
        let known_conditions: HashSet<String> = registry
            .all_condition_ids()
            .into_iter()
            .collect();

        Self {
            discovery,
            registry,
            order_book_state,
            subscription_tx,
            known_conditions,
            kill_switch,
        }
    }

    /// Run the discovery loop (every 60 seconds)
    pub async fn run(&mut self) {
        let mut poll_interval = interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    self.poll_and_update().await;
                }
                _ = self.kill_switch.wait_for_kill() => {
                    info!("MarketManager shutting down");
                    break;
                }
            }
        }
    }

    /// Poll for new markets and update registrations
    async fn poll_and_update(&mut self) {
        let discovered = match self.discovery.discover_all_crypto().await {
            Ok(markets) => markets,
            Err(e) => {
                debug!("Market discovery poll failed: {}", e);
                return;
            }
        };

        let mut new_tokens = Vec::new();
        let mut new_count = 0u32;
        let mut current_conditions: HashSet<String> = HashSet::new();

        for dm in &discovered {
            current_conditions.insert(dm.condition_id.clone());

            // Skip already known markets
            if self.known_conditions.contains(&dm.condition_id) {
                continue;
            }

            // New market found
            let pair = dm.to_market_pair();
            info!(
                "Registered new market: {} ({})",
                dm.condition_id, dm.question
            );

            new_tokens.push(dm.first_token_id.clone());
            new_tokens.push(dm.second_token_id.clone());

            self.registry.register(pair);
            self.known_conditions.insert(dm.condition_id.clone());
            new_count += 1;
        }

        // Remove expired markets (in known but not in current)
        let expired: Vec<String> = self
            .known_conditions
            .difference(&current_conditions)
            .cloned()
            .collect();

        for condition_id in &expired {
            // Remove tokens from order book before unregistering (need pair info)
            if let Some(pair) = self.registry.get_by_condition(condition_id) {
                self.order_book_state.remove_token(&pair.yes_token_id);
                self.order_book_state.remove_token(&pair.no_token_id);
            }
            self.registry.unregister(condition_id);
            self.known_conditions.remove(condition_id);
            debug!("Removed expired market: {}", condition_id);
        }

        // Subscribe WS to new tokens
        if !new_tokens.is_empty() {
            info!(
                "Dynamically subscribing to {} new token(s) from {} market(s)",
                new_tokens.len(),
                new_count
            );
            let _ = self.subscription_tx.send(new_tokens);
        }

        if !expired.is_empty() {
            info!("Removed {} expired market(s)", expired.len());
        }
    }
}
