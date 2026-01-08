//! Configuration management for the Polymarket bot

use crate::error::{BotError, Result};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Bot configuration loaded from environment
#[derive(Debug, Clone)]
pub struct Config {
    // Builder API credentials (for order placement)
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
    pub private_key: String,
    pub wallet_address: String,

    // User API credentials (for User WebSocket fills) - optional
    pub user_api_key: Option<String>,
    pub user_secret_key: Option<String>,
    pub user_passphrase: Option<String>,

    // Operating mode
    pub mode: OperatingMode,
    pub log_level: String,

    // Sizing configuration
    pub max_bet_usd: Decimal,
    pub max_position_per_market_usd: Decimal,
    pub max_total_exposure_usd: Decimal,

    // Risk limits
    pub max_daily_loss_usd: Decimal,
    pub max_open_orders: u32,

    // Maker strategy configuration
    /// Use maker execution (GTC orders, zero fees)
    pub use_maker_mode: bool,
    /// Price offset for maker orders (cents inside spread)
    pub maker_price_offset: Decimal,
    /// TTL for maker orders before cancellation (seconds)
    pub maker_order_ttl_secs: u64,

    // Phase 9 live test configuration
    /// Use aggressive live test mode (very low edge, small positions)
    pub use_live_test_mode: bool,
}

/// Operating mode for the bot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingMode {
    /// Paper trading - log what would happen, no real orders
    Paper,
    /// Live trading with real money
    Live,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        // Load .env file if it exists
        dotenvy::dotenv().ok();

        let api_key = std::env::var("POLYMARKET_API_KEY")
            .or_else(|_| std::env::var("api_key"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_API_KEY or api_key".into()))?;

        let secret_key = std::env::var("POLYMARKET_SECRET")
            .or_else(|_| std::env::var("secret_key"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_SECRET or secret_key".into()))?;

        let passphrase = std::env::var("POLYMARKET_PASSPHRASE")
            .or_else(|_| std::env::var("passphrase"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_PASSPHRASE or passphrase".into()))?;

        let private_key = std::env::var("PRIVATE_KEY")
            .or_else(|_| std::env::var("private_key"))
            .map_err(|_| BotError::Config("Missing PRIVATE_KEY or private_key".into()))?;

        let wallet_address = std::env::var("WALLET_ADDRESS")
            .or_else(|_| std::env::var("builder_address"))
            .map_err(|_| BotError::Config("Missing WALLET_ADDRESS or builder_address".into()))?;

        // User API credentials (optional - for User WebSocket to get fills)
        // These are your personal API keys, not builder keys
        let user_api_key = std::env::var("USER_API_KEY")
            .ok()
            .map(|v| v.trim_matches('"').trim().to_string());
        let user_secret_key = std::env::var("USER_SECRET_KEY")
            .ok()
            .map(|v| v.trim_matches('"').trim().to_string());
        let user_passphrase = std::env::var("USER_PASSPHRASE")
            .ok()
            .map(|v| v.trim_matches('"').trim().to_string());

        // Operating mode (default to paper)
        let mode = match std::env::var("BOT_MODE").as_deref() {
            Ok("live") => OperatingMode::Live,
            _ => OperatingMode::Paper,
        };

        let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());

        // Sizing (with defaults)
        let max_bet_usd = env_decimal("MAX_BET_USD", Decimal::from(100));
        let max_position_per_market_usd =
            env_decimal("MAX_POSITION_PER_MARKET_USD", Decimal::from(500));
        let max_total_exposure_usd = env_decimal("MAX_TOTAL_EXPOSURE_USD", Decimal::from(2000));

        // Risk limits (with defaults)
        let max_daily_loss_usd = env_decimal("MAX_DAILY_LOSS_USD", Decimal::from(100));
        let max_open_orders = std::env::var("MAX_OPEN_ORDERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        // Maker strategy configuration
        let use_maker_mode = std::env::var("USE_MAKER_MODE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let maker_price_offset = env_decimal("MAKER_PRICE_OFFSET", Decimal::new(5, 1)); // 0.5 cents default
        let maker_order_ttl_secs = std::env::var("MAKER_ORDER_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120); // 2 minutes default

        // Phase 9 live test mode - very aggressive settings for testing with small capital
        let use_live_test_mode = std::env::var("USE_LIVE_TEST_MODE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        Ok(Config {
            api_key: api_key.trim_matches('"').trim().to_string(),
            secret_key: secret_key.trim_matches('"').trim().to_string(),
            passphrase: passphrase.trim_matches('"').trim().to_string(),
            private_key: private_key.trim_matches('"').trim().to_string(),
            wallet_address: wallet_address.trim_matches('"').trim().to_string(),
            user_api_key,
            user_secret_key,
            user_passphrase,
            mode,
            log_level,
            max_bet_usd,
            max_position_per_market_usd,
            max_total_exposure_usd,
            max_daily_loss_usd,
            max_open_orders,
            use_maker_mode,
            maker_price_offset,
            maker_order_ttl_secs,
            use_live_test_mode,
        })
    }

    /// Check if running in paper trading mode
    pub fn is_paper_mode(&self) -> bool {
        self.mode == OperatingMode::Paper
    }

    /// Check if user credentials are configured for User WebSocket
    pub fn has_user_credentials(&self) -> bool {
        self.user_api_key.is_some()
            && self.user_secret_key.is_some()
            && self.user_passphrase.is_some()
    }

    /// Get user API credentials for User WebSocket (if configured)
    pub fn user_credentials(&self) -> Option<crate::api::ApiCredentials> {
        if self.has_user_credentials() {
            Some(crate::api::ApiCredentials::new(
                self.user_api_key.clone().unwrap(),
                self.user_secret_key.clone().unwrap(),
                self.user_passphrase.clone().unwrap(),
                self.wallet_address.clone(),
            ))
        } else {
            None
        }
    }
}

/// Helper to read a Decimal from environment with default
fn env_decimal(key: &str, default: Decimal) -> Decimal {
    std::env::var(key)
        .ok()
        .and_then(|v| Decimal::from_str(&v).ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_operating_mode_default_paper() {
        // Without BOT_MODE set, should default to Paper
        std::env::remove_var("BOT_MODE");
        // This would need full env setup to test properly
    }
}
