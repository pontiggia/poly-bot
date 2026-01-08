# Polymarket Bot - Rust Architecture
## Maximum Efficiency Technical Stack

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              VPS (us-east-1)                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐         │
│  │   WebSocket     │    │    Decision     │    │     Order       │         │
│  │   Listener      │───▶│    Engine       │───▶│     Signer      │         │
│  │  (tokio-tung)   │    │   (Strategy)    │    │    (alloy)      │         │
│  └─────────────────┘    └─────────────────┘    └─────────────────┘         │
│          │                      │                      │                    │
│          ▼                      ▼                      ▼                    │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐         │
│  │   In-Memory     │    │    Parallel     │    │   HTTP/2 POST   │         │
│  │   State         │    │    Market       │    │   to CLOB       │         │
│  │  (DashMap)      │    │    Scanner      │    │   (reqwest)     │         │
│  └─────────────────┘    └─────────────────┘    └─────────────────┘         │
│          │                      │                      │                    │
│          └──────────────────────┴──────────────────────┘                    │
│                                 │                                           │
│                    ┌────────────▼────────────┐                              │
│                    │   External Price Feeds  │                              │
│                    │  (Binance WS, Coinbase) │                              │
│                    └─────────────────────────┘                              │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow

```
1. WebSocket receives market data (order book updates)
          ↓
2. State manager updates in-memory order book (DashMap)
          ↓
3. Decision engine analyzes for opportunities:
   - Mathematical arbitrage (YES + NO < threshold)
   - Temporal arbitrage (spot price vs market price)
   - Spread dislocation
          ↓
4. If opportunity found → construct order
          ↓
5. Sign order cryptographically (alloy, ~2-5ms)
          ↓
6. POST to CLOB API (HTTP/2, ~10-30ms)
          ↓
7. Monitor execution via User WebSocket
```

---

## Latency Budget

| Component | Target | Python Baseline | Improvement |
|-----------|--------|-----------------|-------------|
| WebSocket receive | 1-2ms | 1-2ms | - |
| JSON parsing (simd-json) | 0.1ms | 1-5ms | **10-50x** |
| Decision logic | 0.01ms | 0.1ms | **10x** |
| Order book operations | 0.16ms | 1ms | **6x** |
| Spread/mid calculations | 0.00007ms (70ns) | 0.1ms | **1400x** |
| Order construction | 0.1ms | 1ms | **10x** |
| Crypto signing (alloy) | 2-5ms | 1000ms | **200-500x** |
| HTTP POST (optimized) | 8-20ms | 50-100ms | **5-6x** |
| **TOTAL** | **12-30ms** | **1050-1100ms** | **35-90x** |

*Benchmarks based on polyfill-rs optimizations (321.6ms mean vs 409.3ms baseline = 21.4% improvement)*

---

## Cargo.toml Dependencies

```toml
[package]
name = "polymarket-bot"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full", "rt-multi-thread", "fs"] }

# WebSocket
tokio-tungstenite = { version = "0.21", features = ["native-tls"] }
futures-util = "0.3"

# HTTP client (optimized settings below)
reqwest = { version = "0.12", features = ["json", "rustls-tls", "gzip", "http2"] }
hyper = { version = "1.0", features = ["http2"] }

# JSON parsing (SIMD-optimized - 1.77x faster than serde_json)
simd-json = "0.13"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"  # Fallback + state persistence

# Ethereum signing (alloy - production-ready successor to ethers-rs)
# Full EIP-712 support for Polymarket CLOB order signing
alloy = { version = "0.9", features = ["signers", "signer-local", "sol-types", "primitives"] }

# Concurrent data structures (lock-free)
dashmap = "5.5"
crossbeam = "0.8"

# Parallel iteration
rayon = "1.8"

# Fixed-point arithmetic (avoids f64 for price calculations)
rust_decimal = "1.33"
rust_decimal_macros = "1.33"  # For dec!() macro

# DNS caching (eliminates redundant lookups)
trust-dns-resolver = "0.23"

# Buffer pooling (reduces allocation overhead)
bytes = "1.5"

# Date/time with timezone
chrono = { version = "0.4", features = ["serde"] }

# UUID for local order tracking
uuid = { version = "1", features = ["v4", "serde"] }

# Error handling
thiserror = "1.0"
anyhow = "1.0"

# Logging & tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }

# Configuration
config = "0.13"
dotenvy = "0.15"

# HMAC for API auth
hmac = "0.12"
sha2 = "0.10"
base64 = "0.21"
hex = "0.4"

# Rate limiting
governor = "0.6"  # Token bucket rate limiter

# Metrics (for health monitoring)
metrics = "0.21"
metrics-exporter-prometheus = "0.12"

# Histogram for latency tracking (p50/p95/p99)
hdrhistogram = "7.5"

[dev-dependencies]
tokio-test = "0.4"
mockall = "0.12"  # For mocking in tests

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
panic = "abort"
strip = true

[profile.release.package."*"]
opt-level = 3
```

---

## Core Module Structure

```
src/
├── main.rs                 # Entry point, runtime setup
├── config.rs               # Configuration management
├── error.rs                # Error types
├── kill_switch.rs          # Emergency stop mechanism
│
├── websocket/
│   ├── mod.rs
│   ├── market.rs           # Market WebSocket (order books)
│   ├── user.rs             # User WebSocket (trade notifications)
│   └── live_data.rs        # Live data WebSocket (activity)
│
├── api/
│   ├── mod.rs
│   ├── client.rs           # HTTP client with auth
│   ├── endpoints.rs        # CLOB REST endpoints
│   ├── types.rs            # Request/response types
│   └── rate_limiter.rs     # Rate limiting & backpressure
│
├── signing/
│   ├── mod.rs
│   ├── order.rs            # Order construction
│   ├── eip712.rs           # EIP-712 signing
│   └── auth.rs             # API authentication
│
├── ledger/                 # Authoritative state (truth source)
│   ├── mod.rs
│   ├── orders.rs           # OpenOrders: order_id → metadata + status
│   ├── fills.rs            # Fills: trade_id → fill record
│   ├── positions.rs        # Positions: token_id → shares, avg cost, pnl
│   ├── cash.rs             # Cash: available, reserved, total USDC
│   └── reconciliation.rs   # Periodic REST reconciliation loop
│
├── state/
│   ├── mod.rs
│   ├── order_book.rs       # In-memory order book (PRIMITIVES ONLY)
│   └── markets.rs          # MarketRegistry with capabilities
│
├── execution/              # State machine + policies
│   ├── mod.rs
│   ├── state_machine.rs    # Order lifecycle states
│   ├── executor.rs         # Order execution
│   ├── policy.rs           # ExecutionPolicy trait + TakerPolicy/MakerPolicy
│   ├── partial_fill.rs     # Partial fill handling & unwind
│   └── manager.rs          # Position management
│
├── strategy/               # Pluggable strategy system
│   ├── mod.rs
│   ├── traits.rs           # Strategy trait + OrderIntent + Urgency
│   ├── router.rs           # StrategyRouter (enable/disable/priority)
│   ├── context.rs          # StrategyContext (read-only view)
│   ├── arbitrage.rs        # MathArbStrategy implements Strategy (taker)
│   ├── maker_arb.rs        # MakerRebateArbStrategy (maker, Gabagool style)
│   ├── temporal.rs         # TemporalStrategy implements Strategy
│   ├── spread.rs           # SpreadStrategy implements Strategy
│   ├── edge_calculator.rs  # Dynamic edge requirements
│   └── feasibility.rs      # Pre-trade checks
│
├── risk/                   # Risk management
│   ├── mod.rs
│   ├── limits.rs           # Hard risk limits
│   ├── circuit_breaker.rs  # Pause trading on errors
│   └── metrics.rs          # Health metrics & latency tracking
│
└── feeds/
    ├── mod.rs
    ├── binance.rs          # Binance price feed
    └── coinbase.rs         # Coinbase price feed
```

