//! Core API types for Polymarket CLOB
//!
//! All types match the Polymarket API specification.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ============================================================================
// TYPE ALIASES
// ============================================================================

/// Market condition ID (0x-prefixed, 66 chars)
pub type ConditionId = String;

/// Token ID (256-bit integer as string)
pub type TokenId = String;

/// Order ID (0x-prefixed, 66 chars)
pub type OrderId = String;

/// Ethereum address (0x-prefixed, 42 chars)
pub type Address = String;

/// Transaction hash (0x-prefixed, 66 chars)
pub type TxHash = String;

/// EIP-712 signature (0x + 130 hex chars)
pub type Signature = String;

// ============================================================================
// ENUMS
// ============================================================================

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Buy => write!(f, "BUY"),
            Side::Sell => write!(f, "SELL"),
        }
    }
}

impl Side {
    /// Get the enum index as string for order signing (0 = BUY, 1 = SELL)
    pub fn as_index_string(&self) -> &'static str {
        match self {
            Side::Buy => "0",
            Side::Sell => "1",
        }
    }
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Fill And Kill - execute immediately, cancel unfilled
    FAK,
    /// Good Till Cancel - remain on book until filled
    GTC,
    /// Fill Or Kill - execute entire order or cancel
    FOK,
}

/// Market outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Yes,
    No,
}

// ============================================================================
// ORDER BOOK TYPES
// ============================================================================

/// A single price level in the order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

impl PriceLevel {
    /// Parse price as Decimal
    pub fn price_decimal(&self) -> Option<Decimal> {
        self.price.parse().ok()
    }

    /// Parse size as Decimal
    pub fn size_decimal(&self) -> Option<Decimal> {
        self.size.parse().ok()
    }
}

/// Order book snapshot from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub market: ConditionId,
    pub asset_id: TokenId,
    pub timestamp: String,
    pub hash: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    #[serde(default)]
    pub last_trade_price: String,
    #[serde(default)]
    pub event_type: String,
}

/// Price change event from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceChange {
    pub asset_id: TokenId,
    pub price: String,
    pub size: String,
    pub side: Side,
    pub hash: String,
    #[serde(default)]
    pub best_bid: String,
    #[serde(default)]
    pub best_ask: String,
}

/// Price change message containing multiple changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceChangeMessage {
    pub market: ConditionId,
    pub price_changes: Vec<PriceChange>,
    pub timestamp: String,
    pub event_type: String,
}

// ============================================================================
// ORDER TYPES
// ============================================================================

/// Signed order for submission
/// Note: `side` is serialized as enum index ("0" for BUY, "1" for SELL) per Polymarket API spec
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOrder {
    pub salt: u64,
    pub maker: Address,
    pub signer: Address,
    pub taker: Address,
    pub token_id: TokenId,
    pub maker_amount: String,
    pub taker_amount: String,
    pub side: Side,
    pub expiration: String,
    pub nonce: String,
    pub fee_rate_bps: String,
    pub signature_type: u8,
    pub signature: Signature,
}

/// Order submission request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRequest {
    #[serde(default)]
    pub defer_exec: bool,
    pub order: SignedOrder,
    pub owner: String,
    pub order_type: OrderType,
}

/// Order submission response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    #[serde(default, alias = "errorMsg", alias = "error")]
    pub error_msg: String,
    #[serde(default, alias = "orderID")]
    pub order_id: OrderId,
    #[serde(default, alias = "takingAmount")]
    pub taking_amount: String,
    #[serde(default, alias = "makingAmount")]
    pub making_amount: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, alias = "transactionsHashes")]
    pub transactions_hashes: Vec<TxHash>,
    #[serde(default)]
    pub success: bool,
}

// ============================================================================
// TRADE/FILL TYPES
// ============================================================================

/// Trade notification from User WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeNotification {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub id: String,
    pub taker_order_id: OrderId,
    pub market: ConditionId,
    pub asset_id: TokenId,
    pub side: Side,
    pub size: String,
    pub fee_rate_bps: String,
    pub price: String,
    pub status: String,
    pub match_time: String,
    pub last_update: String,
    pub outcome: String,
    pub owner: String,
    pub trade_owner: String,
    pub maker_address: Address,
    #[serde(default)]
    pub transaction_hash: TxHash,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub event_type: String,
}

// ============================================================================
// MARKET TYPES
// ============================================================================

/// Market information from API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub condition_id: ConditionId,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub accepting_orders: bool,
    #[serde(default)]
    pub minimum_order_size: String,
    #[serde(default)]
    pub minimum_tick_size: String,
}

/// Fee rate response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRateResponse {
    pub fee_rate_bps: u32,
}

// ============================================================================
// WEBSOCKET MESSAGE TYPES
// ============================================================================

/// Generic WebSocket message (for initial parsing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsMessage {
    #[serde(default)]
    pub event_type: String,
    #[serde(default)]
    pub market: String,
    #[serde(default)]
    pub asset_id: String,
}

/// WebSocket subscription message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeMessage {
    pub assets_ids: Vec<TokenId>,
    #[serde(rename = "type")]
    pub msg_type: String,
}

/// User WebSocket authentication message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAuthMessage {
    pub auth: UserAuth,
    pub markets: Vec<ConditionId>,
    #[serde(rename = "type")]
    pub msg_type: String,
}

/// User authentication credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAuth {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}
