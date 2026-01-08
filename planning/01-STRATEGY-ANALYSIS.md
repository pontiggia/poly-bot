# Polymarket Bot Strategy Analysis

## Consolidated Intelligence Report for High-Frequency Rust Bot

---

## Executive Summary

Analysis of 12 documented profitable bots reveals clear patterns for maximum efficiency. The most successful strategies combine **speed advantage** (Rust vs Python = 25-70x faster) with **mathematical edges** (arbitrage, temporal lag, spread capture).

**Top Performers:**
| Bot | Profit | Strategy | Key Edge |
|-----|--------|----------|----------|
| ilovecircle | $2.2M (2 months) | AI Probability | 74% accuracy, ensemble models |
| sharky6999 | $480K | Statistical Arbitrage | 100+ markets/min scanning |
| JaneStreetIndia | $324K (25 days) | Temporal Arbitrage | 30-90 second price lag |
| distinct-baguette | $325K | Math Arbitrage | YES+NO < $1, risk-free |
| ScottyNooo | $300K+ | Low-Latency MM | Event-driven, delta-neutral |

---

## Tier 1: Primary Strategies (Highest Priority for Implementation)

### 1. Mathematical Arbitrage (Binary Markets)

**Risk Level:** LOW (not risk-free) | **Competition:** HIGH | **Speed Critical:** YES

**Mechanism:**

```
When YES + NO < $1.00 → Buy both sides → Profit at resolution IF both legs fill
```

**Real Example:**

```
YES price: $0.48
NO price:  $0.49
Combined:  $0.97
Buy both → Pay $0.97 → Receive $1.00 → Profit: $0.03 (3.1%)
```

> **IMPORTANT:** This is NOT "risk-free" on a CLOB. Profit is only guaranteed at resolution
> IF you successfully acquire both legs at intended prices/sizes. See "CLOB Execution Risks" below.

**Key Parameters (As Implemented):**
| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Entry Threshold | 1% min edge | After fees/slippage |
| Position Size | 5-15 shares | Dynamic based on $1 min order value |
| Max Exposure | $50 | Total position limit |
| Target Markets | 15-min crypto | BTC, ETH, SOL, XRP Up/Down |
| Order Type | **GTC (Maker)** | Zero fees, possible rebate |
| Execution | Parallel | Both legs via tokio::join! |

> **UPDATE (2026-01-08):** Originally planned FOK/FAK, but switched to GTC maker orders
> for zero fees on 15-min crypto markets. See STATE-OF-THE-BOT.md for current behavior.

**Implementation Priority:** HIGHEST - Foundation strategy, but requires proper execution handling

---

### 2. Temporal Arbitrage (Price Lag Exploitation)

**Risk Level:** MEDIUM | **Competition:** MEDIUM | **Speed Critical:** VERY HIGH

**Mechanism:**

```
Crypto spot moves → Polymarket lags 30-90 seconds → Buy mispriced certainty
```

**Real Metrics (JaneStreetIndia):**

- Win Rate: 99.5%
- Profit: $324K in 25 days
- Daily PnL: $5K-$33K
- Hold Time: 8-12 minutes average
- Return per Trade: 50-150%

**Key Parameters:**
| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Lag Window | 30-90 seconds | Time before Polymarket catches up |
| Gap Threshold | 3-5% | Between spot price and market price |
| Price Sources | Binance, Coinbase | Fastest spot feeds |
| Entry Timing | During volatility breaks | When direction confirms |

**Implementation Priority:** HIGH - Best risk-adjusted returns

---

### 3. Delta-Neutral Spread Capture

**Risk Level:** LOW | **Competition:** HIGH | **Speed Critical:** YES

**Mechanism:**

```
Hold both YES and NO simultaneously → Wait for spread dislocation → Profit on resolution
```

**Real Metrics:**

- 97.7% of trades are delta-neutral
- Combined cost target: < $0.94
- Daily PnL: $5K-$33K
- Green Days: 23/25

**Entry Strategy:**

1. Wait for volatility compression (both outcomes → 50¢)
2. Load up on both sides when combined < $0.94
3. Directional move hits → one leg resolves at $1
4. Profit = $1 - combined cost