### Architecture Principle: Base Bot + Pluggable Modules

The architecture separates **invariant core** from **changeable strategy logic**:

```
┌─────────────────────────────────────────────────────────────┐
│                    INVARIANT CORE (base_bot)                │
├─────────────────────────────────────────────────────────────┤
│  WebSocket ──► OrderBookState ──► StrategyRouter            │
│                     │                   │                   │
│              MarketRegistry        [Strategy A]             │
│                     │              [Strategy B]             │
│                     ▼              [Strategy C]             │
│                  Ledger                 │                   │
│                     │                   ▼                   │
│                     │            OrderIntent                │
│                     │                   │                   │
│                     ▼                   ▼                   │
│              ExecutionPolicy ◄── TakerPolicy/MakerPolicy    │
│                     │                                       │
│                     ▼                                       │
│                 Executor ──► CLOB API                       │
└─────────────────────────────────────────────────────────────┘
```

**To pivot from taker to maker:** Change ExecutionPolicy, not strategy.
**To add new strategy:** Implement Strategy trait, register with router.
**To support new market type:** Add to MarketRegistry capabilities.

---

## Core Type Definitions

```rust
// src/api/types.rs

use serde::{Deserialize, Serialize};

/// Market/Condition ID (0x-prefixed hex, 66 chars)
pub type ConditionId = String;

/// Token/Asset ID (large 256-bit integer as string)
pub type TokenId = String;

/// Order ID (0x-prefixed hex, 66 chars)
pub type OrderId = String;

/// Ethereum address (0x-prefixed hex, 42 chars)
pub type Address = String;

/// Transaction hash (0x-prefixed hex, 66 chars)
pub type TxHash = String;

/// Price level in order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: String,  // "0.48"
    pub size: String,   // "1234.56"
}

/// Order book snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub market: ConditionId,
    pub asset_id: TokenId,
    pub timestamp: String,
    pub hash: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub last_trade_price: String,
}

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    FAK,  // Fill And Kill
    GTC,  // Good Till Cancel
    FOK,  // Fill Or Kill
}

/// Signed order for submission
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOrder {
    pub salt: u64,
    pub maker: Address,
    pub signer: Address,
    pub taker: Address,
    pub token_id: TokenId,
    pub maker_amount: String,
    pub taker_amount: String,
    pub side: Side,
    pub expiration: String,
    pub nonce: String,
    pub fee_rate_bps: String,
    pub signature_type: u8,
    pub signature: String,
}

/// Order request body
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRequest {
    pub defer_exec: bool,
    pub order: SignedOrder,
    pub owner: String,
    pub order_type: OrderType,
}

/// Order response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub error_msg: String,
    pub order_id: OrderId,
    pub taking_amount: String,
    pub making_amount: String,
    pub status: String,
    pub transactions_hashes: Vec<TxHash>,
    pub success: bool,
}
```

---

## HTTP Client Configuration (Optimized)

```rust
// src/api/client.rs

use reqwest::Client;
use std::time::Duration;

/// Creates a latency-optimized HTTP client based on polyfill-rs benchmarks.
/// Achieves ~21% faster requests vs standard configuration.
pub fn create_optimized_client() -> Client {
    Client::builder()
        // Connection pooling - 5-20 persistent connections per host
        // 70% faster subsequent requests with warm connections
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(Duration::from_secs(90))

        // Force HTTP/2 - multiplexing, faster than HTTP/1.1
        // 512KB stream window optimized for ~469KB payloads
        .http2_prior_knowledge()
        .http2_initial_stream_window_size(512 * 1024)      // 512KB
        .http2_initial_connection_window_size(1024 * 1024) // 1MB
        .http2_adaptive_window(true)

        // Disable Nagle's algorithm - immediate packet transmission
        .tcp_nodelay(true)

        // TCP keepalive for connection persistence
        .tcp_keepalive(Duration::from_secs(60))

        // Aggressive timeout
        .timeout(Duration::from_millis(500))
        .connect_timeout(Duration::from_millis(200))

        // Enable compression
        .gzip(true)

        .build()
        .expect("Failed to create HTTP client")
}

/// Connection warming - call periodically to keep connections hot
/// Reduces first-request latency by maintaining TCP state
pub async fn warm_connection(client: &Client) -> Result<(), reqwest::Error> {
    client.get("https://clob.polymarket.com/time")
        .send()
        .await?;
    Ok(())
}
```

### DNS Caching (Integrated)

```rust
// src/api/dns.rs

use trust_dns_resolver::TokioAsyncResolver;
use trust_dns_resolver::config::*;
use std::sync::Arc;
use std::time::Duration;

/// Cached DNS resolver - eliminates redundant DNS lookups
/// Saves 1-5ms per request after first resolution
pub async fn create_cached_resolver() -> Arc<TokioAsyncResolver> {
    let mut opts = ResolverOpts::default();
    opts.cache_size = 256;
    opts.positive_min_ttl = Some(Duration::from_secs(300)); // Cache for 5 min
    opts.negative_min_ttl = Some(Duration::from_secs(60));

    Arc::new(
        TokioAsyncResolver::tokio(ResolverConfig::cloudflare(), opts)
            .expect("Failed to create DNS resolver")
    )
}
```

---

## WebSocket Connection (Market Data)

```rust
// src/websocket/market.rs

use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::json;

const MARKET_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

pub async fn connect_market_ws(
    token_ids: Vec<String>,
) -> Result<impl Stream<Item = OrderBook>, Error> {
    let (mut ws_stream, _) = connect_async(MARKET_WS_URL).await?;

    // Subscribe to markets
    let subscribe_msg = json!({
        "assets_ids": token_ids,
        "type": "market"
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await?;

    // Return stream of order book updates
    Ok(ws_stream.filter_map(|msg| async move {
        match msg {
            Ok(Message::Text(text)) => {
                // Use simd-json for 2-3x faster parsing
                let mut bytes = text.into_bytes();
                simd_json::from_slice(&mut bytes).ok()
            }
            Ok(Message::Ping(payload)) => {
                // Handle ping (connection keepalive)
                None
            }
            _ => None,
        }
    }))
}
```

---

## Order Signing (alloy)

```rust
// src/signing/order.rs

use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy::primitives::{B256, keccak256};
use alloy::sol_types::{Eip712Domain, SolStruct};

pub struct OrderSigner {
    signer: PrivateKeySigner,
    domain: Eip712Domain,
}

impl OrderSigner {
    pub fn new(private_key: &str) -> Result<Self, Error> {
        let signer: PrivateKeySigner = private_key.parse()?;

        // EIP-712 domain for Polymarket CLOB
        let domain = Eip712Domain::new(
            Some("Polymarket CTF Exchange".into()),
            Some("1".into()),
            Some(137.into()),  // Polygon
            Some(EXCHANGE_CONTRACT.into()),
            None,
        );

        Ok(Self { signer, domain })
    }

    pub async fn sign_order(&self, order: &Order) -> Result<String, Error> {
        // Construct EIP-712 typed data hash
        // alloy handles domain separator + struct hash internally
        let struct_hash = order.eip712_hash_struct();
        let domain_separator = self.domain.separator();

        // Compute EIP-712 signing hash: keccak256("\x19\x01" || domainSeparator || structHash)
        let digest = keccak256(
            [&[0x19, 0x01], domain_separator.as_slice(), struct_hash.as_slice()].concat()
        );

        // Sign (~2-5ms in Rust vs ~1000ms in Python)
        let signature = self.signer.sign_hash(&B256::from(digest)).await?;

        Ok(format!("0x{}", hex::encode(signature.as_bytes())))
    }
}
```

