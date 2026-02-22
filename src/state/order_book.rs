//! Order book state management with lock-free updates
//!
//! Maintains best bid/ask and full book depth for all subscribed markets

use crate::api::types::{PriceLevel, TokenId};
use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::sync::Arc;

/// Order book state for all markets (lock-free)
pub struct OrderBookState {
    /// Map of token_id -> book snapshot
    books: Arc<DashMap<TokenId, BookSnapshot>>,
}

/// Snapshot of an order book at a point in time
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    /// Token ID
    pub token_id: TokenId,
    /// Market ID (condition ID)
    pub market: String,
    /// Best bid levels
    pub bids: Vec<PriceLevel>,
    /// Best ask levels
    pub asks: Vec<PriceLevel>,
    /// Last update timestamp
    pub last_update: Option<i64>,
    /// Hash of book state (from WebSocket)
    pub hash: Option<String>,
    // Pre-parsed top-of-book values (populated on update, avoids repeated .parse())
    /// Best bid price as Decimal
    pub best_bid_dec: Option<Decimal>,
    /// Best bid size as Decimal
    pub best_bid_size_dec: Option<Decimal>,
    /// Best ask price as Decimal
    pub best_ask_dec: Option<Decimal>,
    /// Best ask size as Decimal
    pub best_ask_size_dec: Option<Decimal>,
}

impl OrderBookState {
    /// Create a new empty order book state
    pub fn new() -> Self {
        Self {
            books: Arc::new(DashMap::new()),
        }
    }

    /// Update order book from WebSocket message (full snapshot)
    pub fn update_book(
        &self,
        token_id: TokenId,
        market: String,
        mut bids: Vec<PriceLevel>,
        mut asks: Vec<PriceLevel>,
        timestamp: Option<i64>,
        hash: Option<String>,
    ) {
        // Sort bids descending by price (highest first)
        bids.sort_by(|a, b| {
            let price_a: Decimal = a.price.parse().unwrap_or(Decimal::ZERO);
            let price_b: Decimal = b.price.parse().unwrap_or(Decimal::ZERO);
            price_b.cmp(&price_a) // Descending
        });

        // Sort asks ascending by price (lowest first)
        asks.sort_by(|a, b| {
            let price_a: Decimal = a.price.parse().unwrap_or(Decimal::ZERO);
            let price_b: Decimal = b.price.parse().unwrap_or(Decimal::ZERO);
            price_a.cmp(&price_b) // Ascending
        });

        // Pre-parse top-of-book after sorting
        let best_bid_dec = bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_bid_size_dec = bids.first().and_then(|l| l.size.parse::<Decimal>().ok());
        let best_ask_dec = asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask_size_dec = asks.first().and_then(|l| l.size.parse::<Decimal>().ok());

        let snapshot = BookSnapshot {
            token_id: token_id.clone(),
            market,
            bids,
            asks,
            last_update: timestamp,
            hash,
            best_bid_dec,
            best_bid_size_dec,
            best_ask_dec,
            best_ask_size_dec,
        };

        self.books.insert(token_id, snapshot);
    }

