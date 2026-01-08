# Polymarket CLOB API Reference

## Complete Implementation Guide for Rust Client

---

> ⚠️ **IMPORTANT UPDATE (2026-01-07):** Live API testing revealed several issues with response formats.
> See `planning/05-API-DISCOVERY-FINDINGS.md` for critical fixes needed:
>
> - CLOB `/markets` returns wrapped `{ "data": [...] }` response
> - CLOB `/market/{id}` returns 404 for 15-min crypto markets
> - Fee endpoint returns `base_fee` not `fee_rate_bps`
> - Gamma API uses stringified JSON for `outcomes` and `clobTokenIds`
> - 15-min crypto markets use "Up"/"Down" not "Yes"/"No"

---

## 1. WebSocket Endpoints

### 1.1 MARKET WebSocket (Public - Order Books)

**URL:** `wss://ws-subscriptions-clob.polymarket.com/ws/market`

**Subscription Message:**

```json
{
  "assets_ids": [
    "9116770648672617954873502150906669109632013173266181691206825621739241400461"
  ],
  "type": "market"
}
```

**Order Book Response:**

```json
{
  "market": "0x2e94bb8dd09931d12e6e656fe4fe6ceb3922bc3d6eab864bb6cd24773cf67269",
  "asset_id": "9116770648672617954873502150906669109632013173266181691206825621739241400461",
  "timestamp": "1767642862089",
  "hash": "86cc81dbf4d5aef952e924a78cf51556b4ca6af5",
  "bids": [
    { "price": "0.01", "size": "1027313.8" },
    { "price": "0.02", "size": "129350.81" }
  ],
  "asks": [
    { "price": "0.99", "size": "1979.07" },
    { "price": "0.98", "size": "3500" }
  ],
  "event_type": "book",
  "last_trade_price": "0.330"
}
```

---

### 1.2 USER WebSocket (Private - Trade Notifications)

**URL:** `wss://ws-subscriptions-clob.polymarket.com/ws/user`

**Authentication Message (send immediately after connect):**

```json
{
  "auth": {
    "apiKey": "da85d242-0a7d-6a65-bb82-fb7d9a1c7c14",
    "secret": "pkM5Pzu8fqLKE-sQUosetIQMQcz8sX2dcdz_2iZE6kU=",
    "passphrase": "184fbb107bc28939589d7432df646bf2e5ed942c91a0b0b1be785380c90cd6cc"
  },
  "markets": [],
  "type": "user"
}
```

**Trade Notification:**

```json
{
  "type": "TRADE",
  "id": "3565d9bd-5620-4c70-ab54-28b521dc4ef4",
  "taker_order_id": "0x00e477575fcdb390dfb3b0e4e5c0ec7321e289219b7d6075dfdca203124eb455",
  "market": "0x81d8e0447f946c753a1efad76eef9aa46ca33e14e86c1f50ae4377984b2d12d5",
  "asset_id": "1626193495054297080765627917808881174024341691464027194829514041992558860552",
  "side": "BUY",
  "size": "1.12359",
  "fee_rate_bps": "0",
  "price": "0.89",
  "status": "MATCHED",
  "match_time": "1767658657",
  "last_update": "1767658658",
  "outcome": "No",
  "owner": "da85d242-0a7d-6a65-bb82-fb7d9a1c7c14",
  "trade_owner": "da85d242-0a7d-6a65-bb82-fb7d9a1c7c14",
  "maker_address": "0x838E1cd51542c848E568aaD7dfe682291284E648",
  "transaction_hash": "0x40347bc4aaed2eb48bb548fab94fee3a9fc9071eb2c24527c3c69c6f03dfd2e5",
  "bucket_index": 0,
  "maker_orders": [
    {
      "order_id": "0x4815c07c2a7445ef6c044ee56840a0b43ca0cb57d2184be067fc96d4500c0dca",
      "owner": "147110e7-4f84-2ad1-9737-d462ef4f6e87",
      "maker_address": "0x724db3C436dCc7b26FbE1ae0c0d6aF538B588DeA",
      "matched_amount": "1.12359",
      "price": "0.1100000890004361",
      "fee_rate_bps": "0",
      "asset_id": "60535924391853275297967753306794300787165161956262993483858770165742288324173",
      "outcome": "Yes",
      "outcome_index": 0,
      "side": "BUY"
    }
  ],
  "trader_side": "TAKER",
  "timestamp": "1767658658035",
  "event_type": "trade"
}
```

