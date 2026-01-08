1. Validate API Assumptions Against Live Data

We have documentation, but we haven't verified:

- Actual WebSocket message formats (do they match our type definitions?)
- Real order book depth on 15-min crypto markets (are they as thin as claimed?)
- Actual fill rates and partial fill frequency
- Current fee structure (maker/taker bps)

Action: Write a simple read-only script that connects to the WebSocket and logs raw messages for 30-60 minutes. Verify our types parse correctly.

2. Missing: Error Response Catalog

Our plan handles success paths well, but we don't have a catalog of:

- What error codes does the CLOB API return?
- Which errors are retryable vs fatal?
- What does a rate limit response look like?
- What happens when you submit an order for a closed market?

Action: Read the API docs more carefully for error responses, or find them empirically.

3. Missing: Specific Market Selection Criteria

We say "15-min crypto markets" but:

- How do we identify them programmatically?
- What's the API call to filter these?
- Are there other high-frequency market types worth targeting?

Action: Query the Gamma API and understand the market structure/filtering.

4. Unclear: Alloy vs Ethers-rs

We switched to alloy but:

- Is alloy stable enough for production?
- Does it have the same EIP-712 signing capabilities?
- Are there working examples of Polymarket signing with alloy?

Action: Verify alloy can do what we need, or stick with ethers-rs if safer.
