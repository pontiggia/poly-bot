//! Gamma API client for market discovery
//!
//! Polymarket uses two APIs:
//! - CLOB API: Order book, trading, pricing
//! - Gamma API: Market discovery, event metadata, UI data
//!
//! The Gamma API is essential for discovering 15-minute crypto markets
//! which are NOT accessible via the CLOB `/market/{id}` endpoint.

use reqwest::Client;
use std::time::Duration;
use serde::{Deserialize, Deserializer};
use tracing::{debug, warn};

use crate::constants::GAMMA_API_URL;
use crate::error::{BotError, Result};

/// Custom deserializer for fields that can be either a string or a number
fn deserialize_string_or_number<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    
    struct StringOrNumber;
    
    impl<'de> Visitor<'de> for StringOrNumber {
        type Value = f64;
        
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or number")
        }
        
        fn visit_f64<E>(self, v: f64) -> std::result::Result<f64, E>
        where
            E: de::Error,
        {
            Ok(v)
        }
        
        fn visit_i64<E>(self, v: i64) -> std::result::Result<f64, E>
        where
            E: de::Error,
        {
            Ok(v as f64)
        }
        
        fn visit_u64<E>(self, v: u64) -> std::result::Result<f64, E>
        where
            E: de::Error,
        {
            Ok(v as f64)
        }
        
        fn visit_str<E>(self, v: &str) -> std::result::Result<f64, E>
        where
            E: de::Error,
        {
            v.parse::<f64>().map_err(de::Error::custom)
        }
    }
    
    deserializer.deserialize_any(StringOrNumber)
}

// ============================================================================
// RESPONSE TYPES
// ============================================================================

/// Event from Gamma API - contains multiple markets
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaEvent {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub markets: Vec<GammaMarket>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub tags: Vec<GammaTag>,
}

/// Tag from Gamma API
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaTag {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub slug: String,
}

/// Market from Gamma API (nested in event)
/// 
/// **IMPORTANT:** The `outcomes` and `clob_token_ids` fields are **stringified JSON**,
/// not actual arrays! Use `parse_outcomes()` and `parse_token_ids()` to access them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaMarket {
    /// Unique market identifier (condition ID)
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    
    /// Question/title of the market
    #[serde(default)]
    pub question: String,
    
    /// Market description
    #[serde(default)]
    pub description: String,
    
    /// URL slug
    #[serde(default)]
    pub slug: String,
    
    /// **Stringified JSON array** of outcomes, e.g. "[\"Up\", \"Down\"]"
    /// Use `parse_outcomes()` to get `Vec<String>`
    #[serde(default)]
    pub outcomes: String,
    
    /// **Stringified JSON array** of CLOB token IDs
    /// Use `parse_token_ids()` to get `Vec<String>`
    #[serde(rename = "clobTokenIds", default)]
    pub clob_token_ids: String,
    
    /// Is market currently active
    #[serde(default)]
    pub active: bool,
    
    /// Is market closed
    #[serde(default)]
    pub closed: bool,
    
    /// Is market archived
    #[serde(default)]
    pub archived: bool,
    
    /// Is market accepting orders
    #[serde(default)]
    pub accepting_orders: bool,
    
    /// 24-hour trading volume
    #[serde(rename = "volume24hr", default)]
    pub volume_24hr: f64,
    
    /// Total liquidity
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub liquidity: f64,
    
    /// Best ask price
    #[serde(default)]
    pub best_ask: Option<f64>,
    
    /// Best bid price  
    #[serde(default)]
    pub best_bid: Option<f64>,
    
    /// End date ISO string
    #[serde(default)]
    pub end_date: Option<String>,
    
    /// Fee rate in basis points
    /// 0 = standard market, 1000 = 15-min crypto (10%)
    #[serde(default)]
    pub fee_rate_bps: Option<u32>,
}

