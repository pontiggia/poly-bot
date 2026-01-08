# Polymarket Bot - Master Implementation Plan

## Rust High-Frequency Trading Bot

---

## 🚀 Current Status: Phase 9 Complete - PROFITABLE (Updated: 2026-01-08)

> **See [STATE-OF-THE-BOT.md](../STATE-OF-THE-BOT.md) for the authoritative current behavior documentation.**

### ✅ Completed Phases

- **Phase 0:** Wallet Setup - Trading wallet funded with API credentials
- **Phase 1:** Foundation + Safety - Project structure, types, kill switch (8 tests ✅)
- **Phase 2:** WebSocket + State - Live market data streaming working
- **Phase 3:** HTTP Client & Authentication - REST API + EIP-712 signing (25 tests ✅)
- **Phase 4:** Ledger & State Machine - Authoritative portfolio state tracking (56 tests ✅)
- **Phase 5:** Risk & Circuit Breaker - Safety systems complete (75 tests ✅)
- **Phase 6:** Strategy Framework - Traits, router, executor, policies (94 tests ✅)
- **Event-Driven Refactor:** tokio::select! architecture, <1ms latency
- **Phase 7:** Math Arb Strategy (MVP) - Market pairs, edge calculator, MathArbStrategy (115 tests ✅)
- **Phase 7.5:** API Fix & Market Discovery - Gamma API, slug-based discovery (136 tests ✅)
- **Phase 8:** Wire Execution Policies - Full execution pipeline connected (136 tests ✅)
- **Phase 9:** Integration & Live Testing - **PROFITABLE** +$1.10 (3.24%) (154 tests ✅)

### ✅ Post-Phase 9 Fixes (2026-01-08)

- **Minimum Order Value:** Dynamic share sizing to meet $1.00 API minimum
- **Parallel Execution:** Maker arb legs submitted via `tokio::join!`
- **Critical Error Handling:** Circuit breaker trips on orphaned positions
- **Fill Detection Fix:** Correct size matching with `to_fill_for_order()`

### 📊 Metrics

- **Tests Passing:** 154/154 unit tests
- **Build Time:** ~2s (dev), ~26s (release)
- **Live Session:** +$1.10 profit on $33.85 invested (3.24% return)
- **WebSocket:** Market data + User fills streaming with auto-reconnect
- **Order Book:** Tracking 24 tokens across 12 markets
- **Execution:** Maker orders with zero fees, parallel submission
- **Architecture:** Event-driven with tokio::select!, <1ms latency

---

## Project Overview

**Goal:** Build a maximum-efficiency Polymarket trading bot in Rust
**Primary Focus:** Correctness FIRST, then speed
**Target Latency:** 12-30ms total (vs 1000ms+ Python baseline)
**Target Strategies:** Mathematical arbitrage, temporal arbitrage, spread capture
**Reference Implementation:** polyfill-rs optimizations (21.4% faster than baseline Rust clients)

---

## CRITICAL: Implementation Philosophy

> **Correctness before speed.** A fast bot that loses money is worse than a slow bot that doesn't.

The phases below are ordered to build a **truthful, safe system first**, then add strategies and optimizations.

---

## Phase 1: Foundation + Safety ✅ COMPLETED

### 1.1 Project Setup ✅

- [x] Initialize Cargo project with workspace structure
- [x] Configure Cargo.toml with optimized dependencies (alloy v1.x, simd-json, etc.)
- [x] Set up release profile (LTO, single codegen unit, opt-level 3)
- [x] Configure logging with tracing crate
- [x] Add performance dependencies: simd-json, rust_decimal, dashmap

### 1.2 Configuration Management ✅

- [x] Environment variables via dotenvy
- [x] Config file support (reading from .env)
- [x] Secrets management (private keys, API keys loaded securely)
- [x] Runtime parameter validation
- [x] **Private keys and signatures not logged**

### 1.3 Core Types ✅

- [x] Define all API types (ConditionId, TokenId, Side, OrderType, PriceLevel, etc.)
- [x] Implement serialization with serde
- [x] Add validation traits (OperatingMode, ErrorType classification)
- [x] Unit tests for type conversions (8 tests passing)
- [x] Use rust_decimal::Decimal for all price/amount types
- [x] Module structure created (api/types.rs)

### 1.4 Kill Switch ✅

- [x] Implement KillSwitch struct with atomic bool
- [x] Check env var `POLYBOT_KILL` on every loop iteration
- [x] Check file `/tmp/polybot_kill` existence
- [x] Implement graceful_shutdown() procedure (in kill_switch.rs)
- [x] Ctrl+C handler (calls kill_switch.kill())
- [x] 4 unit tests for kill switch (all passing)
- ⏸️ Print final state snapshot (deferred to Phase 4 - Ledger)
- ⏸️ Persist state to disk for recovery (deferred to Phase 4 - Ledger)

**Deliverable:** ✅ Compilable project skeleton with types and working kill switch

### 1.5 Bot Orchestration Module ✅ (Refactored to Event-Driven)

- [x] Created src/bot.rs (~270 lines) for component coordination
- [x] Simplified main.rs to just initialization (86 lines)
- [x] Clean separation of concerns
- [x] **Event-driven architecture with tokio::select!**
- [x] biased priority: market WS > user WS > tick > heartbeat > kill
- [x] <1ms message latency (replaced 10ms polling sleep)
- [x] Async kill switch with tokio::sync::Notify

---

