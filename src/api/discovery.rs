//! Market discovery - finds tradeable markets from Gamma API
//!
//! This module bridges the Gamma API to the internal MarketPair registry:
//! 1. Queries Gamma API for active events/markets
//! 2. Filters for tradeable binary markets
//! 3. Converts to MarketPair format for arbitrage detection
//!
//! ## Market Types Supported
//!
//! - **Yes/No markets**: Standard binary markets with "Yes"/"No" outcomes
//! - **Up/Down markets**: 15-minute crypto markets with "Up"/"Down" outcomes
//!
//! Both are functionally equivalent for arbitrage:
//! - First outcome (Yes/Up) = "first token"
//! - Second outcome (No/Down) = "second token"

use crate::api::gamma::{GammaClient, GammaEvent, GammaMarket};
use crate::api::types::TokenId;
use crate::error::{BotError, Result};
use crate::strategy::market_pair::MarketPair;

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

// ============================================================================
// MARKET FILTER
// ============================================================================

/// Filter criteria for market discovery
#[derive(Debug, Clone, Default)]
pub struct MarketFilter {
    /// Only include markets with these outcome types
    /// Default: allow all
    pub outcome_types: Option<Vec<OutcomeType>>,
    
    /// Only include markets with minimum 24h volume (in USDC)
    pub min_volume_24h: Option<Decimal>,
    
    /// Only include markets with minimum liquidity (in USDC)
    pub min_liquidity: Option<Decimal>,
    
    /// Only include 15-min crypto markets (fee_rate >= 100 bps)
    pub crypto_15min_only: bool,
    
    /// Maximum number of markets to return
    pub limit: Option<usize>,
}

impl MarketFilter {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Filter for 15-minute crypto markets only
    pub fn crypto_15min() -> Self {
        Self {
            outcome_types: Some(vec![OutcomeType::UpDown]),
            crypto_15min_only: true,
            ..Default::default()
        }
    }
    
    /// Filter for standard Yes/No markets
    pub fn yes_no_only() -> Self {
        Self {
            outcome_types: Some(vec![OutcomeType::YesNo]),
            ..Default::default()
        }
    }
    
    pub fn with_min_volume(mut self, min: Decimal) -> Self {
        self.min_volume_24h = Some(min);
        self
    }
    
    pub fn with_min_liquidity(mut self, min: Decimal) -> Self {
        self.min_liquidity = Some(min);
        self
    }
    
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// Type of binary market outcomes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeType {
    /// Standard binary (Yes/No)
    YesNo,
    /// Crypto 15-min (Up/Down)
    UpDown,
    /// Unknown binary (2 outcomes but not Yes/No or Up/Down)
    Other,
}

// ============================================================================
// DISCOVERED MARKET
// ============================================================================

/// A discovered market ready for trading
/// 
/// This is an intermediate format between GammaMarket and MarketPair
/// that contains parsed token IDs and outcomes.
#[derive(Debug, Clone)]
pub struct DiscoveredMarket {
    /// Condition ID (market identifier)
    pub condition_id: String,
    
    /// Human-readable question/title
    pub question: String,
    
    /// First token ID (for Yes/Up outcome)
    pub first_token_id: TokenId,
    
    /// Second token ID (for No/Down outcome)
    pub second_token_id: TokenId,
    
    /// First outcome label (Yes, Up, etc.)
    pub first_outcome: String,
    
    /// Second outcome label (No, Down, etc.)
    pub second_outcome: String,
    
    /// Outcome type classification
    pub outcome_type: OutcomeType,
    
    /// 24-hour volume in USDC
    pub volume_24h: Decimal,
    
    /// Current liquidity
    pub liquidity: Decimal,
    
    /// Fee rate in basis points (1000 = 10%)
    pub fee_rate_bps: u32,
    
    /// Is this a 15-min crypto market
    pub is_crypto_15min: bool,
    
    /// Source event slug (for reference)
    pub event_slug: String,
}

impl DiscoveredMarket {
    /// Convert to MarketPair for the strategy layer
    pub fn to_market_pair(&self) -> MarketPair {
        MarketPair::new(
            self.condition_id.clone(),
            self.first_token_id.clone(),
            self.second_token_id.clone(),
        )
        .with_fee_rate(self.fee_rate_bps)
        .with_description(&self.question)
    }
}