    /// Update a single price level (incremental update from price_change)
    ///
    /// If size is "0", removes the level. Otherwise, updates or inserts.
    /// Maintains sorted order: bids descending by price, asks ascending.
    pub fn update_level(
        &self,
        token_id: &TokenId,
        market: String,
        side: &str,
        price: &str,
        size: &str,
        timestamp: Option<i64>,
        hash: Option<String>,
    ) {
        // Get or create the book
        let mut entry = self.books.entry(token_id.clone()).or_insert_with(|| {
            BookSnapshot {
                token_id: token_id.clone(),
                market: market.clone(),
                bids: Vec::new(),
                asks: Vec::new(),
                last_update: timestamp,
                hash: None,
                best_bid_dec: None,
                best_bid_size_dec: None,
                best_ask_dec: None,
                best_ask_size_dec: None,
            }
        });

        let book = entry.value_mut();
        book.last_update = timestamp;
        book.hash = hash;

        let price_decimal: Decimal = match price.parse() {
            Ok(p) => p,
            Err(_) => return,
        };
        let size_decimal: Decimal = match size.parse() {
            Ok(s) => s,
            Err(_) => return,
        };

        let levels = match side.to_uppercase().as_str() {
            "BUY" => &mut book.bids,
            "SELL" => &mut book.asks,
            _ => return,
        };

        // Find existing level at this price
        let existing_idx = levels.iter().position(|l| {
            l.price.parse::<Decimal>().map(|p| p == price_decimal).unwrap_or(false)
        });

        if size_decimal.is_zero() {
            // Remove the level
            if let Some(idx) = existing_idx {
                levels.remove(idx);
            }
        } else {
            let new_level = PriceLevel {
                price: price.to_string(),
                size: size.to_string(),
            };

            if let Some(idx) = existing_idx {
                // Update existing level
                levels[idx] = new_level;
            } else {
                // Insert in sorted order
                let insert_idx = if side.to_uppercase() == "BUY" {
                    // Bids: descending order (highest first)
                    levels.iter().position(|l| {
                        l.price.parse::<Decimal>().map(|p| p < price_decimal).unwrap_or(true)
                    }).unwrap_or(levels.len())
                } else {
                    // Asks: ascending order (lowest first)
                    levels.iter().position(|l| {
                        l.price.parse::<Decimal>().map(|p| p > price_decimal).unwrap_or(true)
                    }).unwrap_or(levels.len())
                };
                levels.insert(insert_idx, new_level);
            }
        }

        // Refresh cached top-of-book values
        book.best_bid_dec = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        book.best_bid_size_dec = book.bids.first().and_then(|l| l.size.parse::<Decimal>().ok());
        book.best_ask_dec = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        book.best_ask_size_dec = book.asks.first().and_then(|l| l.size.parse::<Decimal>().ok());
    }

    /// Get current book snapshot for a token
    pub fn get_book(&self, token_id: &TokenId) -> Option<BookSnapshot> {
        self.books.get(token_id).map(|entry| entry.value().clone())
    }

    /// Get best bid (highest buy price) — uses pre-parsed cache
    pub fn best_bid(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books
            .get(token_id)
            .and_then(|book| book.best_bid_dec)
    }

    /// Get best ask (lowest sell price) — uses pre-parsed cache
    pub fn best_ask(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books
            .get(token_id)
            .and_then(|book| book.best_ask_dec)
    }

