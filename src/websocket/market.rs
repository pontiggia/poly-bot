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
use tracing::{debug, error, info, warn};
use std::sync::atomic::{AtomicU64, Ordering};

/// Market WebSocket connection
pub struct MarketWebSocket {
    /// Token IDs to subscribe to
    token_ids: Vec<TokenId>,
    /// Channel to send parsed messages
    message_tx: mpsc::UnboundedSender<MarketMessage>,
    /// Raw WebSocket message counter (increments for every incoming raw message)
    raw_message_counter: Arc<AtomicU64>,
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
    /// Create a new market WebSocket connection
    pub fn new(
        token_ids: Vec<TokenId>,
        message_tx: mpsc::UnboundedSender<MarketMessage>,
        raw_message_counter: Arc<AtomicU64>,
    ) -> Self {
        Self {
            token_ids,
            message_tx,
            raw_message_counter,
        }
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

        // Send subscription message
        let subscribe_msg = SubscribeRequest {
            msg_type: "market".to_string(),
            assets_ids: self.token_ids.clone(),
        };

        let subscribe_json = serde_json::to_string(&subscribe_msg)
            .map_err(|e| BotError::Json(e.to_string()))?;

        write
            .send(Message::Text(subscribe_json))
            .await
            .map_err(|e| BotError::WebSocket(format!("Failed to subscribe: {}", e)))?;

        info!(
            "Subscribed to {} tokens: {:?}",
            self.token_ids.len(),
            self.token_ids
        );

        // Keepalive ping interval (5 seconds)
        let mut ping_interval = interval(Duration::from_secs(WEBSOCKET_PING_INTERVAL_SEC));

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
    async fn handle_message(&self, msg: Message) -> Result<()> {
        // Count every raw message received from the WebSocket
        self.raw_message_counter.fetch_add(1, Ordering::Relaxed);
        match msg {
            Message::Text(text) => {
                // Log raw messages for debugging (truncated)
                debug!("Raw WS message: {}", &text[..text.len().min(200)]);

                // Parse as generic JSON to see what we have
                let json_value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| BotError::Json(format!("Invalid JSON: {}", e)))?;

                // Polymarket book messages have this structure:
                // { "event_type": "book", "asset_id": "...", "market": "...", "timestamp": ..., "hash": "...", "bids": [...], "asks": [...] }
                // OR sometimes: { "asset_id": "...", ...other fields... }
                // Also: "price_change" events for individual level updates

                if let Some(event_type) = json_value.get("event_type").and_then(|v| v.as_str()) {
                    match event_type {
                        "book" => {
                            // Full book snapshot
                            self.handle_book_event(&json_value)?;
                        }
                        "price_change" => {
                            // Price level update - also contains best bid/ask
                            self.handle_price_change_event(&json_value)?;
                        }
                        other => {
                            debug!("Unknown event type: {}", other);
                        }
                    }
                } else {
                    // If no event_type, log it for debugging
                    debug!("Unknown message format: {}", &text[..text.len().min(200)]);
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

    /// Handle a full book snapshot event
    fn handle_book_event(&self, json_value: &serde_json::Value) -> Result<()> {
        if let (Some(asset_id), Some(market), Some(bids_val), Some(asks_val)) = (
            json_value.get("asset_id").and_then(|v| v.as_str()),
            json_value.get("market").and_then(|v| v.as_str()),
            json_value.get("bids"),
            json_value.get("asks"),
        ) {
            let bids: Vec<PriceLevel> = serde_json::from_value(bids_val.clone())
                .unwrap_or_default();
            let asks: Vec<PriceLevel> = serde_json::from_value(asks_val.clone())
                .unwrap_or_default();

            let timestamp = json_value.get("timestamp").and_then(|v| v.as_i64());
            let hash = json_value.get("hash").and_then(|v| v.as_str()).map(String::from);

            let book_update = BookUpdateMessage {
                token_id: asset_id.to_string(),
                market: market.to_string(),
                asset: asset_id.to_string(),
                timestamp,
                hash,
                bids,
                asks,
            };

            info!(
                "Book snapshot: {} levels bid, {} levels ask",
                book_update.bids.len(),
                book_update.asks.len()
            );

            let _ = self.message_tx.send(MarketMessage::BookSnapshot(book_update));
        }
        Ok(())
    }

    /// Handle a price_change event (incremental update)
    fn handle_price_change_event(&self, json_value: &serde_json::Value) -> Result<()> {
        // price_change events contain a list of individual level updates
        // Each update is for a single price level on one side

        if let Some(market) = json_value.get("market").and_then(|v| v.as_str()) {
            let timestamp = json_value.get("timestamp").and_then(|v| v.as_i64());

            if let Some(price_changes) = json_value.get("price_changes").and_then(|v| v.as_array()) {
                for change in price_changes {
                    if let (Some(asset_id), Some(price), Some(size), Some(side)) = (
                        change.get("asset_id").and_then(|v| v.as_str()),
                        change.get("price").and_then(|v| v.as_str()),
                        change.get("size").and_then(|v| v.as_str()),
                        change.get("side").and_then(|v| v.as_str()),
                    ) {
                        let hash = change.get("hash").and_then(|v| v.as_str()).map(String::from);

                        let level_update = LevelUpdateMessage {
                            token_id: asset_id.to_string(),
                            market: market.to_string(),
                            side: side.to_uppercase(),
                            price: price.to_string(),
                            size: size.to_string(),
                            timestamp,
                            hash,
                        };

                        let _ = self.message_tx.send(MarketMessage::LevelUpdate(level_update));
                    }
                }
            }
        }
        Ok(())
    }
}
