# API Discovery Findings

## Date: January 7, 2026

This document details critical findings from live API testing that affect market discovery and arb strategy implementation.

---

## 1. Summary of Issues Found

### 1.1 CLOB `/markets` Response Format

**Expected (in current code):**

```rust
pub async fn get_markets(&self) -> Result<Vec<MarketInfo>>
```

**Actual API Response:**

```json
{
  "data": [
    { "condition_id": "...", "tokens": [...] },
    { "condition_id": "...", "tokens": [...] }
  ]
}
```

**Problem:** Response is wrapped in `{ "data": [...] }`, not a direct array.

**Fix Required:** Add wrapper struct:

```rust
#[derive(Deserialize)]
struct MarketsResponse {
    data: Vec<MarketInfo>
}
```

---

### 1.2 CLOB `/market/{condition_id}` Returns 404 for 15-min Markets

**Tested:**

```bash
curl "https://clob.polymarket.com/market/0x7a8c9be27d64524fc00001cf41026ba734d5749c1ca7deaa89864199ae5fd892"
# Returns: 404 Not Found
```

But the same market works via:

```bash
curl "https://clob.polymarket.com/book?token_id=24987631508471554741620794727487253902047372973830655708822916260927541162666"
# Returns: Full order book ✅
```

**Conclusion:** 15-minute crypto markets are NOT accessible via the single market endpoint. Use Gamma API or book endpoint for discovery.

---

### 1.3 Fee Rate Endpoint Returns Different Field Name

**Expected (in current code):**

```rust
#[derive(Deserialize)]
pub struct FeeRateResponse {
    pub fee_rate_bps: u32,
}
```

**Actual API Response:**

```json
{ "base_fee": 1000 }
```

**Fix Required:**

```rust
#[derive(Deserialize)]
pub struct FeeRateResponse {
    #[serde(alias = "base_fee")]
    pub fee_rate_bps: u32,
}
// OR rename field to base_fee
```

---

### 1.4 Gamma API Uses Stringified JSON for Arrays

**Gamma Response:**

```json
{
  "conditionId": "0x7a8c9be27d64524fc00001cf41026ba734d5749c1ca7deaa89864199ae5fd892",
  "outcomes": "[\"Up\", \"Down\"]",
  "clobTokenIds": "[\"24987631508471554741620794727487253902047372973830655708822916260927541162666\", \"31419718950412866993454632142323756427503490705613267285150156809183569033252\"]"
}
```

**Note:** `outcomes` and `clobTokenIds` are **strings containing JSON**, not actual arrays!

**Fix Required:** Parse these as strings, then deserialize:

```rust
#[derive(Deserialize)]
pub struct GammaMarket {
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    // These are JSON strings that need secondary parsing
    pub outcomes: String,        // "[\"Up\", \"Down\"]"
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: String,  // "[\"token1\", \"token2\"]"
}

// Usage:
let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)?;
let token_ids: Vec<String> = serde_json::from_str(&market.clob_token_ids)?;
```

---

### 1.5 15-Min Crypto Markets Use "Up"/"Down" NOT "Yes"/"No"

**Standard Binary Markets:**

```json
"tokens": [
  { "token_id": "...", "outcome": "Yes" },
  { "token_id": "...", "outcome": "No" }
]
```

**15-Min Crypto Markets:**

```json
"outcomes": "[\"Up\", \"Down\"]"
```

**Fix Required:** Strategy must handle both:

- `"Yes"/"No"` → Standard binary markets
- `"Up"/"Down"` → 15-minute crypto markets

Both are functionally equivalent for arb strategy:

- First token (Yes/Up) = "positive" outcome
- Second token (No/Down) = "negative" outcome

---

## 2. API Endpoints Summary

### 2.1 CLOB API (https://clob.polymarket.com)

| Endpoint                       | Status | Notes                                 |
| ------------------------------ | ------ | ------------------------------------- |
| `GET /markets`                 | ⚠️     | Returns `{ "data": [...] }` wrapper   |
| `GET /market/{id}`             | ❌     | Returns 404 for 15-min markets        |
| `GET /book?token_id=X`         | ✅     | Works correctly                       |
| `GET /price?token_id=X&side=Y` | ✅     | Works correctly                       |
| `GET /fee-rate?token_id=X`     | ⚠️     | Returns `base_fee` not `fee_rate_bps` |
| `GET /time`                    | ✅     | Works correctly                       |
| `POST /order`                  | ✅     | Works correctly                       |
| `GET /orders`                  | ✅     | Works correctly                       |

### 2.2 Gamma API (https://gamma-api.polymarket.com)

| Endpoint                               | Status | Notes                          |
| -------------------------------------- | ------ | ------------------------------ |
| `GET /events?active=true&closed=false` | ✅     | Returns all active events      |
| `GET /events?slug={slug}`              | ✅     | Returns specific event by slug |
| `GET /tags`                            | ✅     | Returns all tags/categories    |

**Key Gamma Fields:**

