//! API module - types and discovery for Polymarket CLOB and Gamma APIs
//!
//! This module provides:
//! - `discovery` - Market discovery from Gamma API
//! - `gamma` - Gamma API client for market discovery
//! - `types` - Core type definitions (PriceLevel, OrderBook, etc.)
//!
//! Note: HTTP client and endpoint wrappers have been replaced by the
//! polymarket-client-sdk in the exchange module.

pub mod discovery;
pub mod gamma;
pub mod types;

pub use discovery::{DiscoveredMarket, MarketDiscovery, MarketFilter, OutcomeType};
pub use gamma::{GammaClient, GammaEvent, GammaMarket};

// Re-export core types
pub use types::{
    Address, ConditionId, OrderBook, OrderId, OrderRequest, OrderResponse, OrderType, Outcome,
    PriceChange, PriceLevel, Side, SignedOrder, Signature, TokenId, TxHash,
};