---

### 4. Maker Rebate Arbitrage (NEW - Gabagool Strategy)

**Risk Level:** LOW | **Competition:** MEDIUM | **Speed Critical:** NO (passive)

**Source:** Twitter analysis of Gabagool bot - $300+/day from rebates alone

**Mechanism:**

```
Same YES + NO < $1.00 arbitrage, but executed as MAKER instead of TAKER
- Post limit orders instead of crossing the spread
- Pay ZERO fees (makers exempt on 15-min crypto)
- Receive rebates from taker fee pool (100% redistribution)
```

**Key Insight (Jan 2026 Fee Changes):**

- Polymarket introduced taker fees on 15-min crypto markets
- Takers pay up to ~3% at 50/50 odds
- Makers get 0% fees + rebates from collected fees
- **Meta shift: Speed → Liquidity provision**

**Real Metrics (Gabagool):**

- ~$58.5 profit per arbitrage trade
- ~$300/day additional from rebates
- Works as pure maker (GTC orders)

**Trade-offs vs Taker Arb:**
| Aspect | Taker | Maker (Gabagool) |
|--------|-------|------------------|
| Execution Speed | Immediate | Passive (wait for fill) |
| Fees | Pay ~3% at 50/50 | Zero |
| Rebates | None | Yes (from fee pool) |
| Fill Rate | High | Lower (queue priority) |
| Capital Efficiency | Higher | Lower (capital locked) |

**When to Use:**

- Markets with sufficient activity to fill passive orders
- When fee savings + rebates > opportunity cost of slower fills
- Can run alongside taker arb for diversification

**Implementation Priority:** HIGH - Uses existing MakerPolicy with MathArb logic

---

## Tier 2: Secondary Strategies (Medium Priority)

### 6. Multi-Outcome Arbitrage

**Target:** Markets with 3+ options (e.g., "Who will win?")

**Mechanism:**

```
All outcomes should sum to $1.00
When sum < $1.00 → Buy one of each → Guaranteed profit
```

**Example (Fed Rate Decision):**

```
Cut 50bp:   $0.001
Cut 25bp:   $0.008
No change:  $0.850
Raise 25bp: $0.100
Total:      $0.959
Profit:     $0.041 per set (4.1%)
```

**Lower competition** due to complexity of tracking multiple outcomes.

---

### 7. Market Making / Spread Farming

**Real Metrics (cry.eth2):** $181K profit, 1M trades, 22,790 markets

**Mechanism:**

1. Enter the SECOND a market opens
2. Capture mathematical best entry before retail
3. Risk: 1-3¢ per position
4. Upside: Growth to 99¢

**Key Insight:** No official AMM on Polymarket = opportunity for custom market making.

---

### 8. Capital Recycling (HFT Compounding)

**Real Metrics:** $120K in 30 days, 7,948 trades, 265 trades/day

**Mechanism:**

```
$4,000 in → $8,600 out (15 min) → Redeploy immediately → 20x/day
```

**Key Metrics:**

- BTC ROI: +169%
- ETH ROI: +227%
- Bet Size: $2K-$7K per window

---

## Tier 3: Niche Strategies (Lower Priority)

### 9. Esports Stream Delay (Highest Reported ROI)

**Real Metrics:** $50 → $230K (4,600x return in 2 months)

**Mechanism:**

- Twitch/YouTube delayed 30-40 seconds
- Game APIs provide real-time data
- Bot sees events before stream viewers

**APIs Required:**

- Riot API (League of Legends)
- Steam API (Dota 2, CS2)

**Complexity:** Requires game-specific knowledge

---

### 10. Weather Bucket Arbitrage

**Real Metrics:** $204 → $24K, 73% win rate, 1,300 trades

**Mechanism:** Exploit mispricing between adjacent temperature ranges

---

### 11. Long-Tail / Penny Stock Strategy

**Real Metrics:** +$960 from $0.001 bets

**Mechanism:** Buy YES at 0.1¢ across thousands of markets

---

## Critical Edge Analysis

### Why 15-Minute Crypto Markets?