---

## In-Memory State (Lock-Free, Cache-Optimized, PRIMITIVES ONLY)

OrderBookState exposes **only primitives**. Strategy-specific logic (like arbitrage detection) lives in strategies, NOT in state.

```rust
// src/state/order_book.rs

use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU64, Ordering};

/// Fixed-point price type - integer-speed performance
/// Avoids f64 parsing overhead in hot paths
#[repr(C, align(64))]  // Cache-aligned for L1/L2 efficiency
#[derive(Debug, Clone, Copy)]
pub struct Price(Decimal);

impl Price {
    #[inline(always)]
    pub fn from_str(s: &str) -> Option<Self> {
        s.parse::<Decimal>().ok().map(Price)
    }

    #[inline(always)]
    pub fn as_decimal(&self) -> Decimal {
        self.0
    }
}

/// Cache-aligned price level for L1/L2 cache efficiency
#[repr(C, align(64))]
#[derive(Debug, Clone)]
pub struct OptimizedPriceLevel {
    pub price: Decimal,
    pub size: Decimal,
}

/// Thread-safe order book state with lock-free updates
///
/// IMPORTANT: This struct exposes ONLY primitive operations.
/// NO strategy-specific logic (like check_arbitrage) belongs here.
/// Strategies use these primitives to implement their own logic.
pub struct OrderBookState {
    // Concurrent hashmap - no mutex needed
    books: DashMap<TokenId, OrderBook>,

    // Atomic counter for updates (lock-free)
    update_count: AtomicU64,

    // Pre-allocated buffer pool to reduce allocation overhead
    buffer_pool: bytes::BytesMut,
}

impl OrderBookState {
    pub fn new() -> Self {
        Self {
            books: DashMap::with_capacity(1000), // Pre-allocate
            update_count: AtomicU64::new(0),
            buffer_pool: bytes::BytesMut::with_capacity(512 * 1024), // 512KB
        }
    }

    /// Lock-free update using compare-and-swap semantics
    pub fn update(&self, book: OrderBook) {
        self.books.insert(book.asset_id.clone(), book);
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get full order book for a token
    pub fn get(&self, token_id: &str) -> Option<OrderBook> {
        self.books.get(token_id).map(|r| r.clone())
    }

    // =========================================================================
    // PRIMITIVE ACCESSORS - These are the ONLY public methods for strategies
    // =========================================================================

    /// Best bid price and size (highest buy order)
    #[inline(always)]
    pub fn best_bid(&self, token_id: &str) -> Option<OptimizedPriceLevel> {
        let book = self.books.get(token_id)?;
        let level = book.bids.first()?;
        Some(OptimizedPriceLevel {
            price: level.price.parse().ok()?,
            size: level.size.parse().ok()?,
        })
    }

    /// Best ask price and size (lowest sell order)
    #[inline(always)]
    pub fn best_ask(&self, token_id: &str) -> Option<OptimizedPriceLevel> {
        let book = self.books.get(token_id)?;
        let level = book.asks.first()?;
        Some(OptimizedPriceLevel {
            price: level.price.parse().ok()?,
            size: level.size.parse().ok()?,
        })
    }

    /// Spread: (best_bid, best_ask, mid_price)
    /// Sub-microsecond calculation (70ns benchmark)
    #[inline(always)]
    pub fn spread(&self, token_id: &str) -> Option<(Decimal, Decimal, Decimal)> {
        let book = self.books.get(token_id)?;
        let best_bid = book.bids.first()?.price.parse::<Decimal>().ok()?;
        let best_ask = book.asks.first()?.price.parse::<Decimal>().ok()?;
        let mid = (best_bid + best_ask) / Decimal::TWO;
        Some((best_bid, best_ask, mid))
    }

    /// Mid price only
    #[inline(always)]
    pub fn mid_price(&self, token_id: &str) -> Option<Decimal> {
        self.spread(token_id).map(|(_, _, mid)| mid)
    }

    /// Depth snapshot: returns N levels from each side
    pub fn depth(&self, token_id: &str, levels: usize) -> Option<DepthSnapshot> {
        let book = self.books.get(token_id)?;
        Some(DepthSnapshot {
            bids: book.bids.iter().take(levels).map(|l| OptimizedPriceLevel {
                price: l.price.parse().unwrap_or_default(),
                size: l.size.parse().unwrap_or_default(),
            }).collect(),
            asks: book.asks.iter().take(levels).map(|l| OptimizedPriceLevel {
                price: l.price.parse().unwrap_or_default(),
                size: l.size.parse().unwrap_or_default(),
            }).collect(),
        })
    }

    /// Total size available at or better than a price
    pub fn size_at_price(&self, token_id: &str, side: Side, price: Decimal) -> Decimal {
        let book = match self.books.get(token_id) {
            Some(b) => b,
            None => return Decimal::ZERO,
        };

        let levels = match side {
            Side::Buy => &book.bids,
            Side::Sell => &book.asks,
        };

        levels.iter()
            .filter_map(|l| {
                let level_price: Decimal = l.price.parse().ok()?;
                let level_size: Decimal = l.size.parse().ok()?;
                match side {
                    Side::Buy if level_price >= price => Some(level_size),
                    Side::Sell if level_price <= price => Some(level_size),
                    _ => None,
                }
            })
            .sum()
    }

    /// Update count (for change detection)
    pub fn update_count(&self) -> u64 {
        self.update_count.load(Ordering::Relaxed)
    }

    // NO check_arbitrage() here!
    // Strategy-specific logic belongs in src/strategy/, not in state.
}

/// Depth snapshot for strategies to analyze
#[derive(Debug, Clone)]
pub struct DepthSnapshot {
    pub bids: Vec<OptimizedPriceLevel>,
    pub asks: Vec<OptimizedPriceLevel>,
}
```

---

## Ledger System (Authoritative State)

The ledger is the **single source of truth** for portfolio state. All other components read from it.

