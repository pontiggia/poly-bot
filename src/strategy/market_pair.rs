//! Market pair registry - tracks binary market token pairs
//!
//! Binary markets on Polymarket have two outcome tokens. For arbitrage detection,
//! we need to know which tokens are complements.
//!
//! ## Supported Market Types
//!
//! - **Yes/No markets**: Standard binary markets with "Yes"/"No" outcomes
//! - **Up/Down markets**: 15-minute crypto markets with "Up"/"Down" outcomes
//!
//! Both are functionally equivalent for arbitrage:
//! - First token (Yes/Up) = positive outcome
//! - Second token (No/Down) = negative outcome
//!
//! ## Key Insight
//!
//! The WebSocket sends updates per token_id, not per market. We need to:
//! 1. Discover which tokens belong to the same market
//! 2. Know which is the first and second outcome
//! 3. Look up the complement when one side updates

use crate::api::types::{ConditionId, TokenId};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::Arc;

/// Type of binary market outcomes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BinaryOutcomeType {
    /// Standard binary (Yes/No)
    #[default]
    YesNo,
    /// Crypto 15-min (Up/Down)
    UpDown,
    /// Unknown binary (2 outcomes but not Yes/No or Up/Down)
    Other,
}

/// A binary market pair (two complement tokens)
///
/// For historical reasons, fields are named `yes_token_id` and `no_token_id`,
/// but they work for any binary market:
/// - `yes_token_id` = first outcome (Yes, Up, etc.)
/// - `no_token_id` = second outcome (No, Down, etc.)
#[derive(Debug, Clone)]
pub struct MarketPair {
    /// Market/condition ID (0x-prefixed hash)
    pub condition_id: ConditionId,

    /// First outcome token ID (Yes/Up - 256-bit integer as string)
    pub yes_token_id: TokenId,

    /// Second outcome token ID (No/Down - 256-bit integer as string)
    pub no_token_id: TokenId,

    /// Fee rate in basis points (1000 = 10% for 15-min crypto)
    pub fee_rate_bps: u32,

    /// Whether this is a neg-risk market (affects signing)
    pub is_neg_risk: bool,

    /// Market description/question (for logging)
    pub description: String,

    /// Minimum order size
    pub min_order_size: Decimal,

    /// Tick size for prices
    pub tick_size: Decimal,
    
    /// Type of binary outcomes
    pub outcome_type: BinaryOutcomeType,
    
    /// First outcome label (Yes, Up, etc.)
    pub first_outcome_label: String,
    
    /// Second outcome label (No, Down, etc.)
    pub second_outcome_label: String,

    /// Event slug from Gamma API (e.g., "btc-updown-5m-1740000000")
    pub event_slug: String,

    /// Unix timestamp when this market closes (if known)
    pub close_time: Option<i64>,
}

impl MarketPair {
    /// Create a new market pair (defaults to Yes/No type)
    pub fn new(
        condition_id: ConditionId,
        yes_token_id: TokenId,
        no_token_id: TokenId,
    ) -> Self {
        Self {
            condition_id,
            yes_token_id,
            no_token_id,
            fee_rate_bps: 0,
            is_neg_risk: false,
            description: String::new(),
            min_order_size: Decimal::ONE,
            tick_size: Decimal::new(1, 2), // 0.01 default
            outcome_type: BinaryOutcomeType::YesNo,
            first_outcome_label: "Yes".to_string(),
            second_outcome_label: "No".to_string(),
            event_slug: String::new(),
            close_time: None,
        }
    }

    /// Create a new Up/Down market pair (for 15-min crypto)
    pub fn new_up_down(
        condition_id: ConditionId,
        up_token_id: TokenId,
        down_token_id: TokenId,
    ) -> Self {
        Self {
            condition_id,
            yes_token_id: up_token_id,
            no_token_id: down_token_id,
            fee_rate_bps: 1000, // 15-min crypto default
            is_neg_risk: false,
            description: String::new(),
            min_order_size: Decimal::ONE,
            tick_size: Decimal::new(1, 2),
            outcome_type: BinaryOutcomeType::UpDown,
            first_outcome_label: "Up".to_string(),
            second_outcome_label: "Down".to_string(),
            event_slug: String::new(),
            close_time: None,
        }
    }
    
