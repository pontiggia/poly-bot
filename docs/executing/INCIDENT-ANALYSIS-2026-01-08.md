# Incident Analysis: Execution Failure Leading to $4 Loss

**Date**: January 8, 2026  
**Session**: Live trading session (BTC 15-min crypto markets)  
**Outcome**: $4 USD loss due to unhedged exposure  
**Root Cause**: API precision validation mismatch (400 Bad Request)

---

## A) Session Timeline and Execution Matrix

### Summary of the Session

| Metric                           | Value                    |
| -------------------------------- | ------------------------ |
| Total Arb Opportunities Detected | 4                        |
| Expected Executions              | 8 (4 arbs × 2 legs each) |
| Actual Successful Executions     | 5                        |
| Failed Submissions (400 error)   | 3                        |
| Final YES Position               | +24.38 shares (UP)       |
| Final NO Position                | +15 shares (DOWN)        |

### Detailed Arb Execution Matrix

#### Arb #1 — 17:22:25 (SUCCESSFUL ✅)

- **Market**: `0xf0abda9056d4210bbbeb16ffc154f86b48b4a8234b95cecd636c75d995d63165`
- **Edge**: 1.0%, Trade size: 15 shares
- **LEG 1 (YES)**: Buy @ $0.60 × 15 → `0x5ebeb4cf28...` → **PENDING → FILLED** @ 17:22:37
- **LEG 2 (NO)**: Buy @ $0.37 × 15 → `0x67f35e7b33...` → **FULLY FILLED immediately**
- **Result**: ✅ Both legs filled, arb complete

#### Arb #2 — 17:23:34 (SUCCESSFUL ✅)

- **Market**: `0xf0abda9056d4210bbbeb16ffc154f86b48b4a8234b95cecd636c75d995d63165`
- **Edge**: 1.0%, Trade size: 15 shares
- **LEG 1 (YES)**: Buy @ $0.41 × 15 → `0x924fa59e59...` → **FILLED** (8.2 + 6.8 = 15)
- **LEG 2 (NO)**: Buy @ $0.56 × 15 → `0xd6538b64ab...` → **PENDING → PARTIAL FILL** (3.07 + 11.93)
- **Result**: ✅ Both legs filled, arb complete

#### Arb #3 — 17:24:21 (PARTIAL FAILURE ⚠️)

- **Market**: `0xf0abda9056d4210bbbeb16ffc154f86b48b4a8234b95cecd636c75d995d63165`
- **Edge**: 1.0%, Trade size: 9.38 shares
- **LEG 1 (YES)**: Buy @ $0.50 × 9.38 → `0x111d87e35a...` → **FILLED** @ 17:24:26
- **LEG 2 (NO)**: Buy @ $0.47 × 9.38 → **400 REJECTED** ❌

**Error Message**:

```
invalid amounts, the maker amount for a $0.469 order of size 9.38 should be '4.3992' but the value submitted is '4.4'
```

- **Result**: ⚠️ **ONE-LEGGED EXPOSURE** — 9.38 YES shares unhedged

#### Arb #4 — 17:25:18 (COMPLETE FAILURE ❌)

- **Market**: `0xb20852fde6d608ea5cbf18724f8213863345d551c5a84e1e80480406cb48513e` (Solana)
- **Edge**: 1.0%, Trade size: 5.5069 shares
- **LEG 1 (YES)**: Buy @ $0.23 × 5.5069 → **400 REJECTED** ❌
- **LEG 2 (NO)**: Buy @ $0.74 × 5.5069 → **400 REJECTED** ❌

**Error Message**:

```
invalid amounts, the buy orders maker amount supports a max accuracy of 4 decimals, taker amount a max of 2 decimals
```

- **Result**: ❌ Both legs failed (no exposure created)

---

## B) Exposure Post-Mortem

### Final Inventory Breakdown

| Token Type | Shares                                          | Source        |
| ---------- | ----------------------------------------------- | ------------- |
| YES (UP)   | 15 (Arb1) + 15 (Arb2) + 9.38 (Arb3) = **39.38** | 3 filled legs |
| NO (DOWN)  | 15 (Arb1) + 15 (Arb2) = **30**                  | 2 filled legs |