```rust
// src/ledger/mod.rs

use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::Arc;

/// Authoritative portfolio state - reconstructable from events
pub struct Ledger {
    pub orders: OpenOrders,
    pub fills: FillHistory,
    pub positions: Positions,
    pub cash: CashBalance,
}

// src/ledger/orders.rs

/// Order lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    CreatedLocal,      // Built locally, not yet signed
    Signed,            // Signed, ready to submit
    Submitted,         // POST sent, awaiting ack
    Acked,             // Got order_id back
    PartiallyFilled,   // Some fills received
    Filled,            // Fully filled
    CancelPending,     // Cancel request sent
    Cancelled,         // Confirmed cancelled
    Expired,           // TTL expired
    Rejected,          // API rejected
    Unknown,           // Requires reconciliation
}

#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub order_id: Option<OrderId>,
    pub local_id: Uuid,              // Our internal ID before ack
    pub state: OrderState,
    pub token_id: TokenId,
    pub side: Side,
    pub price: Decimal,
    pub original_size: Decimal,
    pub filled_size: Decimal,
    pub remaining_size: Decimal,
    pub created_at: DateTime<Utc>,
    pub last_update: DateTime<Utc>,
}

pub struct OpenOrders {
    by_order_id: DashMap<OrderId, TrackedOrder>,
    by_local_id: DashMap<Uuid, TrackedOrder>,
}

impl OpenOrders {
    /// Transition order state - enforces valid transitions only
    pub fn transition(&self, order_id: &OrderId, new_state: OrderState) -> Result<(), Error> {
        // Validate state transition is legal
        // Update timestamps
        // Emit event for logging
    }
}

// src/ledger/positions.rs

#[derive(Debug, Clone)]
pub struct Position {
    pub token_id: TokenId,
    pub shares: Decimal,           // Net shares held
    pub avg_cost: Decimal,         // Average entry price
    pub realized_pnl: Decimal,     // Closed P&L
    pub unrealized_pnl: Decimal,   // Mark-to-market
}

pub struct Positions {
    positions: DashMap<TokenId, Position>,
}

impl Positions {
    /// Update position ONLY from confirmed fills, never from "expected"
    pub fn apply_fill(&self, fill: &Fill) {
        // Atomic update to position
    }
}

// src/ledger/cash.rs

#[derive(Debug)]
pub struct CashBalance {
    pub available: AtomicDecimal,  // Can be used for new orders
    pub reserved: AtomicDecimal,   // Locked in open orders
    pub total: AtomicDecimal,      // available + reserved
}

impl CashBalance {
    /// Reserve cash when order is ACKED (not on submit)
    pub fn reserve(&self, amount: Decimal) -> Result<(), InsufficientFunds> {
        // Atomic compare-and-swap
    }

    /// Release reserved cash on cancel/reject
    pub fn release(&self, amount: Decimal) {
        // Atomic update
    }
}

// src/ledger/reconciliation.rs

/// Periodic reconciliation with REST API (every 2-5 minutes)
pub struct ReconciliationLoop {
    ledger: Arc<Ledger>,
    client: Arc<ApiClient>,
    interval: Duration,
}

impl ReconciliationLoop {
    pub async fn run(&self) {
        loop {
            tokio::time::sleep(self.interval).await;

            // 1. Fetch open orders from REST
            let remote_orders = self.client.get_open_orders().await?;

            // 2. Cross-check with local OpenOrders
            let discrepancies = self.find_discrepancies(&remote_orders);

            // 3. Handle discrepancies
            for d in discrepancies {
                match d {
                    Discrepancy::LocalNotRemote(order) => {
                        // Order vanished - mark as Unknown, investigate
                        self.ledger.orders.transition(&order.order_id, OrderState::Unknown)?;
                    }
                    Discrepancy::RemoteNotLocal(order) => {
                        // Orphan order - cancel it
                        self.client.cancel_order(&order.order_id).await?;
                    }
                    Discrepancy::StateMismatch { local, remote } => {
                        // Reconcile based on remote (source of truth)
                        self.repair_state(local, remote)?;
                    }
                }
            }

            // 4. Emit reconciliation report
            tracing::info!(
                discrepancies = discrepancies.len(),
                "Reconciliation complete"
            );
        }
    }
}
```

---

## Execution State Machine

Orders follow explicit state transitions. Invalid transitions are rejected.

```rust
// src/execution/state_machine.rs

/// Valid state transitions
impl OrderState {
    pub fn can_transition_to(&self, next: OrderState) -> bool {
        use OrderState::*;
        match (self, next) {
            // Normal flow
            (CreatedLocal, Signed) => true,
            (Signed, Submitted) => true,
            (Submitted, Acked) => true,
            (Submitted, Rejected) => true,
            (Acked, PartiallyFilled) => true,
            (Acked, Filled) => true,
            (Acked, CancelPending) => true,
            (PartiallyFilled, Filled) => true,
            (PartiallyFilled, CancelPending) => true,
            (CancelPending, Cancelled) => true,
            (CancelPending, Filled) => true,  // Fill came in before cancel processed

            // Error states
            (_, Unknown) => true,  // Can always go to Unknown
            (_, Expired) => true,  // TTL can expire from most states

            _ => false,
        }
    }
}

/// Invariants that must always hold
pub fn check_invariants(ledger: &Ledger) -> Vec<InvariantViolation> {
    let mut violations = vec![];

    // 1. Every fill must map to an order
    for fill in ledger.fills.iter() {
        if ledger.orders.get(&fill.order_id).is_none() {
            violations.push(InvariantViolation::OrphanFill(fill.clone()));
        }
    }

    // 2. Reserved cash must equal sum of open order values
    let expected_reserved: Decimal = ledger.orders
        .iter()
        .filter(|o| matches!(o.state, OrderState::Acked | OrderState::PartiallyFilled))
        .map(|o| o.remaining_size * o.price)
        .sum();

    if (ledger.cash.reserved.load() - expected_reserved).abs() > Decimal::new(1, 2) {
        violations.push(InvariantViolation::CashMismatch {
            expected: expected_reserved,
            actual: ledger.cash.reserved.load(),
        });
    }

    // 3. Position updates only from fills
    // (enforced by type system - Positions::apply_fill is only public method)

    violations
}
```

---

## Kill Switch (Day 1 Requirement)

```rust
// src/kill_switch.rs

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::fs;

/// Kill switch checked on every loop iteration
pub struct KillSwitch {
    killed: AtomicBool,
    file_path: PathBuf,
    env_var: String,
}

impl KillSwitch {
    pub fn new() -> Self {
        Self {
            killed: AtomicBool::new(false),
            file_path: PathBuf::from("/tmp/polybot_kill"),
            env_var: "POLYBOT_KILL".to_string(),
        }
    }

    /// Check all kill conditions - call this EVERY loop iteration
    #[inline]
    pub fn is_killed(&self) -> bool {
        // Fast path - already killed
        if self.killed.load(Ordering::Relaxed) {
            return true;
        }

        // Check env var
        if std::env::var(&self.env_var).is_ok() {
            self.killed.store(true, Ordering::SeqCst);
            return true;
        }

        // Check file (async in background, cached result)
        if self.file_path.exists() {
            self.killed.store(true, Ordering::SeqCst);
            return true;
        }

        false
    }

    /// Trigger kill switch programmatically
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
    }
}

/// Shutdown procedure when kill switch triggered
pub async fn graceful_shutdown(
    kill_switch: Arc<KillSwitch>,
    ledger: Arc<Ledger>,
    client: Arc<ApiClient>,
) {
    tracing::warn!("Kill switch activated - initiating graceful shutdown");

    // 1. Stop placing new orders
    kill_switch.kill();

    // 2. Cancel all open orders
    let open_orders: Vec<_> = ledger.orders.iter()
        .filter(|o| matches!(o.state,
            OrderState::Acked | OrderState::PartiallyFilled | OrderState::Submitted
        ))
        .collect();

    for order in open_orders {
        if let Some(order_id) = &order.order_id {
            match client.cancel_order(order_id).await {
                Ok(_) => tracing::info!(?order_id, "Cancelled order"),
                Err(e) => tracing::error!(?order_id, ?e, "Failed to cancel order"),
            }
        }
    }

    // 3. Print final state snapshot
    let snapshot = ledger.snapshot();
    tracing::info!(
        open_orders = snapshot.open_orders,
        positions = ?snapshot.positions,
        cash = ?snapshot.cash,
        realized_pnl = ?snapshot.realized_pnl,
        "Final state snapshot"
    );

    // 4. Persist state to disk for recovery
    let state_json = serde_json::to_string_pretty(&snapshot).unwrap();
    fs::write("shutdown_state.json", state_json).await.unwrap();
}
```