---

### 1.3 LIVE DATA WebSocket (Public - Activity Feed)

**URL:** `wss://ws-live-data.polymarket.com/`

**Subscription Message:**

```json
{
  "action": "subscribe",
  "subscriptions": [
    {
      "topic": "activity",
      "type": "orders_matched",
      "filters": "{\"event_slug\":\"khamenei-out-as-supreme-leader-of-iran-by-june-30-747\"}"
    }
  ]
}
```

**Activity Response:**

```json
{
  "connection_id": "WuiFwd4ULPECH7A=",
  "payload": {
    "asset": "9116770648672617954873502150906669109632013173266181691206825621739241400461",
    "conditionId": "0x2e94bb8dd09931d12e6e656fe4fe6ceb3922bc3d6eab864bb6cd24773cf67269",
    "eventSlug": "khamenei-out-as-supreme-leader-of-iran-by-june-30-747",
    "name": "FuriusCamillus",
    "outcome": "Yes",
    "outcomeIndex": 0,
    "price": 0.3499998774999204,
    "proxyWallet": "0x7d85544513634fdb91f7AA08864a0d988d61183E",
    "side": "BUY",
    "size": 2.857141,
    "timestamp": 1767642931,
    "title": "Khamenei out as Supreme Leader of Iran by June 30?",
    "transactionHash": "0x842bf52f945984cbc2e9ce01fba589c4bbf50846cfa901282506c85cd48ee791"
  },
  "timestamp": 1767642931668,
  "topic": "activity",
  "type": "orders_matched"
}
```

---

## 2. REST API Endpoints

### Base URL: `https://clob.polymarket.com`

### Public Endpoints (No Auth Required)

| Method | Endpoint       | Description                          |
| ------ | -------------- | ------------------------------------ |
| GET    | `/markets`     | List all markets                     |
| GET    | `/market/{id}` | Get specific market                  |
| GET    | `/price`       | Get current price                    |
| GET    | `/book`        | Get order book                       |
| GET    | `/midpoint`    | Get midpoint price                   |
| GET    | `/trades`      | Get trade history                    |
| GET    | `/time`        | Server time (for connection warming) |

### Authenticated Endpoints

| Method | Endpoint         | Description             |
| ------ | ---------------- | ----------------------- |
| GET    | `/orders`        | Get your open orders    |
| POST   | `/order`         | Place an order          |
| DELETE | `/order/{id}`    | Cancel an order         |
| GET    | `/notifications` | Get trade notifications |

---

## 3. Authentication Headers

All authenticated requests require these headers:

| Header            | Format            | Example                                                            |
| ----------------- | ----------------- | ------------------------------------------------------------------ |
| `poly_address`    | 0x + 40 hex chars | `0xBeeD38951e4Bb9205b461560a6479763157c606e`                       |
| `poly_api_key`    | UUID              | `da85d242-0a7d-6a65-bb82-fb7d9a1c7c14`                             |
| `poly_passphrase` | 64 hex chars      | `184fbb107bc28939589d7432df646bf2e5ed942c91a0b0b1be785380c90cd6cc` |
| `poly_signature`  | Base64            | `_wQeobJEHYuu6GwMKVLPUN3l1OsdvyR5BPiXtxr4psc=`                     |
| `poly_timestamp`  | Unix seconds      | `1767658169`                                                       |

### Signature Generation

```rust
// HMAC-SHA256 of: timestamp + method + path + body
fn generate_signature(
    secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    let key = base64::decode(secret).unwrap();
    let mut mac = HmacSha256::new_from_slice(&key).unwrap();
    mac.update(message.as_bytes());
    base64::encode(mac.finalize().into_bytes())
}
```

---

## 4. Order Placement

### POST /order

