use std::env;

use polymarket_bot::api::gamma::GammaClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Accept token id via env var TOKEN_ID or first command-line arg
    let token_id = env::args().nth(1).or_else(|| env::var("TOKEN_ID").ok())
        .expect("Provide TOKEN_ID as first arg or TOKEN_ID env var");

    let client = GammaClient::new();

    println!("Searching Gamma API for token id: {}", token_id);

    // Fetch all events (including closed/archived) and search through markets
    // This helps locate condition IDs for markets that are no longer active.
    let events = client.get_all_events().await?;

    for event in events {
        for market in event.markets {
            if let Ok(tokens) = market.parse_token_ids() {
                for (i, t) in tokens.iter().enumerate() {
                    if t == &token_id {
                        println!("Found token in event slug={} market question=\"{}\"", event.slug, market.question);
                        println!("Market flags: active={} closed={} archived={} accepting_orders={}", market.active, market.closed, market.archived, market.accepting_orders);
                        println!("Condition ID: {}", market.condition_id);
                        println!("Outcomes: {:?}", market.parse_outcomes()?);
                        println!("Matching token index (0-based): {}", i);
                        println!("Binary indexSet for this token (1-based bitmask): {}", 1u64 << i);
                        return Ok(());
                    }
                }
            }
        }
    }

    println!("Token id not found in active events. Consider running discover_all_crypto_markets or checking closed/archived events.");
    Ok(())
}