    /// Create with custom outcome labels
    pub fn with_outcomes(
        condition_id: ConditionId,
        first_token_id: TokenId,
        second_token_id: TokenId,
        first_label: impl Into<String>,
        second_label: impl Into<String>,
    ) -> Self {
        let first = first_label.into();
        let second = second_label.into();
        
        // Determine outcome type from labels
        let outcome_type = Self::classify_outcome_type(&first, &second);
        let fee_rate_bps = if outcome_type == BinaryOutcomeType::UpDown { 1000 } else { 0 };
        
        Self {
            condition_id,
            yes_token_id: first_token_id,
            no_token_id: second_token_id,
            fee_rate_bps,
            is_neg_risk: false,
            description: String::new(),
            min_order_size: Decimal::ONE,
            tick_size: Decimal::new(1, 2),
            outcome_type,
            first_outcome_label: first,
            second_outcome_label: second,
            event_slug: String::new(),
            close_time: None,
        }
    }
    
    /// Classify outcome type from labels
    fn classify_outcome_type(first: &str, second: &str) -> BinaryOutcomeType {
        let first_lower = first.to_lowercase();
        let second_lower = second.to_lowercase();
        
        if (first_lower == "yes" && second_lower == "no") ||
           (first_lower == "no" && second_lower == "yes") {
            BinaryOutcomeType::YesNo
        } else if (first_lower == "up" && second_lower == "down") ||
                  (first_lower == "down" && second_lower == "up") {
            BinaryOutcomeType::UpDown
        } else {
            BinaryOutcomeType::Other
        }
    }

    /// Create with fee rate
    pub fn with_fee_rate(mut self, fee_rate_bps: u32) -> Self {
        self.fee_rate_bps = fee_rate_bps;
        self
    }

    /// Create with neg-risk flag
    pub fn with_neg_risk(mut self, is_neg_risk: bool) -> Self {
        self.is_neg_risk = is_neg_risk;
        self
    }

    /// Create with description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Create with min order size
    pub fn with_min_order_size(mut self, min_order_size: Decimal) -> Self {
        self.min_order_size = min_order_size;
        self
    }

    /// Set event slug
    pub fn with_event_slug(mut self, slug: impl Into<String>) -> Self {
        self.event_slug = slug.into();
        self
    }

    /// Set close time (unix timestamp)
    pub fn with_close_time(mut self, close_time: i64) -> Self {
        self.close_time = Some(close_time);
        self
    }

    /// Is this a 15-minute crypto market (has taker fees)?
    pub fn is_crypto_15min(&self) -> bool {
        // 15-min crypto markets have fee_rate_bps = 1000 (10%)
        self.fee_rate_bps >= 100
    }
    
    /// Is this an Up/Down market?
    pub fn is_up_down(&self) -> bool {
        self.outcome_type == BinaryOutcomeType::UpDown
    }
    
    /// Is this a Yes/No market?
    pub fn is_yes_no(&self) -> bool {
        self.outcome_type == BinaryOutcomeType::YesNo
    }
    
    // ========================================================================
    // Token accessors with semantic names
    // ========================================================================
    
    /// Get first outcome token ID (Yes/Up)
    pub fn first_token_id(&self) -> &TokenId {
        &self.yes_token_id
    }
    
    /// Get second outcome token ID (No/Down)
    pub fn second_token_id(&self) -> &TokenId {
        &self.no_token_id
    }
    
    /// Get Up token ID (alias for first_token_id on Up/Down markets)
    pub fn up_token_id(&self) -> &TokenId {
        &self.yes_token_id
    }
    
    /// Get Down token ID (alias for second_token_id on Up/Down markets)
    pub fn down_token_id(&self) -> &TokenId {
        &self.no_token_id
    }

    /// Get the complement token ID
    pub fn complement(&self, token_id: &TokenId) -> Option<&TokenId> {
        if token_id == &self.yes_token_id {
            Some(&self.no_token_id)
        } else if token_id == &self.no_token_id {
            Some(&self.yes_token_id)
        } else {
            None
        }
    }

    /// Check if a token belongs to this pair
    pub fn contains(&self, token_id: &TokenId) -> bool {
        token_id == &self.yes_token_id || token_id == &self.no_token_id
    }