/// Try to convert a GammaMarket to a DiscoveredMarket
fn try_convert_market(market: &GammaMarket, event_slug: &str) -> Result<DiscoveredMarket> {
    // Parse outcomes
    let outcomes = market.parse_outcomes()?;
    if outcomes.len() != 2 {
        return Err(BotError::Config(format!(
            "Market {} has {} outcomes, expected 2",
            market.condition_id,
            outcomes.len()
        )));
    }
    
    // Parse token IDs
    let token_ids = market.parse_token_ids()?;
    if token_ids.len() != 2 {
        return Err(BotError::Config(format!(
            "Market {} has {} tokens, expected 2",
            market.condition_id,
            token_ids.len()
        )));
    }
    
    // Classify outcome type
    let outcome_type = classify_outcomes(&outcomes[0], &outcomes[1]);
    
    // Convert f64 to Decimal
    let volume_24h = Decimal::try_from(market.volume_24hr).unwrap_or_default();
    let liquidity = Decimal::try_from(market.liquidity).unwrap_or_default();
    
    // Determine fee rate - check if provided or infer from market type
    let fee_rate_bps = market.fee_rate_bps.unwrap_or_else(|| {
        // Infer: Up/Down markets are typically 1000 bps (10%)
        if outcome_type == OutcomeType::UpDown {
            1000
        } else {
            0
        }
    });
    
    let is_crypto_15min = outcome_type == OutcomeType::UpDown || fee_rate_bps >= 100;
    
    Ok(DiscoveredMarket {
        condition_id: market.condition_id.clone(),
        question: market.question.clone(),
        first_token_id: token_ids[0].clone(),
        second_token_id: token_ids[1].clone(),
        first_outcome: outcomes[0].clone(),
        second_outcome: outcomes[1].clone(),
        outcome_type,
        volume_24h,
        liquidity,
        fee_rate_bps,
        is_crypto_15min,
        event_slug: event_slug.to_string(),
    })
}

/// Classify the outcome type based on outcome labels
fn classify_outcomes(first: &str, second: &str) -> OutcomeType {
    let first_lower = first.to_lowercase();
    let second_lower = second.to_lowercase();
    
    if (first_lower == "yes" && second_lower == "no") ||
       (first_lower == "no" && second_lower == "yes") {
        OutcomeType::YesNo
    } else if (first_lower == "up" && second_lower == "down") ||
              (first_lower == "down" && second_lower == "up") {
        OutcomeType::UpDown
    } else {
        OutcomeType::Other
    }
}

// ============================================================================
// MARKET DISCOVERY
// ============================================================================

/// Market discovery service
/// 
/// Queries Gamma API and filters for tradeable markets.
pub struct MarketDiscovery {
    gamma_client: GammaClient,
}

impl MarketDiscovery {
    /// Create a new market discovery service
    pub fn new() -> Self {
        Self {
            gamma_client: GammaClient::new(),
        }
    }
    
    /// Create from an existing Gamma client
    pub fn from_client(gamma_client: GammaClient) -> Self {
        Self { gamma_client }
    }
    
    /// Discover all active tradeable markets
    pub async fn discover_all(&self, filter: &MarketFilter) -> Result<Vec<DiscoveredMarket>> {
        info!("Discovering markets...");
        
        let events = self.gamma_client.get_active_events().await?;
        info!(event_count = events.len(), "Found active events");
        
        self.process_events(events, filter)
    }
    
    /// Discover 15-minute crypto markets specifically
    ///
    /// Uses slug-based discovery to find markets like:
    /// - btc-updown-15m-{timestamp}
    /// - eth-updown-15m-{timestamp}
    /// - sol-updown-15m-{timestamp}
    pub async fn discover_crypto_15min(&self) -> Result<Vec<DiscoveredMarket>> {
        info!("Discovering 15-min crypto markets via slug pattern...");

        // Use the new slug-based discovery method
        let events = self.gamma_client.discover_crypto_15min_markets().await?;
        info!(event_count = events.len(), "Found crypto events via slug discovery");

        let filter = MarketFilter::crypto_15min();
        self.process_events(events, &filter)
    }

    /// Discover hourly crypto markets
    ///
    /// Uses slug-based discovery to find markets like:
    /// - bitcoin-up-or-down-january-8-9pm-et
    /// - ethereum-up-or-down-january-8-9pm-et
    /// - solana-up-or-down-january-8-9pm-et
    /// - xrp-up-or-down-january-8-9pm-et
    pub async fn discover_crypto_hourly(&self) -> Result<Vec<DiscoveredMarket>> {
        info!("Discovering hourly crypto markets via slug pattern...");

        let events = self.gamma_client.discover_crypto_hourly_markets().await?;
        info!(event_count = events.len(), "Found hourly crypto events");

        let filter = MarketFilter::default();
        self.process_events(events, &filter)
    }