**Request Body:**

```json
{
  "deferExec": false,
  "order": {
    "salt": 903656186495,
    "maker": "0x838E1cd51542c848E568aaD7dfe682291284E648",
    "signer": "0xBeeD38951e4Bb9205b461560a6479763157c606e",
    "taker": "0x0000000000000000000000000000000000000000",
    "tokenId": "9116770648672617954873502150906669109632013173266181691206825621739241400461",
    "makerAmount": "1000000",
    "takerAmount": "3225800",
    "side": "BUY",
    "expiration": "0",
    "nonce": "0",
    "feeRateBps": "0",
    "signatureType": 1,
    "signature": "0xef492cc07edff126387feb91964f675232913443c08c9728e916fe52ac82229b19c1787111687deb576636635558756a0804a00d57fb5495b1fd18efa9d321501b"
  },
  "owner": "da85d242-0a7d-6a65-bb82-fb7d9a1c7c14",
  "orderType": "FAK"
}
```

### Order Fields Explained

| Field           | Type    | Description                    |
| --------------- | ------- | ------------------------------ |
| `salt`          | u64     | Random nonce for uniqueness    |
| `maker`         | Address | Proxy wallet (trading address) |
| `signer`        | Address | EOA signer (API key address)   |
| `taker`         | Address | `0x0...0` for any taker        |
| `tokenId`       | String  | Asset ID (256-bit integer)     |
| `makerAmount`   | String  | USDC amount (6 decimals)       |
| `takerAmount`   | String  | Token amount (6 decimals)      |
| `side`          | String  | `"BUY"` or `"SELL"`            |
| `expiration`    | String  | Unix timestamp or `"0"`        |
| `nonce`         | String  | Order nonce (usually `"0"`)    |
| `feeRateBps`    | String  | Fee in basis points            |
| `signatureType` | u8      | `1` = EOA, `2` = Gnosis Safe   |
| `signature`     | String  | EIP-712 signature (132 chars)  |

### Amount Calculations

```
For BUY orders:
- makerAmount = USDC to spend (6 decimals)
- takerAmount = tokens to receive (6 decimals)
- price = makerAmount / takerAmount

Example: Buy at $0.31
- makerAmount: "1000000" (1 USDC)
- takerAmount: "3225800" (3.2258 tokens)
- price: 1000000 / 3225800 = 0.31
```

### Order Types

| Type  | Description                                          |
| ----- | ---------------------------------------------------- |
| `FAK` | Fill And Kill - execute immediately, cancel unfilled |
| `GTC` | Good Till Cancel - remain on book until filled       |
| `FOK` | Fill Or Kill - execute entire order or cancel        |

### Response

```json
{
  "errorMsg": "",
  "orderID": "0x980ff3499765b76c43acae02cb92f9407c11eed9e49e140212574d43e166fe72",
  "takingAmount": "3.225805",
  "makingAmount": "0.999999",
  "status": "matched",
  "transactionsHashes": [
    "0x8a66f2cb7525fa21eb92e3f13e4d346b1b0d75c517fc7df551a9e2b9ac08c152"
  ],
  "success": true
}
```

---

## 5. Gamma API (Market Metadata)

### Base URL: `https://gamma-api.polymarket.com`

| Method | Endpoint   | Description                        |
| ------ | ---------- | ---------------------------------- |
| GET    | `/markets` | Market metadata with categories    |
| GET    | `/events`  | Event information with filtering   |
| GET    | `/tags`    | List all available tags/categories |
| GET    | `/sports`  | Automated sports league markets    |

### Market Filtering Parameters

The `/events` endpoint supports filtering by:

| Parameter   | Description                               | Example                  |
| ----------- | ----------------------------------------- | ------------------------ |
| `tag_id`    | Filter by category (crypto, sports, etc.) | `tag_id=<crypto_tag_id>` |
| `active`    | Only active/tradeable markets             | `active=true`            |
| `closed`    | Filter by closed status                   | `closed=false`           |
| `series_id` | Filter by sports league                   | `series_id=<league_id>`  |
| `order`     | Sort results                              | `order=startTime`        |