**Net Exposure**: 9.38 shares **LONG YES** (unhedged from Arb #3)

### Why the Inventory Stacked

1. **All arbs were on the SAME market** (`0xf0abda905...`) for BTC 12:15-12:30 timeframe
2. Arb #1 and #2 completed successfully → net position = 0 (30 YES, 30 NO)
3. Arb #3's NO leg (the hedge) was **rejected by the API** due to precision error
4. The YES leg of Arb #3 was placed successfully and filled
5. **Result**: 9.38 extra YES shares with no corresponding NO hedge

### Why the Loss Occurred

- The market resolved **DOWN** (NO outcome won)
- 30 NO shares paid out at $1 each = $30 (cost was ~$0.37-0.56 each = ~$14)
- 39.38 YES shares worth $0 (cost was ~$0.41-0.60 each = ~$20)
- The unhedged 9.38 YES shares became worthless → **~$4 loss**

---

## C) 400 Error Diagnosis

### The Root Cause: Precision Rounding Mismatch

Based on research of [Polymarket rs-clob-client PR #116](https://github.com/Polymarket/rs-clob-client/pull/116) and [Issue #114](https://github.com/Polymarket/rs-clob-client/issues/114):

#### Polymarket API Precision Requirements

| Field                  | Side: BUY         | Side: SELL               |
| ---------------------- | ----------------- | ------------------------ |
| `makerAmount` (USDC)   | **2 decimal max** | 4 decimal max (shares)   |
| `takerAmount` (shares) | 4 decimal max     | **2 decimal max** (USDC) |

**Key Insight**: USDC amounts must always have **max 2 decimal places** (divisible by 10,000 in 6-decimal base units).

### Your Current Code (signing/order.rs:418-438)

```rust
fn calculate_amounts(side: Side, price: Decimal, size: Decimal) -> (String, String) {
    let scale = Decimal::from(1_000_000u64); // 6 decimals base unit
    let maker_precision = Decimal::from(10_000u64); // 2 decimal places for maker
    let taker_precision = Decimal::from(100u64); // 4 decimal places for taker

    match side {
        Side::Buy => {
            // BUY: makerAmount = USDC (2 decimals), takerAmount = shares (4 decimals)
            let maker_raw = (size * price * scale).trunc();
            let maker_amount = (maker_raw / maker_precision).trunc() * maker_precision;
            // ...
        }
```

### The Bug

The code correctly identifies that for BUY orders:

- `makerAmount` = USDC → 2 decimal max
- `takerAmount` = shares → 4 decimal max

**BUT** the actual calculation is producing inconsistent results:

For Arb #3's NO leg:

- Price: $0.47, Size: 9.38 shares
- USDC = 9.38 × 0.47 = 4.4086
- Your code: `4400000` (base units) = $4.40
- API expected: `4399200` (base units) = $4.3992

**The Issue**: The API's validation uses a different rounding approach. It calculates the **exact** USDC amount using the tick-aligned price ($0.469, not $0.47) and expects that to be rounded to exactly 2 decimals.

### The REAL Problem: Price/Amount Relationship

Polymarket's API enforces that:

```
makerAmount = floor(price × size × 10^6 / 10^4) × 10^4
```

But the price used must be the **actual tick-aligned price** the API sees, not a rounded display price.

For the failing order:

- Your submitted: `makerAmount=4400000` ($4.40)
- API calculated with $0.469 price: 9.38 × 0.469 = 4.3992 → `4399200`

---

## D) Fix Plan

### 1. Primary Fix: Correct Amount Calculation (signing/order.rs)

**File**: `src/signing/order.rs`  
**Function**: `calculate_amounts`

The fix must ensure USDC amounts (makerAmount for BUY, takerAmount for SELL) are truncated to 2 decimal places in the final dollar value:

```rust
/// Calculate maker and taker amounts based on side, price, and size
///
/// Polymarket API precision requirements:
/// - USDC amounts: exactly 2 decimal places (divisible by 10,000 in base units)
/// - Share amounts: up to 2 decimal places (divisible by 10,000 in base units)
fn calculate_amounts(side: Side, price: Decimal, size: Decimal) -> (String, String) {
    let scale = Decimal::from(1_000_000u64); // 6 decimals base unit
    let precision = Decimal::from(10_000u64); // 2 decimal places = divisible by 10,000

    // CRITICAL: Both maker and taker amounts must be divisible by 10,000
    // This ensures max 2 decimal places for both USDC and share amounts

    match side {
        Side::Buy => {
            // BUY: makerAmount = USDC to spend, takerAmount = shares to receive
            // Both truncated to 2 decimals
            let usdc_raw = (size * price * scale).trunc();
            let usdc = (usdc_raw / precision).trunc() * precision;

            let shares_raw = (size * scale).trunc();
            let shares = (shares_raw / precision).trunc() * precision;

            (usdc.to_string(), shares.to_string())
        }
        Side::Sell => {
            // SELL: makerAmount = shares to sell, takerAmount = USDC to receive
            // Both truncated to 2 decimals
            let shares_raw = (size * scale).trunc();
            let shares = (shares_raw / precision).trunc() * precision;

            let usdc_raw = (size * price * scale).trunc();
            let usdc = (usdc_raw / precision).trunc() * precision;

            (shares.to_string(), usdc.to_string())
        }
    }
}
```

### 2. Secondary Fix: Size Pre-Rounding in Strategy

**File**: `src/strategy/arbitrage.rs` (or wherever trade size is calculated)

Round trade sizes to 2 decimal places BEFORE passing to order building:

```rust
// Before creating OrderIntent
let size = calculated_size.round_dp(2); // 2 decimal places for shares
```

This ensures sizes like `5.5069` become `5.50`, preventing fractional precision issues.

### 3. Safety Mechanism: Paired Execution with Rollback

**File**: `src/execution/executor.rs`

The current `execute_grouped` already has logic to cancel leg 1 if leg 2 fails, BUT the critical issue is:

**Problem**: Orders are submitted concurrently in `execute_batch`, so by the time leg 2 fails, leg 1 may already be filled.

**Solution**: Force sequential submission for arb pairs:

```rust
pub async fn execute_grouped(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
    if intents.len() != 2 {
        return self.execute_batch(intents).await;
    }

    // ALWAYS execute first, then second (never concurrent for arbs)
    let result1 = self.execute(&intents[0]).await;

    // If first leg failed OR was immediately filled, don't proceed
    if result1.status == ExecutionStatus::SubmissionFailed
        || result1.status == ExecutionStatus::CircuitOpen {
        // Skip second leg
        warn!("First leg failed, skipping second leg");
        return vec![result1, self.skip_result(&intents[1])];
    }

    // Execute second leg
    let result2 = self.execute(&intents[1]).await;

    // CRITICAL: If second leg failed, we MUST cancel first leg
    if result2.status == ExecutionStatus::SubmissionFailed
        || result2.status == ExecutionStatus::Rejected {

        if let Some(ref order_id) = result1.order_id {
            // Cancel immediately - don't wait
            self.cancel_order_sync(order_id).await;
        }
    }

    vec![result1, result2]
}
```

### 4. Enhanced Logging

Add payload hash and key inputs for debugging:

```rust
// In api/endpoints.rs before order submission
warn!(
    order_id = %order.salt,
    maker_amount = %order.maker_amount,
    taker_amount = %order.taker_amount,
    price = %params.price,
    size = %params.size,
    usdc_calc = %(params.price * params.size),
    "Submitting order"
);
```

---

## Verification Checklist for Next Live Session

### Pre-Session

- [ ] Code changes deployed
- [ ] Run unit tests for `calculate_amounts` with edge cases:
  - 9.38 × 0.47 should produce makerAmount divisible by 10,000
  - 5.5069 × 0.23 should produce makerAmount divisible by 10,000
  - 15 × 0.37 (clean numbers) still work

### During Session

- [ ] Monitor for 400 errors in logs
- [ ] Verify all arb groups register (no "Could not register arb group" warnings)
- [ ] Heartbeat shows `execs` == `intents` for arb pairs

### Post-Session

- [ ] Verify final inventory is balanced (YES ≈ NO for each market)
- [ ] No orphaned positions from one-legged arbs
- [ ] All arb groups marked as complete

---

## Summary

| Issue              | Root Cause                                                | Fix                                                                   |
| ------------------ | --------------------------------------------------------- | --------------------------------------------------------------------- |
| 400 Bad Request    | USDC amount had too much precision (4.40 vs 4.3992)       | Truncate both makerAmount and takerAmount to 2 decimal places         |
| Unhedged exposure  | Failed NO leg couldn't be placed, YES leg already pending | Force sequential execution + immediate cancel on second leg failure   |
| Stacking positions | Same market traded multiple times                         | N/A (expected behavior), but precision fix prevents one-legged trades |

**Most Likely Root Cause**: The `calculate_amounts` function was applying different precision rules (2 decimals for maker, 4 for taker) when Polymarket's API actually requires **2 decimal places for BOTH** amounts.

The PR #116 fix confirms this: the official client now uses `trunc_with_scale(2)` for USDC regardless of side.
