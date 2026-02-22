//! User WebSocket - trade notifications and fills
//!
//! Connects to wss://ws-subscriptions-clob.polymarket.com/ws/user
//! Receives fill notifications for authenticated user's orders.

use crate::api::types::Side;
use crate::constants::*;
use crate::error::{BotError, Result};
use crate::ledger::Fill;
use chrono::{TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// User WebSocket connection for fill notifications
pub struct UserWebSocket {
    /// API key for authentication
    api_key: String,
    /// API secret
    secret: String,
    /// API passphrase
    passphrase: String,
    /// Channel to send fill notifications
    fill_tx: mpsc::UnboundedSender<UserMessage>,
}

/// Message types from user WebSocket
#[derive(Debug, Clone)]
pub enum UserMessage {
    /// Connection established
    Connected,
    /// Connection lost, reconnecting
    Reconnecting,
    /// Trade/fill notification
    Trade(TradeNotification),
    /// Order update (ack, cancel, etc.)
    OrderUpdate(OrderUpdate),
}

/// Trade notification from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeNotification {
    /// Trade ID
    #[serde(default)]
    pub id: String,
    /// Order ID that was filled (only valid when we are TAKER)
    #[serde(default)]
    pub taker_order_id: String,
    /// Market/condition ID
    #[serde(default)]
    pub market: String,
    /// Asset/token ID
    #[serde(default)]
    pub asset_id: String,
    /// Side (BUY/SELL)
    #[serde(default)]
    pub side: String,
    /// Size filled
    #[serde(default)]
    pub size: String,
    /// Price of fill
    #[serde(default)]
    pub price: String,
    /// Fee rate in bps
    #[serde(default)]
    pub fee_rate_bps: String,
    /// Status (MATCHED, etc.)
    #[serde(default)]
    pub status: String,
    /// Timestamp
    #[serde(default)]
    pub timestamp: String,
    /// Whether we are taker or maker
    #[serde(default)]
    pub trader_side: String,
    /// Maker orders that were filled (contains our order_id when we are MAKER)
    #[serde(default)]
    pub maker_orders: Vec<MakerOrderFill>,
}

/// Individual maker order fill within a trade
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerOrderFill {
    /// The maker's order ID
    #[serde(default)]
    pub order_id: String,
    /// Amount matched
    #[serde(default)]
    pub matched_amount: String,
    /// Price
    #[serde(default)]
    pub price: String,
}

impl TradeNotification {
    /// Get the order ID that belongs to us (differs based on maker vs taker)
    pub fn our_order_id(&self) -> Option<&str> {
        if self.trader_side.to_uppercase() == "MAKER" {
            // When we're maker, our order ID is in maker_orders
            self.maker_orders.first().map(|m| m.order_id.as_str())
        } else {
            // When we're taker, our order ID is taker_order_id
            if self.taker_order_id.is_empty() {
                None
            } else {
                Some(&self.taker_order_id)
            }
        }
    }
    
    /// Get all order IDs that could be ours (for maker fills, there may be multiple)
    pub fn our_order_ids(&self) -> Vec<&str> {
        if self.trader_side.to_uppercase() == "MAKER" {
            self.maker_orders.iter().map(|m| m.order_id.as_str()).collect()
        } else if !self.taker_order_id.is_empty() {
            vec![&self.taker_order_id]
        } else {
            vec![]
        }
    }
    
    /// Convert to Fill struct for ledger (legacy, uses first order)
    pub fn to_fill(&self) -> Result<Fill> {
        let order_id = self.our_order_id().unwrap_or("").to_string();
        self.to_fill_for_order(&order_id)
    }
    
    /// Convert to Fill struct for a specific order ID
    /// This correctly uses matched_amount for maker fills instead of total trade size
    pub fn to_fill_for_order(&self, our_order_id: &str) -> Result<Fill> {
        let side = match self.side.to_uppercase().as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => return Err(BotError::Json(format!("Unknown side: {}", self.side))),
        };

        let is_maker = self.trader_side.to_uppercase() == "MAKER";
        
