//! Constants for the Polymarket bot

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Duration;

// ============================================================================
// API ENDPOINTS
// ============================================================================

/// CLOB REST API base URL
pub const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Gamma API base URL (market metadata)
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";

/// Market WebSocket URL (order books)
pub const MARKET_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// User WebSocket URL (fills/trades)
pub const USER_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";

/// Live data WebSocket URL (activity feed)
pub const LIVE_DATA_WS_URL: &str = "wss://ws-live-data.polymarket.com/";

/// Binance WebSocket URL (spot prices)
pub const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";

// ============================================================================
// CHAIN CONFIGURATION
// ============================================================================

/// Polygon chain ID
pub const CHAIN_ID: u64 = 137;

/// USDC decimal places
pub const USDC_DECIMALS: u32 = 6;

/// Polymarket CTF Exchange contract address
pub const EXCHANGE_CONTRACT: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";

/// Polymarket Neg Risk CTF Exchange contract address
pub const NEG_RISK_EXCHANGE_CONTRACT: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";

// ============================================================================
// TIMING CONSTANTS
// ============================================================================

/// WebSocket ping interval (keepalive)
pub const WS_PING_INTERVAL: Duration = Duration::from_secs(5);

/// WebSocket ping interval in seconds (for non-const contexts)
pub const WEBSOCKET_PING_INTERVAL_SEC: u64 = 5;

/// WebSocket reconnect delay
pub const WS_RECONNECT_DELAY: Duration = Duration::from_millis(1000);

/// WebSocket reconnect delay in milliseconds (for non-const contexts)
pub const WEBSOCKET_RECONNECT_DELAY_MS: u64 = 1000;

/// HTTP request timeout (increased from 500ms to reduce timeouts on API latency)
pub const HTTP_TIMEOUT: Duration = Duration::from_millis(2000);

/// HTTP connection timeout (increased from 200ms)
pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);

/// Reconciliation interval (REST sync with server)
pub const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(120);

/// Connection warming interval
pub const CONNECTION_WARM_INTERVAL: Duration = Duration::from_secs(30);

/// Max time to hold unhedged position from partial fill
pub const PARTIAL_FILL_UNWIND_MS: u64 = 500;

// ============================================================================
// POSITION SIZING
// ============================================================================

/// Maximum bet size per trade (USD)
/// Phase 9 live test: lowered from $5000 to $5 for safety
pub const MAX_BET_SIZE_USD: Decimal = dec!(5.0);

/// Minimum bet size (USD)
/// Phase 9 live test: lowered from $10 to $2 for small position testing
pub const MIN_BET_SIZE_USD: Decimal = dec!(2.0);

/// Maximum capital percentage per event
pub const MAX_CAPITAL_PCT: Decimal = dec!(0.40);

// ============================================================================
// RISK LIMITS (HARD GUARDRAILS)
// ============================================================================

/// Maximum daily loss before stopping (USD)
pub const MAX_DAILY_LOSS_USD: Decimal = dec!(500.0);

/// Maximum concurrent open orders
pub const MAX_OPEN_ORDERS: u32 = 10;

/// Maximum unhedged directional exposure (USD)
pub const MAX_UNHEDGED_EXPOSURE: Decimal = dec!(100.0);

/// Maximum exposure from partial fills (USD)
pub const MAX_PARTIAL_FILL_EXPOSURE: Decimal = dec!(200.0);

// ============================================================================
// CIRCUIT BREAKER
// ============================================================================

/// Number of fatal errors before opening circuit breaker
pub const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

/// Reject rate threshold (open circuit if exceeded)
pub const REJECT_RATE_THRESHOLD: f64 = 0.20;

/// Time to wait before trying half-open
pub const CIRCUIT_RESET_TIMEOUT: Duration = Duration::from_secs(60);

// ============================================================================
// KILL SWITCH
// ============================================================================

/// Environment variable for kill switch
pub const KILL_SWITCH_ENV_VAR: &str = "POLYBOT_KILL";

/// File path for kill switch
pub const KILL_SWITCH_FILE: &str = "/tmp/polybot_kill";

// ============================================================================
// STRATEGY THRESHOLDS
// ============================================================================

/// Default minimum edge for taker arb (parabolic fees are ~1-1.5%)
pub const DEFAULT_MIN_EDGE: Decimal = dec!(0.02);

/// Extra margin for thin order books
pub const THIN_BOOK_EXTRA_MARGIN: Decimal = dec!(0.005);

/// Minimum edge for maker arb (no fees)
/// Phase 9 live test: 1% edge for safer execution
pub const MAKER_MIN_EDGE: Decimal = dec!(0.01);

// ============================================================================
// FEE IDENTIFICATION
// ============================================================================

/// Fee rate for 15-min crypto markets (in basis points)
pub const CRYPTO_15MIN_FEE_RATE_BPS: u32 = 1000;

/// Fee rate for standard markets
pub const STANDARD_FEE_RATE_BPS: u32 = 0;

// ============================================================================
// HTTP CLIENT
// ============================================================================

/// Max idle connections per host
pub const POOL_MAX_IDLE: usize = 20;

/// Idle connection timeout
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// TCP keepalive interval
pub const TCP_KEEPALIVE: Duration = Duration::from_secs(60);