    /// Is this the YES token?
    pub fn is_yes(&self, token_id: &TokenId) -> bool {
        token_id == &self.yes_token_id
    }

    /// Is this the NO token?
    pub fn is_no(&self, token_id: &TokenId) -> bool {
        token_id == &self.no_token_id
    }
}

/// Registry of market pairs for arbitrage detection
///
/// Provides O(1) lookup by:
/// - condition_id (market ID)
/// - token_id (either YES or NO)
pub struct MarketPairRegistry {
    /// Map of condition_id -> MarketPair
    by_condition: DashMap<ConditionId, Arc<MarketPair>>,

    /// Map of token_id -> MarketPair (for quick lookup from WebSocket updates)
    by_token: DashMap<TokenId, Arc<MarketPair>>,
}

impl MarketPairRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            by_condition: DashMap::new(),
            by_token: DashMap::new(),
        }
    }

    /// Register a market pair
    pub fn register(&self, pair: MarketPair) {
        let pair = Arc::new(pair);

        // Index by condition ID
        self.by_condition
            .insert(pair.condition_id.clone(), pair.clone());

        // Index by both token IDs for O(1) lookup from WebSocket
        self.by_token
            .insert(pair.yes_token_id.clone(), pair.clone());
        self.by_token.insert(pair.no_token_id.clone(), pair.clone());
    }

    /// Unregister a market pair by condition ID
    pub fn unregister(&self, condition_id: &ConditionId) {
        if let Some((_, pair)) = self.by_condition.remove(condition_id) {
            self.by_token.remove(&pair.yes_token_id);
            self.by_token.remove(&pair.no_token_id);
        }
    }

    /// Get market pair by condition ID
    pub fn get_by_condition(&self, condition_id: &ConditionId) -> Option<Arc<MarketPair>> {
        self.by_condition.get(condition_id).map(|r| r.clone())
    }

    /// Get market pair by token ID (either YES or NO)
    pub fn get_by_token(&self, token_id: &TokenId) -> Option<Arc<MarketPair>> {
        self.by_token.get(token_id).map(|r| r.clone())
    }

    /// Get the complement token ID for a given token
    pub fn get_complement(&self, token_id: &TokenId) -> Option<TokenId> {
        self.by_token
            .get(token_id)
            .and_then(|pair| pair.complement(token_id).cloned())
    }

    /// Get all registered market pairs
    pub fn all_pairs(&self) -> Vec<Arc<MarketPair>> {
        self.by_condition.iter().map(|r| r.value().clone()).collect()
    }

    /// Get all condition IDs
    pub fn all_condition_ids(&self) -> Vec<ConditionId> {
        self.by_condition.iter().map(|r| r.key().clone()).collect()
    }

    /// Number of registered markets
    pub fn len(&self) -> usize {
        self.by_condition.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.by_condition.is_empty()
    }

    /// Clear all registered pairs
    pub fn clear(&self) {
        self.by_condition.clear();
        self.by_token.clear();
    }

    /// Filter pairs by predicate
    pub fn filter<F>(&self, predicate: F) -> Vec<Arc<MarketPair>>
    where
        F: Fn(&MarketPair) -> bool,
    {
        self.by_condition
            .iter()
            .filter(|r| predicate(r.value()))
            .map(|r| r.value().clone())
            .collect()
    }

    /// Get all 15-min crypto markets (for arb strategies)
    pub fn crypto_15min_markets(&self) -> Vec<Arc<MarketPair>> {
        self.filter(|pair| pair.is_crypto_15min())
    }
}