        // ✅ FIX: For maker fills, use matched_amount from our specific maker order
        // The trade.size is the TOTAL trade size, which could include many makers
        let (size, price) = if is_maker {
            // Find our specific maker order to get our matched_amount
            let our_maker_order = self.maker_orders.iter()
                .find(|m| m.order_id == our_order_id);
            
            match our_maker_order {
                Some(maker) => {
                    let matched = Decimal::from_str(&maker.matched_amount)
                        .map_err(|e| BotError::Json(format!("Invalid matched_amount: {}", e)))?;
                    // Use maker's price if available, otherwise fall back to trade price
                    let maker_price = if !maker.price.is_empty() {
                        Decimal::from_str(&maker.price)
                            .unwrap_or_else(|_| Decimal::from_str(&self.price).unwrap_or(Decimal::ZERO))
                    } else {
                        Decimal::from_str(&self.price)
                            .map_err(|e| BotError::Json(format!("Invalid price: {}", e)))?
                    };
                    (matched, maker_price)
                }
                None => {
                    // Fallback: no matching maker order, use trade size
                    let size = Decimal::from_str(&self.size)
                        .map_err(|e| BotError::Json(format!("Invalid size: {}", e)))?;
                    let price = Decimal::from_str(&self.price)
                        .map_err(|e| BotError::Json(format!("Invalid price: {}", e)))?;
                    (size, price)
                }
            }
        } else {
            // Taker: use trade.size directly (we took this amount)
            let size = Decimal::from_str(&self.size)
                .map_err(|e| BotError::Json(format!("Invalid size: {}", e)))?;
            let price = Decimal::from_str(&self.price)
                .map_err(|e| BotError::Json(format!("Invalid price: {}", e)))?;
            (size, price)
        };
        
        let fee = if is_maker {
            // Makers pay no fees
            Decimal::ZERO
        } else {
            // Takers pay parabolic fees based on market tier
            // fee_rate_bps is a tier flag, NOT a linear rate:
            //   1000 = crypto 5m/15m → feeRate=0.25, exponent=2
            //   other > 0 = sports   → feeRate=0.0175, exponent=1
            //   0 = no fees
            let fee_bps = self.fee_rate_bps.parse::<u32>().unwrap_or(0);
            if fee_bps == 0 {
                Decimal::ZERO
            } else {
                let (fee_rate, exponent) = if fee_bps >= 1000 {
                    (dec!(0.25), 2u32)
                } else {
                    (dec!(0.0175), 1u32)
                };
                // Per-leg parabolic: fee = C * p * feeRate * (p * (1-p))^exponent
                let variance = price * (Decimal::ONE - price);
                let mut curve = variance;
                for _ in 1..exponent {
                    curve *= variance;
                }
                size * price * fee_rate * curve
            }
        };

        // Parse timestamp
        let timestamp = self
            .timestamp
            .parse::<i64>()
            .ok()
            .and_then(|ts| {
                if ts > 1_000_000_000_000 {
                    Utc.timestamp_millis_opt(ts).single()
                } else {
                    Utc.timestamp_opt(ts, 0).single()
                }
            })
            .unwrap_or_else(Utc::now);

        Ok(Fill {
            fill_id: self.id.clone(),
            order_id: our_order_id.to_string(),
            token_id: self.asset_id.clone(),
            side,
            price,
            size,
            fee,
            timestamp,
        })
    }
}

/// Order update notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdate {
    /// Order ID
    #[serde(default)]
    pub order_id: String,
    /// New status
    #[serde(default)]
    pub status: String,
    /// Timestamp
    #[serde(default)]
    pub timestamp: String,
}

/// Authentication message for user WebSocket
#[derive(Debug, Serialize)]
struct AuthMessage {
    auth: AuthPayload,
    markets: Vec<String>,
    #[serde(rename = "type")]
    msg_type: String,
}

#[derive(Debug, Serialize)]
struct AuthPayload {
    #[serde(rename = "apiKey")]
    api_key: String,
    secret: String,
    passphrase: String,
}

impl UserWebSocket {
    /// Create a new user WebSocket connection
    pub fn new(
        api_key: String,
        secret: String,
        passphrase: String,
        fill_tx: mpsc::UnboundedSender<UserMessage>,
    ) -> Self {
        Self {
            api_key,
            secret,
            passphrase,
            fill_tx,
        }
    }

