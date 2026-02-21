//! Polymarket Trading Bot
//!
//! A high-frequency trading bot for Polymarket prediction markets.
//!
//! ## Architecture
//!
//! - `api` - Types and market discovery for Polymarket APIs
//! - `exchange` - Exchange abstraction layer (SDK wrapper)
//! - `websocket` - Real-time market data
//! - `state` - Order book and market registry
//! - `ledger` - Authoritative state (orders, fills, positions, cash)
//! - `execution` - Order state machine and execution policies
//! - `strategy` - Pluggable trading strategies
//! - `risk` - Circuit breaker and risk limits

pub mod api;
pub mod bot;
pub mod config;
pub mod constants;
pub mod error;
pub mod exchange;
pub mod execution;
pub mod kill_switch;
pub mod ledger;
pub mod risk;
pub mod state;
pub mod strategy;
pub mod websocket;

pub use bot::Bot;
pub use config::Config;
pub use error::{BotError, Result};
pub use exchange::{Exchange, ExchangeError, SdkExchange};
pub use kill_switch::KillSwitch;