**Example: Get active crypto markets:**

```bash
# Step 1: Get crypto tag ID
curl "https://gamma-api.polymarket.com/tags"

# Step 2: Filter markets by crypto tag
curl "https://gamma-api.polymarket.com/events?tag_id=<crypto_tag_id>&active=true&closed=false"
```

**Sports game bets:** Use `tag_id=100639` for game bets (vs futures).

---

## 5.1 Fee Rate Endpoint (CLOB)

### GET /fee-rate

Identifies market type by fee structure. **Critical for finding 15-min crypto markets.**

**Request:**

```
GET https://clob.polymarket.com/fee-rate?token_id={token_id}
```

**Response:**

```json
{
  "fee_rate_bps": 1000
}
```

**Fee Rate Meanings:**
| fee_rate_bps | Market Type |
|--------------|-------------|
| `1000` | 15-minute crypto markets (high-frequency) |
| `0` | Standard markets (maker rebate) |

**Note:** There is no direct "resolution_interval" or "market_type" filter in the API. Use fee rates or tag patterns to identify high-frequency markets empirically.

---

## 5.2 Fee Structure (VALIDATED - Research-1, Jan 2026)

### Current Fee Structure

| Market Type                  | Maker Fee         | Taker Fee            | Notes                                                     |
| ---------------------------- | ----------------- | -------------------- | --------------------------------------------------------- |
| **Standard Markets**         | 0 bps             | 0 bps                | Most prediction markets                                   |
| **15-Min Crypto Markets**    | 0 bps (+ rebates) | Up to ~300 bps (~3%) | Fee varies by odds - highest at 50/50, drops near 0%/100% |
| **Polymarket U.S. Exchange** | 0 bps (+ rewards) | 1 bps (0.01%)        | Flat fee structure                                        |

### Key Points

1. **Maker Rebate Program**: Taker fees from 15-min crypto markets fund rebates to liquidity providers (makers)
2. **Dynamic Fees**: Crypto market taker fees are NOT flat - they vary based on current odds
3. **Zero Fees for Most**: Primary platform remains fee-free for most participants

### Implications for Bot Strategy

- **Math arb on 15-min crypto**: Must account for ~3% taker fee at 50/50 odds
- **Edge threshold calculation**: `required_edge = base_edge + taker_fee_bps/10000`
- **Maker strategy advantage**: Zero fees + rebates make passive strategies more attractive on crypto markets

---

## 5.3 Order Book Depth Analysis (VALIDATED - Research-1, Jan 2026)

### Observed Depth by Market Type

| Market Type               | Example            | Bid Levels | Ask Levels | Notes                               |
| ------------------------- | ------------------ | ---------- | ---------- | ----------------------------------- |
| **Political (Long-term)** | Venezuela invasion | 47         | 147        | Deep books, good liquidity          |
| **Monetary Policy**       | Fed rate decision  | 5          | 76         | Concentrated at extremes (0.5% Yes) |
| **15-Min Crypto**         | Bitcoin Up/Down    | Variable   | Variable   | High velocity, ~10 updates/sec      |

### 15-Min Crypto Market Characteristics

Live capture analysis (30 seconds, 397 trades):

- **Both sides have liquidity** - trades on both UP and DOWN tokens
- UP token: 223 BUY trades, 36 SELL trades (prices 0.82-0.96)
- DOWN token: 94 BUY trades, 44 SELL trades (prices 0.05-0.18)
- **Math arb IS viable**: Combined ask prices (0.82 + 0.05 = 0.87) can be < $1.00
- Observed edge: **13%** in sample capture

**Note:** Order book snapshots may appear one-sided momentarily due to high velocity (~10 messages/second), but both sides have active trading.

### Order Book Structure

**Standard Markets (Political/General):**

```
Bids: [0.055, 0.053, 0.052, ...] - Multiple levels with depth
Asks: [0.056, 0.061, 0.062, ...] - Multiple levels with depth
Spread: 1 cent typical
```

**15-Min Crypto Markets:**

```
High velocity - book changes rapidly
Both YES and NO tokens have active buyers and sellers
Spread: Variable, can be wide (5-15 cents observed)
```

