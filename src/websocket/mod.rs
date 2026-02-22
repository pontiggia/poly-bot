//! WebSocket connectivity for real-time market data
//!
//! Three WebSocket streams:
//! - Market WS: Order book updates for subscribed markets
//! - User WS: Trade notifications (fills, order status)
//! - Binance WS: Spot prices for BTC/ETH/SOL/XRP

pub mod binance;
pub mod market;
pub mod user;

pub use binance::BinanceWebSocket;
pub use market::{MarketWebSocket, MarketMessage, BookUpdateMessage, LevelUpdateMessage};
pub use user::{UserWebSocket, UserMessage, TradeNotification, OrderUpdate};