impl GammaMarket {
    /// Parse the stringified outcomes JSON into a Vec<String>
    /// 
    /// The Gamma API returns outcomes as: `"[\"Up\", \"Down\"]"`
    /// This parses it into: `vec!["Up", "Down"]`
    pub fn parse_outcomes(&self) -> Result<Vec<String>> {
        if self.outcomes.is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&self.outcomes)
            .map_err(|e| BotError::Json(format!("Failed to parse outcomes '{}': {}", self.outcomes, e)))
    }
    
    /// Parse the stringified CLOB token IDs JSON into a Vec<String>
    /// 
    /// The Gamma API returns token IDs as: `"[\"token1\", \"token2\"]"`
    /// This parses it into: `vec!["token1", "token2"]`
    pub fn parse_token_ids(&self) -> Result<Vec<String>> {
        if self.clob_token_ids.is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&self.clob_token_ids)
            .map_err(|e| BotError::Json(format!("Failed to parse clob_token_ids '{}': {}", self.clob_token_ids, e)))
    }
    
    /// Check if this is a binary market (exactly 2 outcomes)
    pub fn is_binary(&self) -> bool {
        self.parse_outcomes().map(|o| o.len() == 2).unwrap_or(false)
    }
    
    /// Check if this is a 15-minute crypto market based on outcomes
    pub fn is_crypto_15min(&self) -> bool {
        if let Ok(outcomes) = self.parse_outcomes() {
            outcomes.len() == 2 && 
            (outcomes.contains(&"Up".to_string()) || outcomes.contains(&"Down".to_string()))
        } else {
            false
        }
    }
    
    /// Check if this is a standard Yes/No binary market
    pub fn is_yes_no(&self) -> bool {
        if let Ok(outcomes) = self.parse_outcomes() {
            outcomes.len() == 2 && 
            (outcomes.contains(&"Yes".to_string()) || outcomes.contains(&"No".to_string()))
        } else {
            false
        }
    }
    
    /// Check if this market is tradeable (active, not closed, accepting orders)
    pub fn is_tradeable(&self) -> bool {
        self.active && !self.closed && !self.archived && self.accepting_orders
    }
    
    /// Get token ID for the first outcome (Yes/Up)
    pub fn first_token_id(&self) -> Result<Option<String>> {
        let tokens = self.parse_token_ids()?;
        Ok(tokens.into_iter().next())
    }
    
    /// Get token ID for the second outcome (No/Down)
    pub fn second_token_id(&self) -> Result<Option<String>> {
        let tokens = self.parse_token_ids()?;
        Ok(tokens.into_iter().nth(1))
    }
}

// ============================================================================
// GAMMA API CLIENT
// ============================================================================

/// Client for Polymarket's Gamma API
/// 
/// Used for market discovery since the CLOB API's `/market/{id}` endpoint
/// returns 404 for 15-minute crypto markets.
pub struct GammaClient {
    client: Client,
    base_url: String,
}

impl GammaClient {
    /// Create a new Gamma API client with default base URL
    pub fn new() -> Self {
        let client = Client::builder()
            .tcp_nodelay(true)
            .pool_max_idle_per_host(50)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("Failed to build Gamma HTTP client");

        Self {
            client,
            base_url: GAMMA_API_URL.to_string(),
        }
    }
    
    /// Create a Gamma client with custom base URL (for testing)
    pub fn with_base_url(base_url: &str) -> Self {
        let client = Client::builder()
            .tcp_nodelay(true)
            .build()
            .expect("Failed to build Gamma HTTP client");

        Self {
            client,
            base_url: base_url.to_string(),
        }
    }
    
    /// Create from an existing reqwest Client (for connection pooling)
    pub fn from_client(client: Client) -> Self {
        Self {
            client,
            base_url: GAMMA_API_URL.to_string(),
        }
    }
    
    /// Get all active events
    pub async fn get_active_events(&self) -> Result<Vec<GammaEvent>> {
        let url = format!("{}/events?active=true&closed=false", self.base_url);
        self.fetch_events(&url).await
    }

