# State of the Bot - January 8, 2026

> **Single Source of Truth** - This document describes what the bot actually does today.
> All other planning/execution docs should be treated as historical context.

---

## Executive Summary

**poly-bot** is a Rust-based Polymarket trading bot that executes mathematical arbitrage on 15-minute crypto binary markets (BTC, ETH, SOL, XRP Up/Down).

**Current Status:** ✅ **PROFITABLE** - Latest session: +$1.10 (3.24% return)

**Architecture:** Event-driven with tokio::select!, <1ms message latency

---

## 1. What the Bot Does

### Strategy: Mathematical Arbitrage

When `YES_price + NO_price < $1.00 - required_edge`, the bot:

1. Buys both YES and NO tokens simultaneously
2. Waits for market resolution (15 minutes)
3. One side resolves to $1.00, netting guaranteed profit

**Example from latest session:**

```
BTC 11:00PM-11:15PM:
  YES @ $0.70 × 15 shares = $10.50
  NO  @ $0.27 × 15 shares = $4.05
  Total cost: $14.55
  Resolution payout: $15.00
  Gross profit: $0.45 (3.1% edge)
  Fees: $0.00 (maker rebate)
  Net profit: $0.45
```

### Key Parameters (Live Test Config)

| Parameter    | Value       | Rationale                                |
| ------------ | ----------- | ---------------------------------------- |
| Min Edge     | 1.0%        | Safety margin after fees                 |
| Min Shares   | 5           | Market minimum                           |
| Max Shares   | 15          | Limits exposure on low-priced legs       |
| Max Exposure | $50         | Total position limit                     |
| Cooldown     | 3 seconds   | Per-market rate limit                    |
| Order Type   | GTC (Maker) | Zero fees, possible rebate               |
| Fee Rate     | 1000 bps    | Declared in order, auto-rebated if maker |

---

## 2. Execution Lifecycle

```
┌─────────────────────────────────────────────────────────────────┐
│                        BOT LIFECYCLE                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────┐                                           │
│  │  MARKET DISCOVERY │  Gamma API → slug pattern → MarketPairs  │
│  └────────┬─────────┘                                           │
│           ▼                                                      │
│  ┌──────────────────┐                                           │
│  │  WEBSOCKET INIT  │  Market WS (books) + User WS (fills)      │
│  └────────┬─────────┘                                           │
│           ▼                                                      │
│  ┌──────────────────┐                                           │
│  │   EVENT LOOP     │  tokio::select! on WS messages            │
│  │   (biased)       │                                           │
│  └────────┬─────────┘                                           │
│           │                                                      │
│     ┌─────┴─────┐                                               │
│     ▼           ▼                                               │
│  Book Update   Fill Notification                                │
│     │                │                                          │
│     ▼                ▼                                          │
│  ┌──────────────┐  ┌──────────────┐                            │
│  │  STRATEGY    │  │ ORDER TRACKER│                            │
│  │  (MathArb)   │  │ (match fills)│                            │
│  └──────┬───────┘  └──────────────┘                            │
│         │                                                        │
│         ▼                                                        │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   ORDER INTENTS                           │  │
│  │  YES leg: Buy token_A @ $0.70 × 15 [group: arb-uuid]     │  │
│  │  NO leg:  Buy token_B @ $0.27 × 15 [group: arb-uuid]     │  │
│  └──────────────────────────┬───────────────────────────────┘  │
│                             ▼                                    │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   EXECUTION POLICY                        │  │
│  │  DualPolicy: Passive intent → MakerPolicy (GTC)          │  │
│  │              Immediate intent → TakerPolicy (FOK)        │  │
│  └──────────────────────────┬───────────────────────────────┘  │
│                             ▼                                    │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   ORDER EXECUTOR                          │  │
│  │  1. Check circuit breaker                                 │  │
│  │  2. Build Order struct                                    │  │
│  │  3. Sign with EIP-712 (neg-risk domain)                  │  │
│  │  4. Submit via REST API (parallel for maker legs)        │  │
│  │  5. Track pending orders                                  │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 3. Order Signing (Critical Details)

### Addresses & Roles

| Role         | Address                                      | Purpose                       |
| ------------ | -------------------------------------------- | ----------------------------- |
| EOA Signer   | `0x98e73F78E43596fD2bc38293064c306eFE05617C` | Signs orders with private key |
| Proxy/Funder | `0xf70b41e9893fb37061f6ecd7ac56d14cc2b9f7a9` | Holds funds, receives fills   |
| API Key      | `fccc3d5a-2fbc-5fc4-3270-dd49fea07e3c`       | Order `owner` field           |

### Order Structure

```json
{
  "order": {
    "salt": 6690732130909827806,
    "maker": "0xF70B41e9893FB37061f6ECD7ac56d14cC2b9F7a9",
    "signer": "0x98e73F78E43596fD2bc38293064c306eFE05617C",
    "taker": "0x0000000000000000000000000000000000000000",
    "tokenId": "30314892501903928067110224642123342533563491145504589776439305946486009825389",
    "makerAmount": "3150000",
    "takerAmount": "5000000",
    "side": "BUY",
    "expiration": "0",
    "nonce": "0",
    "feeRateBps": "1000",
    "signatureType": 1,
    "signature": "0x..."
  },
  "owner": "fccc3d5a-2fbc-5fc4-3270-dd49fea07e3c",
  "orderType": "GTC",
  "deferExec": false
}
```

### Amount Calculation

```
For BUY @ $0.63 × 5 shares:
  makerAmount = 0.63 × 5 × 1_000_000 = 3_150_000 (USDC, 6 decimals)
  takerAmount = 5 × 1_000_000 = 5_000_000 (shares as outcome tokens)