    /// Discover daily crypto markets
    ///
    /// Uses slug-based discovery to find markets like:
    /// - bitcoin-up-or-down-on-january-9
    /// - ethereum-up-or-down-on-january-9
    /// - solana-up-or-down-on-january-9
    /// - xrp-up-or-down-on-january-9
    pub async fn discover_crypto_daily(&self) -> Result<Vec<DiscoveredMarket>> {
        info!("Discovering daily crypto markets via slug pattern...");

        let events = self.gamma_client.discover_crypto_daily_markets().await?;
        info!(event_count = events.len(), "Found daily crypto events");

        let filter = MarketFilter::default();
        self.process_events(events, &filter)
    }

    /// Discover all crypto markets (15-min, hourly, and daily)
    ///
    /// This is the recommended method for maximum market coverage.
    pub async fn discover_all_crypto(&self) -> Result<Vec<DiscoveredMarket>> {
        info!("Discovering all crypto markets (15-min + hourly + daily)...");

        let events = self.gamma_client.discover_all_crypto_markets().await?;
        info!(event_count = events.len(), "Found total crypto events");

        let filter = MarketFilter::default();
        self.process_events(events, &filter)
    }
    
    /// Discover markets by event slug
    pub async fn discover_by_slug(&self, slug: &str) -> Result<Vec<DiscoveredMarket>> {
        info!(slug = %slug, "Discovering markets by slug");
        
        let event = self.gamma_client.get_event_by_slug(slug).await?;
        
        match event {
            Some(e) => {
                let filter = MarketFilter::default();
                self.process_events(vec![e], &filter)
            }
            None => {
                warn!(slug = %slug, "Event not found");
                Ok(Vec::new())
            }
        }
    }
    
    /// Process events and extract tradeable markets
    fn process_events(&self, events: Vec<GammaEvent>, filter: &MarketFilter) -> Result<Vec<DiscoveredMarket>> {
        let mut discovered = Vec::new();
        
        for event in events {
            for market in event.markets {
                // Skip non-tradeable markets
                if !market.is_tradeable() {
                    continue;
                }
                
                // Skip non-binary markets
                if !market.is_binary() {
                    continue;
                }
                
                // Try to convert to discovered market
                match try_convert_market(&market, &event.slug) {
                    Ok(dm) => {
                        // Apply filters
                        if self.passes_filter(&dm, filter) {
                            debug!(
                                condition_id = %dm.condition_id,
                                question = %dm.question,
                                outcome_type = ?dm.outcome_type,
                                "Discovered market"
                            );
                            discovered.push(dm);
                        }
                    }
                    Err(e) => {
                        warn!(
                            condition_id = %market.condition_id,
                            error = %e,
                            "Failed to parse market"
                        );
                    }
                }
            }
        }
        
        // Apply limit
        if let Some(limit) = filter.limit {
            discovered.truncate(limit);
        }
        
        info!(count = discovered.len(), "Discovered tradeable markets");
        Ok(discovered)
    }
    
    /// Check if a market passes the filter criteria
    fn passes_filter(&self, market: &DiscoveredMarket, filter: &MarketFilter) -> bool {
        // Check outcome type filter
        if let Some(ref allowed_types) = filter.outcome_types {
            if !allowed_types.contains(&market.outcome_type) {
                return false;
            }
        }
        
        // Check 15-min crypto filter
        if filter.crypto_15min_only && !market.is_crypto_15min {
            return false;
        }
        
        // Check minimum volume
        if let Some(min_volume) = filter.min_volume_24h {
            if market.volume_24h < min_volume {
                return false;
            }
        }
        
        // Check minimum liquidity
        if let Some(min_liquidity) = filter.min_liquidity {
            if market.liquidity < min_liquidity {
                return false;
            }
        }
        
        true
    }
}

impl Default for MarketDiscovery {
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

    #[test]
    fn test_classify_outcomes_yes_no() {
        assert_eq!(classify_outcomes("Yes", "No"), OutcomeType::YesNo);
        assert_eq!(classify_outcomes("yes", "no"), OutcomeType::YesNo);
        assert_eq!(classify_outcomes("YES", "NO"), OutcomeType::YesNo);
        assert_eq!(classify_outcomes("No", "Yes"), OutcomeType::YesNo);
    }