- `event.markets[].conditionId` - Market condition ID
- `event.markets[].clobTokenIds` - **Stringified JSON array** of token IDs
- `event.markets[].outcomes` - **Stringified JSON array** of outcomes
- `event.markets[].volume24hr` - 24h trading volume
- `event.markets[].active` - Is market active

---

## 3. Live Market Analysis: BTC 15-Min

**Event URL:** https://polymarket.com/event/btc-updown-15m-1767758400

**Gamma API Query:**

```bash
curl "https://gamma-api.polymarket.com/events?slug=btc-updown-15m-1767758400"
```

**Response (key fields):**

```json
{
  "title": "Bitcoin Up or Down - January 6, 11:00PM-11:15PM ET",
  "markets": [
    {
      "conditionId": "0x7a8c9be27d64524fc00001cf41026ba734d5749c1ca7deaa89864199ae5fd892",
      "outcomes": "[\"Up\", \"Down\"]",
      "clobTokenIds": "[\"24987631508471554741620794727487253902047372973830655708822916260927541162666\", \"31419718950412866993454632142323756427503490705613267285150156809183569033252\"]"
    }
  ]
}
```

**Order Books (live):**

```
UP Token:   Best Bid: $0.77  |  Best Ask: $0.78
DOWN Token: Best Bid: $0.22  |  Best Ask: $0.23
```

**Fee Rate:** `base_fee: 1000` (10% = 1000 bps)

**Arb Check:**

- Combined Ask: $0.78 + $0.23 = $1.01
- No arb opportunity at this moment (combined > $1.00)

---

## 4. Discovery Strategy

### 4.1 For 15-Minute Crypto Markets

**Option A: Gamma API Discovery (Recommended)**

1. Query `GET /events?active=true&closed=false`
2. Filter for events with titles containing "Up or Down" or "15m"
3. Parse `clobTokenIds` and `outcomes` from stringified JSON
4. Build `MarketPairRegistry` with Up/Down token pairs

**Option B: WebSocket-Based Discovery**

1. Subscribe to known token IDs via Market WebSocket
2. When receiving `book` event, note the `market` (condition_id)
3. Build registry dynamically from observed markets

### 4.2 For Standard Binary Markets (Yes/No)

1. Query `GET /markets` from CLOB API
2. Filter for `tokens.length == 2` and outcomes "Yes"/"No"
3. Register in `MarketPairRegistry`

---

## 5. Recommended Fixes

### Phase 7.5: API Fix Implementation

| File                          | Fix Required                                |
| ----------------------------- | ------------------------------------------- |
| `src/api/endpoints.rs`        | Add `MarketsResponse` wrapper struct        |
| `src/api/endpoints.rs`        | Fix `FeeRateResponse` field name            |
| `src/api/gamma.rs` (NEW)      | New Gamma API client module                 |
| `src/strategy/market_pair.rs` | Support "Up"/"Down" outcomes                |
| `src/main.rs`                 | Replace hardcoded tokens with API discovery |

### Implementation Order:

1. Fix CLOB endpoints (wrapper struct, fee field)
2. Add Gamma API client
3. Add market discovery function
4. Update main.rs to use discovery
5. Update MarketPair to handle Up/Down

---

## 6. API Discovery Code (Target Implementation)

```rust
/// Discover active 15-min crypto markets from Gamma API
pub async fn discover_crypto_15min_markets() -> Result<Vec<MarketPair>> {
    let client = reqwest::Client::new();
    let events: Vec<GammaEvent> = client
        .get("https://gamma-api.polymarket.com/events")
        .query(&[("active", "true"), ("closed", "false"), ("limit", "100")])
        .send()
        .await?
        .json()
        .await?;

    let mut pairs = Vec::new();

    for event in events {
        // Filter for 15-min crypto markets
        if !event.title.contains("Up or Down") {
            continue;
        }

        for market in event.markets {
            // Parse stringified JSON arrays
            let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)?;
            let token_ids: Vec<String> = serde_json::from_str(&market.clob_token_ids)?;

            if outcomes.len() == 2 && token_ids.len() == 2 {
                let pair = MarketPair::new(
                    market.condition_id,
                    token_ids[0].clone(),  // Up/Yes token
                    token_ids[1].clone(),  // Down/No token
                )
                .with_fee_rate(1000)  // 15-min markets have 10% fee
                .with_description(&event.title);

                pairs.push(pair);
            }
        }
    }

    Ok(pairs)
}
```

---

## 7. Testing Commands

```bash
# Check fee rate for a token
curl "https://clob.polymarket.com/fee-rate?token_id=TOKEN_ID"

# Get order book for a token
curl "https://clob.polymarket.com/book?token_id=TOKEN_ID"

# Find active crypto markets via Gamma
curl "https://gamma-api.polymarket.com/events?active=true&closed=false" | jq '[.[] | select(.title | test("Up or Down"))]'

# Get specific event by slug
curl "https://gamma-api.polymarket.com/events?slug=btc-updown-15m-TIMESTAMP"
```