### Implications for Bot Strategy

1. **Math arb IS viable on 15-min crypto**: Can buy both YES and NO when combined < $1.00
2. **High velocity requires fast execution**: Book state changes rapidly, latency matters
3. **Dynamic fee consideration**: Taker fees up to ~3% at 50/50 odds reduce edge
4. **Standard markets also viable**: Deep books on both sides enable two-legged trades

---

## 6. Type Definitions for Rust

```rust
// Identifiers
pub type ConditionId = String;  // 0x + 64 hex chars
pub type TokenId = String;       // 256-bit integer as string
pub type OrderId = String;       // 0x + 64 hex chars
pub type Address = String;       // 0x + 40 hex chars
pub type TxHash = String;        // 0x + 64 hex chars
pub type Signature = String;     // 0x + 130 hex chars
pub type Uuid = String;          // UUID format

// Price/size are always strings
pub type Price = String;  // "0.48"
pub type Size = String;   // "1234.56"

// Amounts in base units (6 decimals for USDC)
pub type Amount = String; // "1000000" = 1 USDC
```

---

## 7. WebSocket Implementation Notes

### Connection Management

1. **PING every 5 seconds** - Maintain connection
2. **Reconnect on drop** - Connections timeout after ~20 minutes
3. **Multi-asset subscribe** - Add multiple IDs to `assets_ids` array

### Message Types

| event_type         | Description                               |
| ------------------ | ----------------------------------------- |
| `book`             | Full order book snapshot                  |
| `price_change`     | Price update (includes best_bid/best_ask) |
| `last_trade_price` | Trade executed with transaction hash      |
| `trade`            | Trade executed (User WebSocket)           |

### price_change Event Format (MARKET WebSocket)

**VALIDATED FROM LIVE DATA (Research-1, Jan 2026)**

Received on order book changes. Includes both sides of the market in a single message:

```json
{
  "market": "0x7f3c6b9029a1a4a932509c147a2cc0762e1116b7a4568cde472908b29dd4889d",
  "price_changes": [
    {
      "asset_id": "25723597394415499227218361148528714017737096767507235616700212312538360075985",
      "price": "0.994",
      "size": "57742.4",
      "side": "SELL",
      "hash": "4bf700aebb90694adf18f5f41137cce6b0d1f18a",
      "best_bid": "0.944",
      "best_ask": "0.945"
    },
    {
      "asset_id": "93715017999299261588944061464372509395185250817523863925904889301335289765870",
      "price": "0.006",
      "size": "57742.4",
      "side": "BUY",
      "hash": "50dfee44aac0cef19c7e2b97a1d22b78d300ce12",
      "best_bid": "0.055",
      "best_ask": "0.056"
    }
  ],
  "timestamp": "1767728012103",
  "event_type": "price_change"
}
```

**Note:** A `size` of `"0"` indicates the price level was fully consumed.

### last_trade_price Event Format (MARKET WebSocket)

**VALIDATED FROM LIVE DATA (Research-1, Jan 2026)**

Received when a trade executes:

```json
{
  "market": "0x17815081230e3b9c78b098162c33b1ffa68c4ec29c123d3d14989599e0c2e113",
  "asset_id": "11862165566757345985240476164489718219056735011698825377388402888080786399275",
  "price": "0.006",
  "size": "166.666666",
  "fee_rate_bps": "0",
  "side": "BUY",
  "timestamp": "1767728273799",
  "event_type": "last_trade_price",
  "transaction_hash": "0xa329250ab426d9057b99bdb9968111dfbc276ffc4e59037d29dd3a8bca15deed"
}
```

---

## 8. Error Handling

### HTTP Status Codes

| Code | Meaning                      |
| ---- | ---------------------------- |
| 200  | Success                      |
| 400  | Bad request (invalid order)  |
| 401  | Unauthorized (bad signature) |
| 429  | Rate limited                 |
| 500  | Server error                 |

### Complete Error Code Catalog

#### Order Placement Errors (Insert Error Messages)