impl Default for MarketPairRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pair() -> MarketPair {
        MarketPair::new(
            "0xabc123".to_string(),
            "123456789".to_string(), // YES
            "987654321".to_string(), // NO
        )
        .with_fee_rate(1000)
        .with_description("Will BTC go up?")
    }

    #[test]
    fn test_market_pair_complement() {
        let pair = sample_pair();

        assert_eq!(
            pair.complement(&"123456789".to_string()),
            Some(&"987654321".to_string())
        );
        assert_eq!(
            pair.complement(&"987654321".to_string()),
            Some(&"123456789".to_string())
        );
        assert_eq!(pair.complement(&"unknown".to_string()), None);
    }

    #[test]
    fn test_market_pair_yes_no() {
        let pair = sample_pair();

        assert!(pair.is_yes(&"123456789".to_string()));
        assert!(!pair.is_no(&"123456789".to_string()));
        assert!(pair.is_no(&"987654321".to_string()));
        assert!(!pair.is_yes(&"987654321".to_string()));
    }

    #[test]
    fn test_is_crypto_15min() {
        let crypto = sample_pair().with_fee_rate(1000);
        let standard = sample_pair().with_fee_rate(0);

        assert!(crypto.is_crypto_15min());
        assert!(!standard.is_crypto_15min());
    }

    #[test]
    fn test_registry_register_lookup() {
        let registry = MarketPairRegistry::new();
        let pair = sample_pair();

        registry.register(pair.clone());

        // Lookup by condition
        let found = registry.get_by_condition(&"0xabc123".to_string());
        assert!(found.is_some());
        assert_eq!(found.unwrap().yes_token_id, "123456789");

        // Lookup by YES token
        let found = registry.get_by_token(&"123456789".to_string());
        assert!(found.is_some());

        // Lookup by NO token
        let found = registry.get_by_token(&"987654321".to_string());
        assert!(found.is_some());

        // Lookup unknown
        let found = registry.get_by_token(&"unknown".to_string());
        assert!(found.is_none());
    }

    #[test]
    fn test_registry_get_complement() {
        let registry = MarketPairRegistry::new();
        registry.register(sample_pair());

        let complement = registry.get_complement(&"123456789".to_string());
        assert_eq!(complement, Some("987654321".to_string()));

        let complement = registry.get_complement(&"987654321".to_string());
        assert_eq!(complement, Some("123456789".to_string()));
    }

    #[test]
    fn test_registry_unregister() {
        let registry = MarketPairRegistry::new();
        registry.register(sample_pair());

        assert_eq!(registry.len(), 1);

        registry.unregister(&"0xabc123".to_string());

        assert_eq!(registry.len(), 0);
        assert!(registry.get_by_token(&"123456789".to_string()).is_none());
    }

    #[test]
    fn test_registry_filter() {
        let registry = MarketPairRegistry::new();

        registry.register(sample_pair().with_fee_rate(1000));
        registry.register(
            MarketPair::new(
                "0xdef456".to_string(),
                "111".to_string(),
                "222".to_string(),
            )
            .with_fee_rate(0),
        );

        let crypto = registry.crypto_15min_markets();
        assert_eq!(crypto.len(), 1);
        assert_eq!(crypto[0].condition_id, "0xabc123");
    }
    
    #[test]
    fn test_up_down_market() {
        let pair = MarketPair::new_up_down(
            "0xbtc15m".to_string(),
            "up_token".to_string(),
            "down_token".to_string(),
        );
        
        assert!(pair.is_up_down());
        assert!(!pair.is_yes_no());
        assert_eq!(pair.outcome_type, BinaryOutcomeType::UpDown);
        assert_eq!(pair.first_outcome_label, "Up");
        assert_eq!(pair.second_outcome_label, "Down");
        assert_eq!(pair.up_token_id(), "up_token");
        assert_eq!(pair.down_token_id(), "down_token");
        assert_eq!(pair.fee_rate_bps, 1000); // Default for crypto
    }
    
    #[test]
    fn test_with_outcomes() {
        let pair = MarketPair::with_outcomes(
            "0x123".to_string(),
            "token1".to_string(),
            "token2".to_string(),
            "Up",
            "Down",
        );
        
        assert!(pair.is_up_down());
        assert_eq!(pair.first_outcome_label, "Up");
        assert_eq!(pair.second_outcome_label, "Down");
        
        let pair2 = MarketPair::with_outcomes(
            "0x456".to_string(),
            "tokenA".to_string(),
            "tokenB".to_string(),
            "Yes",
            "No",
        );
        
        assert!(pair2.is_yes_no());
        assert_eq!(pair2.first_outcome_label, "Yes");
    }
    
    #[test]
    fn test_token_accessors() {
        let pair = sample_pair();
        
        // Generic accessors
        assert_eq!(pair.first_token_id(), "123456789");
        assert_eq!(pair.second_token_id(), "987654321");
        
        // Yes/No accessors (backward compatible)
        assert_eq!(&pair.yes_token_id, "123456789");
        assert_eq!(&pair.no_token_id, "987654321");
    }
}
