//! Polymarket Trading Bot
//!
//! A high-frequency trading bot for Polymarket prediction markets.
//!
//! ## Architecture
//!
//! - `api` - HTTP client and API types
//! - `signing` - EIP-712 order signing for Polymarket CTF Exchange
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
pub mod execution;
pub mod kill_switch;
pub mod ledger;
pub mod risk;
pub mod signing;
pub mod state;
pub mod strategy;
pub mod websocket;
pub mod paper;

pub use api::{ApiClient, ApiCredentials};
pub use bot::Bot;
pub use config::Config;
pub use error::{BotError, Result};
pub use kill_switch::KillSwitch;
pub use signing::{Order, OrderBuilder, OrderSigner};
pub use paper::{PaperConfig, PaperTrader};

