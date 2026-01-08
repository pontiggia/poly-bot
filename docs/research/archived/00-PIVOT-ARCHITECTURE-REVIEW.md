# Pivot Architecture Review
## Making the Bot Truly Modular and Reusable

---

## Goal

The bot should serve as a **base platform** that can:
- Pivot from taker → maker strategies
- Pivot from binary → multi-outcome markets
- Add new strategies without touching core components
- Support different market types (crypto 15-min, sports, politics, etc.)

---

## Current State Assessment

### What's Already Good (Aligned with Modularity)

| Component | Status | Notes |
|-----------|--------|-------|
| Ledger as single source of truth | ✅ | Strategies read from it, don't own state |
| Execution as state machine | ✅ | Decoupled from strategy logic |
| Separate strategy files | ✅ | arbitrage.rs, temporal.rs, spread.rs |
| Market Registry concept | ✅ | Validates markets at startup |
| Phase 5 "multiple strategy support" | ✅ | Already planned |

### What's Missing (Gaps to Fix)

| Gap | Impact | Priority |
|-----|--------|----------|
| No formal Strategy trait/interface | Strategies will couple to internals | **HIGH** |
| No StrategyRouter | Can't manage multiple strategies cleanly | **HIGH** |
| No OrderIntent abstraction | Maker/taker requires strategy rewrite | **HIGH** |
| No ExecutionPolicy layer | FOK/FAK hardcoded into strategy | **MEDIUM** |
| `check_arbitrage()` in OrderBookState | Strategy logic in core state | **MEDIUM** |
| MarketRegistry missing capabilities | Hardcoded "15-min crypto" filtering | **MEDIUM** |

---

## Gap 1: Strategy Trait + Router

### Problem

Current code has strategy logic directly calling executor:
```rust
// Current: strategy directly builds and submits orders
self.executor.place_order(&token_id, Side::Buy, price, size, OrderType::FAK)
```

This couples strategy to:
- Order types (FAK/FOK/GTC)
- Execution details
- Error handling

### Solution: Strategy Trait

```rust
// src/strategy/traits.rs

/// What a strategy wants to do (not HOW to do it)
#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub market_id: ConditionId,
    pub token_id: TokenId,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub urgency: Urgency,        // Immediate, Normal, Passive
    pub time_in_force: Option<Duration>,  // Optional TTL hint
}

#[derive(Debug, Clone, Copy)]
pub enum Urgency {
    Immediate,  // Taker: FOK/FAK, cross the spread
    Normal,     // Could be either
    Passive,    // Maker: GTC, post only
}

/// All strategies implement this trait
pub trait Strategy: Send + Sync {
    /// Unique identifier
    fn name(&self) -> &str;

    /// Which markets this strategy cares about
    fn subscribed_markets(&self) -> Vec<ConditionId>;

    /// Called when order book updates
    fn on_book_update(
        &self,
        market_id: &ConditionId,
        book: &OrderBookSnapshot,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called when our order fills
    fn on_fill(
        &self,
        fill: &Fill,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called periodically (e.g., every 100ms)
    fn on_tick(
        &self,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent>;

    /// Called when strategy should clean up (shutdown, market close)
    fn on_shutdown(&self) -> Vec<OrderIntent> {
        vec![]  // Default: do nothing
    }
}

/// Context passed to strategies (read-only view)
pub struct StrategyContext<'a> {
    pub ledger: &'a Ledger,
    pub books: &'a OrderBookState,
    pub registry: &'a MarketRegistry,
    pub clock: Instant,
}
```

### Solution: Strategy Router

```rust
// src/strategy/router.rs

pub struct StrategyRouter {
    strategies: Vec<Box<dyn Strategy>>,
    enabled: DashMap<String, bool>,
    priority: Vec<String>,  // Order of precedence
}

impl StrategyRouter {
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
        book: &OrderBookSnapshot,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        let mut intents = vec![];

        for strategy in &self.strategies {
            if !self.is_enabled(strategy.name()) {
                continue;
            }
            if !strategy.subscribed_markets().contains(market_id) {
                continue;
            }

            let strategy_intents = strategy.on_book_update(market_id, book, ctx);
            intents.extend(strategy_intents);
        }

        // TODO: conflict resolution if multiple strategies want same market
        self.resolve_conflicts(intents)
    }
}
```