    /// Get mid price (average of best bid and ask)
    pub fn mid_price(&self, token_id: &TokenId) -> Option<Decimal> {
        let bid = self.best_bid(token_id)?;
        let ask = self.best_ask(token_id)?;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Get spread (ask - bid)
    pub fn spread(&self, token_id: &TokenId) -> Option<Decimal> {
        let bid = self.best_bid(token_id)?;
        let ask = self.best_ask(token_id)?;
        Some(ask - bid)
    }

    /// Get spread in basis points (bps)
    pub fn spread_bps(&self, token_id: &TokenId) -> Option<u32> {
        let spread = self.spread(token_id)?;
        let mid = self.mid_price(token_id)?;
        if mid.is_zero() {
            return None;
        }
        Some(((spread / mid) * Decimal::from(10000)).to_u32()?)
    }

    /// Check if book has both sides (bid and ask)
    pub fn is_two_sided(&self, token_id: &TokenId) -> bool {
        self.books
            .get(token_id)
            .map(|book| !book.bids.is_empty() && !book.asks.is_empty())
            .unwrap_or(false)
    }

    /// Get total bid depth (size) at top level — uses pre-parsed cache
    pub fn bid_depth(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books
            .get(token_id)
            .and_then(|book| book.best_bid_size_dec)
    }

    /// Get total ask depth (size) at top level — uses pre-parsed cache
    pub fn ask_depth(&self, token_id: &TokenId) -> Option<Decimal> {
        self.books
            .get(token_id)
            .and_then(|book| book.best_ask_size_dec)
    }

    /// Get number of tracked token IDs
    pub fn num_markets(&self) -> usize {
        self.books.len()
    }

    /// Get all token IDs being tracked
    pub fn token_ids(&self) -> Vec<TokenId> {
        self.books.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Remove a token from tracking
    pub fn remove_token(&self, token_id: &TokenId) -> Option<BookSnapshot> {
        self.books.remove(token_id).map(|(_, v)| v)
    }

    /// Clear all books
    pub fn clear(&self) {
        self.books.clear();
    }
}

impl Default for OrderBookState {
    fn default() -> Self {
        Self::new()
    }
}

impl BookSnapshot {
    /// Get best bid price — uses pre-parsed cache
    pub fn best_bid(&self) -> Option<Decimal> {
        self.best_bid_dec
    }

    /// Get best ask price — uses pre-parsed cache
    pub fn best_ask(&self) -> Option<Decimal> {
        self.best_ask_dec
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Get spread
    pub fn spread(&self) -> Option<Decimal> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    /// Check if book is crossed (bid >= ask) - indicates error
    pub fn is_crossed(&self) -> bool {
        if let (Some(bid), Some(ask)) = (self.best_bid(), self.best_ask()) {
            bid >= ask
        } else {
            false
        }
    }

    /// Check if book has both sides
    pub fn is_two_sided(&self) -> bool {
        !self.bids.is_empty() && !self.asks.is_empty()
    }

    /// Total ask depth (shares) within `price_range` of the best ask
    ///
    /// E.g., if best ask is 0.48 and price_range is 0.02, sums all ask
    /// levels from 0.48 to 0.50 inclusive.
    pub fn ask_depth_within(&self, price_range: Decimal) -> Decimal {
        let best = match self.best_ask_dec {
            Some(p) => p,
            None => return Decimal::ZERO,
        };
        let ceiling = best + price_range;
        self.asks
            .iter()
            .filter_map(|l| {
                let price = l.price.parse::<Decimal>().ok()?;
                if price <= ceiling {
                    l.size.parse::<Decimal>().ok()
                } else {
                    None
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_book_update() {
        let state = OrderBookState::new();
        let token_id = "12345".to_string();

        let bids = vec![PriceLevel {
            price: "0.55".to_string(),
            size: "100".to_string(),
        }];

        let asks = vec![PriceLevel {
            price: "0.60".to_string(),
            size: "150".to_string(),
        }];

        state.update_book(
            token_id.clone(),
            "market123".to_string(),
            bids,
            asks,
            Some(1234567890),
            None,
        );

        assert_eq!(state.num_markets(), 1);
        assert!(state.is_two_sided(&token_id));
        assert_eq!(state.best_bid(&token_id), Some(Decimal::new(55, 2)));
        assert_eq!(state.best_ask(&token_id), Some(Decimal::new(60, 2)));
        assert_eq!(state.mid_price(&token_id), Some(Decimal::new(575, 3)));
        assert_eq!(state.spread(&token_id), Some(Decimal::new(5, 2)));
    }

    #[test]
    fn test_book_snapshot_crossed() {
        let snapshot = BookSnapshot {
            token_id: "12345".to_string(),
            market: "market123".to_string(),
            bids: vec![PriceLevel {
                price: "0.60".to_string(),
                size: "100".to_string(),
            }],
            asks: vec![PriceLevel {
                price: "0.55".to_string(),
                size: "100".to_string(),
            }],
            last_update: None,
            hash: None,
            best_bid_dec: Some(Decimal::new(60, 2)),
            best_bid_size_dec: Some(Decimal::from(100)),
            best_ask_dec: Some(Decimal::new(55, 2)),
            best_ask_size_dec: Some(Decimal::from(100)),
        };

        assert!(snapshot.is_crossed());
    }

    #[test]
    fn test_empty_book() {
        let state = OrderBookState::new();
        let token_id = "12345".to_string();

        assert_eq!(state.best_bid(&token_id), None);
        assert_eq!(state.best_ask(&token_id), None);
        assert_eq!(state.mid_price(&token_id), None);
        assert!(!state.is_two_sided(&token_id));
    }
}
