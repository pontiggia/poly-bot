//! State management - order books, market registry, and spot prices
//!
//! Maintains real-time order book state with lock-free updates

pub mod order_book;
pub mod spot_prices;

pub use order_book::{OrderBookState, BookSnapshot};
pub use spot_prices::{PriceHistory, SpotPriceState, SpotPriceUpdate};