---

## Circuit Breaker (Backpressure & Error Handling)

```rust
// src/risk/circuit_breaker.rs

use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircuitState {
    Closed,      // Normal operation
    Open,        // Trading paused
    HalfOpen,    // Testing if we can resume
}

/// Error classification for circuit breaker decisions
/// Based on CLOB API error catalog research
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorType {
    /// Retryable errors - temporary conditions, don't count toward circuit breaker
    Retryable,
    /// Fatal errors - permanent rejection, count toward circuit breaker
    Fatal,
    /// Expected errors (e.g., FOK not filled) - normal operation, don't count
    Expected,
}

impl ErrorType {
    /// Classify API error codes for circuit breaker handling
    /// See 03-API-REFERENCE.md Section 8 for complete error catalog
    pub fn from_error_msg(error_msg: &str) -> Self {
        match error_msg {
            // Retryable - temporary conditions, can retry after delay
            "ORDER_DELAYED" | "MARKET_NOT_READY" => ErrorType::Retryable,

            // Expected - not failures, just unsuccessful (e.g., liquidity insufficient)
            "FOK_ORDER_NOT_FILLED_ERROR" => ErrorType::Expected,

            // Fatal - count toward circuit breaker threshold
            "INVALID_ORDER_MIN_TICK_SIZE" |
            "INVALID_ORDER_MIN_SIZE" |
            "INVALID_ORDER_DUPLICATED" |
            "INVALID_ORDER_NOT_ENOUGH_BALANCE" |
            "INVALID_ORDER_EXPIRATION" |
            "INVALID_ORDER_ERROR" |
            "EXECUTION_ERROR" |
            "INVALID_SIGNATURE" |
            "NONCE_ALREADY_USED" => ErrorType::Fatal,

            // Default unknown errors to Fatal (conservative approach)
            _ => ErrorType::Fatal,
        }
    }

    /// Should this error count toward circuit breaker reject threshold?
    pub fn counts_toward_circuit_breaker(&self) -> bool {
        matches!(self, ErrorType::Fatal)
    }

    /// Should we retry this error after a backoff?
    pub fn is_retryable(&self) -> bool {
        matches!(self, ErrorType::Retryable)
    }
}

pub struct CircuitBreaker {
    state: AtomicU8,
    reject_count: AtomicU32,
    last_reject: Mutex<Instant>,

    // Thresholds
    reject_threshold: u32,        // Open circuit after N rejects
    reset_timeout: Duration,      // Try half-open after this
    half_open_successes: u32,     // Close after N successes in half-open
}

impl CircuitBreaker {
    /// Record an API error - classifies and only counts Fatal errors toward threshold
    pub fn record_error(&self, error_msg: &str) {
        let error_type = ErrorType::from_error_msg(error_msg);

        if error_type.counts_toward_circuit_breaker() {
            let count = self.reject_count.fetch_add(1, Ordering::SeqCst);
            *self.last_reject.lock().unwrap() = Instant::now();

            tracing::warn!(
                error_msg,
                error_type = ?error_type,
                reject_count = count + 1,
                "Fatal API error recorded"
            );

            if count + 1 >= self.reject_threshold {
                self.open();
            }
        } else {
            tracing::debug!(
                error_msg,
                error_type = ?error_type,
                "Non-fatal error (not counted toward circuit breaker)"
            );
        }
    }

    /// Record a successful operation - resets reject count
    pub fn record_success(&self) {
        self.reject_count.store(0, Ordering::SeqCst);

        if self.state() == CircuitState::HalfOpen {
            // TODO: track consecutive successes, close if enough
        }
    }

    /// Check if trading is allowed
    pub fn is_trading_allowed(&self) -> bool {
        match self.state() {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => true,  // Allow test trades
            CircuitState::Open => {
                // Check if we should try half-open
                let elapsed = self.last_reject.lock().unwrap().elapsed();
                if elapsed > self.reset_timeout {
                    self.half_open();
                    true
                } else {
                    false
                }
            }
        }
    }

    fn open(&self) {
        self.state.store(CircuitState::Open as u8, Ordering::SeqCst);
        tracing::warn!("Circuit breaker OPEN - trading paused");
    }

    fn half_open(&self) {
        self.state.store(CircuitState::HalfOpen as u8, Ordering::SeqCst);
        tracing::info!("Circuit breaker HALF-OPEN - testing recovery");
    }
}

/// Conditions that trigger circuit breaker
pub async fn check_circuit_conditions(
    circuit: &CircuitBreaker,
    ws_connected: bool,
    reconciliation_ok: bool,
    reject_rate: f64,
) {
    // WebSocket disconnected
    if !ws_connected {
        circuit.open();
        tracing::error!("Circuit opened: WebSocket disconnected");
    }

    // Reconciliation found unknown state
    if !reconciliation_ok {
        circuit.open();
        tracing::error!("Circuit opened: Reconciliation failure");
    }

    // High reject rate (only Fatal errors count toward this rate)
    if reject_rate > 0.2 {  // >20% rejects
        circuit.open();
        tracing::error!(reject_rate, "Circuit opened: High reject rate");
    }
}
```

---

## Market Registry (With Capabilities for Pivot Support)