| Error Code                         | Description                          | Retryable?          |
| ---------------------------------- | ------------------------------------ | ------------------- |
| `INVALID_ORDER_MIN_TICK_SIZE`      | Price breaks minimum tick size rules | No (Fatal)          |
| `INVALID_ORDER_MIN_SIZE`           | Size lower than minimum              | No (Fatal)          |
| `INVALID_ORDER_DUPLICATED`         | Same order already placed            | No (Fatal)          |
| `INVALID_ORDER_NOT_ENOUGH_BALANCE` | Insufficient balance/allowance       | No (Fatal)          |
| `INVALID_ORDER_EXPIRATION`         | Expiration time is in the past       | No (Fatal)          |
| `INVALID_ORDER_ERROR`              | System error inserting order         | No (Fatal)          |
| `EXECUTION_ERROR`                  | System error executing trade         | No (Fatal)          |
| `ORDER_DELAYED`                    | Order match delayed                  | **Yes (Retryable)** |
| `FOK_ORDER_NOT_FILLED_ERROR`       | FOK order couldn't be fully filled   | No (Expected)       |
| `MARKET_NOT_READY`                 | Market not accepting orders yet      | **Yes (Retryable)** |

#### Authentication Errors

| Error Code               | Description            | Retryable? |
| ------------------------ | ---------------------- | ---------- |
| `INVALID_SIGNATURE`      | Bad EIP-712 signature  | No (Fatal) |
| `NONCE_ALREADY_USED`     | Nonce was already used | No (Fatal) |
| `Invalid Funder Address` | Invalid funder address | No (Fatal) |

### Retryable vs Fatal Error Classification

**Retryable Errors (Temporary - can retry after delay):**

- `ORDER_DELAYED` - Order matching was delayed, retry after short backoff
- `MARKET_NOT_READY` - Market not accepting orders yet, check `accepting_orders` field before retry

**Fatal Errors (Permanent - do not retry same request):**

- All `INVALID_ORDER_*` errors - Fix the order parameters before retrying
- Authentication errors - Fix credentials/signature logic
- `EXECUTION_ERROR` - System error, log and investigate

**Expected Errors (Not failures, just unsuccessful):**

- `FOK_ORDER_NOT_FILLED_ERROR` - Expected for FOK orders when liquidity insufficient; do not count toward circuit breaker

**Error Response Format:**

```json
{
  "success": false,
  "errorMsg": "INVALID_ORDER_MIN_TICK_SIZE",
  "orderID": "",
  "takingAmount": "0",
  "makingAmount": "0",
  "status": "",
  "transactionsHashes": []
}
```

Note: `success=true` with non-empty `errorMsg` indicates the order was processed but rejected.

### Rate Limiting

Rate limits use **Cloudflare throttling** - requests are delayed/queued rather than immediately rejected with HTTP 429. This means:

- You may experience increased latency under high request rates
- The API does not return an explicit rate limit error response format
- Practical limits: ~100 requests/minute for REST API, ~10 orders/second

### Closed Market Handling

Before placing orders, check the market's `accepting_orders: boolean` field:

- `accepting_orders: true` - Market is open for trading
- `accepting_orders: false` - Market is closed, orders will fail with `MARKET_NOT_READY`

Markets close before resolution. Always verify market status before order submission.

---

## 9. Rate Limits

- REST API: ~100 requests/minute
- WebSocket: No explicit limit on messages
- Order placement: ~10 orders/second (practical limit)

---

## 10. Quick Reference

### URLs

```
REST:       https://clob.polymarket.com
Gamma:      https://gamma-api.polymarket.com
WS Market:  wss://ws-subscriptions-clob.polymarket.com/ws/market
WS User:    wss://ws-subscriptions-clob.polymarket.com/ws/user
WS Live:    wss://ws-live-data.polymarket.com/
```

### Chain Info

```
Chain ID:   137 (Polygon)
USDC:       6 decimals
```

### Official Resources

```
Docs:       https://docs.polymarket.com/
Rust SDK:   https://github.com/Polymarket/rs-clob-client
Python SDK: https://github.com/Polymarket/py-clob-client
```
