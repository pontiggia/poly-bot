//! Binance WebSocket - real-time spot prices for BTC/ETH/SOL/XRP
//!
//! Connects to wss://stream.binance.com:9443/ws and subscribes to
//! miniTicker events for crypto assets. Used for:
//! - Adverse selection defense (detect spot moves before Polymarket)
//! - Momentum strategy (5-min trend direction)

use crate::constants::BINANCE_WS_URL;
use crate::state::spot_prices::{PriceHistory, SpotPriceState, SpotPriceUpdate};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;
use std::sync::Arc;
use tokio::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Symbols to subscribe to (lowercase, with "usdt" suffix)
const SYMBOLS: &[&str] = &["btcusdt", "ethusdt", "solusdt", "xrpusdt"];

/// Binance WebSocket client for spot price feeds
pub struct BinanceWebSocket {
    /// Shared spot price state (latest price per asset)
    spot_state: Arc<SpotPriceState>,
    /// Shared price history (ring buffer for momentum)
    price_history: Arc<PriceHistory>,
}

/// Binance miniTicker event
#[derive(Debug, Deserialize)]
struct MiniTicker {
    /// Event type (should be "24hrMiniTicker")
    #[serde(rename = "e")]
    _event_type: String,
    /// Symbol (e.g., "BTCUSDT")
    #[serde(rename = "s")]
    symbol: String,
    /// Close price
    #[serde(rename = "c")]
    close: String,
    /// Event time in milliseconds
    #[serde(rename = "E")]
    event_time: i64,
}

/// Subscription request
#[derive(Debug, serde::Serialize)]
struct SubscribeRequest {
    method: String,
    params: Vec<String>,
    id: u32,
}

impl BinanceWebSocket {
    /// Create a new Binance WebSocket client
    pub fn new(spot_state: Arc<SpotPriceState>, price_history: Arc<PriceHistory>) -> Self {
        Self {
            spot_state,
            price_history,
        }
    }

    /// Run with automatic reconnection
    pub async fn run(self: Arc<Self>) {
        let mut reconnect_delay = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(30);

        loop {
            info!("Connecting to Binance WebSocket: {}", BINANCE_WS_URL);

            match self.connect_and_run().await {
                Ok(_) => {
                    warn!("Binance WebSocket closed normally");
                    reconnect_delay = Duration::from_secs(1);
                }
                Err(e) => {
                    error!("Binance WebSocket error: {}", e);
                    warn!("Binance WS reconnecting in {:?}", reconnect_delay);
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Connect and run (returns on disconnect)
    async fn connect_and_run(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (ws_stream, _) = connect_async(BINANCE_WS_URL).await?;
        info!("Binance WebSocket connected");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to miniTicker for all symbols
        let params: Vec<String> = SYMBOLS.iter().map(|s| format!("{}@miniTicker", s)).collect();
        let sub_msg = SubscribeRequest {
            method: "SUBSCRIBE".to_string(),
            params,
            id: 1,
        };

        let sub_json = serde_json::to_string(&sub_msg)?;
        write.send(Message::Text(sub_json)).await?;
        info!("Binance WS subscribed to {} symbols", SYMBOLS.len());

        // Ping interval
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                Some(msg_result) = read.next() => {
                    match msg_result {
                        Ok(Message::Text(text)) => {
                            self.handle_message(&text);
                        }
                        Ok(Message::Ping(data)) => {
                            let _ = write.send(Message::Pong(data)).await;
                        }
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(frame)) => {
                            warn!("Binance WS close frame: {:?}", frame);
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(Box::new(e));
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        return Err(Box::new(e));
                    }
                }
                else => {
                    return Ok(());
                }
            }
        }
    }

    /// Handle an incoming message
    fn handle_message(&self, text: &str) {
        // Skip subscription confirmations ({"result":null,"id":1})
        if text.contains("\"result\"") {
            return;
        }

        // Parse miniTicker
        let ticker: MiniTicker = match serde_json::from_str(text) {
            Ok(t) => t,
            Err(_) => {
                debug!("Binance WS: non-ticker message: {}", &text[..text.len().min(100)]);
                return;
            }
        };

        // Parse close price
        let price = match Decimal::from_str(&ticker.close) {
            Ok(p) => p,
            Err(_) => return,
        };

        // Extract asset name: "BTCUSDT" -> "btc"
        let asset = ticker
            .symbol
            .to_lowercase()
            .strip_suffix("usdt")
            .unwrap_or(&ticker.symbol.to_lowercase())
            .to_string();

        let update = SpotPriceUpdate {
            symbol: asset.clone(),
            price,
            timestamp_ms: ticker.event_time,
        };

        // Update latest price
        self.spot_state.update(&update);

        // Record in history
        self.price_history
            .record(&asset, ticker.event_time, price);

        debug!("Binance: {} = ${}", asset, price);
    }
}
