//! Position Redeemer - monitors and logs redeemable positions
//!
//! Polls Gamma API every 5 minutes to check for resolved markets where
//! we hold positions. Logs when positions are redeemable so the user
//! can redeem via Polymarket UI or the CTF contract.
//!
//! Note: Direct on-chain redemption (via CTF contract + alloy) is not
//! yet implemented. This module provides monitoring and alerting.

use crate::api::gamma::GammaClient;
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::strategy::MarketPairRegistry;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

/// Monitors for resolved markets and logs redeemable positions
pub struct PositionRedeemer {
    /// Gamma API client
    gamma: GammaClient,
    /// Market pair registry
    registry: Arc<MarketPairRegistry>,
    /// Ledger for position checking
    ledger: Arc<Ledger>,
    /// Kill switch
    kill_switch: Arc<KillSwitch>,
    /// Already-alerted condition IDs (to avoid spamming)
    alerted: HashSet<String>,
}

impl PositionRedeemer {
    /// Create a new redeemer
    pub fn new(
        registry: Arc<MarketPairRegistry>,
        ledger: Arc<Ledger>,
        kill_switch: Arc<KillSwitch>,
    ) -> Self {
        Self {
            gamma: GammaClient::new(),
            registry,
            ledger,
            kill_switch,
            alerted: HashSet::new(),
        }
    }

    /// Run the redemption check loop (every 5 minutes)
    pub async fn run(&mut self) {
        let mut check_interval = interval(Duration::from_secs(300));

        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    self.check_resolved_markets().await;
                }
                _ = self.kill_switch.wait_for_kill() => {
                    info!("PositionRedeemer shutting down");
                    break;
                }
            }
        }
    }

    /// Check for resolved markets where we hold positions
    async fn check_resolved_markets(&mut self) {
        // Get all registered condition IDs
        let condition_ids = self.registry.all_condition_ids();
        if condition_ids.is_empty() {
            return;
        }

        // Check each market against Gamma API for resolution status
        for condition_id in &condition_ids {
            // Skip if already alerted
            if self.alerted.contains(condition_id) {
                continue;
            }

            // Check if we have any position in this market
            let pair = match self.registry.get_by_condition(condition_id) {
                Some(p) => p,
                None => continue,
            };

            let yes_pos = self.ledger.get_position(&pair.yes_token_id);
            let no_pos = self.ledger.get_position(&pair.no_token_id);

            // Skip markets where we have no position
            if yes_pos.shares.is_zero() && no_pos.shares.is_zero() {
                continue;
            }

            // Query Gamma API for this market's status
            match self.gamma.get_all_events().await {
                Ok(events) => {
                    for event in &events {
                        for market in &event.markets {
                            if market.condition_id == *condition_id && market.closed {
                                // Market resolved — we have a redeemable position
                                let total_position = yes_pos.shares + no_pos.shares;
                                warn!(
                                    "REDEEMABLE: Market {} resolved! Position: {} YES + {} NO tokens. \
                                     Redeem via Polymarket UI or CTF contract.",
                                    condition_id,
                                    yes_pos.shares,
                                    no_pos.shares
                                );
                                info!(
                                    "  Market: {} ({})",
                                    market.question,
                                    if total_position > rust_decimal_macros::dec!(0) {
                                        "has position"
                                    } else {
                                        "no position"
                                    }
                                );
                                self.alerted.insert(condition_id.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to check market resolution: {}", e);
                }
            }
        }
    }
}
