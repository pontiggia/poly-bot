//! Redeem winning positions from resolved Polymarket markets.
//!
//! Usage:
//!   # Auto-discover and redeem ALL redeemable positions:
//!   cargo run --example redeem
//!
//!   # Redeem a specific market by condition ID:
//!   CONDITION_ID=0x... WINNING_INDEX=1 cargo run --example redeem
//!
//! Set POLYGON_RPC_URL in .env for a custom RPC endpoint.
//! WINNING_INDEX: 1 = YES/UP (first outcome), 2 = NO/DOWN (second outcome)

use std::env;
use std::str::FromStr as _;

use alloy::primitives::{FixedBytes, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::LocalSigner;
use alloy::signers::Signer as _;
use alloy::sol;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::types::{address, Address};
use polymarket_client_sdk::{POLYGON, contract_config};
use rust_decimal_macros::dec;

const DEFAULT_RPC: &str = "https://polygon-bor-rpc.publicnode.com";
const USDC: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

sol! {
    #[sol(rpc)]
    interface IConditionalTokens {
        function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint256[] indexSets) external;
    }
}

fn rpc_url() -> String {
    env::var("POLYGON_RPC_URL").unwrap_or_else(|_| DEFAULT_RPC.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let private_key = env::var("PRIVATE_KEY")
        .or_else(|_| env::var("POLYMARKET_PRIVATE_KEY"))
        .expect("Need PRIVATE_KEY or POLYMARKET_PRIVATE_KEY");
    let proxy_wallet = env::var("WALLET_ADDRESS")
        .or_else(|_| env::var("POLYMARKET_PROXY_WALLET"))
        .expect("Need WALLET_ADDRESS or POLYMARKET_PROXY_WALLET");

    let signer = LocalSigner::from_str(&private_key)?.with_chain_id(Some(POLYGON));
    let signer_addr = signer.address().to_checksum(None);
    println!("Signer (EOA):    {}", signer_addr);
    println!("Proxy wallet:    {}", proxy_wallet);
    println!("RPC:             {}", rpc_url());

    // Check if user wants manual mode (specific CONDITION_ID)
    if let Ok(condition_hex) = env::var("CONDITION_ID") {
        println!("\n--- Manual redemption mode ---");
        let condition_id: [u8; 32] = hex::decode(condition_hex.trim_start_matches("0x"))?
            .try_into()
            .expect("CONDITION_ID must be 32 bytes");
        let winning_index: u64 = env::var("WINNING_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1u64);

        let provider = ProviderBuilder::new()
            .wallet(signer.clone())
            .connect(&rpc_url())
            .await?;
        let cfg = contract_config(POLYGON, false).expect("contract_config");
        let ctf = IConditionalTokens::new(cfg.conditional_tokens, provider);

        let parent: FixedBytes<32> = FixedBytes::from([0u8; 32]);
        let cond: FixedBytes<32> = FixedBytes::from(condition_id);
        let index_sets = vec![U256::from(winning_index)];

        println!(
            "Redeeming condition {} with index_set={}...",
            condition_hex, winning_index
        );
        let tx_hash = ctf
            .redeemPositions(USDC, parent, cond, index_sets)
            .send()
            .await?
            .watch()
            .await?;
        println!("Redeem tx: {:?}", tx_hash);
        return Ok(());
    }

    // --- Auto-discovery mode ---
    println!("\n--- Auto-discovery: scanning both EOA signer and proxy wallet ---\n");

    let data_client = polymarket_client_sdk::data::Client::default();

    // Query BOTH addresses — tokens can be on either
    let signer_address: Address = signer_addr.parse().expect("Invalid signer address");
    let proxy_addr: Address = proxy_wallet.parse().expect("Invalid proxy wallet address");

    let mut all_positions = Vec::new();

    for (label, addr) in [("EOA signer", signer_address), ("Proxy wallet", proxy_addr)] {
        let request = PositionsRequest::builder()
            .user(addr)
            .build();

        match data_client.positions(&request).await {
            Ok(positions) => {
                println!("[{}] {} — found {} position(s)", label, addr, positions.len());
                all_positions.extend(positions);
            }
            Err(e) => {
                println!("[{}] {} — query failed: {}", label, addr, e);
            }
        }
    }
    println!();

    if all_positions.is_empty() {
        println!("No positions found on either address.");
        return Ok(());
    }

    let redeemable: Vec<_> = all_positions.iter().filter(|p| p.redeemable).collect();
    let winning: Vec<_> = redeemable.iter().filter(|p| p.current_value > dec!(0)).collect();
    let losing: Vec<_> = redeemable.iter().filter(|p| p.current_value <= dec!(0)).collect();
    let non_redeemable: Vec<_> = all_positions.iter().filter(|p| !p.redeemable).collect();

    if !non_redeemable.is_empty() {
        println!("Open positions (not yet resolved):");
        for pos in &non_redeemable {
            println!(
                "  {} — {} x{} @ {} (current value: {})",
                pos.title, pos.outcome, pos.size, pos.avg_price, pos.current_value
            );
        }
        println!();
    }

    if !losing.is_empty() {
        println!("Losing positions (resolved to $0, nothing to claim):");
        for pos in &losing {
            println!(
                "  {} — {} x{} (lost ${:.2})",
                pos.title, pos.outcome, pos.size,
                pos.cash_pnl.abs()
            );
        }
        println!();
    }

    if winning.is_empty() {
        println!("No winning redeemable positions found.");
        if !non_redeemable.is_empty() {
            println!(
                "You have {} open position(s) still waiting to resolve.",
                non_redeemable.len()
            );
        }
        return Ok(());
    }

    println!("WINNING positions to redeem:");
    for pos in &winning {
        println!(
            "  {} — {} x{} → ${:.2} USDC",
            pos.title, pos.outcome, pos.size, pos.current_value
        );
        println!(
            "    Condition: {} (neg_risk: {})",
            pos.condition_id, pos.negative_risk
        );
    }

    println!("\nProceed with redemption? (y/N)");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("Aborted.");
        return Ok(());
    }

    let provider = ProviderBuilder::new()
        .wallet(signer.clone())
        .connect(&rpc_url())
        .await?;

    let parent: FixedBytes<32> = FixedBytes::from([0u8; 32]);

    for pos in &winning {
        let cond: FixedBytes<32> = pos.condition_id;

        let cfg =
            contract_config(POLYGON, pos.negative_risk).expect("contract_config");
        let ctf = IConditionalTokens::new(cfg.conditional_tokens, provider.clone());

        // outcome_index 0 (YES/UP) => bitmask 1, index 1 (NO/DOWN) => bitmask 2
        let index_set = 1u64 << pos.outcome_index;
        let index_sets = vec![U256::from(index_set)];

        println!(
            "Redeeming {} {} (index_set={})...",
            pos.title, pos.outcome, index_set
        );

        match ctf
            .redeemPositions(USDC, parent, cond, index_sets)
            .send()
            .await
        {
            Ok(pending) => match pending.watch().await {
                Ok(tx_hash) => println!("  Success! Tx: {:?}", tx_hash),
                Err(e) => println!("  Tx failed: {}", e),
            },
            Err(e) => println!("  Send failed: {}", e),
        }
    }

    println!("\nDone! Check your USDC balance on Polygonscan.");
    Ok(())
}