```rust
// src/state/markets.rs

use dashmap::DashMap;

/// Market kind - supports binary and multi-outcome
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarketKind {
    Binary,
    MultiOutcome { count: usize },
}

/// Token mapping - different for binary vs multi-outcome
#[derive(Debug, Clone)]
pub enum MarketTokens {
    Binary { yes: TokenId, no: TokenId },
    MultiOutcome { outcomes: Vec<(String, TokenId)> },
}

/// Fee model - varies by market type
/// VALIDATED FROM LIVE DATA (Research-1, Jan 2026):
/// - Standard markets: Zero fees (maker & taker)
/// - 15-min crypto: 0 maker + dynamic taker (up to ~300 bps at 50/50 odds)
/// - US Exchange: 0 maker + 1 bps taker
#[derive(Debug, Clone)]
pub enum FeeModel {
    Zero,                                           // Standard prediction markets
    Flat { maker_bps: u32, taker_bps: u32 },       // US Exchange (0, 1)
    Dynamic { maker_bps: u32, max_taker_bps: u32 }, // 15-min crypto (0, ~300)
}

/// Timing info for market lifecycle
#[derive(Debug, Clone)]
pub struct MarketTiming {
    pub duration: Duration,           // 15 min, 1 hour, etc.
    pub is_recurring: bool,           // New market every interval?
    pub resolution_delay: Duration,   // Time after close before resolution
}

#[derive(Debug, Clone)]
pub struct MarketInfo {
    // Identity
    pub condition_id: ConditionId,
    pub question: String,

    // Token mapping (supports binary and multi-outcome)
    pub tokens: MarketTokens,

    // Capabilities (for strategy filtering)
    pub kind: MarketKind,
    pub fee_model: FeeModel,
    pub timing: MarketTiming,

    // Constraints
    pub tick_size: Decimal,
    pub min_size: Decimal,
    pub max_size: Decimal,

    // Status
    pub is_active: bool,
    pub close_time: Option<DateTime<Utc>>,
    /// Whether market is accepting orders - check BEFORE submitting
    /// If false, orders will fail with MARKET_NOT_READY error
    pub accepting_orders: bool,
}

impl MarketInfo {
    /// Helper for binary markets
    pub fn yes_token(&self) -> Option<&TokenId> {
        match &self.tokens {
            MarketTokens::Binary { yes, .. } => Some(yes),
            _ => None,
        }
    }

    pub fn no_token(&self) -> Option<&TokenId> {
        match &self.tokens {
            MarketTokens::Binary { no, .. } => Some(no),
            _ => None,
        }
    }
}

/// Filter for querying markets by capability
#[derive(Default)]
pub struct MarketFilter {
    pub kind: Option<MarketKind>,
    pub max_duration: Option<Duration>,
    pub is_active: Option<bool>,
    pub has_zero_fees: Option<bool>,
    pub tag_id: Option<String>,        // Gamma API tag filter
    pub is_high_frequency: Option<bool>, // 15-min crypto markets (fee_rate_bps=1000)
}

pub struct MarketRegistry {
    markets: DashMap<ConditionId, MarketInfo>,
}

// =============================================================================
// HIGH-FREQUENCY MARKET IDENTIFICATION
// =============================================================================
//
// The API does NOT expose a direct "resolution_interval" or "market_type" field.
// Use these methods to identify 15-minute crypto markets programmatically:
//
// METHOD 1: Fee Rate Check (Most Reliable)
// -----------------------------------------
// GET https://clob.polymarket.com/fee-rate?token_id={token_id}
// - Returns fee_rate_bps: 1000 for 15-min crypto markets
// - Returns fee_rate_bps: 0 for standard markets
//
// METHOD 2: Gamma API Tag Filtering
// ----------------------------------
// Step 1: GET https://gamma-api.polymarket.com/tags (get crypto tag ID)
// Step 2: GET https://gamma-api.polymarket.com/events?tag_id=<crypto_tag_id>&active=true&closed=false
//
// OTHER HIGH-FREQUENCY MARKET TYPES:
// - Sports game bets: Use tag_id=100639 (game bets vs futures)
// - Automated sports: Use /sports endpoint
// - Short resolution: Check endDate field for markets closing soon
// =============================================================================

impl MarketRegistry {
    /// Startup validation - must pass before trading
    pub async fn initialize(client: &ApiClient) -> Result<Self, Error> {
        let registry = Self {
            markets: DashMap::new(),
        };

        // Fetch all markets
        let markets = client.get_markets().await?;

        for market in markets {
            let info = MarketInfo::from_api(market)?;
            registry.markets.insert(info.condition_id.clone(), info);
        }

        tracing::info!(
            market_count = registry.markets.len(),
            "Market registry initialized"
        );

        Ok(registry)
    }

    /// Filter markets by capabilities (strategies use this, not hardcoded logic)
    pub fn filter(&self, filter: &MarketFilter) -> Vec<MarketInfo> {
        self.markets.iter()
            .filter(|m| {
                if let Some(kind) = &filter.kind {
                    if m.kind != *kind { return false; }
                }
                if let Some(max_dur) = filter.max_duration {
                    if m.timing.duration > max_dur { return false; }
                }
                if let Some(active) = filter.is_active {
                    if m.is_active != active { return false; }
                }
                if let Some(zero_fees) = filter.has_zero_fees {
                    let is_zero = matches!(m.fee_model, FeeModel::Zero);
                    if is_zero != zero_fees { return false; }
                }
                true
            })
            .map(|m| m.clone())
            .collect()
    }

    /// Check if market is tradeable (active AND accepting orders)
    pub fn is_tradeable(&self, condition_id: &ConditionId) -> bool {
        self.markets.get(condition_id)
            .map(|m| m.is_active && m.accepting_orders)
            .unwrap_or(false)
    }

    /// Validate order against market rules
    pub fn validate_order(
        &self,
        condition_id: &ConditionId,
        price: Decimal,
        size: Decimal,
    ) -> Result<(), ValidationError> {
        let market = self.markets.get(condition_id)
            .ok_or(ValidationError::UnknownMarket)?;

        // Check tick size
        if price % market.tick_size != Decimal::ZERO {
            return Err(ValidationError::InvalidTickSize);
        }

        // Check size limits
        if size < market.min_size {
            return Err(ValidationError::BelowMinSize);
        }
        if size > market.max_size {
            return Err(ValidationError::AboveMaxSize);
        }

        // Check market is accepting orders
        if !market.accepting_orders {
            return Err(ValidationError::MarketNotAcceptingOrders);
        }

        // Check market still open
        if let Some(close_time) = market.close_time {
            if Utc::now() > close_time {
                return Err(ValidationError::MarketClosed);
            }
        }

        Ok(())
    }
}
```

---

## Strategy Trait + OrderIntent (Pluggable Strategy System)

Strategies output **intents** (what they want), not orders (how to execute). This enables:
- Pivot from taker → maker by changing ExecutionPolicy
- Multiple strategies running simultaneously via StrategyRouter
- Clean separation between "what" and "how"

```rust
// src/strategy/traits.rs

use rust_decimal::Decimal;
use std::time::Duration;

/// What a strategy wants to do (not HOW to do it)
#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub market_id: ConditionId,
    pub token_id: TokenId,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub urgency: Urgency,
    pub reason: String,  // For logging/debugging
}

/// How urgently the strategy wants this executed
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Urgency {
    Immediate,  // Cross the spread, use FOK/FAK
    Normal,     // Could be taker or maker
    Passive,    // Post only, use GTC
}

/// Read-only context passed to strategies
pub struct StrategyContext<'a> {
    pub books: &'a OrderBookState,
    pub ledger: &'a Ledger,
    pub registry: &'a MarketRegistry,
    pub clock: Instant,
}

/// All strategies implement this trait
pub trait Strategy: Send + Sync {
    /// Unique name for this strategy
    fn name(&self) -> &str;

    /// Which markets this strategy wants updates for
    fn subscribed_markets(&self) -> Vec<ConditionId>;

    /// Called when order book updates - return intents
    fn on_book_update(
        &self,
        market_id: &ConditionId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called when our order fills
    fn on_fill(
        &self,
        fill: &Fill,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called periodically (e.g., every 100ms)
    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        vec![]  // Default: no action
    }

    /// Called on shutdown - return intents to close positions
    fn on_shutdown(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        vec![]  // Default: no action
    }
}
```

---

## Strategy Router (Multiple Strategies)

```rust
// src/strategy/router.rs

pub struct StrategyRouter {
    strategies: Vec<Box<dyn Strategy>>,
    enabled: DashMap<String, bool>,
    priority: Vec<String>,  // Higher = more priority
}

impl StrategyRouter {
    pub fn new() -> Self {
        Self {
            strategies: vec![],
            enabled: DashMap::new(),
            priority: vec![],
        }
    }

    pub fn register(&mut self, strategy: Box<dyn Strategy>) {
        let name = strategy.name().to_string();
        self.strategies.push(strategy);
        self.enabled.insert(name.clone(), true);
        self.priority.push(name);
    }

    pub fn enable(&self, name: &str) {
        self.enabled.insert(name.to_string(), true);
    }

    pub fn disable(&self, name: &str) {
        self.enabled.insert(name.to_string(), false);
    }

    /// Route book update to relevant strategies
    pub fn on_book_update(
        &self,
        market_id: &ConditionId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        let mut all_intents = vec![];

        for strategy in &self.strategies {
            let name = strategy.name();
            if !self.is_enabled(name) {
                continue;
            }
            if !strategy.subscribed_markets().contains(market_id) {
                continue;
            }

            let intents = strategy.on_book_update(market_id, ctx);
            all_intents.extend(intents);
        }

        // Resolve conflicts (e.g., two strategies want same market)
        self.resolve_conflicts(all_intents)
    }

    fn resolve_conflicts(&self, intents: Vec<OrderIntent>) -> Vec<OrderIntent> {
        // For now: first strategy wins (by priority order)
        // Future: capital allocation, risk limits per strategy
        intents
    }
}
```