1. **No Market Makers** - Thin liquidity, wide spreads (8-15 cents)
2. **Retail Guessing** - Price inefficiency from uninformed traders
3. **API vs UI Lag** - 3-5 second advantage for API traders
4. **Spot Price Lag** - 30-90 second delay from exchanges
5. **High Frequency** - New market every 15 minutes = compound opportunities

### How to Identify 15-Minute Crypto Markets Programmatically

The API does not expose a direct "resolution_interval" or "market_type" filter. Use these methods:

**Method 1: Fee Rate Check (Most Reliable)**

```
GET https://clob.polymarket.com/fee-rate?token_id={token_id}
- fee_rate_bps: 1000 → 15-min crypto market
- fee_rate_bps: 0    → Standard market
```

**Method 2: Gamma API Tag Filtering**

```bash
# Get crypto tag ID
curl "https://gamma-api.polymarket.com/tags"

# Filter active crypto markets
curl "https://gamma-api.polymarket.com/events?tag_id=<crypto_tag_id>&active=true&closed=false"
```

**Other High-Frequency Market Types:**

- Sports game bets: `tag_id=100639`
- Automated sports leagues: `/sports` endpoint
- Short resolution markets: Check `endDate` field

### Speed Hierarchy

```
Spot Price (Binance) → [30-90 sec lag] → Polymarket API → [3-5 sec lag] → Polymarket UI
                                              ↑
                                        YOUR BOT HERE
```

---

## Risk Factors

### Critical Risks

| Risk                 | Impact                     | Mitigation                              |
| -------------------- | -------------------------- | --------------------------------------- |
| Non-atomic execution | One leg filled, other not  | Handle partial fills, smaller positions |
| Threshold selection  | Wrong threshold = churning | Use dynamic edge calculation            |
| Thin order books     | Slippage on large orders   | Cap position sizes                      |
| Competition          | HFT bots race each other   | Optimize latency, unique markets        |

### Market Statistics (Academic Research)

- 70% of addresses never profit
- Only 0.51% have profits > $1,000
- 0.04% captured 70% of total profits
- $40M extracted by arbitrageurs (Apr 2024 - Apr 2025)

---

## CLOB Execution Risks (CRITICAL)

Mathematical arbitrage is **NOT risk-free** on a CLOB. The following risks must be handled:

### 1. Partial Fills

```
Scenario: You want to buy 100 YES + 100 NO
Result:   YES fills 100, NO fills only 40
Problem:  You now have directional exposure (60 unhedged YES)
```

**Mitigation:**

- Use FOK (Fill-Or-Kill) when possible - entire order fills or cancels
- Use FAK (Fill-And-Kill) with unwind logic
- Never use GTC for arb legs

### 2. Price Movement Between Legs

```
Scenario: YES ask = $0.48, NO ask = $0.49 (combined $0.97)
Action:   Submit YES order first
Result:   YES fills at $0.48
Problem:  NO ask moved to $0.54 while YES was filling
          Combined now = $1.02 (no longer profitable)
```

**Mitigation:**

- Submit both legs concurrently (tokio::join!)
- Pre-check book depth before execution
- Set maximum acceptable slippage

### 3. Queue Priority / Maker-Taker Dynamics

- Your order may be behind others at the same price
- Large orders may only partially fill at best price
- Remaining size fills at worse prices (slippage)

### 4. Tick Size & Minimum Size

- Prices must conform to tick size (usually $0.01)
- Orders below minimum size will be rejected
- Rounding can eat into edge

### 5. Rate Limits & WebSocket Gaps

- API rate limits: ~100 req/min REST, ~10 orders/sec
- WebSocket can miss updates during high volatility
- Stale book state → bad decisions

### 6. Fee Model & Rounding

- Maker/taker fees in basis points
- Fees reduce your edge
- Amount rounding (6 decimals) can affect P&L

---

## Partial Fill Handling Policy (MANDATORY)

When one arb leg fills and the other doesn't:

### Option A: Immediate Unwind (Safer)

```
1. YES leg fills, NO leg fails/partial
2. Check: Is combined edge still profitable?
3. If NO → Immediately sell YES at best bid
4. Accept small loss to avoid directional exposure
```

### Option B: Brief Hedge Window