```

### Signature Domain (Neg-Risk)

- Contract: `0xC5d563A36AE78145C45a50134d48A1215220f80a` (CTF Exchange)
- Domain separator: `0x82cb6aa85babb812f4b521a12b10f0cbc68d2b44be7bc02c047004f544adb49f`
- EIP-712 typed data with CTF Order structure

---

## 4. Fee Handling

### 15-Minute Crypto Markets

- **Fee Rate:** 1000 bps (10%) applies to TAKER orders
- **Maker Rebate:** If order rests on book before fill, fee is rebated
- **Bot Behavior:** Uses GTC orders → places on book → fills as maker → **zero net fees**

### Fee Rate in Orders

- All orders include `feeRateBps: "1000"` in signed payload
- This declares the expected fee tier
- Actual fee depends on maker/taker status at fill time

### Observed in Logs

```
💰 Fill [MAKER ✅]: Buy 5 @ $0.34 (fee: $0.000000, order: 0x7b11fecdfa3a62)
```

---

## 5. Dynamic Share Sizing

### Problem Solved

Polymarket rejects orders with notional value < $1.00.

### Solution

Calculate minimum shares needed for both legs to exceed $1.00:

```rust
let min_price = yes_price.min(no_price);
let min_shares = (MIN_ORDER_VALUE / min_price).ceil();
let effective_min = config.min_position_size.max(min_shares);
```

**Example:**

- YES @ $0.79, NO @ $0.18
- `min_price = $0.17` (after 1¢ maker offset)
- `min_shares = ceil($1.00 / $0.17) = 6`
- Both legs get 6 shares
- YES notional: $4.74 ✅, NO notional: $1.02 ✅

---

## 6. Parallel Execution for Arb Legs

### Maker Orders (Passive Urgency)

Both legs submitted simultaneously via `tokio::join!`:

- Minimizes time window between leg submissions
- Both orders hit the book at ~same time
- If one fails, the other may still be pending (tracked)

### Taker Orders (Immediate Urgency)

Sequential execution with rollback:

- First leg → if success, second leg
- If second fails, cancel first leg
- Prevents one-legged exposure for FOK orders

---

## 7. Known Issues / Fragilities

### 7.1 Rounding Precision Error (Observed)

```
Order for 7.67 shares at $0.499 rejected:
  Expected makerAmount: 3.8273
  Submitted makerAmount: 3.83