    /// Get all events (no active/closed filters). This will return active, closed,
    /// and archived events and is useful when you need to look up historical
    /// markets by token id or condition id.
    pub async fn get_all_events(&self) -> Result<Vec<GammaEvent>> {
        let url = format!("{}/events", self.base_url);
        self.fetch_events(&url).await
    }
    
    /// Get event by slug
    pub async fn get_event_by_slug(&self, slug: &str) -> Result<Option<GammaEvent>> {
        let url = format!("{}/events?slug={}", self.base_url, slug);
        let events = self.fetch_events(&url).await?;
        Ok(events.into_iter().next())
    }
    
    /// Get events by tag (e.g., "crypto", "sports")
    pub async fn get_events_by_tag(&self, tag: &str) -> Result<Vec<GammaEvent>> {
        let url = format!("{}/events?active=true&closed=false&tag_slug={}", self.base_url, tag);
        self.fetch_events(&url).await
    }
    
    /// Get all crypto 15-minute events
    /// 
    /// **NOTE**: This method filters from get_active_events() which does NOT
    /// return 15-min crypto markets! Use `discover_crypto_15min_markets()` instead.
    #[deprecated(note = "Use discover_crypto_15min_markets() for 15-min crypto discovery")]
    pub async fn get_crypto_15min_events(&self) -> Result<Vec<GammaEvent>> {
        // First get all active events, then filter
        let events = self.get_active_events().await?;
        
        let crypto_events: Vec<GammaEvent> = events
            .into_iter()
            .filter(|e| {
                // Check if any market in the event is a crypto 15-min market
                e.markets.iter().any(|m| m.is_crypto_15min() && m.is_tradeable())
            })
            .collect();
        
        debug!(count = crypto_events.len(), "Found crypto 15-min events");
        Ok(crypto_events)
    }
    