### Where to Add in Plan

**Phase 3** (after execution engine, before strategies):
- [ ] Define `Strategy` trait with `on_book_update`, `on_fill`, `on_tick`
- [ ] Define `OrderIntent` struct (what, not how)
- [ ] Implement `StrategyRouter` with enable/disable
- [ ] Add conflict resolution (priority-based)

---

## Gap 2: OrderIntent + ExecutionPolicy

### Problem

Current Phase 4 says:
> "Arb legs: use FOK (Fill-Or-Kill) or FAK (Fill-And-Kill)"
> "NEVER use GTC for arb legs"

This is correct for arb, but **hardcodes taker behavior**. A maker strategy would need GTC.

### Solution: ExecutionPolicy Layer

```rust
// src/execution/policy.rs

/// Converts OrderIntent → actual CLOB order based on policy
pub trait ExecutionPolicy: Send + Sync {
    fn name(&self) -> &str;

    /// Convert intent to concrete order parameters
    fn apply(&self, intent: &OrderIntent) -> OrderParams;

    /// Handle partial fill according to policy
    fn on_partial_fill(&self, intent: &OrderIntent, filled: Decimal) -> PartialFillAction;
}

#[derive(Debug, Clone)]
pub struct OrderParams {
    pub order_type: OrderType,  // FOK, FAK, GTC
    pub price: Decimal,
    pub size: Decimal,
    pub post_only: bool,
    pub time_in_force: Option<Duration>,
}

pub enum PartialFillAction {
    KeepRemainder,      // Let it sit (maker)
    CancelRemainder,    // Cancel unfilled (taker)
    UnwindFilled,       // Sell back what filled (arb)
}

/// Taker policy: aggressive, immediate execution
pub struct TakerPolicy {
    pub unwind_on_partial: bool,
    pub max_slippage: Decimal,
}

impl ExecutionPolicy for TakerPolicy {
    fn name(&self) -> &str { "taker" }

    fn apply(&self, intent: &OrderIntent) -> OrderParams {
        OrderParams {
            order_type: match intent.urgency {
                Urgency::Immediate => OrderType::FOK,
                _ => OrderType::FAK,
            },
            price: intent.price,
            size: intent.size,
            post_only: false,
            time_in_force: Some(Duration::from_millis(500)),
        }
    }

    fn on_partial_fill(&self, _intent: &OrderIntent, _filled: Decimal) -> PartialFillAction {
        if self.unwind_on_partial {
            PartialFillAction::UnwindFilled
        } else {
            PartialFillAction::CancelRemainder
        }
    }
}

/// Maker policy: passive, post liquidity
pub struct MakerPolicy {
    pub post_only: bool,
    pub requote_on_book_change: bool,
}

impl ExecutionPolicy for MakerPolicy {
    fn name(&self) -> &str { "maker" }

    fn apply(&self, intent: &OrderIntent) -> OrderParams {
        OrderParams {
            order_type: OrderType::GTC,
            price: intent.price,
            size: intent.size,
            post_only: self.post_only,
            time_in_force: None,  // Good till cancel
        }
    }

    fn on_partial_fill(&self, _intent: &OrderIntent, _filled: Decimal) -> PartialFillAction {
        PartialFillAction::KeepRemainder  // Let the rest fill
    }
}
```

### Updated Execution Flow

```
Strategy → OrderIntent → ExecutionPolicy → OrderParams → Executor → CLOB
              ↑                ↑
         "I want to           "Use FOK"
          buy YES"            "Unwind partials"
```

### Where to Add in Plan

**Phase 3** (part of execution engine):
- [ ] Define `ExecutionPolicy` trait
- [ ] Implement `TakerPolicy` (FOK/FAK, unwind partials)
- [ ] Implement `MakerPolicy` (GTC, keep partials)
- [ ] Executor takes `policy` parameter, not hardcoded order type

---

