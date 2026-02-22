//! Market WebSocket - real-time order book updates
//!
//! Connects to wss://ws-subscriptions-clob.polymarket.com/ws/market
//! Subscribes to order book updates for specified token IDs

use crate::api::types::{PriceLevel, TokenId};
use crate::constants::*;
use crate::error::{BotError, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// Market WebSocket connection
pub struct MarketWebSocket {
    /// Channel to send parsed messages
    message_tx: mpsc::UnboundedSender<MarketMessage>,
    /// Receiver for dynamic subscription requests
    subscription_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<TokenId>>>,
    /// All subscribed tokens (initial + dynamic), used on reconnect
    all_subscribed: tokio::sync::Mutex<Vec<TokenId>>,
}

/// Message types from market WebSocket
#[derive(Debug, Clone)]
pub enum MarketMessage {
    /// Full book snapshot for a specific token
    BookSnapshot(BookUpdateMessage),
    /// Single level update (from price_change)
    LevelUpdate(LevelUpdateMessage),
    /// Connection established
    Connected,
    /// Connection lost, reconnecting
    Reconnecting,
}

/// Order book update message (full snapshot)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookUpdateMessage {
    /// Token ID
    pub token_id: TokenId,
    /// Market ID (condition ID)
    pub market: String,
    /// Asset ID (same as token_id for binary markets)
    pub asset: String,
    /// Timestamp of update
    #[serde(default)]
    pub timestamp: Option<i64>,
    /// Hash of book state (for deduplication)
    #[serde(default)]
    pub hash: Option<String>,
    /// Buy side levels (bids)
    pub bids: Vec<PriceLevel>,
    /// Sell side levels (asks)
    pub asks: Vec<PriceLevel>,
}

/// Single price level update (from price_change event)
#[derive(Debug, Clone)]
pub struct LevelUpdateMessage {
    /// Token ID
    pub token_id: TokenId,
    /// Market ID
    pub market: String,
    /// Side: "BUY" or "SELL"
    pub side: String,
    /// Price level
    pub price: String,
    /// Size at this level (0 = remove)
    pub size: String,
    /// Timestamp
    pub timestamp: Option<i64>,
    /// Hash
    pub hash: Option<String>,
}

/// Deserialize a timestamp that may be a JSON string or number into Option<i64>
fn deserialize_string_or_i64<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrI64Visitor;
    impl<'de> de::Visitor<'de> for StringOrI64Visitor {
        type Value = Option<i64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer timestamp")
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
            Ok(Some(v as i64))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            v.parse::<i64>().map(Some).map_err(de::Error::custom)
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(StringOrI64Visitor)
}

/// Single price change entry within a price_change event
#[derive(Debug, Deserialize)]
struct WsPriceChange {
    asset_id: Option<String>,
    price: Option<String>,
    size: Option<String>,
    side: Option<String>,
    #[serde(default)]
    hash: Option<String>,
}

/// Unified envelope for all WS messages.
///
/// Polymarket sends either bare objects `{...}` or arrays `[{...}]`.
/// Book snapshots have no event_type — detected by presence of bids/asks.
/// Price changes have price_changes array.
#[derive(Debug, Deserialize)]
struct WsEnvelope {
    // book snapshot fields
    asset_id: Option<String>,
    market: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_i64")]
    timestamp: Option<i64>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    bids: Vec<PriceLevel>,
    #[serde(default)]
    asks: Vec<PriceLevel>,
    // price_change fields
    #[serde(default)]
    price_changes: Vec<WsPriceChange>,
}

/// Subscription request message
#[derive(Debug, Serialize)]
struct SubscribeRequest {
    /// Must be "market"
    #[serde(rename = "type")]
    msg_type: String,
    /// List of token IDs to subscribe to
    assets_ids: Vec<TokenId>,
}