---

## Execution Policy (Taker vs Maker)

The ExecutionPolicy converts OrderIntents into actual CLOB orders. Different policies for different modes.

```rust
// src/execution/policy.rs

/// Converts OrderIntent → CLOB order parameters
pub trait ExecutionPolicy: Send + Sync {
    fn name(&self) -> &str;

    /// Convert intent to concrete order params
    fn to_order_params(&self, intent: &OrderIntent) -> OrderParams;

    /// What to do on partial fill
    fn on_partial_fill(&self, intent: &OrderIntent, filled: Decimal) -> PartialFillAction;
}

#[derive(Debug, Clone)]
pub struct OrderParams {
    pub order_type: OrderType,  // FOK, FAK, GTC
    pub price: Decimal,
    pub size: Decimal,
    pub post_only: bool,
}

pub enum PartialFillAction {
    KeepRemainder,      // Let rest fill (maker)
    CancelRemainder,    // Cancel unfilled (taker)
    UnwindFilled,       // Sell back what filled (arb)
}

/// Taker policy: aggressive, immediate execution
pub struct TakerPolicy {
    pub unwind_partials: bool,
}

impl ExecutionPolicy for TakerPolicy {
    fn name(&self) -> &str { "taker" }

    fn to_order_params(&self, intent: &OrderIntent) -> OrderParams {
        OrderParams {
            order_type: match intent.urgency {
                Urgency::Immediate => OrderType::FOK,
                _ => OrderType::FAK,
            },
            price: intent.price,
            size: intent.size,
            post_only: false,
        }
    }

    fn on_partial_fill(&self, _: &OrderIntent, _: Decimal) -> PartialFillAction {
        if self.unwind_partials {
            PartialFillAction::UnwindFilled
        } else {
            PartialFillAction::CancelRemainder
        }
    }
}

/// Maker policy: passive, post liquidity
pub struct MakerPolicy {
    pub post_only: bool,
}

impl ExecutionPolicy for MakerPolicy {
    fn name(&self) -> &str { "maker" }

    fn to_order_params(&self, intent: &OrderIntent) -> OrderParams {
        OrderParams {
            order_type: OrderType::GTC,
            price: intent.price,
            size: intent.size,
            post_only: self.post_only,
        }
    }

    fn on_partial_fill(&self, _: &OrderIntent, _: Decimal) -> PartialFillAction {
        PartialFillAction::KeepRemainder
    }
}
```

---

## Strategy: Mathematical Arbitrage (Implements Strategy Trait)

```rust
// src/strategy/arbitrage.rs

use crate::strategy::traits::{Strategy, OrderIntent, Urgency, StrategyContext};
use crate::strategy::edge_calculator::EdgeCalculator;

pub struct MathArbStrategy {
    edge_calc: EdgeCalculator,
    max_position: Decimal,
    subscribed: Vec<ConditionId>,
}

impl Strategy for MathArbStrategy {
    fn name(&self) -> &str { "math_arb" }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        self.subscribed.clone()
    }

    /// Returns OrderIntents for arb opportunities - does NOT execute directly
    fn on_book_update(
        &self,
        market_id: &ConditionId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        let market = match ctx.registry.markets.get(market_id) {
            Some(m) => m,
            None => return vec![],
        };

        // Only binary markets
        let (yes_token, no_token) = match &market.tokens {
            MarketTokens::Binary { yes, no } => (yes, no),
            _ => return vec![],
        };

        // Get order book data using PRIMITIVES (not check_arbitrage)
        let yes_ask = match ctx.books.best_ask(yes_token) {
            Some(a) => a,
            None => return vec![],
        };
        let no_ask = match ctx.books.best_ask(no_token) {
            Some(a) => a,
            None => return vec![],
        };

        // Calculate dynamic edge requirement
        let required_edge = self.edge_calc.calculate(
            ctx.books,
            yes_token,
            no_token,
            self.max_position,
        );

        let combined = yes_ask.price + no_ask.price;
        let actual_edge = Decimal::ONE - combined;

        // No opportunity
        if actual_edge < required_edge {
            return vec![];
        }

        // Calculate size (min of both sides)
        let size = yes_ask.size.min(no_ask.size).min(self.max_position);

        // Return TWO intents - execution layer handles them
        vec![
            OrderIntent {
                market_id: market_id.clone(),
                token_id: yes_token.clone(),
                side: Side::Buy,
                price: yes_ask.price,
                size,
                urgency: Urgency::Immediate,  // Taker policy will use FOK/FAK
                reason: format!("arb_yes_leg edge={}", actual_edge),
            },
            OrderIntent {
                market_id: market_id.clone(),
                token_id: no_token.clone(),
                side: Side::Buy,
                price: no_ask.price,
                size,
                urgency: Urgency::Immediate,
                reason: format!("arb_no_leg edge={}", actual_edge),
            },
        ]
    }

    fn on_fill(&self, fill: &Fill, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Partial fill handling is done by ExecutionPolicy, not strategy
        // Strategy just observes fills for P&L tracking
        vec![]
    }
}

impl MathArbStrategy {
    pub fn new(edge_calc: EdgeCalculator, max_position: Decimal) -> Self {
        Self {
            edge_calc,
            max_position,
            subscribed: vec![],
        }
    }

    /// Subscribe to markets matching filter
    pub fn subscribe(&mut self, registry: &MarketRegistry) {
        let filter = MarketFilter {
            kind: Some(MarketKind::Binary),
            max_duration: Some(Duration::from_secs(15 * 60)),  // 15 min
            is_active: Some(true),
            ..Default::default()
        };

        self.subscribed = registry.filter(&filter)
            .into_iter()
            .map(|m| m.condition_id)
            .collect();
    }
}
```

---

## Strategy: Maker Rebate Arbitrage (Gabagool Strategy)

This strategy uses the same math arbitrage logic but executes as a **maker** to capture rebates instead of paying fees.