    #[test]
    fn test_classify_outcomes_up_down() {
        assert_eq!(classify_outcomes("Up", "Down"), OutcomeType::UpDown);
        assert_eq!(classify_outcomes("up", "down"), OutcomeType::UpDown);
        assert_eq!(classify_outcomes("Down", "Up"), OutcomeType::UpDown);
    }

    #[test]
    fn test_classify_outcomes_other() {
        assert_eq!(classify_outcomes("A", "B"), OutcomeType::Other);
        assert_eq!(classify_outcomes("Heads", "Tails"), OutcomeType::Other);
    }

    #[test]
    fn test_market_filter_default() {
        let filter = MarketFilter::default();
        assert!(filter.outcome_types.is_none());
        assert!(filter.min_volume_24h.is_none());
        assert!(!filter.crypto_15min_only);
    }

    #[test]
    fn test_market_filter_crypto() {
        let filter = MarketFilter::crypto_15min();
        assert!(filter.crypto_15min_only);
        assert!(filter.outcome_types.as_ref().unwrap().contains(&OutcomeType::UpDown));
    }

    #[test]
    fn test_discovered_market_to_pair() {
        let dm = DiscoveredMarket {
            condition_id: "0x123".to_string(),
            question: "Test market".to_string(),
            first_token_id: "token1".to_string(),
            second_token_id: "token2".to_string(),
            first_outcome: "Up".to_string(),
            second_outcome: "Down".to_string(),
            outcome_type: OutcomeType::UpDown,
            volume_24h: Decimal::ZERO,
            liquidity: Decimal::ZERO,
            fee_rate_bps: 1000,
            is_crypto_15min: true,
            event_slug: "btc-15m".to_string(),
        };
        
        let pair = dm.to_market_pair();
        assert_eq!(pair.condition_id, "0x123");
        assert_eq!(pair.yes_token_id, "token1");
        assert_eq!(pair.no_token_id, "token2");
        assert_eq!(pair.fee_rate_bps, 1000);
        assert!(pair.is_crypto_15min());
    }

    #[test]
    fn test_try_convert_market() {
        let gamma_market = crate::api::gamma::GammaMarket {
            condition_id: "0xabc".to_string(),
            question: "Will BTC go up?".to_string(),
            description: String::new(),
            slug: "btc-up".to_string(),
            outcomes: r#"["Up", "Down"]"#.to_string(),
            clob_token_ids: r#"["111", "222"]"#.to_string(),
            active: true,
            closed: false,
            archived: false,
            accepting_orders: true,
            volume_24hr: 50000.0,
            liquidity: 10000.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: Some(1000),
        };
        
        let dm = try_convert_market(&gamma_market, "test-event").unwrap();
        
        assert_eq!(dm.condition_id, "0xabc");
        assert_eq!(dm.first_token_id, "111");
        assert_eq!(dm.second_token_id, "222");
        assert_eq!(dm.first_outcome, "Up");
        assert_eq!(dm.second_outcome, "Down");
        assert_eq!(dm.outcome_type, OutcomeType::UpDown);
        assert_eq!(dm.volume_24h, Decimal::from(50000));
        assert!(dm.is_crypto_15min);
    }

    #[test]
    fn test_filter_passes() {
        let discovery = MarketDiscovery::new();
        
        let market = DiscoveredMarket {
            condition_id: "0x123".to_string(),
            question: "Test".to_string(),
            first_token_id: "1".to_string(),
            second_token_id: "2".to_string(),
            first_outcome: "Up".to_string(),
            second_outcome: "Down".to_string(),
            outcome_type: OutcomeType::UpDown,
            volume_24h: Decimal::from(100000),
            liquidity: Decimal::from(50000),
            fee_rate_bps: 1000,
            is_crypto_15min: true,
            event_slug: "test".to_string(),
        };
        
        // Default filter passes everything
        assert!(discovery.passes_filter(&market, &MarketFilter::default()));
        
        // Crypto filter passes crypto market
        assert!(discovery.passes_filter(&market, &MarketFilter::crypto_15min()));
        
        // Yes/No filter rejects crypto market
        assert!(!discovery.passes_filter(&market, &MarketFilter::yes_no_only()));
        
        // Volume filter
        let high_vol_filter = MarketFilter::new().with_min_volume(Decimal::from(50000));
        assert!(discovery.passes_filter(&market, &high_vol_filter));
        
        let too_high_vol = MarketFilter::new().with_min_volume(Decimal::from(500000));
        assert!(!discovery.passes_filter(&market, &too_high_vol));
    }
}