```
1. YES leg fills, NO leg fails/partial
2. Allow 200-500ms retry window for NO leg
3. If still not filled → unwind YES position
4. Never hold unhedged position > 500ms
```

**Recommended:** Option A for MVP, Option B after system is stable

---

## Dynamic Edge Calculation (Replace Static Thresholds)

Instead of static `ARB_THRESHOLD = 0.97`, compute dynamically:

```rust
fn calculate_required_edge(
    yes_book: &OrderBook,
    no_book: &OrderBook,
    intended_size: Decimal,
) -> Decimal {
    let fees = get_fee_rate_bps() / 10000;  // e.g., 0.001
    let slippage = estimate_slippage(yes_book, no_book, intended_size);
    let partial_fill_risk = if is_thin_book(yes_book) || is_thin_book(no_book) {
        Decimal::new(1, 2)  // 1% extra margin for thin books
    } else {
        Decimal::new(5, 3)  // 0.5% for liquid books
    };
    let spread_penalty = calculate_spread_penalty(yes_book, no_book);

    fees + slippage + partial_fill_risk + spread_penalty
}

fn should_execute(yes_price: Decimal, no_price: Decimal, required_edge: Decimal) -> bool {
    let actual_edge = Decimal::ONE - (yes_price + no_price);
    actual_edge >= required_edge
}
```

**Default minimum edge:** 3 cents ($0.03), but adjust based on:

- Book depth (thin books need more margin)
- Recent fill rates (low fills = increase margin)
- Volatility (high vol = increase margin)

---

## Recommended Implementation Order

### Phase 1: Foundation (Weeks 1-2)

1. WebSocket listener for market data
2. Order book state management
3. Mathematical arbitrage detection (YES+NO < threshold)

### Phase 2: Core Trading (Weeks 2-3)

4. Order signing with ethers-rs
5. Order execution via CLOB API
6. Position tracking and P&L calculation

### Phase 3: Edge Enhancement (Weeks 3-4)

7. Binance/Coinbase price feeds
8. Temporal arbitrage detection
9. Delta-neutral position management

### Phase 4: Optimization (Ongoing)

10. Latency profiling and reduction
11. Capital recycling automation
12. Multi-market parallel scanning

---

## Key Numbers for Configuration

```rust
use rust_decimal::Decimal;

// Strategy thresholds (DYNAMIC - see calculate_required_edge())
const DEFAULT_MIN_EDGE: Decimal = Decimal::new(3, 2);     // $0.03 default minimum
const THIN_BOOK_EXTRA_MARGIN: Decimal = Decimal::new(1, 2); // +$0.01 for thin books
const TEMPORAL_GAP_PCT: Decimal = Decimal::new(3, 2);     // 3% spot vs market gap

// Timing
const POLL_INTERVAL_MS: u64 = 1500;        // 1-3 seconds
const PRICE_LAG_WINDOW_SEC: u64 = 90;      // Max lag window
const HOLD_TIME_MINUTES: u64 = 12;         // Average hold
const PARTIAL_FILL_UNWIND_MS: u64 = 500;   // Max time to hold unhedged position

// Position sizing
const MAX_BET_SIZE: Decimal = Decimal::new(5000, 0);      // Per window
const MAX_CAPITAL_PCT: Decimal = Decimal::new(40, 2);     // Max 40% per event
const CAPITAL_RECYCLES_PER_DAY: u32 = 20;  // Target throughput

// Risk limits (HARD GUARDRAILS)
const MAX_DAILY_LOSS: Decimal = Decimal::new(500, 0);     // Stop trading if hit
const MAX_OPEN_ORDERS: u32 = 10;           // Maximum concurrent orders
const MAX_UNHEDGED_EXPOSURE: Decimal = Decimal::new(100, 0); // Max directional exposure
const MAX_PARTIAL_FILL_EXPOSURE: Decimal = Decimal::new(200, 0); // Max from partial fills
```

---

## Sources

- JaneStreetIndia wallet analysis (14,822 trades)
- distinct-baguette arbitrage bot tracking
- cry.eth2 market making analysis
- Academic research on Polymarket arbitrage ($40M extracted)
- Multiple documented profitable bot strategies