    /// Supported crypto assets for Up/Down markets
    pub const CRYPTO_ASSETS: &'static [&'static str] = &["btc", "eth", "sol", "xrp"];

    /// Crypto asset names for 1-hour markets (use full names in slugs)
    pub const CRYPTO_ASSET_NAMES: &'static [(&'static str, &'static str)] = &[
        ("btc", "bitcoin"),
        ("eth", "ethereum"),
        ("sol", "solana"),
        ("xrp", "xrp"),
    ];

    /// Discover 15-minute crypto markets by slug pattern
    ///
    /// 15-min crypto markets use slug format: `{asset}-updown-15m-{timestamp}`
    /// where timestamp is a Unix epoch rounded to 15-minute intervals.
    ///
    /// This method queries for:
    /// - Current active market (current 15-min interval)
    /// - Next upcoming market (next 15-min interval)
    ///
    /// For each supported crypto asset (BTC, ETH, SOL, XRP).
    pub async fn discover_crypto_15min_markets(&self) -> Result<Vec<GammaEvent>> {
        use std::time::{SystemTime, UNIX_EPOCH};
        
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BotError::Config(format!("System time error: {}", e)))?
            .as_secs();
        
        // Round to 15-minute intervals (900 seconds)
        let interval = 900u64;
        let current_interval = (now / interval) * interval;
        
        // Generate intervals to check:
        // - Current (may be in progress or about to start)
        // - Next (upcoming)
        // - One after (further ahead)
        let intervals = [
            current_interval,
            current_interval + interval,
            current_interval + 2 * interval,
        ];
        
        let mut all_events = Vec::new();
        
        for asset in Self::CRYPTO_ASSETS {
            for &ts in &intervals {
                let slug = format!("{}-updown-15m-{}", asset, ts);
                match self.get_event_by_slug(&slug).await {
                    Ok(Some(event)) => {
                        // Check if event has tradeable markets
                        let has_tradeable = event.markets.iter().any(|m| {
                            m.is_crypto_15min() && m.is_tradeable()
                        });
                        
                        if has_tradeable {
                            debug!(
                                slug = %slug, 
                                asset = %asset,
                                markets = event.markets.len(),
                                "Found 15-min crypto event"
                            );
                            all_events.push(event);
                        } else {
                            debug!(slug = %slug, "Event exists but no tradeable markets");
                        }
                    }
                    Ok(None) => {
                        debug!(slug = %slug, "No event found for slug");
                    }
                    Err(e) => {
                        warn!(slug = %slug, error = %e, "Error fetching event");
                    }
                }
            }
        }
        
        debug!(count = all_events.len(), "Discovered 15-min crypto events");
        Ok(all_events)
    }

    /// Discover hourly crypto markets by slug pattern
    ///
    /// Hourly markets use slug format: `{asset}-up-or-down-{month}-{day}-{hour}{am/pm}-et`
    /// Example: `bitcoin-up-or-down-january-8-9pm-et`
    pub async fn discover_crypto_hourly_markets(&self) -> Result<Vec<GammaEvent>> {
        use chrono::{Datelike, Timelike, Utc, Duration};

        let now = Utc::now();
        // Convert to Eastern Time (UTC-5)
        let et_offset = Duration::hours(-5);
        let et_now = now + et_offset;

        let months = [
            "january", "february", "march", "april", "may", "june",
            "july", "august", "september", "october", "november", "december"
        ];

        let mut all_events = Vec::new();

        // Check current hour and next few hours
        let hours_to_check = [
            et_now,
            et_now + Duration::hours(1),
            et_now + Duration::hours(2),
            et_now + Duration::hours(3),
        ];

        for (_, asset_name) in Self::CRYPTO_ASSET_NAMES {
            for check_time in &hours_to_check {
                let month = months[check_time.month0() as usize];
                let day = check_time.day();
                let hour_24 = check_time.hour();

                // Convert to 12-hour format
                let (hour_12, am_pm) = if hour_24 == 0 {
                    (12, "am")
                } else if hour_24 < 12 {
                    (hour_24, "am")
                } else if hour_24 == 12 {
                    (12, "pm")
                } else {
                    (hour_24 - 12, "pm")
                };

                // Format: bitcoin-up-or-down-january-8-9pm-et
                let slug = format!("{}-up-or-down-{}-{}-{}{}-et",
                    asset_name, month, day, hour_12, am_pm);

                if let Ok(Some(event)) = self.get_event_by_slug(&slug).await {
                    if event.markets.iter().any(|m| m.is_tradeable() && m.is_binary()) {
                        debug!(slug = %slug, "Found hourly crypto event");
                        all_events.push(event);
                    }
                }
            }
        }

        debug!(count = all_events.len(), "Discovered hourly crypto events");
        Ok(all_events)
    }

    /// Discover daily crypto markets by slug pattern
    ///
    /// Daily markets use slug format: `{asset}-up-or-down-on-{month}-{day}`
    /// Example: `bitcoin-up-or-down-on-january-9`
    pub async fn discover_crypto_daily_markets(&self) -> Result<Vec<GammaEvent>> {
        use chrono::{Datelike, Utc, Duration};

        let now = Utc::now();
        // Convert to Eastern Time
        let et_offset = Duration::hours(-5);
        let et_now = now + et_offset;

        let months = [
            "january", "february", "march", "april", "may", "june",
            "july", "august", "september", "october", "november", "december"
        ];

        let mut all_events = Vec::new();

        // Check today and tomorrow
        let days_to_check = [
            et_now,
            et_now + Duration::days(1),
        ];

        for (_, asset_name) in Self::CRYPTO_ASSET_NAMES {
            for check_time in &days_to_check {
                let month = months[check_time.month0() as usize];
                let day = check_time.day();

                // Format: bitcoin-up-or-down-on-january-9
                let slug = format!("{}-up-or-down-on-{}-{}", asset_name, month, day);

                if let Ok(Some(event)) = self.get_event_by_slug(&slug).await {
                    if event.markets.iter().any(|m| m.is_tradeable() && m.is_binary()) {
                        debug!(slug = %slug, "Found daily crypto event");
                        all_events.push(event);
                    }
                }
            }
        }

        debug!(count = all_events.len(), "Discovered daily crypto events");
        Ok(all_events)
    }

    /// Discover 5-minute crypto markets by slug pattern
    ///
    /// 5-min crypto markets use slug format: `{asset}-updown-5m-{timestamp}`
    /// where timestamp is a Unix epoch rounded to 5-minute (300s) intervals.
    pub async fn discover_crypto_5min_markets(&self) -> Result<Vec<GammaEvent>> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BotError::Config(format!("System time error: {}", e)))?
            .as_secs();

        let interval = 300u64; // 5 minutes
        let current_interval = (now / interval) * interval;

        let intervals = [
            current_interval,
            current_interval + interval,
            current_interval + 2 * interval,
        ];

        let mut all_events = Vec::new();

        for asset in Self::CRYPTO_ASSETS {
            for &ts in &intervals {
                let slug = format!("{}-updown-5m-{}", asset, ts);
                match self.get_event_by_slug(&slug).await {
                    Ok(Some(event)) => {
                        let has_tradeable = event.markets.iter().any(|m| {
                            m.is_crypto_15min() && m.is_tradeable()
                        });

                        if has_tradeable {
                            debug!(
                                slug = %slug,
                                asset = %asset,
                                markets = event.markets.len(),
                                "Found 5-min crypto event"
                            );
                            all_events.push(event);
                        }
                    }
                    Ok(None) => {
                        debug!(slug = %slug, "No event found for 5-min slug");
                    }
                    Err(e) => {
                        warn!(slug = %slug, error = %e, "Error fetching 5-min event");
                    }
                }
            }
        }

        debug!(count = all_events.len(), "Discovered 5-min crypto events");
        Ok(all_events)
    }

    /// Discover all crypto markets (5-min, 15-min, hourly, daily)
    pub async fn discover_all_crypto_markets(&self) -> Result<Vec<GammaEvent>> {
        let mut all_events = Vec::new();

        // Discover 5-min markets
        if let Ok(events) = self.discover_crypto_5min_markets().await {
            debug!(count = events.len(), "Found 5-min markets");
            all_events.extend(events);
        }

        // Discover 15-min markets
        if let Ok(events) = self.discover_crypto_15min_markets().await {
            debug!(count = events.len(), "Found 15-min markets");
            all_events.extend(events);
        }

        // Discover hourly markets
        if let Ok(events) = self.discover_crypto_hourly_markets().await {
            debug!(count = events.len(), "Found hourly markets");
            all_events.extend(events);
        }

        // Discover daily markets
        if let Ok(events) = self.discover_crypto_daily_markets().await {
            debug!(count = events.len(), "Found daily markets");
            all_events.extend(events);
        }

        debug!(count = all_events.len(), "Total crypto events discovered");
        Ok(all_events)
    }

    /// Internal fetch helper
    async fn fetch_events(&self, url: &str) -> Result<Vec<GammaEvent>> {
        debug!(url = %url, "Fetching from Gamma API");
        
        let response = self.client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| BotError::Http(e))?;
            
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "Gamma API error");
            return Err(BotError::Api {
                code: status.to_string(),
                message: format!("Gamma API error: {}", body),
            });
        }
        
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        debug!(len = body.len(), "Received Gamma API response");
        
        serde_json::from_str(&body)
            .map_err(|e| BotError::Json(format!("Failed to parse Gamma events: {}", e)))
    }
}

