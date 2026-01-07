//! Resolution tracker - polls market endpoints and records resolved markets
//!
//! Uses `ApiClient::get_market` to determine when a market is closed and which
//! token won. This is used by the paper trader to settle simulated positions.

use crate::api::ApiClient;
use crate::api::types::MarketInfo;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Resolved outcome enumerated
#[derive(Debug, Clone)]
pub enum ResolvedOutcome {
    /// YES / UP token won - token id provided
    Token(String),
    /// Market voided / invalid
    Invalid,
}

/// Market resolution record
#[derive(Debug, Clone)]
pub struct MarketResolution {
    pub market_id: String,
    pub end_time: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub outcome: Option<ResolvedOutcome>,
}

/// Tracks markets we care about and polls the API for resolution
pub struct ResolutionTracker {
    api_client: Arc<ApiClient>,
    /// Tracked markets: condition_id -> end_time (optional)
    tracked: HashMap<String, Option<DateTime<Utc>>>,
    /// Poll interval
    poll_interval: Duration,
}

impl ResolutionTracker {
    /// Create a new tracker
    pub fn new(api_client: Arc<ApiClient>, poll_interval: Duration) -> Self {
        Self { api_client, tracked: HashMap::new(), poll_interval }
    }

    /// Track a market by condition id and optional end time
    pub fn track_market(&mut self, condition_id: &str, end_time: Option<DateTime<Utc>>) {
        self.tracked.insert(condition_id.to_string(), end_time);
        debug!(market = %condition_id, "Tracking market for resolution");
    }

    /// Remove a tracked market
    pub fn untrack_market(&mut self, condition_id: &str) {
        self.tracked.remove(condition_id);
    }

    /// Returns the poll interval for background scheduling
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Check tracked markets for resolutions. For each resolved market, returns
    /// a MarketResolution with the winning token (if determinable).
    pub async fn check_resolutions(&self) -> Vec<MarketResolution> {
        let mut resolved = Vec::new();

        for (condition_id, _end_time) in self.tracked.iter() {
            match self.api_client.get_market(condition_id).await {
                Ok(market_info) => {
                    if market_info.closed {
                        // Look for a token with winner==true
                        let winner = market_info.tokens.iter().find(|t| t.winner);
                        let outcome = if let Some(t) = winner {
                            ResolvedOutcome::Token(t.token_id.clone())
                        } else {
                            // No explicit winner field - mark Invalid
                            ResolvedOutcome::Invalid
                        };

                        let end_time = if !market_info.end_date_iso.is_empty() {
                            chrono::DateTime::parse_from_rfc3339(&market_info.end_date_iso)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc))
                        } else {
                            None
                        };

                        let mr = MarketResolution {
                            market_id: condition_id.clone(),
                            end_time,
                            resolved_at: Some(Utc::now()),
                            outcome: Some(outcome),
                        };

                        info!(market = %condition_id, "Market resolved (closed)");
                        resolved.push(mr);
                    } else {
                        debug!(market = %condition_id, "Market not yet closed");
                    }
                }
                Err(e) => {
                    warn!(market = %condition_id, error = %e, "Failed to fetch market info");
                }
            }
        }

        resolved
    }
}