    /// Start the WebSocket connection with automatic reconnection
    ///
    /// Never gives up — retries indefinitely with exponential backoff.
    /// The bot MUST receive fill notifications to track positions accurately.
    pub async fn run(self: Arc<Self>) {
        let mut reconnect_delay = Duration::from_secs(30);
        const MAX_BACKOFF: Duration = Duration::from_secs(300); // Max 5 minutes
        let mut consecutive_failures = 0u32;

        loop {
            if consecutive_failures == 0 {
                info!("Connecting to user WebSocket: {}", USER_WS_URL);
            } else {
                debug!("Reconnecting to user WebSocket (attempt {})", consecutive_failures + 1);
            }

            match self.connect_and_run().await {
                Ok(_) => {
                    debug!("User WebSocket connection closed normally");
                    reconnect_delay = Duration::from_secs(30);
                    consecutive_failures = 0;
                }
                Err(e) => {
                    consecutive_failures += 1;

                    if consecutive_failures == 1 {
                        warn!("User WebSocket error: {} (will retry)", e);
                    } else {
                        debug!("User WebSocket error (attempt {}): {}", consecutive_failures, e);
                    }

                    let _ = self.fill_tx.send(UserMessage::Reconnecting);

                    // After extended failures, log periodic warnings but keep retrying
                    if consecutive_failures % 10 == 0 {
                        warn!(
                            "User WebSocket still unavailable after {} attempts. Retrying in {:?}...",
                            consecutive_failures, reconnect_delay
                        );
                    }

                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Connect and run the WebSocket (returns on disconnect)
    async fn connect_and_run(&self) -> Result<()> {
        // Connect to WebSocket
        let (ws_stream, _) = connect_async(USER_WS_URL)
            .await
            .map_err(|e| BotError::WebSocket(format!("Connection failed: {}", e)))?;

        info!("User WebSocket connected");
        let _ = self.fill_tx.send(UserMessage::Connected);

        let (mut write, mut read) = ws_stream.split();

        // Send authentication message
        let auth_msg = AuthMessage {
            auth: AuthPayload {
                api_key: self.api_key.clone(),
                secret: self.secret.clone(),
                passphrase: self.passphrase.clone(),
            },
            markets: vec![], // Subscribe to all markets
            msg_type: "user".to_string(),
        };

        let auth_json = serde_json::to_string(&auth_msg)
            .map_err(|e| BotError::Json(e.to_string()))?;

        write
            .send(Message::Text(auth_json))
            .await
            .map_err(|e| BotError::WebSocket(format!("Failed to authenticate: {}", e)))?;

        debug!("User WebSocket auth message sent, waiting for response...");

        // Keepalive ping interval
        let mut ping_interval = interval(Duration::from_secs(WEBSOCKET_PING_INTERVAL_SEC));
        let mut authenticated = false;

        loop {
            tokio::select! {
                // Receive messages
                Some(msg_result) = read.next() => {
                    match msg_result {
                        Ok(msg) => {
                            // Log first message (auth response) at INFO level
                            if !authenticated {
                                if let Message::Text(ref text) = msg {
                                    info!("User WebSocket auth response: {}", &text[..text.len().min(500)]);
                                }
                                authenticated = true;
                                info!("User WebSocket authenticated successfully");
                            }
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
                    debug!("Sent user WebSocket ping");
                }

                else => {
                    return Err(BotError::WebSocket("Stream ended unexpectedly".to_string()));
                }
            }
        }
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, msg: Message) -> Result<()> {
        match msg {
            Message::Text(text) => {
                // Parse as generic JSON to see what we have
                let json_value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| BotError::Json(format!("Invalid JSON: {}", e)))?;

                // Check event_type or type field
                let event_type = json_value
                    .get("event_type")
                    .or_else(|| json_value.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match event_type.to_lowercase().as_str() {
                    "trade" => {
                        // Parse trade notification
                        let trade: TradeNotification = serde_json::from_value(json_value)
                            .map_err(|e| BotError::Json(format!("Failed to parse trade: {}", e)))?;

                        info!(
                            "Trade notification: {} {} {} @ {}",
                            trade.side, trade.size, trade.asset_id, trade.price
                        );

                        let _ = self.fill_tx.send(UserMessage::Trade(trade));
                    }
                    "order" | "order_update" => {
                        // Parse order update
                        let update: OrderUpdate = serde_json::from_value(json_value)
                            .map_err(|e| BotError::Json(format!("Failed to parse order update: {}", e)))?;

                        debug!("Order update: {} -> {}", update.order_id, update.status);

                        let _ = self.fill_tx.send(UserMessage::OrderUpdate(update));
                    }
                    "" => {
                        // Could be auth response or other message
                        debug!("User WS message: {}", &text[..text.len().min(200)]);
                    }
                    _ => {
                        debug!("Unknown user message type: {}", event_type);
                    }
                }
            }
            Message::Pong(_) => {
                debug!("Received user WebSocket pong");
            }
            Message::Close(frame) => {
                warn!("User WebSocket close frame received: {:?}", frame);
                return Err(BotError::WebSocket("Connection closed by server".to_string()));
            }
            _ => {
                debug!("Received other message type: {:?}", msg);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_trade_notification_to_fill_taker_crypto() {
        // Crypto market: fee_rate_bps=1000 → parabolic feeRate=0.25, exponent=2
        let trade = TradeNotification {
            id: "trade123".to_string(),
            taker_order_id: "order456".to_string(),
            market: "market789".to_string(),
            asset_id: "token_abc".to_string(),
            side: "BUY".to_string(),
            size: "100".to_string(),
            price: "0.50".to_string(),
            fee_rate_bps: "1000".to_string(), // crypto tier
            status: "MATCHED".to_string(),
            timestamp: "1704067200000".to_string(),
            trader_side: "TAKER".to_string(),
            maker_orders: vec![],
        };

        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.fill_id, "trade123");
        assert_eq!(fill.order_id, "order456");
        assert_eq!(fill.token_id, "token_abc");
        assert_eq!(fill.side, Side::Buy);
        assert_eq!(fill.price, dec!(0.50));
        assert_eq!(fill.size, dec!(100));
        // Parabolic: 100 * 0.50 * 0.25 * (0.50 * 0.50)^2 = 50 * 0.25 * 0.0625 = 0.78125
        assert_eq!(fill.fee, dec!(0.78125));
    }

    #[test]
    fn test_trade_notification_to_fill_taker_no_fees() {
        // Standard market: fee_rate_bps=0 → no fees
        let trade = TradeNotification {
            id: "trade_free".to_string(),
            taker_order_id: "order_free".to_string(),
            market: "market_std".to_string(),
            asset_id: "token_std".to_string(),
            side: "BUY".to_string(),
            size: "100".to_string(),
            price: "0.55".to_string(),
            fee_rate_bps: "0".to_string(),
            status: "MATCHED".to_string(),
            timestamp: "1704067200000".to_string(),
            trader_side: "TAKER".to_string(),
            maker_orders: vec![],
        };

        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.fee, dec!(0));
    }

    #[test]
    fn test_trade_notification_maker_order_id() {
        // When we're maker, our order ID is in maker_orders, not taker_order_id
        let trade = TradeNotification {
            id: "trade_maker".to_string(),
            taker_order_id: "other_persons_order".to_string(),
            market: "market".to_string(),
            asset_id: "token".to_string(),
            side: "SELL".to_string(),
            size: "50".to_string(),
            price: "0.80".to_string(),
            fee_rate_bps: "0".to_string(),
            status: "MATCHED".to_string(),
            timestamp: "1704067200".to_string(),
            trader_side: "MAKER".to_string(),
            maker_orders: vec![MakerOrderFill {
                order_id: "my_maker_order_id".to_string(),
                matched_amount: "50".to_string(),
                price: "0.80".to_string(),
            }],
        };

        // our_order_id() should return the maker order, not the taker
        assert_eq!(trade.our_order_id(), Some("my_maker_order_id"));
        
        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.order_id, "my_maker_order_id");
        assert_eq!(fill.side, Side::Sell);
        assert_eq!(fill.size, dec!(50));
        assert_eq!(fill.price, dec!(0.80));
        // Maker pays no fees
        assert_eq!(fill.fee, dec!(0));
    }

    #[test]
    fn test_trade_notification_sell() {
        let trade = TradeNotification {
            id: "trade_sell".to_string(),
            taker_order_id: "order_sell".to_string(),
            market: "market".to_string(),
            asset_id: "token".to_string(),
            side: "SELL".to_string(),
            size: "50".to_string(),
            price: "0.80".to_string(),
            fee_rate_bps: "0".to_string(),
            status: "MATCHED".to_string(),
            timestamp: "1704067200".to_string(),
            trader_side: "MAKER".to_string(),
            maker_orders: vec![MakerOrderFill {
                order_id: "order_sell".to_string(),
                matched_amount: "50".to_string(),
                price: "0.80".to_string(),
            }],
        };

        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.side, Side::Sell);
        assert_eq!(fill.size, dec!(50));
        assert_eq!(fill.price, dec!(0.80));
        assert_eq!(fill.fee, dec!(0));
    }
}