```rust
// src/strategy/maker_arb.rs

use crate::strategy::traits::{Strategy, OrderIntent, Urgency, StrategyContext};
use crate::strategy::edge_calculator::EdgeCalculator;

/// Maker-based arbitrage strategy (Gabagool approach)
///
/// Key differences from MathArbStrategy:
/// - Uses Urgency::Passive → MakerPolicy converts to GTC orders
/// - Zero fees on 15-min crypto markets
/// - Receives rebates from taker fee pool
/// - Lower fill rate, but higher profit per filled trade
pub struct MakerRebateArbStrategy {
    edge_calc: EdgeCalculator,
    max_position: Decimal,
    subscribed: Vec<ConditionId>,

    // Maker-specific settings
    price_offset: Decimal,    // How far inside the spread to post (e.g., 0.01)
    min_rebate_edge: Decimal, // Minimum edge after accounting for no fees
}

impl Strategy for MakerRebateArbStrategy {
    fn name(&self) -> &str { "maker_rebate_arb" }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        self.subscribed.clone()
    }

    fn on_book_update(
        &self,
        market_id: &ConditionId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        let market = match ctx.registry.markets.get(market_id) {
            Some(m) => m,
            None => return vec![],
        };

        // Only 15-min crypto markets (where maker rebates apply)
        if !matches!(market.fee_model, FeeModel::Dynamic { .. }) {
            return vec![];
        }

        let (yes_token, no_token) = match &market.tokens {
            MarketTokens::Binary { yes, no } => (yes, no),
            _ => return vec![],
        };

        // Get best ask prices
        let yes_ask = match ctx.books.best_ask(yes_token) {
            Some(a) => a,
            None => return vec![],
        };
        let no_ask = match ctx.books.best_ask(no_token) {
            Some(a) => a,
            None => return vec![],
        };

        // For maker strategy, we need less edge because we pay zero fees
        // and receive rebates. Edge calculation is simpler:
        let combined = yes_ask.price + no_ask.price;
        let actual_edge = Decimal::ONE - combined;

        // Maker arb is profitable even with smaller edges (no fees!)
        // Minimum edge just needs to cover slippage risk
        if actual_edge < self.min_rebate_edge {
            return vec![];
        }

        let size = yes_ask.size.min(no_ask.size).min(self.max_position);

        // Post limit orders slightly inside the spread
        // This increases fill probability while remaining maker
        let yes_price = yes_ask.price - self.price_offset;
        let no_price = no_ask.price - self.price_offset;

        vec![
            OrderIntent {
                market_id: market_id.clone(),
                token_id: yes_token.clone(),
                side: Side::Buy,
                price: yes_price,
                size,
                urgency: Urgency::Passive,  // MakerPolicy → GTC
                reason: format!("maker_arb_yes edge={}", actual_edge),
            },
            OrderIntent {
                market_id: market_id.clone(),
                token_id: no_token.clone(),
                side: Side::Buy,
                price: no_price,
                size,
                urgency: Urgency::Passive,  // MakerPolicy → GTC
                reason: format!("maker_arb_no edge={}", actual_edge),
            },
        ]
    }

    fn on_fill(&self, fill: &Fill, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // When one leg fills, we may need to adjust the other leg
        // to maintain delta-neutral position
        //
        // Options:
        // 1. Cancel other leg and re-quote closer to market
        // 2. Wait for organic fill (simpler, may leave unhedged)
        // 3. Convert unfilled leg to taker (hybrid approach)
        vec![]
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Periodically check for stale orders that should be re-quoted
        // if the market has moved significantly
        vec![]
    }
}

impl MakerRebateArbStrategy {
    pub fn new(max_position: Decimal) -> Self {
        Self {
            edge_calc: EdgeCalculator::default(),
            max_position,
            subscribed: vec![],
            price_offset: Decimal::new(1, 2),     // 0.01 inside spread
            min_rebate_edge: Decimal::new(1, 2),  // 1% min edge (no fees to cover)
        }
    }

    /// Subscribe to 15-min crypto markets only (where rebates apply)
    pub fn subscribe(&mut self, registry: &MarketRegistry) {
        let filter = MarketFilter {
            kind: Some(MarketKind::Binary),
            is_active: Some(true),
            is_high_frequency: Some(true),  // 15-min crypto only
            ..Default::default()
        };

        self.subscribed = registry.filter(&filter)
            .into_iter()
            .map(|m| m.condition_id)
            .collect();
    }
}
```

### Key Differences: Taker vs Maker Arb

| Aspect | MathArbStrategy (Taker) | MakerRebateArbStrategy (Maker) |
|--------|------------------------|-------------------------------|
| Order Type | FOK/FAK (immediate) | GTC (passive) |
| Fees | Pay ~3% at 50/50 | Zero |
| Rebates | None | Yes (from fee pool) |
| Urgency | Immediate | Passive |
| Min Edge Required | ~3% (to cover fees) | ~1% (just slippage) |
| Fill Rate | High (~80%+) | Lower (~40-60%) |
| Capital Locked | Brief (fast cycle) | Longer (wait for fills) |

### Running Both Strategies Together

```rust
// In main.rs or strategy setup
let mut router = StrategyRouter::new();

// Register both strategies
router.register(Box::new(MathArbStrategy::new(edge_calc.clone(), max_pos)));
router.register(Box::new(MakerRebateArbStrategy::new(max_pos)));

// Taker strategy gets priority (faster execution)
router.set_priority("math_arb", 10);
router.set_priority("maker_rebate_arb", 5);

// Can run both simultaneously - they target different edge thresholds
// Taker catches large edges quickly
// Maker catches smaller edges passively for rebates
```

---

## Optimization Checklist

### Tier 1: Must Implement (Core Latency Gains)
- [ ] Rust for all components (25-70x faster than Python)
- [ ] alloy for signing (modern ethers-rs successor)
- [ ] HTTP/2 with connection pooling (5-20 connections/host)
- [ ] simd-json for parsing (1.77x faster than serde_json)
- [ ] TCP_NODELAY (disable Nagle's algorithm)
- [ ] Fixed-point arithmetic with rust_decimal (avoid f64)

### Tier 2: Strongly Recommended (polyfill-rs optimizations)
- [ ] DashMap for concurrent state (no mutex)
- [ ] DNS caching with trust-dns-resolver (saves 1-5ms/request)
- [ ] Connection warming (periodic /time requests)
- [ ] HTTP/2 stream windows (512KB optimized for payload size)
- [ ] Pre-allocated buffers with bytes crate (reduce allocation)
- [ ] Cache-aligned data structures (#[repr(C, align(64))])
- [ ] Lock-free updates (compare-and-swap operations)

### Tier 3: Advanced
- [ ] rayon for parallel market scanning
- [ ] crossbeam ArrayQueue for order passing
- [ ] io_uring for async I/O (Linux)
- [ ] Buffer pooling for zero-allocation hot paths
- [ ] CPU affinity for critical threads

### Benchmark Targets (based on polyfill-rs)
| Operation | Target | Benchmark |
|-----------|--------|-----------|
| Order book update (1000 ops) | < 160µs | 159.6µs ± 32µs |
| Spread/mid calculation | < 100ns | 70ns ± 77ns |
| JSON parsing (480KB) | < 2.5ms | ~2.3ms |
| Full API request | < 350ms | 321.6ms ± 92.9ms |

---

## VPS Configuration

### Recommended Specs
| Spec | Minimum | Recommended |
|------|---------|-------------|
| CPU | 2 cores | 4 cores |
| RAM | 2GB | 4GB |
| Storage | 20GB SSD | 40GB NVMe |
| Network | 1Gbps | 1Gbps+ |
| Latency | <20ms | <10ms |
| Cost | ~$20/mo | ~$50/mo |

### Recommended Providers (Priority Order)
1. **AWS us-east-1** (N. Virginia) - Most crypto infrastructure
2. **GCP us-east4** - Good alternative
3. **Hetzner US East** (Ashburn) - Budget option
4. **Vultr New Jersey** - Budget option

### Linux Kernel Tuning (/etc/sysctl.conf)

```bash
# Network buffers
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.ipv4.tcp_rmem = 4096 87380 16777216
net.ipv4.tcp_wmem = 4096 65536 16777216

# Latency reduction
net.ipv4.tcp_nodelay = 1
net.ipv4.tcp_low_latency = 1

# Connection handling
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.core.netdev_max_backlog = 65535

# Keepalive
net.ipv4.tcp_keepalive_time = 60
net.ipv4.tcp_keepalive_intvl = 10
net.ipv4.tcp_keepalive_probes = 6

# Socket reuse
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15

# File descriptors
fs.file-max = 2097152
```

### File Descriptor Limits (/etc/security/limits.conf)

```bash
*         soft    nofile      1048576
*         hard    nofile      1048576
```