## Phase 2: Connectivity + State ✅ PARTIALLY COMPLETED

### 2.1 Market WebSocket (Order Books) ✅

- [x] Connect to `wss://ws-subscriptions-clob.polymarket.com/ws/market`
- [x] Implement subscription message `{"type": "market", "assets_ids": [...]}`
- [x] Parse order book updates with flexible JSON parsing (serde_json::Value)
- [x] Handle reconnection logic with exponential backoff
- [x] Implement PING/PONG keepalive (5 sec interval)
- [x] WebSocket spawned as async task in bot.rs
- ⏸️ **Track WS connection state for circuit breaker** (deferred to Phase 5)

### 2.2 Order Book State ✅

- [x] Created OrderBookState with DashMap (lock-free)
- [x] Implemented best_bid(), best_ask(), mid_price(), spread()
- [x] Implemented spread_bps() for basis point calculation
- [x] bid_depth(), ask_depth() for liquidity analysis
- [x] is_two_sided(), is_crossed() for validation
- [x] 3 unit tests (all passing)
- [x] Integrated into bot.rs event loop

### 2.3 User WebSocket (Trade Notifications) ✅ COMPLETED IN PHASE 4

- [x] Connect to `wss://ws-subscriptions-clob.polymarket.com/ws/user`
- [x] Implement authentication message
- [x] Parse trade notifications (fills)
- [x] **This is the PRIMARY source for position updates**
- [x] Track order status updates

### 2.4 Ledger System (Authoritative State) ✅ COMPLETED IN PHASE 4

- [x] **OpenOrders:** order_id → metadata + status (state machine)
- [x] **Fills:** trade_id → fill record (immutable log)
- [x] **Positions:** token_id → shares, avg cost, realized P&L
- [x] **Cash:** available, reserved, total USDC

### 2.5 Reconciliation Loop ✅ IMPLEMENTED IN PHASE 5

- [x] Every 2-5 minutes: GET /orders?status=open
- [x] Cross-check with local OpenOrders
- [x] Handle discrepancies
- Note: Implemented in src/risk/reconciliation.rs

### 2.6 Market Registry ⏸️ DEFERRED TO PHASE 6

- [ ] Fetch markets list from REST API
- [ ] Build mapping: condition_id → {yes_token_id, no_token_id}
- [ ] Validate binary vs multi-outcome
- [ ] Track `accepting_orders` field per market

**Deliverable:** ✅ Real-time market data streaming + order book state (Ledger deferred to Phase 4)

---

## Phase 3: Execution Engine + Strategy Framework

### 3.1 Order State Machine

- [ ] Implement OrderState enum:
  - CreatedLocal → Signed → Submitted → Acked
  - Acked → PartiallyFilled → Filled
  - Acked → CancelPending → Cancelled
  - Any → Unknown (requires reconciliation)
  - Any → Rejected / Expired
- [ ] Enforce valid state transitions only
- [ ] Emit events on state changes for logging

### 3.2 HTTP Client (with Rate Limiting)

- [ ] Optimized reqwest client (HTTP/2, pooling, TCP_NODELAY)
- [ ] Connection pooling: 5-20 persistent connections per host
- [ ] HTTP/2 stream windows: 512KB initial stream, 1MB connection window
- [ ] DNS caching with trust-dns-resolver
- [ ] Connection warming (periodic /time requests every 30s)
- [ ] **Rate limiter: cap orders/second**
- [ ] **Track reject rate for circuit breaker**

### 3.3 Order Signing

- [ ] Private key loading (secure, never log)
- [ ] Derive signer/maker addresses
- [ ] EIP-712 domain separator + struct hash
- [ ] Sign with alloy LocalWallet
- [ ] Verify signatures locally before sending

### 3.4 Order Submission

- [ ] POST /order endpoint
- [ ] Transition state: Signed → Submitted → Acked/Rejected
- [ ] Handle error responses
- [ ] **Record rejects for circuit breaker**

### 3.5 Order Management

- [ ] Cancel order: DELETE /order/{id}
- [ ] Transition: Acked → CancelPending → Cancelled
- [ ] Cancel-all helper for shutdown

### 3.6 Circuit Breaker ✅ IMPLEMENTED IN PHASE 5