```

**Cause:** `max_size` from EdgeCalculator returns fractional shares based on book depth.

**Impact:** Order rejected, arb opportunity missed.

**Future Fix:** Round trade_size to whole numbers or match Polymarket's expected precision.

### 7.2 Partial Fills May Linger

- Orders that partially fill remain active
- If bot shuts down, they stay on book
- **Mitigation:** Shutdown logs active orders for manual review

### 7.3 WebSocket Disconnections

- Both Market and User WS can disconnect mid-session
- Bot auto-reconnects with exponential backoff
- **Observed:** Recovered from WS disconnect at 04:11:04 UTC

### 7.4 Near-Miss Opportunities

- Some arb opportunities have edge > 0 but < required threshold
- These are logged as "near-miss" for diagnostics
- Not a bug, but indicates conservative edge calculator

---

## 8. Circuit Breaker

### States

- **Closed:** Normal trading allowed
- **Open:** All orders rejected (safety halt)
- **Half-Open:** Testing recovery after cooldown

### Trip Conditions

| Trigger                           | Action           |
| --------------------------------- | ---------------- |
| Fatal error threshold (3)         | Open             |
| Reject rate > 50%                 | Open             |
| WebSocket disconnect              | Open             |
| Reconciliation failure            | Open             |
| Daily loss limit                  | Open             |
| Orphaned position (cancel failed) | Open immediately |

---

## 9. Test Coverage

- **154 unit tests** passing
- Key coverage areas:
  - Order signing (EIP-712)
  - Edge calculation
  - Position tracking
  - Circuit breaker logic
  - WebSocket message parsing

---

## 10. File Structure (Key Files)

```
src/
├── main.rs              # Entry point, market discovery
├── bot.rs               # Event loop, orchestration
├── config.rs            # Environment loading
├── strategy/
│   ├── arbitrage.rs     # MathArbStrategy (core logic)
│   ├── edge_calculator.rs # Dynamic edge thresholds
│   └── market_pair.rs   # YES/NO token pair registry
├── execution/
│   ├── executor.rs      # Order submission
│   ├── policy.rs        # Maker/Taker routing
│   └── order_tracker.rs # Track pending orders
├── signing/
│   └── order.rs         # EIP-712 signing
├── api/
│   ├── endpoints.rs     # REST API calls
│   └── discovery.rs     # Market discovery
├── websocket/
│   ├── market.rs        # Order book streaming
│   └── user.rs          # Fill notifications
└── risk/
    └── circuit_breaker.rs # Safety controls
```

---

## 11. Configuration Reference

### Environment Variables (.env)

```bash
# API Credentials
POLYMARKET_API_KEY=xxx
POLYMARKET_SECRET=xxx
POLYMARKET_PASSPHRASE=xxx
POLYMARKET_PRIVATE_KEY=0x...
POLYMARKET_WALLET=0x...

# Operating Mode
POLYMARKET_MODE=live  # or "paper"
POLYMARKET_MAX_BET=100
POLYMARKET_MAX_DAILY_LOSS=100
```

### Strategy Config (MathArbConfig::live_test)

```rust
MathArbConfig {
    min_edge: 0.01,           // 1% minimum edge
    max_position_size: 15,    // Up to 15 shares per leg
    min_position_size: 5,     // Minimum 5 shares
    max_total_exposure: 50,   // $50 total
    cooldown_ms: 3000,        // 3 second per-market cooldown
    use_maker_execution: true // GTC orders for zero fees
}
```

---

## 12. Session Metrics (Latest Run)

| Metric             | Value             |
| ------------------ | ----------------- |
| Duration           | ~10 minutes       |
| Markets Tracked    | 12 (24 tokens)    |
| Arb Opportunities  | 4 detected        |
| Orders Placed      | 8                 |
| Orders Filled      | 7                 |
| Near Misses        | 2                 |
| WebSocket Messages | ~100k             |
| Message Rate       | 400-1000 msg/s    |
| Net Profit         | +$1.10 (3.24%)    |
| Fees Paid          | $0.00 (all maker) |

---

## Document History

- **2026-01-08:** Initial creation after profitable live session
- This document supersedes all previous planning/execution docs for current behavior