impl Default for GammaClient {
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
    fn test_parse_outcomes_updown() {
        let market = GammaMarket {
            condition_id: "test".to_string(),
            question: "Will BTC go up?".to_string(),
            description: String::new(),
            slug: String::new(),
            outcomes: r#"["Up", "Down"]"#.to_string(),
            clob_token_ids: String::new(),
            active: true,
            closed: false,
            archived: false,
            accepting_orders: true,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: None,
        };
        
        let outcomes = market.parse_outcomes().unwrap();
        assert_eq!(outcomes, vec!["Up", "Down"]);
        assert!(market.is_crypto_15min());
        assert!(!market.is_yes_no());
    }

    #[test]
    fn test_parse_outcomes_yesno() {
        let market = GammaMarket {
            condition_id: "test".to_string(),
            question: "Test?".to_string(),
            description: String::new(),
            slug: String::new(),
            outcomes: r#"["Yes", "No"]"#.to_string(),
            clob_token_ids: String::new(),
            active: true,
            closed: false,
            archived: false,
            accepting_orders: true,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: None,
        };
        
        let outcomes = market.parse_outcomes().unwrap();
        assert_eq!(outcomes, vec!["Yes", "No"]);
        assert!(!market.is_crypto_15min());
        assert!(market.is_yes_no());
    }