- [x] Track reject count and rate
- [x] States: Closed (normal) → Open (paused) → HalfOpen (testing)
- [x] Implement `ErrorType` classification (see 02-RUST-ARCHITECTURE.md):
  - [x] `Retryable`: ORDER_DELAYED, MARKET_NOT_READY (don't count toward threshold)
  - [x] `Expected`: FOK_ORDER_NOT_FILLED_ERROR (don't count toward threshold)
  - [x] `Fatal`: All INVALID*ORDER*\*, EXECUTION_ERROR, auth errors (count toward threshold)
- [x] Open circuit on:
  - [x] WebSocket disconnected
  - [x] Reconciliation failure
  - [x] Reject rate > 20% (only Fatal errors count)
- [x] Auto-reset after timeout (try half-open)
- [x] **Check circuit.is_trading_allowed() before every order**
- [ ] **Check market.accepting_orders before submitting orders** (Phase 7)

### 3.7 Strategy Trait + OrderIntent ✅ IMPLEMENTED IN PHASE 6

- [x] Define `Strategy` trait in `src/strategy/traits.rs`:
  - [x] `fn name(&self) -> &str`
  - [x] `fn subscribed_markets(&self) -> Vec<ConditionId>`
  - [x] `fn on_book_update(&self, market_id, ctx) -> Vec<OrderIntent>`
  - [x] `fn on_fill(&self, fill, ctx) -> Vec<OrderIntent>`
  - [x] `fn on_tick(&self, ctx) -> Vec<OrderIntent>`
  - [x] `fn on_shutdown(&self, ctx) -> Vec<OrderIntent>`
- [x] Define `OrderIntent` struct:
  - [x] market_id, token_id, side, price, size
  - [x] `urgency: Urgency` (Immediate, Normal, Passive)
  - [x] reason: String (for logging)
  - [x] group_id, priority for multi-leg and conflict resolution
- [x] Define `StrategyContext` (read-only view):
  - [x] `&OrderBookState`, `&Ledger`, `clock: Instant`
- [x] **Strategies output WHAT they want, not HOW to execute**

### 3.8 Strategy Router ✅ IMPLEMENTED IN PHASE 6

- [x] Implement `StrategyRouter` in `src/strategy/router.rs`:
  - [x] `register(strategy: Box<dyn Strategy>)`
  - [x] `enable(name: &str)` / `disable(name: &str)`
  - [x] `on_book_update()` → routes to subscribed strategies
  - [x] `on_fill()` → routes to relevant strategies
- [x] Priority-based conflict resolution (higher priority wins)
- [ ] **Future: per-strategy capital allocation**

### 3.9 Execution Policy ✅ IMPLEMENTED IN PHASE 6

- [x] Define `ExecutionPolicy` trait in `src/execution/policy.rs`:
  - [x] `fn to_order_params(&self, intent: &OrderIntent) -> OrderParams`
  - [x] `fn on_partial_fill(&self, intent, filled) -> PartialFillAction`
- [x] Implement `TakerPolicy`:
  - [x] Urgency::Immediate → FOK
  - [x] Urgency::Normal/Passive → FAK
  - [x] on_partial_fill → UnwindFilled (for arb) or CancelRemainder
- [x] Implement `MakerPolicy`:
  - [x] Always GTC
  - [x] price_offset for aggressive posting
  - [x] on_partial_fill → KeepRemainder
- [x] **To pivot from taker to maker: change policy, not strategy**

### 3.10 Order Executor ✅ IMPLEMENTED IN PHASE 6

- [x] Implement `OrderExecutor` in `src/execution/executor.rs`
- [x] execute() - single order intent with circuit breaker check
- [x] execute_batch() - concurrent multi-leg with tokio::join!
- [x] execute_grouped() - partial fill handling for arb
- [x] Integrates with CircuitBreaker for trading safety
- [x] Error classification for circuit breaker

**Deliverable:** ✅ Strategy framework complete. Ready for MathArbStrategy implementation.

---

## Phase 7: Strategy MVP (Math Arb Implements Strategy Trait) ✅ COMPLETED

### 7.1 Dynamic Edge Calculator ✅

- [x] Implement `EdgeCalculator` in `src/strategy/edge_calculator.rs`
- [x] Replace static ARB_THRESHOLD = 0.97 with dynamic calculation
- [x] `required_edge = fees + slippage + partial_fill_risk + spread_penalty`
- [x] Estimate slippage from book depth using `OrderBookState::depth()`
- [x] Increase margin for thin books

### 7.2 MathArbStrategy (Implements Strategy Trait) ✅

- [x] Implement `Strategy` trait for `MathArbStrategy`:
  - `on_book_update()` → check for arb opportunity, return `Vec<OrderIntent>`
  - Uses `ctx.books.best_ask()` primitives (NOT check_arbitrage)
  - Uses `ctx.registry.filter()` for market selection
- [x] Subscribe to 15-min binary crypto markets via `MarketFilter`
- [x] Return TWO `OrderIntent`s for both arb legs (YES + NO)
- [x] Set `urgency: Urgency::Immediate` for taker execution

### 7.3 Execution Flow (Strategy → Policy → Executor) - Partially Complete

- [x] StrategyRouter receives book update
- [x] Routes to MathArbStrategy.on_book_update()
- [x] Strategy returns `Vec<OrderIntent>`
- [ ] ExecutionPolicy (TakerPolicy) converts to `OrderParams` ⏳ Phase 8
- [ ] Executor submits both legs concurrently: `tokio::join!` ⏳ Phase 8

### 7.4 Partial Fill Handling (via TakerPolicy) ⏳ Deferred to Phase 8

- [ ] If both filled equally → success
- [ ] If partial fills unequal:
  - [ ] `TakerPolicy::on_partial_fill()` returns `UnwindFilled`
  - [ ] Executor immediately sells excess at best bid
  - [ ] Accept small loss to avoid directional exposure
- [ ] If one leg fails completely with fill:
  - [ ] Unwind the filled leg immediately
- [ ] **Never hold unhedged position > 500ms**

### 7.5 Risk Limits (Hard Guardrails) ✅

- [x] Max notional per market
- [x] Max total open exposure
- [x] Max outstanding orders (e.g., 10)
- [x] Max daily loss → stop trading
- [x] Max partial-fill exposure

**Deliverable:** ✅ MathArbStrategy with dynamic edge detection, intent generation (115 tests)

---

## Phase 7.5: API Fix & Market Discovery ✅ COMPLETED

### 7.5.1 Gamma API Client ✅

- [x] Implement `GammaClient` in `src/api/gamma.rs`
- [x] Parse `GammaEvent` and `GammaMarket` types
- [x] Handle stringified JSON fields (outcomes, clobTokenIds)
- [x] Custom deserializer for mixed string/number fields

### 7.5.2 Slug-Based Discovery ✅

- [x] Implement `discover_crypto_15min_markets()`
- [x] Generate slug patterns: `{asset}-updown-15m-{timestamp}`
- [x] Support BTC, ETH, SOL assets
- [x] Query current + next 2 time intervals

### 7.5.3 Market Discovery Integration ✅

- [x] Implement `MarketDiscovery` in `src/api/discovery.rs`
- [x] `DiscoveredMarket` struct with parsed tokens
- [x] `to_market_pair()` conversion
- [x] Update `main.rs` to use discovery on startup
- [x] Bot auto-discovers live 15-min crypto markets

### 7.5.4 Up/Down Market Support ✅

- [x] `MarketPair::new_up_down()` constructor
- [x] Generic `first_token_id` / `second_token_id` naming
- [x] Compatibility aliases for `yes_token_id` / `no_token_id`

**Deliverable:** ✅ Bot discovers and subscribes to live 15-min crypto markets (136 tests)

---

## Phase 8: Wire Execution Policies ✅ COMPLETED

### 8.0 Complete Execution Pipeline ✅

- [x] Wire `TakerPolicy` / `MakerPolicy` into `Bot::process_intents_async()`
- [x] Check circuit breaker before order submission
- [x] Convert `OrderIntent` → `OrderParams` via policy
- [x] Sign orders via `OrderSigner`
- [x] Submit to CLOB API via `ApiClient`
- [x] Track in `Ledger`
- [x] Paper mode vs Live mode support
- [x] Execution result logging

**Deliverable:** ✅ Full execution pipeline: Strategy → Policy → Executor → API (136 tests)

---

## Phase 9: Integration & Live Testing 🚧 IN PROGRESS

### 9.0 Live Testing

- [ ] Test with `BOT_MODE=live` on funded wallet (small amounts)
- [ ] Verify EIP-712 signing works with real orders
- [ ] Test order cancellation flow
- [ ] Monitor for arb opportunities and actual executions

### 9.1 User WebSocket Integration

- [ ] Wire User WebSocket to Bot event loop
- [ ] Update ledger from actual fill notifications
- [ ] Handle order status changes (acked, filled, cancelled)

### 9.2 MakerRebateArbStrategy (Gabagool Strategy) - Deferred

- [ ] Implement `Strategy` trait for `MakerRebateArbStrategy` in `src/strategy/maker_arb.rs`
- [ ] Same arb logic as MathArb but with `Urgency::Passive` → GTC orders
- [ ] Filter to 15-min crypto markets only (where rebates apply)
- [ ] Configure `price_offset` for posting inside spread
- [ ] Configure `min_rebate_edge` (lower than taker - no fees!)
- [ ] Track rebate earnings separately in metrics
- [ ] Test alongside MathArbStrategy (both can run simultaneously)

**Key Implementation Notes:**

- Uses existing `MakerPolicy` infrastructure
- Posts limit orders slightly inside spread to increase fill probability
- Zero fees + rebates = profitable at lower edge thresholds (~1% vs ~3%)
- Lower fill rate but higher profit per trade

### 9.3 External Price Feeds (for Temporal Arb) - Deferred

- [ ] Binance WebSocket connection in `src/feeds/binance.rs`
- [ ] Coinbase WebSocket connection in `src/feeds/coinbase.rs`
- [ ] Spot price aggregation in StrategyContext

### 8.3 TemporalArbStrategy (Implements Strategy Trait)

- [ ] Implement `Strategy` trait for `TemporalArbStrategy`:
  - `on_book_update()` → compare spot vs Polymarket, return `Vec<OrderIntent>`
  - `on_tick()` → check for time-based exits
- [ ] Detect mispricing windows (30-90 sec lag)
- [ ] Calculate directional conviction
- [ ] Set `urgency: Urgency::Immediate` for taker execution
- [ ] Time-based position exit via `on_tick()`

### 5.4 SpreadCaptureStrategy (Implements Strategy Trait)

- [ ] Implement `Strategy` trait for `SpreadCaptureStrategy`:
  - `on_book_update()` → detect spread dislocation, return `Vec<OrderIntent>`
- [ ] Delta-neutral position building
- [ ] Volatility compression detection
- [ ] Can use `MakerPolicy` for passive liquidity provision

### 5.5 Market Selection Scoring

- [ ] Score markets by liquidity, spread, volatility
- [ ] Use `MarketFilter` capabilities for filtering
- [ ] Strategy subscribes dynamically via `subscribed_markets()`

### 5.6 Strategy Hot-Swap Support

- [ ] Enable/disable strategies at runtime via `StrategyRouter::enable/disable`
- [ ] Graceful strategy shutdown: call `strategy.on_shutdown()` before disabling
- [ ] **Future: per-strategy capital allocation and risk limits**

**Deliverable:** Multiple strategies running via StrategyRouter, including MakerRebateArb alongside MathArb

---

## Phase 6: Monitoring & Observability

### 6.1 Latency Instrumentation

- [ ] Log timestamps for critical path:
  - [ ] WS recv → parse done → book updated
  - [ ] opportunity detected → order signed → POST sent
  - [ ] ack received → fill received
- [ ] Store p50 / p95 / p99 (not just averages)
- [ ] Track "max in last 1 min"

### 6.2 Health Metrics

- [ ] WS reconnect count
- [ ] Missed sequence / resync events
- [ ] Order reject count and rate
- [ ] Cancel success rate
- [ ] Fill ratio: filled / submitted
- [ ] Partial fill rate
- [ ] Reconciliation corrections count

### 6.3 Alerting

- [ ] Discord/Telegram webhooks for:
  - [ ] Circuit breaker opened
  - [ ] Daily loss limit hit
  - [ ] Reconciliation failures
  - [ ] Kill switch activated

**Deliverable:** Full observability with health metrics and alerts

---

## Phase 7: Deployment

### 7.1 VPS Deployment

- [ ] Provision VPS (AWS us-east-1 recommended)
- [ ] Apply kernel tuning (sysctl.conf)
- [ ] Configure file descriptor limits
- [ ] Set up systemd service with auto-restart

### 7.2 Testing Progression

- [ ] Paper trading mode (log-only, no orders)
- [ ] Small amount testing ($10-50)
- [ ] Gradual size increase
- [ ] Full deployment after 1 week stable

**Deliverable:** Production-ready bot with monitoring

---

## Phase 8: Optimization (ONLY AFTER CORRECTNESS)

> **Do not start Phase 8 until Phases 1-6 are stable for at least 1 week.**

### 8.1 Latency Profiling

- [ ] Instrument all critical paths
- [ ] Identify actual bottlenecks (not assumed ones)
- [ ] Optimize hot paths

### 8.2 Advanced Optimizations

- [ ] Lock-free order queue (crossbeam)
- [ ] io_uring for async I/O (Linux)
- [ ] Memory pre-allocation tuning
- [ ] CPU affinity for critical threads

**These are nice-to-have. Your bottlenecks will be IO + WS + rate limits first.**

---

## Implementation Priority Matrix (Updated for Pivot Architecture)

| Component                             | Priority | Complexity | Dependencies                   | Phase |
| ------------------------------------- | -------- | ---------- | ------------------------------ | ----- |
| Types/Config                          | P0       | Low        | None                           | 1     |
| **Kill Switch**                       | **P0**   | Low        | None                           | **1** |
| Market WebSocket                      | P0       | Medium     | Types                          | 2     |
| **User WebSocket (Fills)**            | **P0**   | Medium     | Types                          | **2** |
| **Ledger System**                     | **P0**   | High       | Types                          | **2** |
| **Reconciliation Loop**               | **P0**   | Medium     | Ledger, HTTP                   | **2** |
| **Market Registry (w/ Capabilities)** | **P0**   | Medium     | HTTP                           | **2** |
| **Order State Machine**               | **P0**   | Medium     | Ledger                         | **3** |
| HTTP Client + Rate Limit              | P0       | Medium     | Types                          | 3     |
| EIP-712 Signing                       | P0       | High       | Types                          | 3     |
| **Circuit Breaker**                   | **P0**   | Medium     | Rate Limit                     | **3** |
| Order Submission                      | P0       | Medium     | Signing, HTTP, State Machine   | 3     |
| **Strategy Trait + OrderIntent**      | **P0**   | Medium     | State, Ledger                  | **3** |
| **StrategyRouter**                    | **P0**   | Medium     | Strategy Trait                 | **3** |
| **ExecutionPolicy (Taker/Maker)**     | **P0**   | Medium     | OrderIntent                    | **3** |
| **Dynamic Edge Calculator**           | **P1**   | Medium     | OrderBookState                 | **4** |
| **Partial Fill Handling**             | **P1**   | High       | Ledger, ExecutionPolicy        | **4** |
| **MathArbStrategy**                   | **P1**   | Medium     | Strategy Trait, Edge Calc      | **4** |
| **MakerRebateArbStrategy**            | **P1**   | Low        | Strategy Trait, MakerPolicy    | **5** |
| **TemporalArbStrategy**               | **P2**   | High       | Strategy Trait, External feeds | **5** |
| **SpreadCaptureStrategy**             | **P2**   | Medium     | Strategy Trait, MakerPolicy    | **5** |
| **Latency Instrumentation**           | P2       | Medium     | All                            | **6** |
| **Health Metrics**                    | P2       | Medium     | All                            | **6** |
| Monitoring/Alerting                   | P3       | Medium     | All                            | 6     |

**Bold items are from feedback integration and pivot architecture review.**

### Key Architecture Changes (Pivot Support)

| Change                                   | Purpose                                       |
| ---------------------------------------- | --------------------------------------------- |
| `OrderBookState` exposes only primitives | Strategy logic decoupled from state           |
| `Strategy` trait with `on_book_update()` | Pluggable strategies, clean interface         |
| `OrderIntent` abstraction                | Strategies declare WHAT, not HOW              |
| `ExecutionPolicy` (Taker/Maker)          | Pivot execution mode without strategy rewrite |
| `StrategyRouter`                         | Multiple concurrent strategies                |
| `MarketRegistry` with capabilities       | Filter markets by kind, fees, timing          |

---

## Operator Configuration Guide

This section explains how YOU control the bot - which markets to trade, bet sizes, and operational modes.

### Configuration Hierarchy

```
┌─────────────────────────────────────────────────────────────────┐
│                    config.toml (Operator Settings)               │
├─────────────────────────────────────────────────────────────────┤
│  [general]                                                       │
│  mode = "live"  # "paper" | "live"                              │
│  log_level = "info"                                             │
│                                                                 │
│  [markets]                                                      │
│  # Option 1: Auto-discover by criteria                          │
│  auto_discover = true                                           │
│  market_types = ["crypto_15min"]  # What types to trade         │
│  # Option 2: Explicit market list                               │
│  # explicit_markets = ["0xabc...", "0xdef..."]                  │
│                                                                 │
│  [sizing]                                                       │
│  max_bet_usd = 500.0            # Max per single trade          │
│  max_position_usd = 2000.0      # Max exposure per market       │
│  max_total_exposure_usd = 10000.0  # Total across all markets   │
│  kelly_fraction = 0.25          # Fraction of Kelly criterion   │
│                                                                 │
│  [strategies]                                                   │
│  enabled = ["math_arb", "maker_rebate_arb"]                     │
│  math_arb.min_edge = 0.03       # 3% minimum edge for taker     │
│  maker_rebate_arb.min_edge = 0.01  # 1% for maker (no fees)     │
│                                                                 │
│  [risk]                                                         │
│  max_daily_loss_usd = 500.0     # Stop trading if hit           │
│  max_open_orders = 10                                           │
│  circuit_breaker_rejects = 5    # Pause after N rejects         │
└─────────────────────────────────────────────────────────────────┘
```

### Market Selection Modes

**Mode 1: Auto-Discovery (Recommended for 15-min crypto)**

The bot automatically finds and subscribes to markets matching your criteria:

```toml
[markets]
auto_discover = true

# Market type filters
market_types = ["crypto_15min"]  # Options: "crypto_15min", "sports", "politics", "all"

# Additional filters
min_liquidity_usd = 1000.0    # Skip illiquid markets
min_spread_edge = 0.02        # Only markets with potential edge
max_time_to_resolution = 900  # 15 minutes in seconds
```

**Mode 2: Explicit Market List (For specific events)**

You specify exact markets to trade:

```toml
[markets]
auto_discover = false

# List of condition_ids to trade
explicit_markets = [
    "0x2e94bb8dd09931d12e6e656fe4fe6ceb3922bc3d6eab864bb6cd24773cf67269",
    "0x81d8e0447f946c753a1efad76eef9aa46ca33e14e86c1f50ae4377984b2d12d5",
]

# Or use market slugs (resolved to IDs at startup)
# market_slugs = ["btc-above-100k-jan-15", "eth-above-4k-jan-15"]
```

**Mode 3: Tag-Based Selection**

Use Polymarket's category tags:

```toml
[markets]
auto_discover = true
tag_ids = ["crypto", "btc"]  # From gamma-api.polymarket.com/tags
exclude_tags = ["long_term"]  # Skip these categories
```

### Bet Sizing Configuration

```toml
[sizing]
# === Fixed Sizing (Simple) ===
sizing_mode = "fixed"
fixed_bet_usd = 100.0         # Always bet this amount

# === Percentage of Bankroll ===
# sizing_mode = "percentage"
# bet_percentage = 0.02        # 2% of available balance

# === Kelly Criterion (Advanced) ===
# sizing_mode = "kelly"
# kelly_fraction = 0.25        # Use 25% of full Kelly (conservative)
# max_kelly_bet_usd = 1000.0   # Cap Kelly suggestions

# === Edge-Scaled (Dynamic) ===
# sizing_mode = "edge_scaled"
# base_bet_usd = 50.0          # Minimum bet
# edge_multiplier = 10.0       # Multiply by edge (e.g., 5% edge = $500)
# max_scaled_bet_usd = 500.0   # Cap

# === Position Limits (Always Apply) ===
max_position_per_market_usd = 2000.0   # Max exposure per market
max_total_exposure_usd = 10000.0       # Total across all positions
max_single_bet_usd = 500.0             # Hard cap per trade
min_bet_usd = 10.0                     # Below this, skip trade
```

### Strategy-Specific Configuration

```toml
[strategies]
# Which strategies to run
enabled = ["math_arb", "maker_rebate_arb"]

# === Math Arb (Taker) ===
[strategies.math_arb]
enabled = true
min_edge = 0.03              # 3% minimum (covers ~3% taker fee at 50/50)
max_position = 500.0         # Max per arb pair
order_type = "FOK"           # FOK or FAK
markets = "auto"             # "auto" or list of specific market IDs

# === Maker Rebate Arb (Gabagool) ===
[strategies.maker_rebate_arb]
enabled = true
min_edge = 0.01              # 1% minimum (no fees to cover)
price_offset = 0.01          # Post 1 cent inside spread
max_position = 1000.0        # Can be larger (longer hold)
requote_threshold = 0.02     # Requote if market moves 2%
markets = "auto"             # Only 15-min crypto (auto-filtered)

# === Temporal Arb ===
[strategies.temporal_arb]
enabled = false              # Not implemented yet
min_gap_pct = 0.03           # 3% gap between spot and market
price_sources = ["binance", "coinbase"]
```

### Runtime Controls

**CLI Flags (Override config.toml):**

```bash
# Start in paper trading mode
./polybot --mode paper

# Trade specific market only
./polybot --market 0xabc123...

# Override max bet
./polybot --max-bet 100

# Enable/disable strategies
./polybot --enable math_arb --disable maker_rebate_arb

# Dry run (log what would happen, don't execute)
./polybot --dry-run
```

**Hot Reload (While Running):**

```bash
# Reload config without restart
kill -HUP $(pgrep polybot)

# Or via control file
echo "reload" > /tmp/polybot_control
```

**Kill Switch:**

```bash
# Immediate stop
touch /tmp/polybot_kill
# Or
export POLYBOT_KILL=1
```

### Example Configurations

**Conservative Starter (Recommended):**

```toml
[general]
mode = "paper"  # Start with paper trading!

[markets]
auto_discover = true
market_types = ["crypto_15min"]

[sizing]
sizing_mode = "fixed"
fixed_bet_usd = 50.0
max_position_per_market_usd = 200.0
max_total_exposure_usd = 1000.0

[strategies]
enabled = ["math_arb"]  # Start with one strategy

[strategies.math_arb]
min_edge = 0.04  # Higher threshold = fewer but safer trades

[risk]
max_daily_loss_usd = 100.0
```

**Aggressive (After Validation):**

```toml
[general]
mode = "live"

[markets]
auto_discover = true
market_types = ["crypto_15min"]

[sizing]
sizing_mode = "kelly"
kelly_fraction = 0.25
max_single_bet_usd = 1000.0
max_position_per_market_usd = 5000.0
max_total_exposure_usd = 20000.0

[strategies]
enabled = ["math_arb", "maker_rebate_arb"]

[strategies.math_arb]
min_edge = 0.03

[strategies.maker_rebate_arb]
min_edge = 0.01

[risk]
max_daily_loss_usd = 1000.0
```

### Monitoring Dashboard Values

The bot exposes these metrics for monitoring:

```
polybot_markets_subscribed          # How many markets being watched
polybot_opportunities_detected      # Arb opportunities found
polybot_trades_executed            # Orders placed
polybot_trades_filled              # Orders filled
polybot_current_exposure_usd       # Total position value
polybot_daily_pnl_usd              # Today's P&L
polybot_strategy_enabled{name}     # Which strategies active
```

---

## Key Configuration Constants

```rust
use rust_decimal::Decimal;
use std::time::Duration;

// === DYNAMIC EDGE (replaces static thresholds) ===
// required_edge = fees + slippage + partial_fill_risk + spread_penalty
pub const DEFAULT_MIN_EDGE: Decimal = dec!(0.03);      // $0.03 fallback minimum
pub const THIN_BOOK_EXTRA_MARGIN: Decimal = dec!(0.01); // +$0.01 for thin books

// === TIMING ===
pub const POLL_INTERVAL_MS: u64 = 1500;               // 1.5 seconds
pub const WS_PING_INTERVAL_SEC: u64 = 5;              // Keepalive
pub const WS_RECONNECT_DELAY_MS: u64 = 1000;
pub const HTTP_TIMEOUT_MS: u64 = 500;
pub const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(120); // 2 min
pub const CONNECTION_WARM_INTERVAL: Duration = Duration::from_secs(30);

// === POSITION SIZING ===
pub const MAX_BET_SIZE_USD: Decimal = dec!(5000.0);   // Per window
pub const MAX_CAPITAL_PCT: Decimal = dec!(0.40);       // 40% max
pub const MIN_BET_SIZE_USD: Decimal = dec!(10.0);      // Minimum

// === RISK LIMITS (HARD GUARDRAILS) ===
pub const MAX_DAILY_LOSS_USD: Decimal = dec!(500.0);   // Stop trading if hit
pub const MAX_OPEN_ORDERS: u32 = 10;                   // Maximum concurrent orders
pub const MAX_UNHEDGED_EXPOSURE: Decimal = dec!(100.0); // Max directional exposure
pub const MAX_PARTIAL_FILL_EXPOSURE: Decimal = dec!(200.0); // Max from partial fills
pub const PARTIAL_FILL_UNWIND_MS: u64 = 500;           // Max time to hold unhedged

// === CIRCUIT BREAKER ===
pub const REJECT_THRESHOLD: u32 = 5;                   // Open circuit after N rejects
pub const REJECT_RATE_THRESHOLD: f64 = 0.20;           // Open circuit if >20% rejects
pub const CIRCUIT_RESET_TIMEOUT: Duration = Duration::from_secs(60); // Try half-open after

// === KILL SWITCH ===
pub const KILL_SWITCH_ENV_VAR: &str = "POLYBOT_KILL";
pub const KILL_SWITCH_FILE: &str = "/tmp/polybot_kill";
```

---

## Testing Strategy

### Unit Tests

- Type serialization/deserialization
- Signature generation
- Amount calculations
- Arbitrage detection logic

### Integration Tests

- WebSocket connection and subscription
- Order placement (testnet or small amounts)
- Full trade cycle

### Load Tests

- Multi-market parallel scanning
- Order throughput
- Memory under sustained load

---

## Success Metrics

| Metric               | Target  | Measurement                               |
| -------------------- | ------- | ----------------------------------------- |
| Total Latency        | < 30ms  | Timestamp at WS receive to POST sent      |
| Order Book Update    | < 160µs | 1000 ops benchmark                        |
| Spread Calculation   | < 100ns | Single operation                          |
| JSON Parsing (480KB) | < 2.5ms | simd-json benchmark                       |
| Signing Time         | < 5ms   | Instrument alloy sign call                |
| HTTP POST            | < 20ms  | Request/response timing (warm connection) |
| Uptime               | > 99%   | Monitoring                                |
| Fill Rate            | > 80%   | Orders matched / orders sent              |
| Win Rate (Arb)       | > 95%   | Profitable trades / total                 |
| Daily P&L            | > $100  | Net after fees                            |

---

## Risk Checklist

Before going live, verify:

### Safety Systems (MUST PASS)

- [ ] **Kill switch working** (test with env var AND file)
- [ ] **Graceful shutdown cancels all orders**
- [ ] **Circuit breaker triggers on high reject rate**
- [ ] **Reconciliation loop running and finding no discrepancies**

### Security

- [ ] Private key securely stored (not in code)
- [ ] Private key/signatures never logged
- [ ] API keys generated and tested
- [ ] Using "trading key" with limited funds (operational separation)

### Trading Systems

- [ ] USDC balance sufficient
- [ ] Small test trades successful ($1-10)
- [ ] **Partial fill handling tested** (manually trigger partial)
- [ ] **Unwind logic verified** (fills sell back correctly)
- [ ] **State machine transitions correct** (check logs)

### Risk Limits

- [ ] Loss limits configured and tested
- [ ] **Max open orders enforced**
- [ ] **Max unhedged exposure enforced**
- [ ] **Max daily loss stops trading**

### Monitoring

- [ ] Monitoring alerts working
- [ ] **Health metrics collecting**
- [ ] **Latency p50/p95/p99 tracked**

### Infrastructure

- [ ] VPS in us-east-1
- [ ] Kernel tuning applied
- [ ] systemd service with auto-restart
- [ ] Backup VPS ready (optional)

---

## Resources

### Documentation

- Polymarket Docs: https://docs.polymarket.com/
- CLOB API: https://docs.polymarket.com/developers/CLOB/quickstart
- ethers-rs: https://docs.rs/ethers/latest/ethers/

### Official SDKs

- Rust: https://github.com/Polymarket/rs-clob-client
- Python: https://github.com/Polymarket/py-clob-client

### High-Performance Reference

- polyfill-rs: https://github.com/floor-licker/polyfill-rs (21.4% faster than official client)

### Planning Documents

- `./01-STRATEGY-ANALYSIS.md` - Strategy details and metrics
- `./02-RUST-ARCHITECTURE.md` - Technical architecture
- `./03-API-REFERENCE.md` - Complete API reference

---

## Key Dependency Changes Summary

| Old              | New                   | Reason                              |
| ---------------- | --------------------- | ----------------------------------- |
| ethers = "2.0"   | alloy = "0.1"         | Modern successor, better maintained |
| reqwest = "0.11" | reqwest = "0.12"      | HTTP/2 improvements                 |
| f64 for prices   | rust_decimal + macros | Fixed-point, integer-speed          |
| -                | trust-dns-resolver    | DNS caching (1-5ms savings)         |
| -                | bytes                 | Buffer pooling, zero-alloc          |
| -                | hyper = "1.0"         | HTTP/2 stream window control        |
| -                | governor              | Rate limiting for API calls         |
| -                | metrics + prometheus  | Health metrics export               |
| -                | hdrhistogram          | Latency p50/p95/p99 tracking        |
| -                | mockall               | Testing with mocks                  |

## New Components Summary (from feedback + pivot architecture)

| Component                             | Purpose                                                         | Phase |
| ------------------------------------- | --------------------------------------------------------------- | ----- |
| **Kill Switch**                       | Emergency stop, graceful shutdown                               | 1     |
| **Ledger System**                     | Authoritative state (orders, fills, positions, cash)            | 2     |
| **Reconciliation Loop**               | REST sync every 2-5 min, find discrepancies                     | 2     |
| **Market Registry (w/ Capabilities)** | Startup validation, tick/size rules, MarketKind/FeeModel/Timing | 2     |
| **Order State Machine**               | Explicit states + valid transitions                             | 3     |
| **Circuit Breaker**                   | Pause trading on errors/rejects                                 | 3     |
| **Rate Limiter**                      | Cap API calls, backpressure                                     | 3     |
| **Strategy Trait**                    | Pluggable strategies, `on_book_update()` → `Vec<OrderIntent>`   | 3     |
| **OrderIntent**                       | Declare WHAT strategy wants (not HOW)                           | 3     |
| **StrategyRouter**                    | Route events to multiple strategies, enable/disable             | 3     |
| **ExecutionPolicy**                   | Taker (FOK/FAK) vs Maker (GTC) conversion                       | 3     |
| **TakerPolicy**                       | Immediate execution, unwind partials                            | 3     |
| **MakerPolicy**                       | Passive liquidity, keep remainder                               | 3     |
| **Dynamic Edge Calculator**           | Replace static 0.97 threshold                                   | 4     |
| **Partial Fill Handler**              | Unwind unhedged exposure via policy                             | 4     |
| **MathArbStrategy**                   | Implements Strategy trait for arb                               | 4     |
| **MakerRebateArbStrategy**            | Same arb logic, Urgency::Passive for maker rebates              | 5     |
| **TemporalArbStrategy**               | Implements Strategy trait for temporal arb                      | 5     |
| **SpreadCaptureStrategy**             | Implements Strategy trait for spread capture                    | 5     |
| **Health Metrics**                    | WS reconnects, reject rate, fill ratio                          | 6     |
| **Latency Instrumentation**           | p50/p95/p99 for critical path                                   | 6     |

## HTTP Client Optimization Summary

```
Before: 10-30ms HTTP POST
After:  8-20ms HTTP POST (with warm connections)

Optimizations applied:
✓ Connection pooling (20 idle connections)
✓ HTTP/2 stream windows (512KB/1MB)
✓ DNS caching
✓ TCP_NODELAY + keepalive
✓ Connection warming
✓ Rate limiting (governor)
```