## Gap 3: Remove Strategy Logic from Core State

### Problem

In `02-RUST-ARCHITECTURE.md`, `OrderBookState` has:
```rust
pub fn check_arbitrage(&self, yes_id: &str, no_id: &str) -> Option<ArbitrageOpportunity>
```

This couples arbitrage detection to core state. Other strategies can't reuse `OrderBookState` cleanly.

### Solution

Move to strategy, keep state as pure data:

```rust
// src/state/order_book.rs - ONLY primitives

impl OrderBookState {
    pub fn get(&self, token_id: &str) -> Option<OrderBook>;
    pub fn best_bid(&self, token_id: &str) -> Option<PriceLevel>;
    pub fn best_ask(&self, token_id: &str) -> Option<PriceLevel>;
    pub fn spread(&self, token_id: &str) -> Option<Spread>;
    pub fn depth(&self, token_id: &str, levels: usize) -> Option<DepthSnapshot>;
    pub fn mid_price(&self, token_id: &str) -> Option<Decimal>;

    // NO check_arbitrage() here!
}

// src/strategy/arbitrage.rs - strategy owns its logic

impl MathArbStrategy {
    fn check_opportunity(&self, market: &MarketInfo, ctx: &StrategyContext) -> Option<OrderIntent> {
        let yes_ask = ctx.books.best_ask(&market.yes_token_id)?;
        let no_ask = ctx.books.best_ask(&market.no_token_id)?;

        let combined = yes_ask.price + no_ask.price;
        let required_edge = self.edge_calc.calculate(...);

        if Decimal::ONE - combined >= required_edge {
            // Return intent, not order
            Some(OrderIntent { ... })
        } else {
            None
        }
    }
}
```

### Where to Add in Plan

**Phase 2** (state management):
- [ ] `OrderBookState` exposes ONLY: `get`, `best_bid`, `best_ask`, `spread`, `depth`, `mid_price`
- [ ] Remove any strategy-specific methods from state
- [ ] Strategy logic lives in `src/strategy/` only

---

## Gap 4: Enhanced MarketRegistry

### Problem

Current `MarketRegistry` has basic validation, but strategies need to know:
- Is this binary or multi-outcome?
- What's the fee model?
- Does it support maker rebates?
- Is it a 15-min window or longer duration?

### Solution: Market Capabilities

```rust
// src/state/markets.rs

#[derive(Debug, Clone)]
pub struct MarketInfo {
    // Identity
    pub condition_id: ConditionId,
    pub question: String,

    // Token mapping
    pub tokens: MarketTokens,

    // Capabilities
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
}

#[derive(Debug, Clone)]
pub enum MarketTokens {
    Binary { yes: TokenId, no: TokenId },
    MultiOutcome { outcomes: Vec<(String, TokenId)> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarketKind {
    Binary,
    MultiOutcome { count: usize },
}

#[derive(Debug, Clone)]
pub enum FeeModel {
    Zero,
    Flat { maker_bps: u32, taker_bps: u32 },
    Tiered { tiers: Vec<FeeTier> },
}

#[derive(Debug, Clone)]
pub struct MarketTiming {
    pub duration: Duration,           // 15 min, 1 hour, etc.
    pub is_recurring: bool,           // New market every interval?
    pub resolution_delay: Duration,   // Time after close before resolution
}

impl MarketRegistry {
    /// Filter markets by capabilities
    pub fn filter(&self, filter: MarketFilter) -> Vec<&MarketInfo> {
        self.markets.iter()
            .filter(|m| filter.matches(m))
            .collect()
    }
}

#[derive(Default)]
pub struct MarketFilter {
    pub kind: Option<MarketKind>,
    pub max_duration: Option<Duration>,
    pub is_active: Option<bool>,
    pub min_liquidity: Option<Decimal>,
}

// Usage in strategy:
let crypto_15min = registry.filter(MarketFilter {
    kind: Some(MarketKind::Binary),
    max_duration: Some(Duration::from_secs(15 * 60)),
    is_active: Some(true),
    ..Default::default()
});
```

### Where to Add in Plan