    #[test]
    fn test_parse_token_ids() {
        let market = GammaMarket {
            condition_id: "test".to_string(),
            question: String::new(),
            description: String::new(),
            slug: String::new(),
            outcomes: String::new(),
            clob_token_ids: r#"["token1", "token2"]"#.to_string(),
            active: false,
            closed: false,
            archived: false,
            accepting_orders: false,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: None,
        };
        
        let tokens = market.parse_token_ids().unwrap();
        assert_eq!(tokens, vec!["token1", "token2"]);
        assert_eq!(market.first_token_id().unwrap(), Some("token1".to_string()));
        assert_eq!(market.second_token_id().unwrap(), Some("token2".to_string()));
    }

    #[test]
    fn test_is_tradeable() {
        let mut market = GammaMarket {
            condition_id: "test".to_string(),
            question: String::new(),
            description: String::new(),
            slug: String::new(),
            outcomes: String::new(),
            clob_token_ids: String::new(),
            active: true,
            closed: false,
            archived: false,
            accepting_orders: true,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: None,
        };
        
        assert!(market.is_tradeable());
        
        market.closed = true;
        assert!(!market.is_tradeable());
        
        market.closed = false;
        market.accepting_orders = false;
        assert!(!market.is_tradeable());
    }

    #[test]
    fn test_is_binary() {
        let binary = GammaMarket {
            condition_id: "test".to_string(),
            question: String::new(),
            description: String::new(),
            slug: String::new(),
            outcomes: r#"["Yes", "No"]"#.to_string(),
            clob_token_ids: String::new(),
            active: false,
            closed: false,
            archived: false,
            accepting_orders: false,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_ask: None,
            best_bid: None,
            end_date: None,
            fee_rate_bps: None,
        };
        
        assert!(binary.is_binary());
        
        let multi = GammaMarket {
            outcomes: r#"["A", "B", "C"]"#.to_string(),
            ..binary.clone()
        };
        
        assert!(!multi.is_binary());
    }

    #[test]
    fn test_gamma_event_deserialize() {
        let json = r#"{
            "id": "event123",
            "slug": "btc-updown-15m",
            "title": "BTC 15 min",
            "description": "Test event",
            "active": true,
            "closed": false,
            "archived": false,
            "markets": [
                {
                    "conditionId": "0x123",
                    "question": "Will BTC go up?",
                    "outcomes": "[\"Up\", \"Down\"]",
                    "clobTokenIds": "[\"token1\", \"token2\"]",
                    "active": true,
                    "closed": false,
                    "archived": false,
                    "acceptingOrders": true,
                    "volume24hr": 1000000.0,
                    "liquidity": 50000.0
                }
            ],
            "tags": []
        }"#;
        
        let event: GammaEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.id, "event123");
        assert_eq!(event.slug, "btc-updown-15m");
        assert!(event.active);
        assert_eq!(event.markets.len(), 1);
        
        let market = &event.markets[0];
        assert_eq!(market.condition_id, "0x123");
        assert!(market.is_crypto_15min());
        assert!(market.is_tradeable());
    }

    #[test]
    fn test_gamma_client_creation() {
        let client = GammaClient::new();
        assert_eq!(client.base_url, GAMMA_API_URL);
        
        let custom = GammaClient::with_base_url("http://localhost:8080");
        assert_eq!(custom.base_url, "http://localhost:8080");
    }
}
