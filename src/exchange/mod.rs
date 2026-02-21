//! Exchange abstraction layer
//!
//! This module provides a trait-based abstraction over exchange operations,
//! allowing strategies to remain decoupled from the underlying SDK.
//!
//! ## Architecture
//!
//! - `Exchange` trait: Core abstraction for order operations
//! - `SdkExchange`: Implementation using the official polymarket-client-sdk
//! - `ExchangeError`: Error types with retry classification

mod errors;
mod sdk_exchange;
mod traits;
mod types;

pub use errors::ExchangeError;
pub use sdk_exchange::SdkExchange;
pub use traits::{Exchange, ExchangeOrderParams, ExchangeOrderType, ExchangeResult};
pub use types::{DomainOrder, OrderId, OrderStatus};