**Phase 2** (Market Registry):
- [ ] Add `MarketKind` enum (Binary, MultiOutcome)
- [ ] Add `FeeModel` enum with maker/taker bps
- [ ] Add `MarketTiming` struct
- [ ] Add `filter()` method with `MarketFilter`
- [ ] Strategies use filter, not hardcoded logic

---

## Updated Module Structure

```
src/
├── main.rs
├── config.rs
├── error.rs
├── kill_switch.rs
│
├── api/
│   ├── mod.rs
│   ├── client.rs
│   ├── endpoints.rs
│   ├── types.rs
│   └── rate_limiter.rs
│
├── websocket/
│   ├── mod.rs
│   ├── market.rs
│   ├── user.rs
│   └── live_data.rs
│
├── state/
│   ├── mod.rs
│   ├── order_book.rs      # ONLY primitives (best_bid, best_ask, spread)
│   └── markets.rs         # MarketRegistry with capabilities
│
├── ledger/
│   ├── mod.rs
│   ├── orders.rs
│   ├── fills.rs
│   ├── positions.rs
│   ├── cash.rs
│   └── reconciliation.rs
│
├── execution/
│   ├── mod.rs
│   ├── state_machine.rs
│   ├── executor.rs
│   ├── policy.rs          # NEW: ExecutionPolicy trait + TakerPolicy + MakerPolicy
│   ├── partial_fill.rs
│   └── manager.rs
│
├── strategy/
│   ├── mod.rs
│   ├── traits.rs          # NEW: Strategy trait + OrderIntent
│   ├── router.rs          # NEW: StrategyRouter
│   ├── context.rs         # NEW: StrategyContext (read-only view)
│   ├── arbitrage.rs       # Implements Strategy trait
│   ├── temporal.rs        # Implements Strategy trait
│   ├── spread.rs          # Implements Strategy trait
│   ├── edge_calculator.rs
│   └── feasibility.rs
│
├── risk/
│   ├── mod.rs
│   ├── limits.rs
│   ├── circuit_breaker.rs
│   └── metrics.rs
│
└── feeds/
    ├── mod.rs
    ├── binance.rs
    └── coinbase.rs
```

---

## Summary: Changes to Planning Docs

### 02-RUST-ARCHITECTURE.md

1. **Remove** `check_arbitrage()` from `OrderBookState`
2. **Add** `Strategy` trait definition
3. **Add** `OrderIntent` struct
4. **Add** `StrategyRouter` code
5. **Add** `ExecutionPolicy` trait + implementations
6. **Update** `MarketRegistry` with capabilities

### 04-IMPLEMENTATION-PLAN.md

**Phase 2** additions:
- [ ] `OrderBookState` exposes only primitives (no strategy logic)
- [ ] `MarketRegistry` has `MarketKind`, `FeeModel`, `MarketTiming`
- [ ] `MarketFilter` for querying markets by capability

**Phase 3** additions (NEW: after execution engine):
- [ ] Define `Strategy` trait (`on_book_update`, `on_fill`, `on_tick`)
- [ ] Define `OrderIntent` struct (urgency, not order type)
- [ ] Implement `StrategyRouter` with enable/disable/priority
- [ ] Define `ExecutionPolicy` trait
- [ ] Implement `TakerPolicy` (FOK/FAK, unwind partials)
- [ ] Implement `MakerPolicy` (GTC, keep remainder)

**Phase 4** changes:
- [ ] `MathArbStrategy` implements `Strategy` trait
- [ ] Returns `Vec<OrderIntent>`, not direct executor calls
- [ ] Uses `TakerPolicy` for execution

---

## Research Questions to Resolve

1. **Conflict Resolution**: If two strategies want to trade same market, who wins?
   - Priority order?
   - First come first served?
   - Capital allocation per strategy?

2. **Strategy Isolation**: Should strategies have separate capital pools?
   - Shared ledger but isolated risk limits?

3. **Hot-Swap Strategies**: Can we enable/disable strategies at runtime?
   - Needs graceful handoff (close positions first?)

---

## Conclusions Placeholder

*Add your research conclusions here after investigation.*