impl MarketWebSocket {
    /// Create a new market WebSocket connection with dynamic subscription support
    ///
    /// Returns `(Arc<MarketWebSocket>, mpsc::UnboundedSender<Vec<TokenId>>)`
    /// The sender can be used to dynamically subscribe to new tokens at runtime.
    pub fn new(
        token_ids: Vec<TokenId>,
        message_tx: mpsc::UnboundedSender<MarketMessage>,
    ) -> (Arc<Self>, mpsc::UnboundedSender<Vec<TokenId>>) {
        let (sub_tx, sub_rx) = mpsc::unbounded_channel();
        let ws = Arc::new(Self {
            all_subscribed: tokio::sync::Mutex::new(token_ids),
            message_tx,
            subscription_rx: tokio::sync::Mutex::new(sub_rx),
        });
        (ws, sub_tx)
    }

    /// Start the WebSocket connection with automatic reconnection
    pub async fn run(self: Arc<Self>) {
        let mut reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
        const MAX_BACKOFF: Duration = Duration::from_secs(30);

        loop {
            info!("Connecting to market WebSocket: {}", MARKET_WS_URL);

            match self.connect_and_run().await {
                Ok(_) => {
                    warn!("Market WebSocket connection closed normally");
                    reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
                }
                Err(e) => {
                    error!("Market WebSocket error: {}", e);
                    let _ = self.message_tx.send(MarketMessage::Reconnecting);

                    // Exponential backoff
                    warn!("Reconnecting in {:?}", reconnect_delay);
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Connect and run the WebSocket (returns on disconnect)
    async fn connect_and_run(&self) -> Result<()> {
        // Connect to WebSocket
        let (ws_stream, _) = connect_async(MARKET_WS_URL)
            .await
            .map_err(|e| BotError::WebSocket(format!("Connection failed: {}", e)))?;

        info!("Market WebSocket connected");
        let _ = self.message_tx.send(MarketMessage::Connected);

        let (mut write, mut read) = ws_stream.split();

        // On reconnect, subscribe to ALL known tokens (initial + dynamically added)
        let all_tokens = self.all_subscribed.lock().await.clone();

        let subscribe_msg = SubscribeRequest {
            msg_type: "market".to_string(),
            assets_ids: all_tokens.clone(),
        };

        let subscribe_json = serde_json::to_string(&subscribe_msg)
            .map_err(|e| BotError::Json(e.to_string()))?;

        write
            .send(Message::Text(subscribe_json))
            .await
            .map_err(|e| BotError::WebSocket(format!("Failed to subscribe: {}", e)))?;

        info!("Subscribed to {} tokens", all_tokens.len());

        // Keepalive ping interval (5 seconds)
        let mut ping_interval = interval(Duration::from_secs(WEBSOCKET_PING_INTERVAL_SEC));
        let mut sub_rx = self.subscription_rx.lock().await;

        loop {
            tokio::select! {
                // Receive messages
                Some(msg_result) = read.next() => {
                    match msg_result {
                        Ok(msg) => {
                            if let Err(e) = self.handle_message(msg).await {
                                error!("Failed to handle message: {}", e);
                            }
                        }
                        Err(e) => {
                            return Err(BotError::WebSocket(format!("Read error: {}", e)));
                        }
                    }
                }

                // Dynamic subscription requests
                Some(new_tokens) = sub_rx.recv() => {
                    if new_tokens.is_empty() {
                        continue;
                    }

                    // Track for reconnects
                    self.all_subscribed.lock().await.extend(new_tokens.clone());

                    let sub_msg = SubscribeRequest {
                        msg_type: "market".to_string(),
                        assets_ids: new_tokens.clone(),
                    };

                    match serde_json::to_string(&sub_msg) {
                        Ok(json) => {
                            if let Err(e) = write.send(Message::Text(json)).await {
                                warn!("Failed to send dynamic subscription: {}", e);
                            } else {
                                info!("Dynamically subscribed to {} new token(s)", new_tokens.len());
                            }
                        }
                        Err(e) => warn!("Failed to serialize subscription: {}", e),
                    }
                }

                // Send periodic ping
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        return Err(BotError::WebSocket(format!("Ping failed: {}", e)));
                    }
                    debug!("Sent WebSocket ping");
                }

                else => {
                    return Err(BotError::WebSocket("Stream ended unexpectedly".to_string()));
                }
            }
        }
    }

    /// Handle incoming WebSocket message
    ///
    /// Polymarket sends arrays of event objects: `[{"event_type":"book",...}, ...]`.
    /// We parse the whole array in one simd-json pass, then dispatch each item.
    async fn handle_message(&self, msg: Message) -> Result<()> {
        match msg {
            Message::Text(text) => {
                if text.len() < 3 {
                    return Ok(());
                }

                let parse_start = Instant::now();
                debug!("Raw WS message: {}", &text[..text.len().min(200)]);

                // Single-pass parsing into typed structs using serde_json.
                // The key perf win is eliminating the old double-parse pattern
                // (Value → clone → from_value). simd-json requires SIMD padding
                // on the buffer which is fragile, so we use serde_json directly.

                // Polymarket sends either a bare object `{...}` or an array `[{...}]`.
                // Peek at first non-whitespace byte to decide. Ignore non-JSON messages
                // (e.g. subscription acks, keepalives).
                let first_byte = text.as_bytes().iter().find(|&&b| b != b' ' && b != b'\n' && b != b'\r').copied();
                if first_byte != Some(b'{') && first_byte != Some(b'[') {
                    debug!("Ignoring non-JSON WS message: {}", &text[..text.len().min(100)]);
                    return Ok(());
                }
                let envelopes: Vec<WsEnvelope> =
                    if first_byte == Some(b'[') {
                        match serde_json::from_str::<Vec<WsEnvelope>>(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("Failed to parse WS array message: {}", e);
                                vec![]
                            }
                        }
                    } else {
                        match serde_json::from_str::<WsEnvelope>(&text) {
                            Ok(env) => vec![env],
                            Err(e) => {
                                warn!("Failed to parse WS object message: {}", e);
                                vec![]
                            }
                        }
                    };

                let parse_us = parse_start.elapsed().as_micros();
                if parse_us > 500 {
                    debug!(parse_us = parse_us, "[PERF] WS parse slow");
                }

                for envelope in envelopes {
                    // Dispatch: no event_type on book snapshots — detect by presence of bids/asks
                    if !envelope.bids.is_empty() || !envelope.asks.is_empty() {
                        self.handle_envelope_book(envelope)?;
                    } else if !envelope.price_changes.is_empty() {
                        self.handle_envelope_price_change(envelope)?;
                    } else {
                        debug!("Unknown WS envelope (no bids/asks/price_changes)");
                    }
                }
            }
            Message::Pong(_) => {
                debug!("Received WebSocket pong");
            }
            Message::Close(frame) => {
                warn!("WebSocket close frame received: {:?}", frame);
                return Err(BotError::WebSocket("Connection closed by server".to_string()));
            }
            _ => {
                debug!("Received other message type: {:?}", msg);
            }
        }

        Ok(())
    }

    /// Handle a book snapshot envelope
    fn handle_envelope_book(&self, env: WsEnvelope) -> Result<()> {
        if let (Some(asset_id), Some(market)) = (env.asset_id, env.market) {
            debug!(
                "Book snapshot: {} levels bid, {} levels ask",
                env.bids.len(),
                env.asks.len()
            );
            let book_update = BookUpdateMessage {
                token_id: asset_id.clone(),
                market,
                asset: asset_id,
                timestamp: env.timestamp,
                hash: env.hash,
                bids: env.bids,
                asks: env.asks,
            };
            let _ = self.message_tx.send(MarketMessage::BookSnapshot(book_update));
        }
        Ok(())
    }

    /// Handle a price_change envelope
    fn handle_envelope_price_change(&self, env: WsEnvelope) -> Result<()> {
        if let Some(market) = env.market {
            for change in env.price_changes {
                if let (Some(asset_id), Some(price), Some(size), Some(side)) =
                    (change.asset_id, change.price, change.size, change.side)
                {
                    let level_update = LevelUpdateMessage {
                        token_id: asset_id,
                        market: market.clone(),
                        side: side.to_uppercase(),
                        price,
                        size,
                        timestamp: env.timestamp,
                        hash: change.hash,
                    };
                    let _ = self.message_tx.send(MarketMessage::LevelUpdate(level_update));
                }
            }
        }
        Ok(())
    }
}
